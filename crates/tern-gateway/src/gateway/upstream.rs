//! 组装上游请求头并发送。
//!
//! 请求头规则取自 cc-switch `forwarder.rs` 的有序 HeaderMap 构建段，去掉了
//! Claude Code 伪装、本地覆盖、Copilot 优化器等可选功能。

use std::time::Duration;

use http::{HeaderMap, HeaderName, HeaderValue};

use super::{needs_token_provider, GatewayState};
use crate::adapter::{auth_headers, resolve_auth, PreparedRequest};
use crate::provider::{ApiFormat, ProviderAuth, ProviderSpec};
use crate::proxy::content_encoding::{decompress_body_with_limit, get_content_encoding};
use crate::proxy::copilot_optimizer::deterministic_interaction_id;
use crate::proxy::hyper_client::MAX_RESPONSE_BODY_BYTES;
use crate::proxy::providers::AuthStrategy;
use crate::proxy::ProxyError;

const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
const CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";

/// 发往上游所需的请求上下文
pub(crate) struct UpstreamRequest<'a> {
    pub spec: &'a ProviderSpec,
    pub client_format: ApiFormat,
    pub prepared: &'a PreparedRequest,
    pub client_headers: &'a HeaderMap,
    /// 客户端自带的稳定会话 ID（网关兜底生成的不算）
    pub client_session_id: Option<&'a str>,
    /// 模型名带 `[1m]`，Anthropic 上游需要 `context-1m` beta
    pub one_m_context: bool,
}

impl UpstreamRequest<'_> {
    /// 上游会返回 SSE：请求体 `stream: true`，或 Gemini 流式端点
    pub fn is_streaming(&self) -> bool {
        self.prepared
            .body
            .get("stream")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || self.prepared.url.contains(":streamGenerateContent")
            || self.prepared.url.contains("alt=sse")
    }
}

/// 发送请求。2xx 返回响应本体；其余状态码读出（解压后的）错误体，作为
/// `UpstreamError` 返回，交给错误层按客户端协议重新包装。
pub(crate) async fn send(
    state: &GatewayState,
    req: &UpstreamRequest<'_>,
) -> Result<reqwest::Response, ProxyError> {
    let headers = build_headers(state, req).await?;
    let body = serde_json::to_vec(&req.prepared.body)
        .map_err(|e| ProxyError::Internal(format!("序列化请求体失败: {e}")))?;
    let streaming = req.is_streaming();

    log::info!(
        "[Gateway] >>> {} {} (model={})",
        req.spec.id,
        redact_url(&req.prepared.url),
        req.prepared
            .body
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<url>")
    );

    let mut builder = state
        .client
        .post(&req.prepared.url)
        .headers(headers)
        .body(body);
    // reqwest 的 timeout 管整个请求；流式响应可能持续很久，只限制等响应头的时间，
    // 之后由响应层的空闲超时接管
    if !streaming {
        builder = builder.timeout(state.timeouts.request);
    }
    let send = builder.send();
    let response = if streaming {
        tokio::time::timeout(state.timeouts.request, send)
            .await
            .map_err(|_| {
                ProxyError::Timeout(format!(
                    "等待上游响应头超时（{}s）",
                    state.timeouts.request.as_secs()
                ))
            })?
    } else {
        send.await
    }
    .map_err(map_send_error)?;

    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    log::warn!(
        "[Gateway] <<< {} 返回 HTTP {}",
        req.spec.id,
        status.as_u16()
    );
    let body = read_error_body(response).await;
    Err(ProxyError::UpstreamError {
        status: status.as_u16(),
        body,
    })
}

async fn build_headers(
    state: &GatewayState,
    req: &UpstreamRequest<'_>,
) -> Result<HeaderMap, ProxyError> {
    let spec = req.spec;
    let upstream_format = req.prepared.upstream_format;
    let is_copilot = spec.is_github_copilot();
    let to_anthropic = upstream_format == ApiFormat::Anthropic;
    let codex_to_anthropic = req.client_format == ApiFormat::OpenaiResponses && to_anthropic;

    let mut headers = HeaderMap::new();
    let mut saw_anthropic_version = false;
    let mut client_betas: Option<String> = None;

    for (name, value) in req.client_headers {
        let key = name.as_str();
        if should_drop_client_header(key)
            || (is_copilot && is_copilot_fingerprint_header(key))
            // Codex 的会话 / 账号指纹不能泄露给 Anthropic 上游，严格网关还会据此拒绝
            || (codex_to_anthropic && is_codex_client_fingerprint_header(key))
        {
            continue;
        }
        match key {
            "anthropic-version" => {
                if to_anthropic {
                    saw_anthropic_version = true;
                    headers.append(name.clone(), value.clone());
                }
            }
            "anthropic-beta" => {
                if to_anthropic && client_betas.is_none() {
                    client_betas = value.to_str().ok().map(ToString::to_string);
                }
            }
            // Codex CLI 发 `Accept: text/event-stream`，严格的 Anthropic 网关会 406；
            // 流式由请求体的 stream 字段决定，统一改成 application/json
            "accept" if codex_to_anthropic => {}
            _ => {
                headers.append(name.clone(), value.clone());
            }
        }
    }

    // 网关自己按 content-encoding 解压，不让上游按客户端偏好压缩转换路径的响应
    headers.insert(
        http::header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if codex_to_anthropic {
        headers.insert(
            http::header::ACCEPT,
            HeaderValue::from_static("application/json"),
        );
    }

    if to_anthropic {
        if !saw_anthropic_version {
            headers.insert(
                "anthropic-version",
                HeaderValue::from_static(DEFAULT_ANTHROPIC_VERSION),
            );
        }
        if let Some(betas) = merge_betas(client_betas.as_deref(), req.one_m_context) {
            headers.insert("anthropic-beta", hv(&betas)?);
        }
    }

    for (name, value) in resolve_auth_headers(state, req).await? {
        headers.insert(name, value);
    }

    Ok(headers)
}

async fn resolve_auth_headers(
    state: &GatewayState,
    req: &UpstreamRequest<'_>,
) -> Result<Vec<(HeaderName, HeaderValue)>, ProxyError> {
    let spec = req.spec;
    let Some(mut auth) = resolve_auth(spec) else {
        return Ok(Vec::new());
    };

    let mut account_id = None;
    if needs_token_provider(spec) {
        let tokens = state.tokens.as_ref().ok_or_else(|| {
            ProxyError::AuthError(format!(
                "供应商 {} 需要订阅登录，但网关未接入 token 来源",
                spec.id
            ))
        })?;
        let managed = tokens.token(spec).await?;
        auth.api_key = managed.token;
        account_id = managed
            .account_id
            .or_else(|| configured_account_id(&spec.auth));
    }

    let mut headers = auth_headers(&auth)?;
    let session_id = req
        .client_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match auth.strategy {
        AuthStrategy::CodexOAuth => {
            if let Some(account_id) = account_id.as_deref() {
                headers.push((
                    HeaderName::from_static("chatgpt-account-id"),
                    hv(account_id)?,
                ));
            }
            // 对齐官方 Codex CLI 的会话路由信号。只用客户端给的 ID：
            // 每次随机生成会破坏前缀缓存
            if let Some(session_id) = session_id {
                let value = hv(session_id)?;
                headers.push((HeaderName::from_static("session_id"), value.clone()));
                headers.push((HeaderName::from_static("x-client-request-id"), value));
                headers.push((
                    HeaderName::from_static("x-codex-window-id"),
                    hv(&format!("{session_id}:0"))?,
                ));
            }
        }
        AuthStrategy::GitHubCopilot => {
            if let Some(interaction_id) = session_id.and_then(deterministic_interaction_id) {
                headers.push((
                    HeaderName::from_static("x-interaction-id"),
                    hv(&interaction_id)?,
                ));
            }
        }
        _ => {}
    }
    Ok(headers)
}

fn configured_account_id(auth: &ProviderAuth) -> Option<String> {
    match auth {
        ProviderAuth::GithubCopilot { account_id }
        | ProviderAuth::CodexOauth { account_id }
        | ProviderAuth::XaiOauth { account_id } => account_id.clone(),
        _ => None,
    }
}

/// 客户端的 beta 列表加上 `context-1m`（去重）
fn merge_betas(client: Option<&str>, one_m_context: bool) -> Option<String> {
    let mut betas: Vec<&str> = client
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|beta| !beta.is_empty())
        .collect();
    if one_m_context && !betas.contains(&CONTEXT_1M_BETA) {
        betas.push(CONTEXT_1M_BETA);
    }
    (!betas.is_empty()).then(|| betas.join(","))
}

/// 连接、实体、认证、代理链路追踪类的头一律不转发
fn should_drop_client_header(key: &str) -> bool {
    matches!(
        key,
        "host"
            | "content-length"
            | "content-type"
            | "content-encoding"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
            | "accept-encoding"
            | "authorization"
            | "x-api-key"
            | "x-goog-api-key"
            | "x-forwarded-for"
            | "x-forwarded-host"
            | "x-forwarded-port"
            | "x-forwarded-proto"
            | "x-real-ip"
            | "forwarded"
            | "cf-connecting-ip"
            | "cf-ipcountry"
            | "cf-ray"
            | "cf-visitor"
            | "true-client-ip"
            | "x-request-id"
            | "x-correlation-id"
            | "x-trace-id"
            | "x-amzn-trace-id"
            | "traceparent"
            | "tracestate"
    ) || key.starts_with("x-b3-")
}

/// 与 cc-switch `is_codex_client_fingerprint_header` 一致
fn is_codex_client_fingerprint_header(key: &str) -> bool {
    matches!(
        key,
        "originator"
            | "session_id"
            | "session-id"
            | "thread-id"
            | "conversation_id"
            | "chatgpt-account-id"
            | "x-openai-subagent"
            | "x-client-request-id"
            | "openai-beta"
            | "openai-organization"
            | "openai-project"
    ) || key.starts_with("x-stainless-")
        || key.starts_with("x-codex-")
}

/// Copilot 指纹头由认证层统一提供，客户端的同名头要去掉
fn is_copilot_fingerprint_header(key: &str) -> bool {
    matches!(
        key,
        "user-agent"
            | "editor-version"
            | "editor-plugin-version"
            | "copilot-integration-id"
            | "x-github-api-version"
            | "openai-intent"
            | "x-initiator"
            | "x-interaction-type"
            | "x-interaction-id"
            | "x-vscode-user-agent-library-version"
            | "x-agent-task-id"
    )
}

fn hv(value: &str) -> Result<HeaderValue, ProxyError> {
    HeaderValue::from_str(value)
        .map_err(|e| ProxyError::AuthError(format!("invalid header value: {e}")))
}

fn map_send_error(error: reqwest::Error) -> ProxyError {
    let error = error.without_url();
    if error.is_timeout() {
        ProxyError::Timeout(format!("上游请求超时: {error}"))
    } else if error.is_connect() {
        ProxyError::ForwardFailed(format!("上游连接失败: {error}"))
    } else {
        ProxyError::ForwardFailed(format!("上游请求发送失败: {error}"))
    }
}

/// 错误体也可能被压缩；解压失败时退回原始字节，尽量保留上游的限流 / 鉴权详情
async fn read_error_body(response: reqwest::Response) -> Option<String> {
    let encoding = get_content_encoding(response.headers());
    let raw = tokio::time::timeout(Duration::from_secs(30), read_limited(response))
        .await
        .ok()??;
    let decoded = match encoding {
        Some(encoding) => {
            match decompress_body_with_limit(&encoding, &raw, MAX_RESPONSE_BODY_BYTES) {
                Ok(Some(decompressed)) => decompressed,
                _ => raw,
            }
        }
        None => raw,
    };
    String::from_utf8(decoded).ok()
}

/// 错误体最多读 1 MB：足够诊断，又不会被异常上游撑爆内存
async fn read_limited(mut response: reqwest::Response) -> Option<Vec<u8>> {
    const MAX_ERROR_BODY_BYTES: usize = 1024 * 1024;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        let room = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() >= MAX_ERROR_BODY_BYTES {
            break;
        }
    }
    Some(body)
}

/// 日志里只留 scheme://host/path：Gemini 等上游可能把 key 放在 query 里
pub(crate) fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.to_string()
        }
        Err(_) => "<invalid url>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_betas_appends_context_1m_once() {
        assert_eq!(merge_betas(None, false), None);
        assert_eq!(merge_betas(None, true).as_deref(), Some(CONTEXT_1M_BETA));
        assert_eq!(
            merge_betas(Some("a, b"), true).as_deref(),
            Some("a,b,context-1m-2025-08-07")
        );
        assert_eq!(
            merge_betas(Some("context-1m-2025-08-07"), true).as_deref(),
            Some(CONTEXT_1M_BETA)
        );
    }

    #[test]
    fn redact_url_strips_query_and_userinfo() {
        assert_eq!(
            redact_url("https://user:pw@example.com/v1beta/models/x:generateContent?key=AIza"),
            "https://example.com/v1beta/models/x:generateContent"
        );
    }

    #[test]
    fn credential_and_hop_headers_are_dropped() {
        for key in [
            "authorization",
            "x-api-key",
            "host",
            "content-length",
            "x-b3-traceid",
        ] {
            assert!(should_drop_client_header(key), "{key}");
        }
        assert!(!should_drop_client_header("user-agent"));
        assert!(!should_drop_client_header("anthropic-beta"));
    }
}
