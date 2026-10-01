//! Anthropic Messages 入站（Claude Code）的请求改写。
//!
//! 取自 cc-switch `proxy/providers/claude.rs`：厂商兼容修正逻辑原样保留，
//! 只把"从 settings_config 快照里找 base_url"换成读 `ProviderSpec`。

use serde_json::{json, Value};

use super::RequestContext;
use crate::provider::{ApiFormat, ProviderSpec};
use crate::proxy::providers::{transform, transform_gemini, transform_responses};
use crate::proxy::ProxyError;

const ANTHROPIC_THINKING_PLACEHOLDER: &str = "tool call";
const ANTHROPIC_REDACTED_THINKING_PLACEHOLDER: &str = "[redacted thinking]";
// Keep hints lowercase; matching lowercases only the input value.
const REASONING_VENDOR_HINTS: &[&str] = &["moonshot", "kimi", "deepseek", "mimo", "xiaomimimo"];
/// DeepSeek official Anthropic-compatible endpoint URL
const DEEPSEEK_OFFICIAL_ANTHROPIC_URL: &str = "https://api.deepseek.com/anthropic";
const REASONING_ENCRYPTED_CONTENT: &str = "reasoning.encrypted_content";

/// 把 Claude Code 的请求改写成上游协议
pub fn transform_request(
    spec: &ProviderSpec,
    upstream_format: ApiFormat,
    mut body: Value,
    ctx: &RequestContext<'_>,
) -> Result<Value, ProxyError> {
    match upstream_format {
        ApiFormat::Anthropic => {
            normalize_anthropic_messages(&mut body, spec);
            Ok(body)
        }
        ApiFormat::OpenaiResponses => to_responses(spec, body, ctx),
        ApiFormat::OpenaiChat => to_chat(spec, body),
        ApiFormat::GeminiNative => transform_gemini::anthropic_to_gemini_with_shadow(
            body,
            ctx.gemini_shadow,
            Some(&spec.id),
            ctx.client_session_id,
        ),
    }
}

fn to_responses(
    spec: &ProviderSpec,
    body: Value,
    ctx: &RequestContext<'_>,
) -> Result<Value, ProxyError> {
    let session_cache_key = session_cache_key(spec, &body, ctx);
    let cache_key = spec
        .prompt_cache_key
        .as_deref()
        .or(session_cache_key.as_deref());

    // Codex OAuth（ChatGPT 后端）需要 store: false + include reasoning.encrypted_content，
    // 由转换层统一处理
    let mut result = transform_responses::anthropic_to_responses(
        body,
        cache_key,
        spec.is_codex_oauth(),
        spec.codex_fast_mode,
    )?;

    if spec.is_xai_oauth() {
        let mut include = result
            .get("include")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !include
            .iter()
            .any(|item| item.as_str() == Some(REASONING_ENCRYPTED_CONTENT))
        {
            include.push(json!(REASONING_ENCRYPTED_CONTENT));
        }
        result["include"] = json!(include);
    }
    Ok(result)
}

fn to_chat(spec: &ProviderSpec, body: Value) -> Result<Value, ProxyError> {
    let preserve_reasoning_content = should_preserve_reasoning_content(spec, &body);
    let mut result =
        transform::anthropic_to_openai_with_reasoning_content(body, preserve_reasoning_content)?;
    // Chat 上游只在显式配置时带 prompt_cache_key：未知网关遇到陌生字段常直接 400
    if let Some(key) = spec.prompt_cache_key.as_deref() {
        result["prompt_cache_key"] = json!(key);
    }
    // 流式请求必须注入 stream_options.include_usage，否则 OpenAI 兼容上游
    // 不在 SSE 末尾吐 usage → 转换出的 Anthropic message_delta 全 0 → 用量漏记
    transform::inject_openai_stream_include_usage(&mut result);
    Ok(result)
}

/// Responses 上游的 prompt_cache_key 来源。
///
/// Copilot 从 `metadata.user_id` 的 `_session_` 后缀或 `metadata.session_id` 取，
/// 让同一会话共享缓存；其他上游用客户端自带的会话 ID。
fn session_cache_key(
    spec: &ProviderSpec,
    body: &Value,
    ctx: &RequestContext<'_>,
) -> Option<String> {
    if spec.is_github_copilot() {
        let metadata = body.get("metadata");
        return metadata
            .and_then(|m| m.get("user_id"))
            .and_then(Value::as_str)
            .and_then(parse_session_from_user_id)
            .or_else(|| {
                metadata
                    .and_then(|m| m.get("session_id"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            });
    }
    ctx.client_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

/// 从 `user_xxx_session_yyy` 形式的 user_id 取出会话 ID。
///
/// 与 `proxy::session::parse_session_from_user_id` 相同；那边是 `pub(super)`，
/// 为保持搬运文件与上游一致，这里不改它的可见性，复制一份。
fn parse_session_from_user_id(user_id: &str) -> Option<String> {
    let (_, session_id) = user_id.split_once("_session_")?;
    (!session_id.is_empty()).then(|| session_id.to_string())
}

/// Anthropic 上游的厂商兼容修正。返回是否改动了请求体。
pub fn normalize_anthropic_messages(body: &mut Value, spec: &ProviderSpec) -> bool {
    let mut changed = normalize_tool_thinking_history_for_provider(body, spec);
    changed |= normalize_deepseek_thinking_disabled_strip_effort(body, spec);
    changed
}

fn is_reasoning_vendor_identifier(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    REASONING_VENDOR_HINTS
        .iter()
        .any(|hint| value.contains(hint))
}

/// 模型名或上游地址带推理厂商标识（Kimi / DeepSeek / MiMo）
fn targets_reasoning_vendor(spec: &ProviderSpec, body: &Value) -> bool {
    body.get("model")
        .and_then(Value::as_str)
        .is_some_and(is_reasoning_vendor_identifier)
        || is_reasoning_vendor_identifier(&spec.base_url)
}

/// DeepSeek's Anthropic-compatible endpoint requires thinking history to be
/// replayed on every assistant turn that contains tool_use. Some Anthropic SDK
/// clients keep the tool history but drop or redact the thinking block, which
/// makes DeepSeek reject the next request with `content[].thinking ... must be
/// passed back`. Normalize only the narrow tool-call history shape for
/// providers known to require plain `thinking` blocks.
fn normalize_tool_thinking_history_for_provider(body: &mut Value, spec: &ProviderSpec) -> bool {
    if !targets_reasoning_vendor(spec, body) {
        return false;
    }
    normalize_tool_thinking_history(body)
}

fn is_deepseek_official_anthropic_endpoint(spec: &ProviderSpec) -> bool {
    spec.base_url.trim().trim_end_matches('/') == DEEPSEEK_OFFICIAL_ANTHROPIC_URL
}

/// DeepSeek's official Anthropic-compatible endpoint treats
/// `thinking: { type: "disabled" }` and effort parameters (`output_config.effort`
/// or `reasoning_effort`) as mutually exclusive, returning HTTP 400:
/// "thinking options type cannot be disabled when reasoning_effort is set".
/// This breaks Claude Code 2.1.166+ Workflow/Dynamic Workflow features.
///
/// Rather than overriding Claude Code's intentional `thinking: disabled` for
/// sub-agents, we respect that decision and remove the conflicting effort
/// parameters instead.
///
/// <https://github.com/deepseek-ai/DeepSeek-V3/issues/1397>
fn normalize_deepseek_thinking_disabled_strip_effort(
    body: &mut Value,
    spec: &ProviderSpec,
) -> bool {
    if !is_deepseek_official_anthropic_endpoint(spec) {
        return false;
    }

    let thinking_type = body
        .get("thinking")
        .and_then(|t| t.get("type"))
        .and_then(|t| t.as_str());
    if thinking_type != Some("disabled") {
        return false;
    }

    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    let mut changed = false;

    // Remove output_config.effort (Anthropic format)
    let output_config_now_empty = match obj.get_mut("output_config").and_then(Value::as_object_mut)
    {
        Some(oc) => {
            changed |= oc.remove("effort").is_some();
            oc.is_empty()
        }
        None => false,
    };
    if output_config_now_empty {
        obj.remove("output_config");
    }

    // Remove reasoning_effort (OpenAI format, may be present in passthrough)
    changed |= obj.remove("reasoning_effort").is_some();

    changed
}

fn normalize_tool_thinking_history(body: &mut Value) -> bool {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };

    let mut changed = false;
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }

        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        if !content
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        {
            continue;
        }

        let mut has_thinking = false;
        for block in content.iter_mut() {
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => {
                    let has_non_empty_thinking = block
                        .get("thinking")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.trim().is_empty());
                    if let Some(obj) = block.as_object_mut() {
                        if obj.remove("signature").is_some() {
                            changed = true;
                        }
                        if !has_non_empty_thinking {
                            obj.insert(
                                "thinking".to_string(),
                                json!(ANTHROPIC_THINKING_PLACEHOLDER),
                            );
                            changed = true;
                        }
                    }
                    has_thinking = true;
                }
                Some("redacted_thinking") => {
                    *block = json!({
                        "type": "thinking",
                        "thinking": ANTHROPIC_REDACTED_THINKING_PLACEHOLDER
                    });
                    has_thinking = true;
                    changed = true;
                }
                _ => {}
            }
        }

        if !has_thinking {
            content.insert(
                0,
                json!({
                    "type": "thinking",
                    "thinking": ANTHROPIC_THINKING_PLACEHOLDER
                }),
            );
            changed = true;
        }
    }

    changed
}

fn should_preserve_reasoning_content(spec: &ProviderSpec, body: &Value) -> bool {
    targets_reasoning_vendor(spec, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderAuth;
    use crate::proxy::providers::gemini_shadow::GeminiShadowStore;

    fn spec(base_url: &str, format: ApiFormat) -> ProviderSpec {
        ProviderSpec::new("test", "Test", base_url, format, ProviderAuth::api_key("k"))
    }

    fn transform(spec: &ProviderSpec, body: Value, session: Option<&str>) -> Value {
        let ctx = RequestContext {
            client_session_id: session,
            gemini_shadow: None,
        };
        transform_request(spec, spec.effective_api_format(), body, &ctx).unwrap()
    }

    fn hello(model: &str) -> Value {
        json!({
            "model": model,
            "messages": [{ "role": "user", "content": "hello" }],
            "max_tokens": 128
        })
    }

    fn tool_turn(model: &str) -> Value {
        json!({
            "model": model,
            "max_tokens": 64,
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "I should call the tool."},
                    {"type": "tool_use", "id": "call_123", "name": "get_weather", "input": {"location": "Tokyo"}}
                ]
            }]
        })
    }

    // ---- Responses 上游 ----

    #[test]
    fn responses_upstream_basic_shape() {
        let out = transform(
            &spec("https://api.openai.example.com", ApiFormat::OpenaiResponses),
            hello("gpt-5.4"),
            None,
        );
        assert_eq!(out["model"], "gpt-5.4");
        assert!(out.get("input").is_some());
        assert!(out.get("max_output_tokens").is_some());
    }

    #[test]
    fn responses_upstream_cache_key_from_session_only_when_present() {
        let s = spec("https://api.openai.example.com", ApiFormat::OpenaiResponses);
        assert_eq!(
            transform(&s, hello("gpt-5.4"), Some("claude-session-123"))["prompt_cache_key"],
            "claude-session-123"
        );
        assert!(transform(&s, hello("gpt-5.4"), None)
            .get("prompt_cache_key")
            .is_none());
    }

    #[test]
    fn codex_oauth_session_key_explicit_key_and_store_flags() {
        let mut s = ProviderSpec::new(
            "chatgpt",
            "ChatGPT",
            "",
            ApiFormat::Anthropic,
            ProviderAuth::CodexOauth { account_id: None },
        );

        let out = transform(&s, hello("gpt-5.4"), Some("session-123"));
        assert_eq!(out["prompt_cache_key"], "session-123");
        assert_eq!(out["store"], json!(false));
        assert!(out.get("service_tier").is_none());
        assert_eq!(out["include"], json!([REASONING_ENCRYPTED_CONTENT]));

        s.prompt_cache_key = Some("explicit-cache-key".into());
        assert_eq!(
            transform(&s, hello("gpt-5.4"), Some("session-123"))["prompt_cache_key"],
            "explicit-cache-key"
        );

        s.codex_fast_mode = true;
        assert_eq!(
            transform(&s, hello("gpt-5.4"), None)["service_tier"],
            "priority"
        );
    }

    #[test]
    fn xai_oauth_forces_responses_and_encrypted_reasoning() {
        let s = ProviderSpec::new(
            "grok",
            "Grok",
            "https://attacker.example/anthropic",
            ApiFormat::Anthropic,
            ProviderAuth::XaiOauth { account_id: None },
        );
        let out = transform(
            &s,
            json!({
                "model": "grok-4.5",
                "max_tokens": 2048,
                "thinking": { "type": "enabled", "budget_tokens": 20000 },
                "messages": [{ "role": "user", "content": "hello" }]
            }),
            None,
        );
        assert_eq!(out["reasoning"]["effort"], json!("high"));
        assert_eq!(out["include"], json!([REASONING_ENCRYPTED_CONTENT]));
        assert!(out.get("store").is_none());
    }

    #[test]
    fn copilot_cache_key_comes_from_metadata_not_gateway_session() {
        let s = ProviderSpec::new(
            "copilot",
            "Copilot",
            "",
            ApiFormat::OpenaiResponses,
            ProviderAuth::GithubCopilot { account_id: None },
        );
        let mut body = hello("gpt-5.4");
        body["metadata"] = json!({ "user_id": "user_abc_session_sess-42" });
        assert_eq!(
            transform(&s, body, Some("ignored"))["prompt_cache_key"],
            "sess-42"
        );
    }

    // ---- Chat 上游 ----

    #[test]
    fn chat_upstream_stream_options_only_when_streaming() {
        let s = spec("https://openrouter.ai/api/v1", ApiFormat::OpenaiChat);
        let mut streaming = hello("moonshotai/kimi-k2");
        streaming["stream"] = json!(true);
        let out = transform(&s, streaming, None);
        assert_eq!(out["stream"], true);
        assert_eq!(out["stream_options"]["include_usage"], true);

        assert!(transform(&s, hello("moonshotai/kimi-k2"), None)
            .get("stream_options")
            .is_none());
    }

    #[test]
    fn chat_upstream_prompt_cache_key_only_when_explicit() {
        let mut s = spec("https://api.example.com", ApiFormat::OpenaiChat);
        assert!(transform(&s, hello("gpt-5.4"), Some("session"))
            .get("prompt_cache_key")
            .is_none());

        s.prompt_cache_key = Some("claude-cache-route".into());
        assert_eq!(
            transform(&s, hello("gpt-5.4"), None)["prompt_cache_key"],
            "claude-cache-route"
        );
    }

    #[test]
    fn chat_upstream_reasoning_content_only_for_reasoning_vendors() {
        let generic = transform(
            &spec("https://api.example.com", ApiFormat::OpenaiChat),
            tool_turn("gpt-5.4"),
            None,
        );
        assert!(generic["messages"][0].get("tool_calls").is_some());
        assert!(generic["messages"][0].get("reasoning_content").is_none());

        for (base_url, model) in [
            ("https://api.moonshot.cn/v1", "kimi-k2.6"),
            ("https://api.deepseek.com/v1", "deepseek-v4-flash"),
            ("https://api.xiaomimimo.com/v1", "mimo-v2.5-pro"),
        ] {
            let out = transform(
                &spec(base_url, ApiFormat::OpenaiChat),
                tool_turn(model),
                None,
            );
            let msg = &out["messages"][0];
            assert_eq!(
                msg["reasoning_content"], "I should call the tool.",
                "{base_url}"
            );
            assert!(msg.get("tool_calls").is_some());
        }
    }

    // ---- Gemini 上游 ----

    #[test]
    fn gemini_upstream_shape() {
        let shadow = GeminiShadowStore::default();
        let ctx = RequestContext {
            client_session_id: None,
            gemini_shadow: Some(&shadow),
        };
        let out = transform_request(
            &spec(
                "https://generativelanguage.googleapis.com",
                ApiFormat::GeminiNative,
            ),
            ApiFormat::GeminiNative,
            json!({
                "model": "gemini-2.5-pro",
                "system": "You are helpful.",
                "messages": [{ "role": "user", "content": "hello" }],
                "max_tokens": 64
            }),
            &ctx,
        )
        .unwrap();

        assert!(out.get("contents").is_some());
        assert_eq!(
            out["systemInstruction"]["parts"][0]["text"],
            "You are helpful."
        );
        assert_eq!(out["generationConfig"]["maxOutputTokens"], 64);
    }

    // ---- Anthropic 上游的厂商修正 ----

    fn tool_history(content: Value) -> Value {
        json!({
            "model": "deepseek-v4-pro",
            "messages": [{ "role": "assistant", "content": content }]
        })
    }

    #[test]
    fn reasoning_vendor_tool_history_injects_missing_thinking() {
        for base_url in [
            "https://api.deepseek.com/anthropic",
            "https://api.kimi.com/coding",
        ] {
            let mut body = tool_history(json!([
                {"type": "text", "text": "I will inspect the repo."},
                {"type": "tool_use", "id": "call_123", "name": "read_file", "input": {"path": "README.md"}}
            ]));
            body["model"] = json!("some-model");
            assert!(normalize_anthropic_messages(
                &mut body,
                &spec(base_url, ApiFormat::Anthropic)
            ));
            let content = body["messages"][0]["content"].as_array().unwrap();
            assert_eq!(content[0]["type"], "thinking");
            assert_eq!(content[0]["thinking"], ANTHROPIC_THINKING_PLACEHOLDER);
            assert_eq!(content.last().unwrap()["type"], "tool_use");
        }
    }

    #[test]
    fn reasoning_vendor_tool_history_rewrites_redacted_and_drops_signature() {
        let s = spec("https://api.deepseek.com/anthropic", ApiFormat::Anthropic);

        let mut redacted = tool_history(json!([
            {"type": "redacted_thinking", "data": "opaque"},
            {"type": "tool_use", "id": "call_123", "name": "read_file", "input": {}}
        ]));
        assert!(normalize_anthropic_messages(&mut redacted, &s));
        let block = &redacted["messages"][0]["content"][0];
        assert_eq!(block["thinking"], ANTHROPIC_REDACTED_THINKING_PLACEHOLDER);
        assert!(block.get("data").is_none());

        let mut signed = tool_history(json!([
            {"type": "thinking", "thinking": "Need to inspect the file.", "signature": "sig"},
            {"type": "tool_use", "id": "call_123", "name": "read_file", "input": {}}
        ]));
        assert!(normalize_anthropic_messages(&mut signed, &s));
        let block = &signed["messages"][0]["content"][0];
        assert_eq!(block["thinking"], "Need to inspect the file.");
        assert!(block.get("signature").is_none());
    }

    #[test]
    fn generic_anthropic_history_and_system_messages_are_untouched() {
        let mut body = json!({
            "system": "Existing top-level system.",
            "model": "claude-sonnet-4.6",
            "messages": [
                { "role": "system", "content": "Message system one." },
                {
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "c", "name": "read_file", "input": {}}]
                }
            ]
        });
        let original = body.clone();
        assert!(!normalize_anthropic_messages(
            &mut body,
            &spec("https://api.example.com/anthropic", ApiFormat::Anthropic)
        ));
        assert_eq!(body, original);
    }

    fn deepseek_disabled(extra: Value) -> Value {
        let mut body = json!({
            "model": "deepseek-v4-pro",
            "thinking": { "type": "disabled" },
            "max_tokens": 100000
        });
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    }

    #[test]
    fn deepseek_official_strips_effort_when_thinking_disabled() {
        let s = spec("https://api.deepseek.com/anthropic/", ApiFormat::Anthropic);

        let mut both = deepseek_disabled(json!({
            "output_config": { "effort": "max" },
            "reasoning_effort": "high"
        }));
        assert!(normalize_deepseek_thinking_disabled_strip_effort(
            &mut both, &s
        ));
        assert_eq!(both["thinking"]["type"], "disabled");
        assert!(both.get("output_config").is_none());
        assert!(both.get("reasoning_effort").is_none());

        let mut keeps_other = deepseek_disabled(json!({
            "output_config": { "effort": "max", "temperature": 0.5 }
        }));
        assert!(normalize_deepseek_thinking_disabled_strip_effort(
            &mut keeps_other,
            &s
        ));
        assert_eq!(keeps_other["output_config"]["temperature"], 0.5);
        assert!(keeps_other["output_config"].get("effort").is_none());

        let mut nothing = deepseek_disabled(json!({}));
        let original = nothing.clone();
        assert!(!normalize_deepseek_thinking_disabled_strip_effort(
            &mut nothing,
            &s
        ));
        assert_eq!(nothing, original);
    }

    #[test]
    fn deepseek_effort_kept_when_thinking_enabled_or_other_endpoint() {
        let official = spec("https://api.deepseek.com/anthropic", ApiFormat::Anthropic);
        for thinking in [
            json!({ "type": "enabled", "budget_tokens": 16000 }),
            json!({ "type": "adaptive" }),
        ] {
            let mut body = json!({
                "model": "deepseek-v4-pro",
                "thinking": thinking,
                "output_config": { "effort": "max" }
            });
            let original = body.clone();
            assert!(!normalize_deepseek_thinking_disabled_strip_effort(
                &mut body, &official
            ));
            assert_eq!(body, original);
        }

        for base_url in [
            "https://other-api.com/anthropic",
            "https://api.anthropic.com",
        ] {
            let mut body = deepseek_disabled(json!({ "output_config": { "effort": "max" } }));
            let original = body.clone();
            assert!(!normalize_deepseek_thinking_disabled_strip_effort(
                &mut body,
                &spec(base_url, ApiFormat::Anthropic)
            ));
            assert_eq!(body, original, "{base_url}");
        }
    }
}
