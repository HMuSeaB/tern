//! 上游适配层：把客户端（agent）发来的请求改写成某个 `ProviderSpec` 能接受的样子。
//!
//! 对应 cc-switch 的 `proxy/providers/{claude,codex,gemini}.rs` 与 forwarder 里的请求
//! 准备逻辑。cc-switch 按 agent 拆适配器，再从配置快照里猜上游协议；tern 由
//! `(客户端协议, ProviderSpec)` 直接决定转换路径：
//!
//! - `claude`：Anthropic Messages 入站（Claude Code）
//! - `codex`：OpenAI Responses 入站（Codex）
//! - `endpoint`：上游端点改写与 URL 拼接
//! - `auth`：认证头，含原 `gemini.rs` 的 OAuth 凭证解析
//!
//! 这里只做纯计算，不发请求、不取动态 token。以下留给转发层：
//! - 订阅（Copilot / Codex / xAI）的 token 换取，以及 `chatgpt-account-id`、
//!   Copilot `x-interaction-id` 这类依赖运行时状态的请求头
//! - Codex → Chat 的工具调用历史补全（`CodexChatHistoryStore`，有状态）
//! - xAI 原生 Responses 响应里的 namespace 还原（`namespace_restore_map`）
//! - `[1m]` 模型后缀与 `context-1m` beta 头、`anthropic-version` 等协议头

pub mod auth;
pub mod claude;
pub mod codex;
pub mod endpoint;

use serde_json::Value;

use crate::provider::{ApiFormat, ProviderSpec};
use crate::proxy::body_filter::filter_private_params_with_whitelist;
use crate::proxy::json_canonical::canonicalize_value;
use crate::proxy::providers::gemini_shadow::GeminiShadowStore;
use crate::proxy::ProxyError;

pub use auth::{auth_headers, requires_managed_token, resolve_auth};

/// 单次请求的运行时上下文
#[derive(Debug, Default, Clone, Copy)]
pub struct RequestContext<'a> {
    /// 客户端自己带来的稳定会话 ID。网关兜底生成的随机 ID 不能传进来：
    /// 它会被用作 prompt_cache_key，每轮都换就等于没有缓存。
    pub client_session_id: Option<&'a str>,
    /// Gemini Native 上游需要的思维签名影子存储
    pub gemini_shadow: Option<&'a GeminiShadowStore>,
}

/// 准备好发往上游的请求（不含认证头，见 [`resolve_auth`] / [`auth_headers`]）
#[derive(Debug, Clone)]
pub struct PreparedRequest {
    pub url: String,
    pub body: Value,
    /// 上游实际说的协议，转发层据此选择响应转换器
    pub upstream_format: ApiFormat,
}

/// 把客户端请求改写成上游请求。
///
/// `body` 中的 `model` 应已是上游真实模型名（`provider/` 前缀由路由层去掉）。
pub fn prepare_request(
    spec: &ProviderSpec,
    client_format: ApiFormat,
    client_endpoint: &str,
    body: Value,
    ctx: &RequestContext<'_>,
) -> Result<PreparedRequest, ProxyError> {
    let upstream_format = spec.effective_api_format();
    // URL 要用转换前的请求体：Gemini 的模型名和 stream 标志转换后就不在 body 里了
    let url = endpoint::build_upstream_url(spec, client_format, client_endpoint, &body)?;

    let body = match client_format {
        ApiFormat::Anthropic => claude::transform_request(spec, upstream_format, body, ctx)?,
        ApiFormat::OpenaiResponses => codex::transform_request(spec, upstream_format, body, ctx)?,
        // build_upstream_url 已经拒绝了其他客户端协议
        other => return Err(ProxyError::ConfigError(format!("暂不支持 {other} 客户端"))),
    };

    Ok(PreparedRequest {
        url,
        body: prepare_upstream_body(body),
        upstream_format,
    })
}

/// 过滤 `_` 开头的私有字段并按键排序，与 cc-switch `prepare_upstream_request_body` 一致。
/// 键序稳定能让上游 prompt cache 的前缀保持一致。
fn prepare_upstream_body(body: Value) -> Value {
    canonicalize_value(filter_private_params_with_whitelist(body, &[]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderAuth;
    use serde_json::json;

    fn spec(base_url: &str, format: ApiFormat) -> ProviderSpec {
        ProviderSpec::new("test", "Test", base_url, format, ProviderAuth::api_key("k"))
    }

    #[test]
    fn claude_to_chat_upstream_rewrites_url_and_body() {
        let prepared = prepare_request(
            &spec("https://api.example.com/v1", ApiFormat::OpenaiChat),
            ApiFormat::Anthropic,
            "/v1/messages?beta=true",
            json!({
                "model": "gpt-5.4",
                "max_tokens": 64,
                "stream": true,
                "_internal": "must not leak",
                "messages": [{ "role": "user", "content": "hello" }]
            }),
            &RequestContext::default(),
        )
        .unwrap();

        assert_eq!(prepared.url, "https://api.example.com/v1/chat/completions");
        assert_eq!(prepared.upstream_format, ApiFormat::OpenaiChat);
        assert_eq!(prepared.body["stream_options"]["include_usage"], true);
        assert!(prepared.body.get("_internal").is_none());
        assert!(prepared.body.get("messages").is_some());
    }

    #[test]
    fn claude_passthrough_keeps_query_and_applies_vendor_fixes() {
        let prepared = prepare_request(
            &spec("https://api.deepseek.com/anthropic", ApiFormat::Anthropic),
            ApiFormat::Anthropic,
            "/v1/messages?beta=true",
            json!({
                "model": "deepseek-v4-pro",
                "max_tokens": 100,
                "thinking": { "type": "disabled" },
                "output_config": { "effort": "max" },
                "messages": [{ "role": "user", "content": "hello" }]
            }),
            &RequestContext::default(),
        )
        .unwrap();

        assert_eq!(
            prepared.url,
            "https://api.deepseek.com/anthropic/v1/messages?beta=true"
        );
        assert!(prepared.body.get("output_config").is_none());
    }

    #[test]
    fn claude_count_tokens_only_passes_through_to_anthropic() {
        let body = json!({ "model": "m", "messages": [] });
        let ctx = RequestContext::default();

        let passthrough = prepare_request(
            &spec("https://api.anthropic.com", ApiFormat::Anthropic),
            ApiFormat::Anthropic,
            "/v1/messages/count_tokens",
            body.clone(),
            &ctx,
        )
        .unwrap();
        assert_eq!(
            passthrough.url,
            "https://api.anthropic.com/v1/messages/count_tokens"
        );

        let err = prepare_request(
            &spec("https://api.example.com/v1", ApiFormat::OpenaiChat),
            ApiFormat::Anthropic,
            "/v1/messages/count_tokens",
            body,
            &ctx,
        )
        .unwrap_err();
        assert!(matches!(err, ProxyError::InvalidRequest(_)), "{err:?}");
    }

    #[test]
    fn claude_to_gemini_url_uses_pre_transform_model() {
        let prepared = prepare_request(
            &spec(
                "https://generativelanguage.googleapis.com",
                ApiFormat::GeminiNative,
            ),
            ApiFormat::Anthropic,
            "/v1/messages",
            json!({
                "model": "gemini-2.5-pro",
                "max_tokens": 64,
                "stream": true,
                "messages": [{ "role": "user", "content": "hello" }]
            }),
            &RequestContext::default(),
        )
        .unwrap();

        assert!(
            prepared
                .url
                .contains("/v1beta/models/gemini-2.5-pro:streamGenerateContent"),
            "{}",
            prepared.url
        );
        assert!(prepared.url.contains("alt=sse"), "{}", prepared.url);
        assert!(prepared.body.get("contents").is_some());
    }

    #[test]
    fn codex_to_anthropic_applies_output_ceiling_and_cache_breakpoints() {
        let mut spec = spec("https://api.example.com/anthropic", ApiFormat::Anthropic);
        spec.max_output_tokens = Some(32_000);

        let prepared = prepare_request(
            &spec,
            ApiFormat::OpenaiResponses,
            "/v1/responses",
            json!({
                "model": "claude-sonnet-5",
                "max_output_tokens": 1000,
                "instructions": "You are helpful.",
                "input": [{ "role": "user", "content": "hi" }]
            }),
            &RequestContext::default(),
        )
        .unwrap();

        assert_eq!(
            prepared.url,
            "https://api.example.com/anthropic/v1/messages"
        );
        assert_eq!(prepared.body["max_tokens"], 32_000);
        let system = prepared.body["system"].as_array().expect("system array");
        assert_eq!(system.last().unwrap()["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn codex_to_xai_subscription_scrubs_unsupported_fields() {
        let spec = ProviderSpec::new(
            "grok",
            "Grok",
            "",
            ApiFormat::OpenaiResponses,
            ProviderAuth::XaiOauth { account_id: None },
        );

        let prepared = prepare_request(
            &spec,
            ApiFormat::OpenaiResponses,
            "/responses",
            json!({
                "model": "grok-4.5",
                "prompt_cache_retention": "24h",
                "safety_identifier": "user-1",
                "input": [{ "role": "user", "content": "hi" }]
            }),
            &RequestContext::default(),
        )
        .unwrap();

        assert_eq!(prepared.url, "https://api.x.ai/v1/responses");
        assert!(prepared.body.get("prompt_cache_retention").is_none());
        assert!(prepared.body.get("safety_identifier").is_none());
    }

    #[test]
    fn codex_to_gemini_is_rejected() {
        let err = prepare_request(
            &spec(
                "https://generativelanguage.googleapis.com",
                ApiFormat::GeminiNative,
            ),
            ApiFormat::OpenaiResponses,
            "/v1/responses",
            json!({ "model": "gemini-2.5-pro", "input": [] }),
            &RequestContext::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ProxyError::ConfigError(_)), "{err:?}");
    }
}
