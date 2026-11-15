//! 上游端点改写与 URL 拼接。
//!
//! 合并自 cc-switch 的 `ClaudeAdapter::build_url`、`CodexAdapter::build_url` 与 forwarder
//! 里的 `rewrite_*_endpoint` 系列函数，规则保持一致：
//! - Anthropic 系 base_url 不带版本，端点自带 `/v1`
//! - OpenAI 系 base_url 按 SDK 约定可带 `/v1`；纯 origin 时自动补
//! - 两边都带版本时去掉重复的 `/v1/v1`

use serde_json::Value;

use crate::provider::{ApiFormat, ProviderSpec};
use crate::proxy::gemini_url::{normalize_gemini_model_id, resolve_gemini_native_url};
use crate::proxy::providers::transform_gemini::extract_gemini_model;
use crate::proxy::providers::CHATGPT_CODEX_BASE_URL;
use crate::proxy::ProxyError;

/// 计算上游完整 URL。`body` 必须是格式转换前的客户端请求体。
pub fn build_upstream_url(
    spec: &ProviderSpec,
    client_format: ApiFormat,
    client_endpoint: &str,
    body: &Value,
) -> Result<String, ProxyError> {
    let upstream_format = spec.effective_api_format();
    let (path, query) = split_endpoint_and_query(client_endpoint);
    let base_url = spec.effective_base_url();
    if base_url.is_empty() {
        return Err(ProxyError::ConfigError(format!(
            "供应商 {} 缺少 base_url",
            spec.id
        )));
    }

    match client_format {
        ApiFormat::Anthropic => {
            build_url_for_claude_client(spec, &base_url, upstream_format, path, query, body)
        }
        ApiFormat::OpenaiResponses => {
            build_url_for_codex_client(spec, &base_url, upstream_format, path, query)
        }
        other => Err(ProxyError::ConfigError(format!("暂不支持 {other} 客户端"))),
    }
}

fn build_url_for_claude_client(
    spec: &ProviderSpec,
    base_url: &str,
    upstream_format: ApiFormat,
    path: &str,
    query: Option<&str>,
    body: &Value,
) -> Result<String, ProxyError> {
    if !is_claude_messages_path(path) {
        // count_tokens 等辅助端点只有 Anthropic 上游原生支持
        if upstream_format != ApiFormat::Anthropic {
            return Err(ProxyError::InvalidRequest(format!(
                "{path} 无法转换到 {upstream_format} 上游"
            )));
        }
        return Ok(join_or_full(spec, base_url, path, query, join_anthropic));
    }

    match upstream_format {
        ApiFormat::Anthropic => Ok(join_or_full(spec, base_url, path, query, join_anthropic)),
        ApiFormat::GeminiNative => {
            let model = normalize_gemini_model_id(extract_gemini_model(body).unwrap_or("unknown"));
            let is_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
            let target_path = if is_stream {
                format!("/v1beta/models/{model}:streamGenerateContent")
            } else {
                format!("/v1beta/models/{model}:generateContent")
            };
            let query = merge_query_params(
                strip_beta_query(query).as_deref(),
                is_stream.then_some("alt=sse"),
            );
            let endpoint = with_query(&target_path, query.as_deref());
            Ok(resolve_gemini_native_url(
                base_url,
                &endpoint,
                spec.full_url,
            ))
        }
        ApiFormat::OpenaiResponses | ApiFormat::OpenaiChat => {
            // Claude Code 自带的 `?beta=true` 只对 Anthropic 有意义
            let query = strip_beta_query(query);
            if spec.has_pinned_endpoint() {
                return Ok(pinned_responses_url(spec, base_url, query.as_deref()));
            }
            let target_path = match (upstream_format, spec.is_github_copilot()) {
                (ApiFormat::OpenaiResponses, _) => "/v1/responses",
                (_, true) => "/chat/completions",
                _ => "/v1/chat/completions",
            };
            Ok(join_or_full(
                spec,
                base_url,
                target_path,
                query.as_deref(),
                join_anthropic,
            ))
        }
    }
}

fn build_url_for_codex_client(
    spec: &ProviderSpec,
    base_url: &str,
    upstream_format: ApiFormat,
    path: &str,
    query: Option<&str>,
) -> Result<String, ProxyError> {
    if upstream_format == ApiFormat::OpenaiResponses {
        if spec.has_pinned_endpoint() {
            // 订阅后端的 base_url 是固定的，路径去掉客户端带的 `/v1` 后直接拼
            let path = path.strip_prefix("/v1").unwrap_or(path);
            return Ok(with_query(&format!("{base_url}{path}"), query));
        }
        return Ok(join_or_full(spec, base_url, path, query, join_openai));
    }

    if !is_responses_path(path) {
        return Err(ProxyError::InvalidRequest(format!(
            "{path} 无法转换到 {upstream_format} 上游"
        )));
    }

    match upstream_format {
        ApiFormat::OpenaiChat => {
            let full = spec.full_url || base_url_is_full_endpoint(base_url, "/chat/completions");
            Ok(if full {
                append_query_to_full_url(base_url, query)
            } else {
                with_query(&join_openai(base_url, "/chat/completions"), query)
            })
        }
        ApiFormat::Anthropic => {
            // 用户把 `.../v1/messages` 整段粘进 base_url 却没开 full_url 时，
            // 不再重复拼接（否则 `.../v1/messages/v1/messages` 会 400 且不可重试）
            let full = spec.full_url || base_url_is_full_endpoint(base_url, "/v1/messages");
            Ok(if full {
                append_query_to_full_url(base_url, query)
            } else {
                with_query(&join_openai(base_url, "/v1/messages"), query)
            })
        }
        ApiFormat::GeminiNative => Err(ProxyError::ConfigError(format!(
            "Codex 暂不支持 Gemini Native 上游（供应商 {}），请改用该厂商的 OpenAI 兼容端点",
            spec.id
        ))),
        ApiFormat::OpenaiResponses => unreachable!("handled above"),
    }
}

/// Claude → 订阅：ChatGPT 后端统一走 `/responses` 且忽略原始 query；xAI 保留 query
fn pinned_responses_url(spec: &ProviderSpec, base_url: &str, query: Option<&str>) -> String {
    if spec.is_codex_oauth() {
        return format!("{CHATGPT_CODEX_BASE_URL}/responses");
    }
    with_query(&format!("{base_url}/responses"), query)
}

fn join_or_full(
    spec: &ProviderSpec,
    base_url: &str,
    path: &str,
    query: Option<&str>,
    join: fn(&str, &str) -> String,
) -> String {
    if spec.full_url {
        append_query_to_full_url(base_url, query)
    } else {
        with_query(&join(base_url, path), query)
    }
}

/// cc-switch `ClaudeAdapter::build_url`：直接拼接，去掉重复的 `/v1/v1`
fn join_anthropic(base_url: &str, endpoint: &str) -> String {
    let mut url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        endpoint.trim_start_matches('/')
    );
    while url.contains("/v1/v1") {
        url = url.replace("/v1/v1", "/v1");
    }
    url
}

/// cc-switch `CodexAdapter::build_url`：纯 origin 自动补 `/v1`，自定义前缀原样拼接
fn join_openai(base_url: &str, endpoint: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let endpoint = endpoint.trim_start_matches('/');
    let mut url = if !base.ends_with("/v1") && is_origin_only_url(base) {
        format!("{base}/v1/{endpoint}")
    } else {
        format!("{base}/{endpoint}")
    };
    while url.contains("/v1/v1") {
        url = url.replace("/v1/v1", "/v1");
    }
    url
}

/// `scheme://host` 之后没有路径段
pub fn is_origin_only_url(value: &str) -> bool {
    let trimmed = value.trim_end_matches('/');
    match trimmed.split_once("://") {
        Some((_scheme, rest)) => !rest.contains('/'),
        None => !trimmed.contains('/'),
    }
}

fn is_claude_messages_path(path: &str) -> bool {
    matches!(path, "/v1/messages" | "/claude/v1/messages")
}

fn is_responses_path(path: &str) -> bool {
    matches!(
        path,
        "/responses" | "/v1/responses" | "/responses/compact" | "/v1/responses/compact"
    )
}

fn split_endpoint_and_query(endpoint: &str) -> (&str, Option<&str>) {
    endpoint
        .split_once('?')
        .map_or((endpoint, None), |(path, query)| (path, Some(query)))
}

fn with_query(path: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => format!("{path}?{query}"),
        _ => path.to_string(),
    }
}

fn strip_beta_query(query: Option<&str>) -> Option<String> {
    let filtered = query.map(|query| {
        query
            .split('&')
            .filter(|pair| !pair.is_empty() && !pair.starts_with("beta="))
            .collect::<Vec<_>>()
            .join("&")
    });

    match filtered.as_deref() {
        Some("") | None => None,
        Some(_) => filtered,
    }
}

fn merge_query_params(base_query: Option<&str>, extra_param: Option<&str>) -> Option<String> {
    let mut params: Vec<String> = base_query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|pair| !pair.is_empty())
        .filter(|pair| !pair.starts_with("alt="))
        .map(ToString::to_string)
        .collect();

    if let Some(extra_param) = extra_param {
        params.push(extra_param.to_string());
    }

    if params.is_empty() {
        None
    } else {
        Some(params.join("&"))
    }
}

fn base_url_is_full_endpoint(base_url: &str, endpoint_suffix: &str) -> bool {
    let trimmed = base_url.trim();
    // 只比较路径部分：完整端点 URL 带 `?query` / `#fragment` 时后缀依然成立
    let path = match trimmed.split_once(['?', '#']) {
        Some((head, _)) => head,
        None => trimmed,
    };
    path.trim_end_matches('/')
        .to_ascii_lowercase()
        .ends_with(endpoint_suffix)
}

fn append_query_to_full_url(base_url: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => {
            if base_url.contains('?') {
                format!("{base_url}&{query}")
            } else {
                format!("{base_url}?{query}")
            }
        }
        _ => base_url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderAuth;
    use serde_json::json;

    fn spec(base_url: &str, format: ApiFormat) -> ProviderSpec {
        ProviderSpec::new("test", "Test", base_url, format, ProviderAuth::api_key("k"))
    }

    fn claude_url(spec: &ProviderSpec, endpoint: &str) -> String {
        build_upstream_url(spec, ApiFormat::Anthropic, endpoint, &json!({})).unwrap()
    }

    fn codex_url(spec: &ProviderSpec, endpoint: &str) -> String {
        build_upstream_url(spec, ApiFormat::OpenaiResponses, endpoint, &json!({})).unwrap()
    }

    // ---- 原 ClaudeAdapter::build_url 测试 ----

    #[test]
    fn anthropic_passthrough_joins_endpoint() {
        let s = spec("https://api.anthropic.com", ApiFormat::Anthropic);
        assert_eq!(
            claude_url(&s, "/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            claude_url(&s, "/v1/messages?foo=bar"),
            "https://api.anthropic.com/v1/messages?foo=bar"
        );
    }

    #[test]
    fn anthropic_passthrough_keeps_prefix_and_dedups_v1() {
        let openrouter = spec("https://openrouter.ai/api", ApiFormat::Anthropic);
        assert_eq!(
            claude_url(&openrouter, "/v1/messages"),
            "https://openrouter.ai/api/v1/messages"
        );

        let versioned = spec("https://relay.example.com/v1", ApiFormat::Anthropic);
        assert_eq!(
            claude_url(&versioned, "/v1/messages"),
            "https://relay.example.com/v1/messages"
        );
    }

    #[test]
    fn claude_to_chat_strips_beta_query() {
        let s = spec("https://integrate.api.nvidia.com", ApiFormat::OpenaiChat);
        assert_eq!(
            claude_url(&s, "/v1/messages?beta=true"),
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
        assert_eq!(
            claude_url(&s, "/v1/messages?beta=true&x=1"),
            "https://integrate.api.nvidia.com/v1/chat/completions?x=1"
        );
    }

    #[test]
    fn claude_to_copilot_uses_unversioned_chat_path() {
        let s = ProviderSpec::new(
            "copilot",
            "Copilot",
            "",
            ApiFormat::OpenaiChat,
            ProviderAuth::GithubCopilot { account_id: None },
        );
        assert_eq!(
            claude_url(&s, "/v1/messages"),
            "https://api.githubcopilot.com/chat/completions"
        );
    }

    #[test]
    fn claude_to_codex_subscription_always_hits_responses_without_query() {
        let s = ProviderSpec::new(
            "chatgpt",
            "ChatGPT",
            "https://ignored.example",
            ApiFormat::Anthropic,
            ProviderAuth::CodexOauth { account_id: None },
        );
        assert_eq!(
            claude_url(&s, "/v1/messages?beta=true&x=1"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn claude_to_xai_subscription_keeps_non_beta_query() {
        let s = ProviderSpec::new(
            "grok",
            "Grok",
            "https://attacker.example/anthropic",
            ApiFormat::Anthropic,
            ProviderAuth::XaiOauth { account_id: None },
        );
        assert_eq!(
            claude_url(&s, "/v1/messages?beta=1&x=1"),
            "https://api.x.ai/v1/responses?x=1"
        );
    }

    #[test]
    fn full_url_is_used_verbatim() {
        let mut s = spec(
            "https://relay.example.com/custom/chat",
            ApiFormat::OpenaiChat,
        );
        s.full_url = true;
        assert_eq!(
            claude_url(&s, "/v1/messages?beta=true"),
            "https://relay.example.com/custom/chat"
        );
    }

    #[test]
    fn claude_to_gemini_builds_generate_content_url() {
        let s = spec(
            "https://generativelanguage.googleapis.com",
            ApiFormat::GeminiNative,
        );
        let url = build_upstream_url(
            &s,
            ApiFormat::Anthropic,
            "/v1/messages",
            &json!({ "model": "models/gemini-2.5-pro" }),
        )
        .unwrap();
        assert!(
            url.ends_with("/v1beta/models/gemini-2.5-pro:generateContent"),
            "{url}"
        );
        assert!(!url.contains("models/models/"), "{url}");
    }

    // ---- 原 CodexAdapter::build_url 测试 ----

    #[test]
    fn responses_passthrough_follows_openai_base_url_rules() {
        assert_eq!(
            codex_url(
                &spec("https://api.openai.com/v1", ApiFormat::OpenaiResponses),
                "/responses"
            ),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            codex_url(
                &spec("https://api.openai.com", ApiFormat::OpenaiResponses),
                "/responses"
            ),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            codex_url(
                &spec("https://example.com/openai", ApiFormat::OpenaiResponses),
                "/responses"
            ),
            "https://example.com/openai/responses"
        );
        assert_eq!(
            codex_url(
                &spec("https://api.openai.com/v1", ApiFormat::OpenaiResponses),
                "/v1/responses"
            ),
            "https://api.openai.com/v1/responses"
        );
    }

    #[test]
    fn codex_subscription_strips_client_version_prefix() {
        let s = ProviderSpec::new(
            "chatgpt",
            "ChatGPT",
            "",
            ApiFormat::OpenaiResponses,
            ProviderAuth::CodexOauth { account_id: None },
        );
        assert_eq!(
            codex_url(&s, "/v1/responses"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            codex_url(&s, "/v1/responses/compact"),
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
    }

    #[test]
    fn codex_to_chat_and_anthropic_rewrite_endpoint() {
        assert_eq!(
            codex_url(
                &spec("https://api.deepseek.com", ApiFormat::OpenaiChat),
                "/v1/responses"
            ),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            codex_url(
                &spec("https://api.example.com/anthropic", ApiFormat::Anthropic),
                "/v1/responses/compact"
            ),
            "https://api.example.com/anthropic/v1/messages"
        );
        assert_eq!(
            codex_url(
                &spec("https://api.anthropic.com", ApiFormat::Anthropic),
                "/responses"
            ),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn codex_pasted_full_endpoint_is_not_appended_twice() {
        assert_eq!(
            codex_url(
                &spec(
                    "https://relay.example.com/api/v1/messages",
                    ApiFormat::Anthropic
                ),
                "/v1/responses"
            ),
            "https://relay.example.com/api/v1/messages"
        );
        assert_eq!(
            codex_url(
                &spec(
                    "https://relay.example.com/v1/chat/completions",
                    ApiFormat::OpenaiChat
                ),
                "/v1/responses"
            ),
            "https://relay.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn non_responses_codex_path_cannot_be_converted() {
        let err = build_upstream_url(
            &spec("https://api.deepseek.com", ApiFormat::OpenaiChat),
            ApiFormat::OpenaiResponses,
            "/v1/models",
            &json!({}),
        )
        .unwrap_err();
        assert!(matches!(err, ProxyError::InvalidRequest(_)), "{err:?}");
    }

    #[test]
    fn missing_base_url_is_config_error() {
        let err = build_upstream_url(
            &spec("  ", ApiFormat::Anthropic),
            ApiFormat::Anthropic,
            "/v1/messages",
            &json!({}),
        )
        .unwrap_err();
        assert!(matches!(err, ProxyError::ConfigError(_)), "{err:?}");
    }
}
