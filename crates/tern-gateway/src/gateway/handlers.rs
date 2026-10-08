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
use super::usage::{self, RequestMeta};
use super::GatewayState;
use crate::adapter::{prepare_request, RequestContext};
use crate::provider::ApiFormat;
use crate::proxy::circuit_breaker::CircuitBreakerConfig;
use crate::proxy::content_encoding::{
    decompress_body, get_content_encoding, is_supported_content_encoding,
};
use crate::proxy::providers::transform_codex_chat::build_codex_tool_context_from_request;
use crate::proxy::providers::transform_codex_responses_namespace::namespace_restore_map;
use crate::proxy::providers::transform_gemini::extract_anthropic_tool_schema_hints;
use crate::proxy::session::extract_session_id;
use crate::proxy::ProxyError;
use crate::resilience;
use crate::router::{Route, RouteKind};
use crate::ProviderSpec;

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
    let mut meta = RequestMeta::new(client_format, uri.path());
    match try_forward(
        state,
        client_format,
        uri,
        &mut headers,
        body,
        &mut error_ctx,
        &mut meta,
    )
    .await
    {
        Ok(response) => usage::track(state.usage.as_ref(), meta, response),
        Err(error) => {
            log::warn!(
                "[Gateway] 请求失败 (provider={}, model={}): {error}",
                error_ctx.provider.as_deref().unwrap_or("-"),
                error_ctx.model.as_deref().unwrap_or("-"),
            );
            let response = error_response(client_format, &error, &error_ctx);
            usage::record_failure(
                state.usage.as_ref(),
                meta,
                response.status().as_u16(),
                &error,
            );
            response
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
    meta: &mut RequestMeta,
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
    meta.client_model = requested_model.clone();
    meta.stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    // 角色要看原始请求（子代理标记、工具列表），转换成上游协议后就认不出了
    meta.role = usage::infer_role(meta.client, uri.path(), headers, &body);
    let route = state.router().resolve(&requested_model)?;

    // 故障转移链。显式指定的 `provider/model` 只有一家——用户指定了就是指定了，
    // 悄悄转给别家比报错更糟；fallback 才从默认那家开始按顺序把所有可用的过一遍。
    let chain = resilience::failover_chain(
        &state.router(),
        &route.provider.id,
        route.kind == RouteKind::Explicit,
        &state.breakers,
        &state.resilience,
    )
    .await;
    // 链为空说明供应商被从配置里删了。failover_chain 全熔断时会放开一个出去，
    // 所以走到这里只可能是路由表与配置不一致
    if chain.is_empty() {
        return Err(ProxyError::NoAvailableProvider);
    }

    // 与选哪家无关的部分只准备一次。故障转移的每一轮都复用同一份——
    // 每个供应商只换 `body["model"]` 和 spec 自己
    let ctx = ForwardCtx {
        client_format,
        uri,
        headers,
        route: &route,
    };

    // 熔断配置转一次就好：五个字段的拷贝，比在 GatewayState 里同时存两份
    // resilience 和它的 CircuitBreakerConfig、还要操心两者同步要简单
    let circuit = CircuitBreakerConfig::from(&state.resilience);

    // 逐个试。**只在上游连响应头都没给的时候才换下一家**：流式响应发了一半再换
    // 一家重发，用户会看到两段拼在一起的回答。`upstream::send` 返回 Ok 即代表
    // 已经拿到响应头，后面的响应转换就算失败也不该重试。
    let mut last_error = None;
    for (index, spec) in chain.iter().enumerate() {
        error_ctx.provider = Some(spec.id.clone());
        meta.provider_id = Some(spec.id.clone());
        meta.route_kind = Some(route.kind);
        meta.upstream_model = Some(route.upstream_model.clone());
        error_ctx.upstream_format = Some(spec.effective_api_format());
        body["model"] = Value::String(route.upstream_model.clone());

        match send_to(state, &ctx, spec, &body, meta).await {
            Ok(response) => {
                state.breakers.record_success(&spec.id, &circuit).await;
                return Ok(response);
            }
            Err(error) => {
                state.breakers.record_failure(&spec.id, &circuit).await;
                // 最后一家不留余地：它的错就是用户该看到的错，
                // 包一层"全都试过了"反而把上游的原话藏了
                if index + 1 == chain.len() {
                    return Err(error);
                }
                log::warn!("[Gateway] {} 失败，转到下一家：{error}", spec.id);
                last_error = Some(error);
            }
        }
    }
    // 走不到：链非空时上面的循环一定 return 或 continue 到底
    Err(last_error.unwrap_or(ProxyError::NoAvailableProvider))
}

/// 一次请求里**与选哪家供应商无关**的部分。
///
/// 抽出来是因为 `send_to` 有七个参数，而其中六个在故障转移的每一轮里都一模一样
/// ——罗列一遍既难读又容易在换供应商时漏改。收成一个结构体之后，循环体里那句
/// `send_to(state, &ctx, spec, meta).await` 读得出"同一件事换个人做"。
struct ForwardCtx<'a> {
    client_format: ApiFormat,
    uri: &'a Uri,
    headers: &'a HeaderMap,
    route: &'a Route,
}

/// 往一家上游发一次请求并转换响应。
///
/// 从 `try_forward` 里拆出来是为了让故障转移的循环读起来是"试一家、记一家、
/// 换下一家"，而不是三十行准备逻辑里夹一个 send。`meta` 里 `session_id` 在这里
/// 填，用量由 `response::convert` 填。
async fn send_to(
    state: &GatewayState,
    ctx: &ForwardCtx<'_>,
    spec: &Arc<ProviderSpec>,
    // 这一轮的请求体。每轮 clone 一份：换供应商时要改 `model`，
    // 而三家收到的请求体除了模型名应当完全一致
    body: &Value,
    meta: &mut RequestMeta,
) -> Result<Response, ProxyError> {
    let ForwardCtx {
        client_format,
        uri,
        headers,
        route,
    } = *ctx;
    let upstream_format = spec.effective_api_format();
    let session = extract_session_id(
        headers,
        body,
        match client_format {
            ApiFormat::Anthropic => "claude",
            _ => "codex",
        },
    );
    let client_session_id = session
        .client_provided
        .then_some(session.session_id.as_str());
    // 只记客户端自己带的：网关兜底生成的随机 ID 每轮都不同，
    // 拿它聚合 session 会得到一堆只含一条请求的"会话"
    meta.session_id = client_session_id.map(str::to_string);
    let client_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    // Codex → Chat：Chat 协议没有 previous_response_id，要把缓存的工具调用补回 input
    let mut body = body.clone();
    if client_format == ApiFormat::OpenaiResponses && upstream_format == ApiFormat::OpenaiChat {
        let restored = state.chat_history.enrich_request(&mut body).await;
        if restored > 0 {
            log::debug!("[Gateway] 为 Chat 上游补全了 {restored} 个工具调用历史");
        }
    }

    // 三段上下文从**转换前**的请求体上取：Gemini 的模型名和 stream 标志转换后
    // 就不在 body 里了，xAI 的 namespace 展开后也认不出原样
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

    let endpoint = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let prepared = prepare_request(
        spec,
        client_format,
        endpoint,
        body,
        &RequestContext {
            client_session_id,
            gemini_shadow: Some(state.gemini_shadow.as_ref()),
        },
    )?;

    let request = UpstreamRequest {
        spec,
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
