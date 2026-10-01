//! OpenAI Responses 入站（Codex）的请求改写。
//!
//! 取自 cc-switch `proxy/providers/codex.rs` 与 forwarder 的 Codex 分支。
//! reasoning 参数推断、prompt_cache_key 路由规则原样保留；去掉了从 Codex
//! `config.toml` 快照里解析 base_url / wire_api / model 的部分，这些在 tern 里
//! 是 `ProviderSpec` 的显式字段，模型名由路由层决定。

use serde_json::Value;

use super::RequestContext;
use crate::provider::{ApiFormat, CodexChatReasoningConfig, PromptCacheRouting, ProviderSpec};
use crate::proxy::cache_injector;
use crate::proxy::providers::{
    transform_codex_anthropic, transform_codex_chat, transform_codex_responses_namespace,
    transform_codex_responses_xai_sanitize,
};
use crate::proxy::types::OptimizerConfig;
use crate::proxy::ProxyError;

/// Anthropic 要求 max_tokens；只在 Codex 请求没带 max_output_tokens 时兜底（很少见）。
/// 取保守值，避免低输出上限的模型或中转站直接 400（400 不可重试）；
/// 8192 被当前所有 Claude 模型和几乎所有网关接受，转换层会把 thinking 预算压到它以下。
const DEFAULT_CODEX_ANTHROPIC_MAX_TOKENS: u64 = 8192;

/// 把 Codex 的 Responses 请求改写成上游协议。
///
/// 上游是 Chat 时，若需要补全工具调用历史（`CodexChatHistoryStore::enrich_request`），
/// 转发层应在调用本函数之前对 Responses 请求体执行。
pub fn transform_request(
    spec: &ProviderSpec,
    upstream_format: ApiFormat,
    body: Value,
    ctx: &RequestContext<'_>,
) -> Result<Value, ProxyError> {
    match upstream_format {
        ApiFormat::OpenaiResponses => passthrough(spec, body),
        ApiFormat::OpenaiChat => to_chat(spec, body, ctx),
        ApiFormat::Anthropic => to_anthropic(spec, body),
        ApiFormat::GeminiNative => Err(ProxyError::ConfigError(format!(
            "Codex 暂不支持 Gemini Native 上游（供应商 {}）",
            spec.id
        ))),
    }
}

/// 原生 Responses 透传。严格的第三方网关（目前是 xAI）需要额外清理：
/// 先把 Codex 私有的 `namespace` 工具展平成顶层 function，再删掉 xAI 不认识的字段。
/// 顺序不能反：展平后的工具才能通过清理阶段的工具类型白名单。
/// 响应侧的名称还原由转发层用 `namespace_restore_map` 完成。
fn passthrough(spec: &ProviderSpec, mut body: Value) -> Result<Value, ProxyError> {
    if spec.is_xai_oauth() {
        if transform_codex_responses_namespace::flatten_request_namespaces(&mut body)? {
            log::debug!(
                "[Codex] 已为原生 Responses 上游展平 namespace 工具（{}）",
                spec.id
            );
        }
        if transform_codex_responses_xai_sanitize::sanitize_xai_responses_request(&mut body) {
            log::debug!("[Codex] 已清理 xAI 不支持的 Responses 字段（{}）", spec.id);
        }
    }
    Ok(body)
}

fn to_chat(
    spec: &ProviderSpec,
    body: Value,
    ctx: &RequestContext<'_>,
) -> Result<Value, ProxyError> {
    let explicit_prompt_cache_key = body
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let reasoning_config = resolve_reasoning_config(spec, &body);
    let mut chat_body = transform_codex_chat::responses_to_chat_completions_with_reasoning(
        body,
        reasoning_config.as_ref(),
    )?;
    inject_chat_prompt_cache_key(
        spec,
        &mut chat_body,
        explicit_prompt_cache_key.as_deref(),
        ctx.client_session_id,
    );
    Ok(chat_body)
}

fn to_anthropic(spec: &ProviderSpec, mut body: Value) -> Result<Value, ProxyError> {
    // Codex 不会把 model_max_output_tokens 放进请求体，以供应商配置为准，
    // 优先于请求自带的值。注入到请求体（而不是转换后覆盖）能让 thinking 预算
    // 按真实上限留余量。按供应商配置，避免全局大默认值在低上限网关上 400。
    if let Some(max_out) = spec.max_output_tokens.filter(|v| *v > 0) {
        body["max_output_tokens"] = Value::from(max_out);
    }
    let mut anthropic_body = transform_codex_anthropic::responses_request_to_anthropic(
        body,
        DEFAULT_CODEX_ANTHROPIC_MAX_TOKENS,
    )?;
    // 开启 Anthropic prompt caching（不需要 beta 头），否则 system / tools / 历史
    // 每轮都按全价重发，成本和首 token 延迟都会上去
    cache_injector::inject(
        &mut anthropic_body,
        &OptimizerConfig {
            enabled: true,
            thinking_optimizer: false,
            cache_injection: true,
        },
    );
    Ok(anthropic_body)
}

/// Chat 上游的 reasoning 参数：显式配置优先，否则按名称 / 地址 / 模型推断
pub fn resolve_reasoning_config(
    spec: &ProviderSpec,
    body: &Value,
) -> Option<CodexChatReasoningConfig> {
    if let Some(config) = spec.reasoning.clone() {
        return Some(normalize_reasoning_config(config));
    }
    infer_reasoning_config(spec, body)
}

fn normalize_reasoning_config(mut config: CodexChatReasoningConfig) -> CodexChatReasoningConfig {
    if config.supports_effort.unwrap_or(false) && config.supports_thinking.is_none() {
        config.supports_thinking = Some(true);
    }
    config
}

fn infer_reasoning_config(spec: &ProviderSpec, body: &Value) -> Option<CodexChatReasoningConfig> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let base_url = spec.base_url.to_ascii_lowercase();
    let name = spec.name.to_ascii_lowercase();

    // 平台优先：聚合 / 托管平台的 reasoning 接口由平台的推理框架决定，而非模型官方实现，
    // 因此先按平台标识（仅 name + base_url，不含 model 名）判定并覆盖模型规则。
    if let Some(config) = infer_aggregator_platform_config(&name, &base_url) {
        return Some(config);
    }

    let haystack = format!("{name} {base_url} {model}");
    let config = |thinking: &str, effort: Option<&str>, mode: Option<&str>, output: &str| {
        CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(effort.is_some()),
            thinking_param: Some(thinking.to_string()),
            effort_param: Some(effort.unwrap_or("none").to_string()),
            effort_value_mode: mode.map(ToString::to_string),
            output_format: Some(output.to_string()),
        }
    };

    if haystack.contains("deepseek") {
        return Some(config(
            "thinking",
            Some("reasoning_effort"),
            Some("deepseek"),
            "reasoning_content",
        ));
    }

    // StepFun：仅 step-3.5-flash-2603 这一版支持 reasoning effort（low/high 两档），
    // 其余 step 模型不暴露 effort，故 supports_effort 仅对含 "2603" 的模型置真。
    // 第二个 OR 分支覆盖「经中转/聚合跑该模型、但平台 name/base_url 不含 stepfun」的情况。
    if haystack.contains("stepfun") || haystack.contains("step-3.5-flash-2603") {
        return Some(CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(model.contains("2603")),
            thinking_param: Some("none".to_string()),
            effort_param: Some("reasoning_effort".to_string()),
            effort_value_mode: Some("low_high".to_string()),
            output_format: Some("reasoning".to_string()),
        });
    }

    if haystack.contains("kimi") || haystack.contains("moonshot") {
        return Some(config("thinking", None, None, "reasoning_content"));
    }
    if haystack.contains("glm") || haystack.contains("zhipu") || haystack.contains("z.ai") {
        return Some(config("thinking", None, None, "reasoning_content"));
    }
    if haystack.contains("qwen") || haystack.contains("dashscope") || haystack.contains("bailian") {
        return Some(config("enable_thinking", None, None, "reasoning_content"));
    }
    if haystack.contains("minimax") {
        return Some(config("reasoning_split", None, None, "reasoning_details"));
    }
    if haystack.contains("mimo") {
        return Some(config("thinking", None, None, "reasoning_content"));
    }

    None
}

/// 聚合 / 托管平台的 reasoning 接口由平台决定：同一个模型在不同平台参数可能完全不同
/// （DeepSeek 官方用 `thinking:{type}`、SiliconFlow 用 `enable_thinking`、
/// OpenRouter 用原生 `reasoning:{effort}` 对象）。仅以平台标识（name / base_url）判定，
/// 绝不掺入 model 名——model 名属于模型厂商，会把托管平台误判成模型官方接口。
fn infer_aggregator_platform_config(
    name: &str,
    base_url: &str,
) -> Option<CodexChatReasoningConfig> {
    let platform = format!("{name} {base_url}");

    // OpenRouter：用原生归一化对象 `reasoning: { effort }`。effort 走 "openrouter" 值映射：
    // 枚举为 xhigh|high|medium|low|minimal，无 max——max 会触发
    // `400 reasoning_effort: Invalid option`（见 openclaw#77350），故钳到 xhigh。
    // 不发 `thinking:{type}`（OpenRouter 不认该字段）。
    if platform.contains("openrouter") {
        return Some(CodexChatReasoningConfig {
            supports_thinking: Some(false),
            supports_effort: Some(true),
            thinking_param: Some("none".to_string()),
            effort_param: Some("reasoning.effort".to_string()),
            effort_value_mode: Some("openrouter".to_string()),
            output_format: Some("auto".to_string()),
        });
    }

    // SiliconFlow：平台级统一 `enable_thinking`，思维回传 reasoning_content。
    // 不按 reasoning_effort 发 effort（平台用 thinking_budget 控制深度）。
    if platform.contains("siliconflow") {
        return Some(CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(false),
            thinking_param: Some("enable_thinking".to_string()),
            effort_param: Some("none".to_string()),
            effort_value_mode: None,
            output_format: Some("reasoning_content".to_string()),
        });
    }

    None
}

/// Responses → Chat 后是否可以带 `prompt_cache_key`。
/// 未知的 OpenAI 兼容网关默认不带：很多网关遇到不支持的字段直接 400。
pub fn should_send_chat_prompt_cache_key(spec: &ProviderSpec) -> bool {
    match spec.prompt_cache_routing {
        PromptCacheRouting::Enabled => return true,
        PromptCacheRouting::Disabled => return false,
        PromptCacheRouting::Auto => {}
    }

    let Ok(url) = url::Url::parse(spec.base_url.trim()) else {
        return false;
    };
    match url.host_str() {
        Some("api.openai.com") => true,
        Some("api.kimi.com") => {
            let path = url.path().trim_end_matches('/');
            path == "/coding" || path.starts_with("/coding/")
        }
        _ => false,
    }
}

/// 转换后补一个稳定的缓存路由 key：客户端显式给的优先，其次是客户端自带的会话 ID。
/// 网关逐请求生成的 UUID 绝不能用在这里。
pub fn inject_chat_prompt_cache_key(
    spec: &ProviderSpec,
    chat_body: &mut Value,
    explicit_key: Option<&str>,
    client_session_id: Option<&str>,
) -> bool {
    if !should_send_chat_prompt_cache_key(spec) {
        return false;
    }

    let key = explicit_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .or_else(|| {
            client_session_id
                .map(str::trim)
                .filter(|session_id| !session_id.is_empty())
        });
    let Some(key) = key else {
        return false;
    };

    chat_body["prompt_cache_key"] = Value::String(key.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderAuth;
    use serde_json::json;

    fn spec(name: &str, base_url: &str, format: ApiFormat) -> ProviderSpec {
        ProviderSpec::new("test", name, base_url, format, ProviderAuth::api_key("k"))
    }

    fn chat(name: &str, base_url: &str) -> ProviderSpec {
        spec(name, base_url, ApiFormat::OpenaiChat)
    }

    // ---- reasoning 推断（原 codex.rs 测试） ----

    #[test]
    fn infers_deepseek_effort_support() {
        let config = resolve_reasoning_config(
            &chat("DeepSeek", "https://api.deepseek.com"),
            &json!({ "model": "deepseek-v4-pro" }),
        )
        .unwrap();
        assert_eq!(config.supports_thinking, Some(true));
        assert_eq!(config.supports_effort, Some(true));
        assert_eq!(config.effort_value_mode.as_deref(), Some("deepseek"));
    }

    #[test]
    fn explicit_config_overrides_inference_and_is_normalized() {
        let mut s = chat("DeepSeek", "https://api.deepseek.com");
        s.reasoning = Some(CodexChatReasoningConfig {
            supports_thinking: Some(false),
            supports_effort: Some(false),
            thinking_param: Some("none".to_string()),
            effort_param: Some("none".to_string()),
            effort_value_mode: None,
            output_format: Some("auto".to_string()),
        });
        let config = resolve_reasoning_config(&s, &json!({ "model": "deepseek-v4-pro" })).unwrap();
        assert_eq!(config.supports_thinking, Some(false));
        assert_eq!(config.thinking_param.as_deref(), Some("none"));

        s.reasoning = Some(CodexChatReasoningConfig {
            supports_effort: Some(true),
            ..Default::default()
        });
        let config = resolve_reasoning_config(&s, &json!({})).unwrap();
        assert_eq!(config.supports_thinking, Some(true));
    }

    #[test]
    fn aggregator_platform_overrides_model_rules() {
        let openrouter = resolve_reasoning_config(
            &chat("OpenRouter", "https://openrouter.ai/api/v1"),
            &json!({ "model": "deepseek/deepseek-chat-v3.1" }),
        )
        .unwrap();
        assert_eq!(openrouter.thinking_param.as_deref(), Some("none"));
        assert_eq!(openrouter.effort_param.as_deref(), Some("reasoning.effort"));
        assert_eq!(openrouter.effort_value_mode.as_deref(), Some("openrouter"));

        let siliconflow = resolve_reasoning_config(
            &chat("SiliconFlow", "https://api.siliconflow.cn/v1"),
            &json!({ "model": "MiniMaxAI/MiniMax-M2.7" }),
        )
        .unwrap();
        assert_eq!(
            siliconflow.thinking_param.as_deref(),
            Some("enable_thinking")
        );
        assert_eq!(siliconflow.supports_effort, Some(false));
        assert_eq!(
            siliconflow.output_format.as_deref(),
            Some("reasoning_content")
        );
    }

    #[test]
    fn vendor_rules_and_unknown_vendor() {
        let cases = [
            ("Kimi", "https://api.moonshot.cn/v1", "kimi-k2", "thinking"),
            (
                "Zhipu",
                "https://open.bigmodel.cn/api/paas/v4",
                "glm-5",
                "thinking",
            ),
            (
                "Bailian",
                "https://dashscope.aliyuncs.com/v1",
                "qwen3",
                "enable_thinking",
            ),
            (
                "MiniMax",
                "https://api.minimax.io/v1",
                "MiniMax-M2",
                "reasoning_split",
            ),
        ];
        for (name, base_url, model, thinking_param) in cases {
            let config =
                resolve_reasoning_config(&chat(name, base_url), &json!({ "model": model }))
                    .unwrap();
            assert_eq!(
                config.thinking_param.as_deref(),
                Some(thinking_param),
                "{name}"
            );
            assert_eq!(config.supports_effort, Some(false), "{name}");
        }

        let stepfun = resolve_reasoning_config(
            &chat("StepFun", "https://api.stepfun.com/v1"),
            &json!({ "model": "step-3.5-flash-2603" }),
        )
        .unwrap();
        assert_eq!(stepfun.supports_effort, Some(true));
        assert_eq!(stepfun.effort_value_mode.as_deref(), Some("low_high"));

        assert!(resolve_reasoning_config(
            &chat("Relay", "https://relay.example.com/v1"),
            &json!({ "model": "gpt-5.4" })
        )
        .is_none());
    }

    // ---- prompt_cache_key 路由（原 codex.rs 测试） ----

    #[test]
    fn prompt_cache_routing_auto_enables_known_upstreams_only() {
        assert!(should_send_chat_prompt_cache_key(&chat(
            "Kimi",
            "https://api.kimi.com/coding/v1"
        )));
        assert!(should_send_chat_prompt_cache_key(&chat(
            "OpenAI",
            "https://api.openai.com/v1"
        )));
        assert!(!should_send_chat_prompt_cache_key(&chat(
            "Kimi",
            "https://api.kimi.com/v1"
        )));
        assert!(!should_send_chat_prompt_cache_key(&chat(
            "Strict",
            "https://strict.example.com/v1"
        )));
    }

    #[test]
    fn prompt_cache_routing_user_override_wins() {
        let mut kimi = chat("Kimi", "https://api.kimi.com/coding/v1");
        kimi.prompt_cache_routing = PromptCacheRouting::Disabled;
        assert!(!should_send_chat_prompt_cache_key(&kimi));

        let mut unknown = chat("Strict", "https://strict.example.com/v1");
        unknown.prompt_cache_routing = PromptCacheRouting::Enabled;
        assert!(should_send_chat_prompt_cache_key(&unknown));
    }

    #[test]
    fn prompt_cache_key_prefers_explicit_then_session() {
        let kimi = chat("Kimi", "https://api.kimi.com/coding/v1");

        let mut body = json!({});
        assert!(inject_chat_prompt_cache_key(
            &kimi,
            &mut body,
            Some("request-key"),
            Some("session-key")
        ));
        assert_eq!(body["prompt_cache_key"], "request-key");

        let mut body = json!({});
        assert!(inject_chat_prompt_cache_key(
            &kimi,
            &mut body,
            None,
            Some("session-key")
        ));
        assert_eq!(body["prompt_cache_key"], "session-key");

        let mut body = json!({});
        assert!(!inject_chat_prompt_cache_key(&kimi, &mut body, None, None));
        assert!(body.get("prompt_cache_key").is_none());

        let mut body = json!({});
        assert!(!inject_chat_prompt_cache_key(
            &chat("Strict", "https://strict.example.com/v1"),
            &mut body,
            Some("request-key"),
            Some("session-key")
        ));
        assert!(body.get("prompt_cache_key").is_none());
    }

    // ---- 端到端改写 ----

    fn run(spec: &ProviderSpec, body: Value, session: Option<&str>) -> Value {
        let ctx = RequestContext {
            client_session_id: session,
            gemini_shadow: None,
        };
        transform_request(spec, spec.effective_api_format(), body, &ctx).unwrap()
    }

    #[test]
    fn chat_upstream_converts_and_injects_session_cache_key() {
        let out = run(
            &chat("Kimi", "https://api.kimi.com/coding/v1"),
            json!({
                "model": "kimi-for-coding",
                "instructions": "You are helpful.",
                "input": [{ "role": "user", "content": "hi" }]
            }),
            Some("codex_session-abc"),
        );
        assert!(out.get("messages").is_some());
        assert!(out.get("input").is_none());
        assert_eq!(out["prompt_cache_key"], "codex_session-abc");
    }

    #[test]
    fn anthropic_upstream_uses_request_tokens_or_default() {
        let s = spec(
            "Relay",
            "https://api.example.com/anthropic",
            ApiFormat::Anthropic,
        );
        let with_tokens = run(
            &s,
            json!({
                "model": "claude",
                "max_output_tokens": 1000,
                "input": [{ "role": "user", "content": "hi" }]
            }),
            None,
        );
        assert_eq!(with_tokens["max_tokens"], 1000);

        let without = run(
            &s,
            json!({ "model": "claude", "input": [{ "role": "user", "content": "hi" }] }),
            None,
        );
        assert_eq!(without["max_tokens"], DEFAULT_CODEX_ANTHROPIC_MAX_TOKENS);
    }

    #[test]
    fn plain_responses_passthrough_is_untouched() {
        let body = json!({
            "model": "gpt-5.4",
            "prompt_cache_retention": "24h",
            "input": [{ "role": "user", "content": "hi" }]
        });
        let out = run(
            &spec(
                "OpenAI",
                "https://api.openai.com/v1",
                ApiFormat::OpenaiResponses,
            ),
            body.clone(),
            None,
        );
        assert_eq!(out, body);
    }
}
