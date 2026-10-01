//! 按客户端协议包装错误。
//!
//! 上游错误体的形状取决于上游协议（Chat 的 `base_resp`、Gemini 的 `status` 等），
//! 原样透传会让客户端认不出错误码。这里统一改写成客户端自己的错误形状，保留
//! 原始 HTTP 状态码。Codex 侧的富化信息取自 cc-switch `codex_proxy_error_json`。

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use crate::provider::ApiFormat;
use crate::proxy::error_mapper::{get_error_message, map_proxy_error_to_status};
use crate::proxy::providers::transform_codex_chat::chat_error_to_response_error;
use crate::proxy::ProxyError;

/// 出错时已知的请求信息，写进错误消息方便定位
#[derive(Debug, Clone, Default)]
pub(crate) struct ErrorContext {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub upstream_format: Option<ApiFormat>,
}

pub(crate) fn error_response(
    client_format: ApiFormat,
    error: &ProxyError,
    ctx: &ErrorContext,
) -> Response {
    let status = StatusCode::from_u16(map_proxy_error_to_status(error))
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = match client_format {
        ApiFormat::OpenaiResponses => responses_error_json(error, ctx),
        _ => anthropic_error_json(error, ctx),
    };
    let mut response = (status, axum::Json(body)).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// Anthropic 错误形状：`{"type":"error","error":{"type","message"}}`
fn anthropic_error_json(error: &ProxyError, ctx: &ErrorContext) -> Value {
    if let ProxyError::UpstreamError { status, body } = error {
        let parsed = body
            .as_deref()
            .and_then(|b| serde_json::from_str::<Value>(b).ok());
        // Anthropic 上游的错误本来就是客户端认识的形状，原样透传
        if ctx.upstream_format == Some(ApiFormat::Anthropic)
            && parsed
                .as_ref()
                .is_some_and(|v| v.get("type").and_then(Value::as_str) == Some("error"))
        {
            return parsed.unwrap();
        }
        let cause = parsed
            .as_ref()
            .map(|v| chat_error_to_response_error(Some(v)))
            .and_then(|v| {
                v.pointer("/error/message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| body.as_deref().map(|b| compact_error_message(b, 1024)))
            .unwrap_or_else(|| format!("Upstream error (status {status})"));
        return json!({
            "type": "error",
            "error": {
                "type": anthropic_error_type(*status),
                "message": with_context(&cause, ctx, Some(*status)),
            }
        });
    }

    json!({
        "type": "error",
        "error": {
            "type": anthropic_error_type(map_proxy_error_to_status(error)),
            "message": with_context(&get_error_message(error), ctx, None),
        }
    })
}

/// Anthropic 客户端按 error.type 决定是否重试（overloaded / rate_limit 会重试）
fn anthropic_error_type(status: u16) -> &'static str {
    match status {
        400 | 422 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        529 => "overloaded_error",
        502..=504 => "api_error",
        _ => "api_error",
    }
}

/// Responses 错误形状：`{"error":{message,type,code,param,...}}`
pub(crate) fn responses_error_json(error: &ProxyError, ctx: &ErrorContext) -> Value {
    let (mut body, upstream_status) = match error {
        ProxyError::UpstreamError { status, body } => {
            let parsed = body
                .as_deref()
                .map(|body| serde_json::from_str::<Value>(body).unwrap_or_else(|_| json!(body)));
            (chat_error_to_response_error(parsed.as_ref()), Some(*status))
        }
        _ => (
            json!({
                "error": {
                    "message": get_error_message(error),
                    "type": "proxy_error",
                    "code": proxy_error_code(error),
                    "param": Value::Null,
                }
            }),
            None,
        ),
    };

    let Some(error_obj) = body.get_mut("error").and_then(Value::as_object_mut) else {
        return body;
    };

    let cause = error_obj
        .get("message")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| get_error_message(error));
    error_obj.insert(
        "message".to_string(),
        Value::String(with_context(&cause, ctx, upstream_status)),
    );
    if error_obj
        .get("type")
        .and_then(Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
    {
        error_obj.insert("type".to_string(), json!("proxy_error"));
    }
    if error_obj.get("code").is_none_or(Value::is_null) {
        error_obj.insert("code".to_string(), json!(proxy_error_code(error)));
    }
    error_obj.entry("param").or_insert(Value::Null);
    if let Some(provider) = &ctx.provider {
        error_obj.insert("provider".to_string(), json!(provider));
    }
    if let Some(model) = &ctx.model {
        error_obj.insert("model".to_string(), json!(model));
    }
    if let Some(status) = upstream_status {
        error_obj.insert("upstream_status".to_string(), json!(status));
    }
    body
}

fn with_context(cause: &str, ctx: &ErrorContext, upstream_status: Option<u16>) -> String {
    let message = if upstream_status == Some(413) {
        // 413 来自上游网关（典型是 nginx client_max_body_size），不是 tern 的限制；
        // 上游给的往往是一整段 HTML，对用户没有帮助
        "Upstream provider rejected the request with HTTP 413 (Payload Too Large). \
         This is the provider's server-side limit, not a tern limit. \
         Shrink the request: run /compact, or remove large pasted logs or inline images."
            .to_string()
    } else {
        cause.to_string()
    };

    let mut parts = Vec::new();
    if let Some(provider) = &ctx.provider {
        parts.push(format!("provider: {provider}"));
    }
    if let Some(model) = &ctx.model {
        parts.push(format!("model: {model}"));
    }
    if let Some(status) = upstream_status {
        parts.push(format!("upstream_status: HTTP {status}"));
    }
    let message = if parts.is_empty() {
        format!("[tern] {message}")
    } else {
        format!("[tern] {message} ({})", parts.join("; "))
    };
    compact_error_message(&message, 1800)
}

fn proxy_error_code(error: &ProxyError) -> &'static str {
    match error {
        ProxyError::ForwardFailed(_) => "tern_forward_failed",
        ProxyError::Timeout(_) | ProxyError::StreamIdleTimeout(_) => "tern_timeout",
        ProxyError::ConfigError(_) => "tern_config_error",
        ProxyError::TransformError(_) => "tern_transform_error",
        ProxyError::InvalidRequest(_) => "tern_invalid_request",
        ProxyError::AuthError(_) => "tern_auth_error",
        ProxyError::UpstreamError { .. } => "tern_upstream_error",
        ProxyError::ResponseBodyTooLarge(_) => "tern_response_too_large",
        _ => "tern_internal_error",
    }
}

fn compact_error_message(message: &str, max_chars: usize) -> String {
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let truncated: String = normalized.chars().take(max_chars).collect();
    format!("{}…(truncated)", truncated.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ErrorContext {
        ErrorContext {
            provider: Some("deepseek".into()),
            model: Some("deepseek-chat".into()),
            upstream_format: Some(ApiFormat::OpenaiChat),
        }
    }

    #[test]
    fn responses_error_normalizes_nonstandard_upstream_body() {
        let error = ProxyError::UpstreamError {
            status: 429,
            body: Some(r#"{"base_resp":{"status_code":1002,"status_msg":"rate limited"}}"#.into()),
        };
        let body = responses_error_json(&error, &ctx());
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains("rate limited"), "{message}");
        assert!(message.contains("deepseek"), "{message}");
        assert_eq!(body["error"]["upstream_status"], 429);
        assert_eq!(body["error"]["provider"], "deepseek");
    }

    #[test]
    fn responses_error_for_local_failure_has_code() {
        let error = ProxyError::ForwardFailed("dns lookup failed".into());
        let body = responses_error_json(&error, &ErrorContext::default());
        assert_eq!(body["error"]["code"], "tern_forward_failed");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("dns lookup failed"));
    }

    #[test]
    fn anthropic_upstream_error_passes_through() {
        let original = json!({
            "type": "error",
            "error": { "type": "overloaded_error", "message": "Overloaded" }
        });
        let error = ProxyError::UpstreamError {
            status: 529,
            body: Some(original.to_string()),
        };
        let ctx = ErrorContext {
            upstream_format: Some(ApiFormat::Anthropic),
            ..ctx()
        };
        assert_eq!(anthropic_error_json(&error, &ctx), original);
    }

    #[test]
    fn non_anthropic_upstream_error_is_reshaped_for_claude() {
        let error = ProxyError::UpstreamError {
            status: 401,
            body: Some(
                r#"{"error":{"message":"Invalid API key","type":"invalid_request_error"}}"#.into(),
            ),
        };
        let body = anthropic_error_json(&error, &ctx());
        assert_eq!(body["type"], "error");
        assert_eq!(body["error"]["type"], "authentication_error");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid API key"));
    }

    #[test]
    fn html_413_is_replaced_with_actionable_hint() {
        let error = ProxyError::UpstreamError {
            status: 413,
            body: Some("<html><body>nginx</body></html>".into()),
        };
        let message = anthropic_error_json(&error, &ctx())["error"]["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(message.contains("not a tern limit"), "{message}");
        assert!(!message.contains("nginx"), "{message}");
    }
}
