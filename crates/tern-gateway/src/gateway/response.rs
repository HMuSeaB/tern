//! 把上游 2xx 响应转换回客户端协议。
//!
//! 路径选择与 cc-switch `handlers.rs` 的 `handle_claude_transform`、
//! `handle_codex_*_to_responses_transform`、`handle_codex_responses_namespace_restore`
//! 一致；去掉了用量记录和故障转移相关的缓冲校验。

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::Value;

use super::aggregate::{
    aggregate_fallback_error, body_looks_like_sse, chat_sse_to_response_value,
    responses_sse_to_response_value, upstream_body_parse_error,
};
use super::GatewayState;
use crate::provider::{ApiFormat, ProviderSpec};
use crate::proxy::content_encoding::{decompress_body_with_limit, get_content_encoding};
use crate::proxy::hyper_client::MAX_RESPONSE_BODY_BYTES;
use crate::proxy::providers::codex_chat_history::record_responses_sse_stream;
use crate::proxy::providers::streaming::create_anthropic_sse_stream;
use crate::proxy::providers::streaming_codex_anthropic::{
    create_responses_sse_stream_from_anthropic_with_context,
    responses_sse_events_from_anthropic_message,
};
use crate::proxy::providers::streaming_codex_chat::create_responses_sse_stream_from_chat_with_context;
use crate::proxy::providers::streaming_gemini::create_anthropic_sse_stream_from_gemini;
use crate::proxy::providers::streaming_responses::create_anthropic_sse_stream_from_responses;
use crate::proxy::providers::transform_codex_chat::CodexToolContext;
use crate::proxy::providers::transform_codex_responses_namespace::{
    create_namespace_restore_sse_stream, restore_response_namespaces, NamespacedName,
};
use crate::proxy::providers::transform_gemini::AnthropicToolSchemaHints;
use crate::proxy::providers::{
    transform, transform_codex_anthropic, transform_codex_chat, transform_gemini,
    transform_responses,
};
use crate::proxy::ProxyError;

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

/// 转换响应所需、但请求体被转换后就拿不到的信息
pub(crate) struct ResponseContext {
    pub spec: Arc<ProviderSpec>,
    pub client_format: ApiFormat,
    pub upstream_format: ApiFormat,
    pub client_stream: bool,
    /// Gemini 思维签名影子存储的会话键
    pub session_id: String,
    /// Claude → Gemini：按工具 schema 修正 Gemini 返回的参数类型
    pub tool_schema_hints: Option<AnthropicToolSchemaHints>,
    /// Codex → Chat / Anthropic：还原 namespace / custom / tool_search 工具
    pub codex_tool_context: CodexToolContext,
    /// Codex → xAI 原生 Responses：展平后的工具名 → 原 namespace
    pub namespace_restore_map: HashMap<String, NamespacedName>,
}

pub(crate) async fn convert(
    state: &GatewayState,
    ctx: ResponseContext,
    response: reqwest::Response,
) -> Result<Response, ProxyError> {
    match ctx.client_format {
        ApiFormat::Anthropic => to_claude(state, ctx, response).await,
        ApiFormat::OpenaiResponses => to_codex(state, ctx, response).await,
        other => Err(ProxyError::ConfigError(format!("暂不支持 {other} 客户端"))),
    }
}

// ---------------------------------------------------------------------------
// Claude Code（Anthropic Messages）
// ---------------------------------------------------------------------------

async fn to_claude(
    state: &GatewayState,
    ctx: ResponseContext,
    response: reqwest::Response,
) -> Result<Response, ProxyError> {
    let idle = state.timeouts.stream_idle;
    if ctx.upstream_format == ApiFormat::Anthropic {
        return Ok(passthrough(response, idle));
    }

    let is_sse = is_sse(response.headers());
    // Gemini 没有 SSE 聚合器：非流请求拿到 SSE 时只能按流式转换
    let stream = ctx.client_stream || (is_sse && ctx.upstream_format == ApiFormat::GeminiNative);
    if stream {
        let upstream = response.bytes_stream();
        let converted: ByteStream = match ctx.upstream_format {
            ApiFormat::OpenaiResponses => {
                Box::pin(create_anthropic_sse_stream_from_responses(upstream))
            }
            ApiFormat::GeminiNative => Box::pin(create_anthropic_sse_stream_from_gemini(
                upstream,
                Some(state.gemini_shadow.clone()),
                Some(ctx.spec.id.clone()),
                Some(ctx.session_id.clone()),
                ctx.tool_schema_hints.clone(),
            )),
            _ => Box::pin(create_anthropic_sse_stream(upstream)),
        };
        return Ok(sse_response(converted, idle));
    }

    // 非流式：ChatGPT 订阅后端总是返回 SSE；部分网关对 stream:false 返回未标记的 SSE。
    // 两种情况都聚合成单个 JSON 再转换，客户端仍拿到 Anthropic JSON。
    let (status, headers, body) = read_body(response, state.timeouts.request).await?;
    let body_str = String::from_utf8_lossy(&body);
    let upstream: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) if body_looks_like_sse(&body_str) => {
            log::warn!(
                "[Gateway] {} 对非流请求返回 SSE 体，按 SSE 聚合（{}）",
                ctx.spec.id,
                ctx.upstream_format
            );
            let aggregated = if ctx.upstream_format == ApiFormat::OpenaiResponses {
                responses_sse_to_response_value(&body_str)
            } else {
                chat_sse_to_response_value(&body_str)
            };
            aggregated.map_err(|e| aggregate_fallback_error(e, &headers, &body_str))?
        }
        Err(e) => {
            return Err(upstream_body_parse_error(
                "Failed to parse upstream response",
                &e,
                &headers,
                &body_str,
            ))
        }
    };

    let anthropic = match ctx.upstream_format {
        ApiFormat::OpenaiResponses => transform_responses::responses_to_anthropic(upstream)?,
        ApiFormat::GeminiNative => transform_gemini::gemini_to_anthropic_with_shadow_and_hints(
            upstream,
            Some(state.gemini_shadow.as_ref()),
            Some(&ctx.spec.id),
            Some(&ctx.session_id),
            ctx.tool_schema_hints.as_ref(),
        )?,
        _ => transform::openai_to_anthropic(upstream)?,
    };
    json_response(status, headers, &anthropic)
}

// ---------------------------------------------------------------------------
// Codex（OpenAI Responses）
// ---------------------------------------------------------------------------

async fn to_codex(
    state: &GatewayState,
    ctx: ResponseContext,
    response: reqwest::Response,
) -> Result<Response, ProxyError> {
    match ctx.upstream_format {
        ApiFormat::OpenaiResponses => codex_passthrough(state, ctx, response).await,
        ApiFormat::OpenaiChat => codex_from_chat(state, ctx, response).await,
        ApiFormat::Anthropic => codex_from_anthropic(state, ctx, response).await,
        ApiFormat::GeminiNative => Err(ProxyError::ConfigError(
            "Codex 暂不支持 Gemini Native 上游".to_string(),
        )),
    }
}

/// 原生 Responses 透传；请求侧展平过 namespace 工具时还原函数名
async fn codex_passthrough(
    state: &GatewayState,
    ctx: ResponseContext,
    response: reqwest::Response,
) -> Result<Response, ProxyError> {
    let idle = state.timeouts.stream_idle;
    if ctx.namespace_restore_map.is_empty() {
        return Ok(passthrough(response, idle));
    }

    if is_sse(response.headers()) {
        let restored =
            create_namespace_restore_sse_stream(response.bytes_stream(), ctx.namespace_restore_map);
        return Ok(sse_response(Box::pin(restored), idle));
    }

    let (status, headers, body) = read_body(response, state.timeouts.request).await?;
    match serde_json::from_slice::<Value>(&body) {
        Ok(mut value) => {
            restore_response_namespaces(&mut value, &ctx.namespace_restore_map);
            json_response(status, headers, &value)
        }
        // 原生 Responses 的非流响应总是 JSON，这里只防御异常上游
        Err(_) => Ok(bytes_response(status, headers, body)),
    }
}

async fn codex_from_chat(
    state: &GatewayState,
    ctx: ResponseContext,
    response: reqwest::Response,
) -> Result<Response, ProxyError> {
    if ctx.client_stream || is_sse(response.headers()) {
        let converted = create_responses_sse_stream_from_chat_with_context(
            response.bytes_stream(),
            ctx.codex_tool_context,
        );
        let recorded = record_responses_sse_stream(converted, state.chat_history.clone());
        return Ok(sse_response(Box::pin(recorded), state.timeouts.stream_idle));
    }

    let (status, headers, body) = read_body(response, state.timeouts.request).await?;
    let body_str = String::from_utf8_lossy(&body);
    let chat: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) if body_looks_like_sse(&body_str) => {
            log::warn!(
                "[Gateway] {} 对非流请求返回 SSE 体，按 Chat SSE 聚合",
                ctx.spec.id
            );
            chat_sse_to_response_value(&body_str)
                .map_err(|e| aggregate_fallback_error(e, &headers, &body_str))?
        }
        Err(e) => {
            return Err(upstream_body_parse_error(
                "Failed to parse upstream chat response",
                &e,
                &headers,
                &body_str,
            ))
        }
    };
    let responses = transform_codex_chat::chat_completion_to_response_with_context(
        chat,
        &ctx.codex_tool_context,
    )?;
    state.chat_history.record_response(&responses).await;
    json_response(status, headers, &responses)
}

async fn codex_from_anthropic(
    state: &GatewayState,
    ctx: ResponseContext,
    response: reqwest::Response,
) -> Result<Response, ProxyError> {
    let idle = state.timeouts.stream_idle;
    // 正确标了 SSE、或流式请求没声明 JSON 时直接流式转换；
    // 明确是 JSON 的先缓冲，网关忽略 stream:true 时也能如实转换
    if is_sse(response.headers()) || (ctx.client_stream && !is_json(response.headers())) {
        let converted = create_responses_sse_stream_from_anthropic_with_context(
            response.bytes_stream(),
            ctx.codex_tool_context,
        );
        return Ok(sse_response(Box::pin(converted), idle));
    }

    let (status, headers, body) = read_body(response, state.timeouts.request).await?;
    let body_str = String::from_utf8_lossy(&body);
    let message: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) if body_looks_like_sse(&body_str) => {
            log::warn!(
                "[Gateway] {} 返回未标记的 Anthropic SSE 体，按 SSE 聚合",
                ctx.spec.id
            );
            transform_codex_anthropic::anthropic_sse_to_message_value(&body_str)?
        }
        Err(e) => {
            return Err(upstream_body_parse_error(
                "Failed to parse upstream anthropic response",
                &e,
                &headers,
                &body_str,
            ))
        }
    };

    if ctx.client_stream {
        let events = responses_sse_events_from_anthropic_message(&message, ctx.codex_tool_context);
        let replay = futures::stream::iter(events.into_iter().map(Ok::<Bytes, std::io::Error>));
        return Ok(sse_response(Box::pin(replay), idle));
    }

    let responses = transform_codex_anthropic::anthropic_response_to_responses_with_context(
        message,
        &ctx.codex_tool_context,
    )?;
    json_response(status, headers, &responses)
}

// ---------------------------------------------------------------------------
// 构建响应
// ---------------------------------------------------------------------------

/// 原样转发上游响应（状态码、头、字节流）
fn passthrough(response: reqwest::Response, idle: Duration) -> Response {
    let status = response.status();
    let mut headers = response.headers().clone();
    strip_hop_by_hop_headers(&mut headers);
    headers.remove(axum::http::header::CONTENT_LENGTH);

    let body = response
        .bytes_stream()
        .map(|chunk| chunk.map_err(std::io::Error::other));
    let mut out = Response::new(Body::from_stream(with_idle_timeout(Box::pin(body), idle)));
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    out
}

fn sse_response(stream: ByteStream, idle: Duration) -> Response {
    let mut out = Response::new(Body::from_stream(with_idle_timeout(stream, idle)));
    let headers = out.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    out
}

fn json_response(
    status: StatusCode,
    headers: HeaderMap,
    value: &Value,
) -> Result<Response, ProxyError> {
    let body = serde_json::to_vec(value)
        .map_err(|e| ProxyError::TransformError(format!("Failed to serialize response: {e}")))?;
    let mut out = bytes_response(status, headers, Bytes::from(body));
    out.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(out)
}

/// 重建过的响应体：去掉会失真的实体头和 hop-by-hop 头，保留上游的其他头（如 request-id、限流头）
fn bytes_response(status: StatusCode, mut headers: HeaderMap, body: Bytes) -> Response {
    strip_hop_by_hop_headers(&mut headers);
    headers.remove(axum::http::header::CONTENT_ENCODING);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    let mut out = Response::new(Body::from(body));
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    out
}

/// 两个数据块之间超过 `idle` 没有数据就结束流，避免上游挂住时客户端无限等待
fn with_idle_timeout(
    stream: ByteStream,
    idle: Duration,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut stream = stream;
        loop {
            let next = if idle.is_zero() {
                stream.next().await
            } else {
                match tokio::time::timeout(idle, stream.next()).await {
                    Ok(next) => next,
                    Err(_) => {
                        log::warn!("[Gateway] 流式响应 {}s 无数据，断开", idle.as_secs());
                        yield Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("upstream stream idle for {}s", idle.as_secs()),
                        ));
                        break;
                    }
                }
            };
            match next {
                Some(item) => yield item,
                None => break,
            }
        }
    }
}

/// 读取完整响应体并按 content-encoding 解压
async fn read_body(
    response: reqwest::Response,
    timeout: Duration,
) -> Result<(StatusCode, HeaderMap, Bytes), ProxyError> {
    let status = response.status();
    let mut headers = response.headers().clone();
    let raw = tokio::time::timeout(timeout, read_with_limit(response))
        .await
        .map_err(|_| {
            ProxyError::Timeout(format!(
                "响应体读取超时: {}s（上游发完响应头后 body 未到达）",
                timeout.as_secs()
            ))
        })??;

    let Some(encoding) = get_content_encoding(&headers) else {
        return Ok((status, headers, raw));
    };
    match decompress_body_with_limit(&encoding, &raw, MAX_RESPONSE_BODY_BYTES) {
        Ok(Some(decompressed)) => {
            headers.remove(axum::http::header::CONTENT_ENCODING);
            Ok((status, headers, Bytes::from(decompressed)))
        }
        // 不支持的编码：保留 content-encoding 头原样透传
        Ok(None) => Ok((status, headers, raw)),
        Err(e) => {
            log::warn!("[Gateway] 解压响应失败 ({encoding}): {e}");
            Err(ProxyError::TransformError(format!(
                "Failed to decompress upstream response ({encoding}): {e}"
            )))
        }
    }
}

async fn read_with_limit(mut response: reqwest::Response) -> Result<Bytes, ProxyError> {
    let mut body = bytes::BytesMut::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| ProxyError::ForwardFailed(format!("读取上游响应失败: {}", e.without_url())))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BODY_BYTES {
            return Err(ProxyError::ResponseBodyTooLarge(body.len() + chunk.len()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}

fn content_type(headers: &HeaderMap) -> &str {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

fn is_sse(headers: &HeaderMap) -> bool {
    content_type(headers).contains("text/event-stream")
}

fn is_json(headers: &HeaderMap) -> bool {
    let media = content_type(headers)
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    media == "application/json" || media.ends_with("+json")
}

/// 与 cc-switch `strip_hop_by_hop_response_headers` 一致，含 `Connection` 点名的扩展头
fn strip_hop_by_hop_headers(headers: &mut HeaderMap) {
    const HOP_BY_HOP: &[&str] = &[
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "trailers",
        "transfer-encoding",
        "upgrade",
    ];
    let listed: Vec<axum::http::HeaderName> = headers
        .get_all(axum::http::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| axum::http::HeaderName::from_bytes(name.as_bytes()).ok())
        .collect();
    for name in HOP_BY_HOP {
        headers.remove(*name);
    }
    for name in listed {
        headers.remove(name);
    }
}
