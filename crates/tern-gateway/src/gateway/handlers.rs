//! HTTP 入口：解析请求 → 路由 → 改写 → 发送 → 转换响应。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::errors::{error_response, ErrorContext};
use super::response::{self, ResponseContext};
use super::upstream::{self, UpstreamRequest};
use super::GatewayState;
use crate::adapter::{prepare_request, RequestContext};
use crate::provider::ApiFormat;
use crate::proxy::content_encoding::{
    decompress_body, get_content_encoding, is_supported_content_encoding,
};
use crate::proxy::providers::transform_codex_chat::build_codex_tool_context_from_request;
use crate::proxy::providers::transform_codex_responses_namespace::namespace_restore_map;
use crate::proxy::providers::transform_gemini::extract_anthropic_tool_schema_hints;
use crate::proxy::session::extract_session_id;
use crate::proxy::ProxyError;

type SharedState = State<Arc<GatewayState>>;

pub(crate) async fn health() -> Json<Value> {
    Json(json!({
        "status": "healthy",
        "timestamp": chrono::Utc::now().to_rfc3339(),
    }))
}

/// OpenAI 风格的模型列表：每个供应商一项 `provider/*`，告诉客户端该怎么写模型名。
/// 供应商的真实模型目录由宿主应用维护，网关不去上游拉取。
pub(crate) async fn models(State(state): SharedState, headers: HeaderMap) -> Response {
    if let Err(error) = check_access(&state, &headers) {
        return error_response(ApiFormat::OpenaiResponses, &error, &ErrorContext::default());
    }
    let data: Vec<Value> = state
        .router()
        .providers()
        .map(|spec| {
            json!({
                "id": format!("{}/*", spec.id),
                "object": "model",
                "owned_by": spec.name,
            })
        })
        .collect();
    Json(json!({ "object": "list", "data": data })).into_response()
}

pub(crate) async fn claude_messages(
    State(state): SharedState,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(&state, ApiFormat::Anthropic, &uri, headers, body).await
}

pub(crate) async fn codex_responses(
    State(state): SharedState,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(&state, ApiFormat::OpenaiResponses, &uri, headers, body).await
}

async fn forward(
    state: &GatewayState,
    client_format: ApiFormat,
    uri: &Uri,
    mut headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut error_ctx = ErrorContext::default();
    match try_forward(
        state,
        client_format,
        uri,
        &mut headers,
        body,
        &mut error_ctx,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            log::warn!(
                "[Gateway] 请求失败 (provider={}, model={}): {error}",
                error_ctx.provider.as_deref().unwrap_or("-"),
                error_ctx.model.as_deref().unwrap_or("-"),
            );
            error_response(client_format, &error, &error_ctx)
        }
    }
}

async fn try_forward(
    state: &GatewayState,
    client_format: ApiFormat,
    uri: &Uri,
    headers: &mut HeaderMap,
    body: Bytes,
    error_ctx: &mut ErrorContext,
) -> Result<Response, ProxyError> {
    check_access(state, headers)?;

    let body = decode_request_body(headers, body)?;
    let mut body: Value = serde_json::from_slice(&body)
        .map_err(|e| ProxyError::InvalidRequest(format!("请求体不是合法 JSON: {e}")))?;

    let requested_model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    error_ctx.model = Some(requested_model.clone());
    let route = state.router().resolve(&requested_model)?;
    let spec = route.provider.clone();
    error_ctx.provider = Some(spec.id.clone());
    error_ctx.upstream_format = Some(spec.effective_api_format());
    body["model"] = Value::String(route.upstream_model.clone());

    let session = extract_session_id(
        headers,
        &body,
        match client_format {
            ApiFormat::Anthropic => "claude",
            _ => "codex",
        },
    );
    let client_session_id = session
        .client_provided
        .then_some(session.session_id.as_str());
    let client_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let upstream_format = spec.effective_api_format();

    // 转换前从原始请求里取出响应转换要用的信息
    let tool_schema_hints = (client_format == ApiFormat::Anthropic
        && upstream_format == ApiFormat::GeminiNative)
        .then(|| extract_anthropic_tool_schema_hints(&body))
        .filter(|hints| !hints.is_empty());
    let codex_tool_context = if client_format == ApiFormat::OpenaiResponses {
        build_codex_tool_context_from_request(&body)
    } else {
        Default::default()
    };
    let namespace_restore_map =
        if client_format == ApiFormat::OpenaiResponses && spec.is_xai_oauth() {
            namespace_restore_map(&body)
        } else {
            Default::default()
        };

    // Codex → Chat：Chat 协议没有 previous_response_id，要把缓存的工具调用补回 input
    if client_format == ApiFormat::OpenaiResponses && upstream_format == ApiFormat::OpenaiChat {
        let restored = state.chat_history.enrich_request(&mut body).await;
        if restored > 0 {
            log::debug!("[Gateway] 为 Chat 上游补全了 {restored} 个工具调用历史");
        }
    }

    let endpoint = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let prepared = prepare_request(
        &spec,
        client_format,
        endpoint,
        body,
        &RequestContext {
            client_session_id,
            gemini_shadow: Some(state.gemini_shadow.as_ref()),
        },
    )?;

    let request = UpstreamRequest {
        spec: &spec,
        client_format,
        prepared: &prepared,
        client_headers: headers,
        client_session_id,
        one_m_context: route.one_m_context,
    };
    let upstream_response = upstream::send(state, &request).await?;

    response::convert(
        state,
        ResponseContext {
            spec: spec.clone(),
            client_format,
            upstream_format: prepared.upstream_format,
            client_stream,
            session_id: session.session_id,
            tool_schema_hints,
            codex_tool_context,
            namespace_restore_map,
        },
        upstream_response,
    )
    .await
}

/// 配置了 access token 时校验客户端凭证。
///
/// Claude Code 用 `x-api-key`（`ANTHROPIC_API_KEY`）或 `Authorization: Bearer`
/// （`ANTHROPIC_AUTH_TOKEN`），Codex 用 `Authorization: Bearer`，两种都接受。
fn check_access(state: &GatewayState, headers: &HeaderMap) -> Result<(), ProxyError> {
    let Some(expected) = state.access_token.as_deref() else {
        return Ok(());
    };
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim);
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim);

    let presented = [bearer, api_key].into_iter().flatten();
    if presented
        .into_iter()
        .any(|token| constant_time_eq(token, expected))
    {
        Ok(())
    } else {
        Err(ProxyError::AuthError(
            "网关 access token 无效或缺失".to_string(),
        ))
    }
}

/// 逐字节比较时不提前退出，避免按耗时猜出 token
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Codex Desktop 登录态可能用 zstd 压缩请求体（取自 cc-switch `decode_codex_request_body`）
fn decode_request_body(headers: &mut HeaderMap, body: Bytes) -> Result<Bytes, ProxyError> {
    let Some(encoding) = get_content_encoding(headers) else {
        return Ok(body);
    };
    if !is_supported_content_encoding(&encoding) {
        return Err(ProxyError::InvalidRequest(format!(
            "Unsupported request content-encoding: {encoding}"
        )));
    }
    let decompressed = match decompress_body(&encoding, &body) {
        Ok(Some(decompressed)) => decompressed,
        Ok(None) => {
            return Err(ProxyError::InvalidRequest(format!(
                "Unsupported request content-encoding: {encoding}"
            )))
        }
        Err(e) => {
            return Err(ProxyError::InvalidRequest(format!(
                "Failed to decompress request body ({encoding}): {e}"
            )))
        }
    };
    headers.remove(axum::http::header::CONTENT_ENCODING);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    Ok(Bytes::from(decompressed))
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_matches_only_identical_strings() {
        assert!(constant_time_eq("secret", "secret"));
        assert!(!constant_time_eq("secret", "secreT"));
        assert!(!constant_time_eq("secret", "secret2"));
        assert!(!constant_time_eq("", "x"));
    }
}
