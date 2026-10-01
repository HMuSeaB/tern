//! 用量采集：网关只产出 [`UsageEvent`]，存储由宿主实现的 [`UsageSink`] 负责。
//!
//! 采集点在出口：响应已经转换成客户端协议，只需解析两种格式（Claude Messages、
//! OpenAI Responses），不用像 cc-switch 那样为每种上游各写一套收集器。
//!
//! 流式响应包一层旁路解析，不缓冲、不阻塞转发；流被客户端中断时 `Drop` 里补记一条
//! `aborted`。

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use axum::body::{Body, BodyDataStream};
use axum::http::HeaderMap;
use axum::response::Response;
use bytes::{Bytes, BytesMut};
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::ApiFormat;
use crate::proxy::copilot_optimizer::classify_request;
use crate::proxy::error_mapper::get_error_message;
use crate::proxy::sse::{append_utf8_safe, strip_sse_field, take_sse_block};
use crate::proxy::usage::parser::TokenUsage;
use crate::proxy::ProxyError;
use crate::router::RouteKind;

/// 非流式响应体最多缓冲这么多字节用于解析 usage，超出就放弃解析（照常转发）
const MAX_JSON_CAPTURE_BYTES: usize = 32 * 1024 * 1024;
/// SSE 单个事件块的上限；超过说明不是正常的事件流，停止解析
const MAX_SSE_BLOCK_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_MESSAGE_CHARS: usize = 500;

/// 宿主实现：落库、推送到界面等。`record` 在请求路径上调用，必须立即返回。
pub trait UsageSink: Send + Sync {
    fn record(&self, event: UsageEvent);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Claude,
    Codex,
}

impl ClientKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ClientKind::Claude => "claude",
            ClientKind::Codex => "codex",
        }
    }

    fn from_format(format: ApiFormat) -> Self {
        match format {
            ApiFormat::OpenaiResponses => ClientKind::Codex,
            _ => ClientKind::Claude,
        }
    }
}

/// 请求在 agent 里扮演的角色，回答"谁在花钱"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestRole {
    /// 主对话
    Main,
    /// 子代理（Claude Code 的 Task / Agent tool，Codex 的 review 等）
    Subagent,
    /// 上下文压缩
    Compact,
    /// 后台小请求：标题生成、话题检测等，不带工具
    Background,
}

impl RequestRole {
    pub fn as_str(self) -> &'static str {
        match self {
            RequestRole::Main => "main",
            RequestRole::Subagent => "subagent",
            RequestRole::Compact => "compact",
            RequestRole::Background => "background",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    /// 客户端在响应结束前断开（用户按了 Esc、进程退出）
    Aborted,
    Failed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Aborted => "aborted",
            Outcome::Failed => "failed",
        }
    }
}

/// 失败原因的粗分类，面板按它聚类，不混进模型统计
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 429，含中转站的并发限制
    RateLimited,
    /// 529 等过载
    Overloaded,
    /// 上游 401/403，或 tern 自己的 accessToken 校验失败
    Auth,
    Timeout,
    /// 连不上上游
    Connection,
    /// 上游其余 4xx（参数、上下文超长等）
    UpstreamRejected,
    /// 上游 5xx
    UpstreamServer,
    /// 请求本身有问题：路由不到、不是合法 JSON 等，没有发往上游
    InvalidRequest,
    /// 协议转换失败
    Transform,
    /// 流式响应中途出错（上游发了 error 事件、读取失败）
    Stream,
    Internal,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::RateLimited => "rate_limited",
            ErrorKind::Overloaded => "overloaded",
            ErrorKind::Auth => "auth",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Connection => "connection",
            ErrorKind::UpstreamRejected => "upstream_rejected",
            ErrorKind::UpstreamServer => "upstream_server",
            ErrorKind::InvalidRequest => "invalid_request",
            ErrorKind::Transform => "transform",
            ErrorKind::Stream => "stream",
            ErrorKind::Internal => "internal",
        }
    }

    pub fn from_error(error: &ProxyError) -> Self {
        match error {
            ProxyError::UpstreamError { status, .. } => match *status {
                429 => ErrorKind::RateLimited,
                529 => ErrorKind::Overloaded,
                401 | 403 => ErrorKind::Auth,
                408 | 504 => ErrorKind::Timeout,
                400..=499 => ErrorKind::UpstreamRejected,
                _ => ErrorKind::UpstreamServer,
            },
            ProxyError::Timeout(_) | ProxyError::StreamIdleTimeout(_) => ErrorKind::Timeout,
            ProxyError::ForwardFailed(_) => ErrorKind::Connection,
            ProxyError::AuthError(_) => ErrorKind::Auth,
            ProxyError::InvalidRequest(_) | ProxyError::ConfigError(_) => ErrorKind::InvalidRequest,
            ProxyError::TransformError(_) => ErrorKind::Transform,
            _ => ErrorKind::Internal,
        }
    }
}

/// 统一口径后的 token 数。四个桶互斥，`fresh_input` 不含任何缓存。
///
/// Anthropic 的 `input_tokens` 本来就不含缓存；OpenAI Responses 的含缓存读写，
/// 入库前必须扣掉，否则缓存命中率和成本都会算错（cc-switch 踩过）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCounts {
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenCounts {
    fn from_usage(client: ClientKind, usage: &TokenUsage) -> Self {
        let input = u64::from(usage.input_tokens);
        let cache_read = u64::from(usage.cache_read_tokens);
        let cache_write = u64::from(usage.cache_creation_tokens);
        let fresh_input = match client {
            ClientKind::Claude => input,
            ClientKind::Codex => input.saturating_sub(cache_read).saturating_sub(cache_write),
        };
        Self {
            fresh_input,
            output: u64::from(usage.output_tokens),
            cache_read,
            cache_write,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.fresh_input == 0 && self.output == 0 && self.cache_read == 0 && self.cache_write == 0
    }
}

/// 一次请求的完整记录。不含请求 / 响应正文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEvent {
    /// 收到请求的时间，Unix 毫秒
    pub started_at_ms: i64,
    pub client: ClientKind,
    pub endpoint: String,
    /// 路由失败时为空
    pub provider_id: Option<String>,
    pub route_kind: Option<RouteKind>,
    /// 客户端原样发来的模型名（如 `deepseek/deepseek-v4-pro`、`claude-haiku-4-5`）
    pub client_model: String,
    /// 路由后实际发给上游的模型名
    pub upstream_model: Option<String>,
    /// 上游响应里回显的模型名
    pub response_model: Option<String>,
    pub role: RequestRole,
    /// 客户端自带的会话 ID（网关兜底生成的不记，聚合没有意义）
    pub session_id: Option<String>,
    pub stream: bool,
    /// 返回给客户端的 HTTP 状态码
    pub status: u16,
    pub outcome: Outcome,
    pub error_kind: Option<ErrorKind>,
    pub error_message: Option<String>,
    /// 响应里没有 usage、或 usage 全 0 时为空（上游省略 usage 时转换器会合成 0）
    pub tokens: Option<TokenCounts>,
    /// 上游的消息 ID，用于去重
    pub message_id: Option<String>,
    pub first_token_ms: Option<u64>,
    pub duration_ms: u64,
}

/// 请求处理过程中逐步填充的元数据
pub(crate) struct RequestMeta {
    pub started: Instant,
    pub started_at_ms: i64,
    pub client: ClientKind,
    pub endpoint: String,
    /// `count_tokens` 等不计费的端点不记录
    pub skip: bool,
    pub provider_id: Option<String>,
    pub route_kind: Option<RouteKind>,
    pub client_model: String,
    pub upstream_model: Option<String>,
    pub role: RequestRole,
    pub session_id: Option<String>,
    pub stream: bool,
}

impl RequestMeta {
    pub fn new(client_format: ApiFormat, path: &str) -> Self {
        Self {
            started: Instant::now(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            client: ClientKind::from_format(client_format),
            endpoint: path.to_string(),
            skip: path.ends_with("/count_tokens"),
            provider_id: None,
            route_kind: None,
            client_model: String::new(),
            upstream_model: None,
            role: RequestRole::Main,
            session_id: None,
            stream: false,
        }
    }

    fn into_event(self, status: u16, outcome: Outcome) -> UsageEvent {
        UsageEvent {
            started_at_ms: self.started_at_ms,
            client: self.client,
            endpoint: self.endpoint,
            provider_id: self.provider_id,
            route_kind: self.route_kind,
            client_model: self.client_model,
            upstream_model: self.upstream_model,
            response_model: None,
            role: self.role,
            session_id: self.session_id,
            stream: self.stream,
            status,
            outcome,
            error_kind: None,
            error_message: None,
            tokens: None,
            message_id: None,
            first_token_ms: None,
            duration_ms: elapsed_ms(self.started),
        }
    }
}

/// 从客户端请求推断角色。必须在请求体被转换成上游协议之前调用。
pub(crate) fn infer_role(
    client: ClientKind,
    endpoint: &str,
    headers: &HeaderMap,
    body: &Value,
) -> RequestRole {
    let no_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_none_or(|tools| tools.is_empty());
    match client {
        ClientKind::Claude => {
            // 复用 cc-switch Copilot 优化器的分类：子代理看 __SUBAGENT_MARKER__ 和
            // metadata.user_id 的 `_agent_`，压缩看 Claude Code compact 的固定提示词
            let class = classify_request(body, false, true, true);
            let model = body.get("model").and_then(Value::as_str).unwrap_or("");
            if class.is_subagent {
                RequestRole::Subagent
            } else if class.is_compact {
                RequestRole::Compact
            } else if no_tools || model.to_ascii_lowercase().contains("haiku") {
                // 主对话和子代理总是带工具；不带工具的是标题生成之类的后台请求
                RequestRole::Background
            } else {
                RequestRole::Main
            }
        }
        ClientKind::Codex => {
            let subagent = headers
                .get("x-openai-subagent")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim().to_ascii_lowercase())
                .filter(|v| !v.is_empty());
            if endpoint.ends_with("/compact") || subagent.as_deref() == Some("compact") {
                RequestRole::Compact
            } else if subagent.is_some() {
                RequestRole::Subagent
            } else if no_tools {
                RequestRole::Background
            } else {
                RequestRole::Main
            }
        }
    }
}

/// 请求在拿到上游 2xx 之前失败
pub(crate) fn record_failure(
    sink: Option<&Arc<dyn UsageSink>>,
    meta: RequestMeta,
    status: u16,
    error: &ProxyError,
) {
    let Some(sink) = sink else { return };
    if meta.skip {
        return;
    }
    let mut event = meta.into_event(status, Outcome::Failed);
    event.error_kind = Some(ErrorKind::from_error(error));
    event.error_message = Some(truncate_chars(&get_error_message(error)));
    sink.record(event);
}

/// 给成功的响应包一层旁路解析，响应结束（或被丢弃）时产出事件
pub(crate) fn track(
    sink: Option<&Arc<dyn UsageSink>>,
    meta: RequestMeta,
    response: Response,
) -> Response {
    let Some(sink) = sink else { return response };
    if meta.skip {
        return response;
    }
    let sse = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    let status = response.status().as_u16();
    let (parts, body) = response.into_parts();
    let tap = UsageTap {
        inner: body.into_data_stream(),
        collector: if sse {
            Collector::Sse(SseCollector::default())
        } else {
            Collector::Json(JsonCollector::default())
        },
        pending: Some(Pending {
            sink: sink.clone(),
            meta,
            status,
        }),
        first_chunk_ms: None,
        stream_error: None,
    };
    Response::from_parts(parts, Body::from_stream(tap))
}

struct Pending {
    sink: Arc<dyn UsageSink>,
    meta: RequestMeta,
    status: u16,
}

struct UsageTap {
    inner: BodyDataStream,
    collector: Collector,
    /// 事件发出后置空，保证每个请求只记一次
    pending: Option<Pending>,
    first_chunk_ms: Option<u64>,
    stream_error: Option<String>,
}

impl Stream for UsageTap {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        let polled = Pin::new(&mut this.inner).poll_next(cx);
        match &polled {
            Poll::Ready(Some(Ok(chunk))) => {
                if this.first_chunk_ms.is_none() && !chunk.is_empty() {
                    this.first_chunk_ms = this
                        .pending
                        .as_ref()
                        .map(|pending| elapsed_ms(pending.meta.started));
                }
                this.collector.feed(chunk);
            }
            Poll::Ready(Some(Err(error))) => {
                this.stream_error = Some(error.to_string());
            }
            Poll::Ready(None) => this.finish(true),
            Poll::Pending => {}
        }
        polled
    }
}

impl Drop for UsageTap {
    fn drop(&mut self) {
        self.finish(false);
    }
}

impl UsageTap {
    /// `completed`：流正常结束；否则是被丢弃（客户端断开，或读取出错后 axum 停止拉取）
    fn finish(&mut self, completed: bool) {
        let Some(Pending { sink, meta, status }) = self.pending.take() else {
            return;
        };
        let client = meta.client;
        let mut event = meta.into_event(status, Outcome::Success);
        event.first_token_ms = event.stream.then_some(self.first_chunk_ms).flatten();

        let parsed = self.collector.parse(client);
        if let Some(usage) = &parsed.usage {
            event.response_model = usage.model.clone().filter(|m| !m.is_empty());
            event.message_id = usage.message_id.clone();
            let tokens = TokenCounts::from_usage(client, usage);
            event.tokens = (!tokens.is_zero()).then_some(tokens);
        }

        if let Some(message) = self.stream_error.take() {
            event.outcome = Outcome::Failed;
            event.error_kind = Some(if message.contains("idle") {
                ErrorKind::Timeout
            } else {
                ErrorKind::Stream
            });
            event.error_message = Some(truncate_chars(&message));
        } else if let Some(message) = parsed.error {
            event.outcome = Outcome::Failed;
            event.error_kind = Some(ErrorKind::Stream);
            event.error_message = Some(truncate_chars(&message));
        } else if !completed {
            event.outcome = Outcome::Aborted;
        }
        sink.record(event);
    }
}

#[derive(Default)]
struct Parsed {
    usage: Option<TokenUsage>,
    /// 流里的 error 事件（Claude `error` / Responses `response.failed`）
    error: Option<String>,
}

enum Collector {
    Sse(SseCollector),
    Json(JsonCollector),
}

impl Collector {
    fn feed(&mut self, chunk: &Bytes) {
        match self {
            Collector::Sse(c) => c.feed(chunk),
            Collector::Json(c) => c.feed(chunk),
        }
    }

    fn parse(&mut self, client: ClientKind) -> Parsed {
        match self {
            Collector::Sse(c) => c.parse(client),
            Collector::Json(c) => c.parse(client),
        }
    }
}

/// 只保留和用量 / 错误有关的事件，内容增量直接丢掉，内存占用与响应长度无关
#[derive(Default)]
struct SseCollector {
    buffer: String,
    remainder: Vec<u8>,
    events: Vec<Value>,
    gave_up: bool,
}

impl SseCollector {
    fn feed(&mut self, chunk: &[u8]) {
        if self.gave_up {
            return;
        }
        append_utf8_safe(&mut self.buffer, &mut self.remainder, chunk);
        while let Some(block) = take_sse_block(&mut self.buffer) {
            self.handle_block(&block);
        }
        if self.buffer.len() > MAX_SSE_BLOCK_BYTES {
            log::warn!("[Usage] SSE 事件块超过 {MAX_SSE_BLOCK_BYTES} 字节，停止解析用量");
            self.buffer.clear();
            self.gave_up = true;
        }
    }

    fn handle_block(&mut self, block: &str) {
        let data: Vec<&str> = block
            .lines()
            .filter_map(|line| strip_sse_field(line.trim_end_matches('\r'), "data"))
            .collect();
        if data.is_empty() {
            return;
        }
        let data = data.join("\n");
        if data.trim() == "[DONE]" {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            return;
        };
        let keep = matches!(
            event.get("type").and_then(Value::as_str),
            Some(
                "message_start"
                    | "message_delta"
                    | "error"
                    | "response.completed"
                    | "response.incomplete"
                    | "response.failed"
            )
        );
        if keep {
            self.events.push(event);
        }
    }

    fn parse(&mut self, client: ClientKind) -> Parsed {
        // 流在没有空行结尾时结束：把最后一块也处理掉
        let tail = std::mem::take(&mut self.buffer);
        if !self.gave_up && !tail.trim().is_empty() {
            self.handle_block(&tail);
        }
        let usage = match client {
            ClientKind::Claude => TokenUsage::from_claude_stream_events(&self.events),
            ClientKind::Codex => TokenUsage::from_codex_stream_events_auto(&self.events)
                .or_else(|| incomplete_response_usage(&self.events)),
        };
        Parsed {
            usage,
            error: self.events.iter().find_map(stream_error_message),
        }
    }
}

/// `response.incomplete`（达到 max_output_tokens 等）也带 usage，照样计费
fn incomplete_response_usage(events: &[Value]) -> Option<TokenUsage> {
    events
        .iter()
        .filter(|e| e.get("type").and_then(Value::as_str) == Some("response.incomplete"))
        .find_map(|e| {
            e.get("response")
                .and_then(TokenUsage::from_codex_response_auto)
        })
}

fn stream_error_message(event: &Value) -> Option<String> {
    let pointer = match event.get("type").and_then(Value::as_str)? {
        "error" => "/error/message",
        "response.failed" => "/response/error/message",
        _ => return None,
    };
    Some(
        event
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| event.to_string()),
    )
}

#[derive(Default)]
struct JsonCollector {
    body: BytesMut,
    overflow: bool,
}

impl JsonCollector {
    fn feed(&mut self, chunk: &[u8]) {
        if self.overflow {
            return;
        }
        if self.body.len() + chunk.len() > MAX_JSON_CAPTURE_BYTES {
            self.overflow = true;
            self.body = BytesMut::new();
            return;
        }
        self.body.extend_from_slice(chunk);
    }

    fn parse(&mut self, client: ClientKind) -> Parsed {
        let Ok(body) = serde_json::from_slice::<Value>(&self.body) else {
            return Parsed::default();
        };
        let usage = match client {
            ClientKind::Claude => TokenUsage::from_claude_response(&body),
            ClientKind::Codex => TokenUsage::from_codex_response_auto(&body),
        };
        Parsed { usage, error: None }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn truncate_chars(message: &str) -> String {
    let message = message.trim();
    if message.chars().count() <= MAX_ERROR_MESSAGE_CHARS {
        return message.to_string();
    }
    let mut out: String = message.chars().take(MAX_ERROR_MESSAGE_CHARS).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| format!("event: x\ndata: {e}\n\n"))
            .collect()
    }

    #[test]
    fn codex_tokens_subtract_cache_from_input() {
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 50,
            cache_read_tokens: 700,
            cache_creation_tokens: 100,
            model: None,
            message_id: None,
        };
        let codex = TokenCounts::from_usage(ClientKind::Codex, &usage);
        assert_eq!(codex.fresh_input, 200);
        let claude = TokenCounts::from_usage(ClientKind::Claude, &usage);
        assert_eq!(
            claude.fresh_input, 1000,
            "Anthropic 的 input 本来就不含缓存"
        );
    }

    #[test]
    fn sse_collector_keeps_only_usage_events_across_chunk_splits() {
        let body = sse(&[
            json!({"type":"message_start","message":{"id":"msg_1","model":"m","usage":{"input_tokens":12,"cache_read_input_tokens":30}}}),
            json!({"type":"content_block_delta","delta":{"text":"你好".repeat(100)}}),
            json!({"type":"message_delta","usage":{"output_tokens":7}}),
        ]);
        let mut collector = SseCollector::default();
        // 按 3 字节切，覆盖中文被截断的情况
        for chunk in body.as_bytes().chunks(3) {
            collector.feed(chunk);
        }
        assert_eq!(collector.events.len(), 2);
        let parsed = collector.parse(ClientKind::Claude);
        let usage = parsed.usage.unwrap();
        assert_eq!(
            (
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_tokens
            ),
            (12, 7, 30)
        );
        assert_eq!(usage.message_id.as_deref(), Some("msg_1"));
        assert!(parsed.error.is_none());
    }

    #[test]
    fn sse_collector_reports_stream_errors_and_trailing_block() {
        let mut collector = SseCollector::default();
        collector.feed(
            b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}",
        );
        let parsed = collector.parse(ClientKind::Codex);
        assert_eq!(parsed.error.as_deref(), Some("boom"));
    }

    #[test]
    fn incomplete_responses_still_count_usage() {
        let mut collector = SseCollector::default();
        collector.feed(sse(&[json!({
            "type": "response.incomplete",
            "response": {"id":"resp_1","model":"gpt","usage":{"input_tokens":10,"output_tokens":3}}
        })]).as_bytes());
        let usage = collector.parse(ClientKind::Codex).usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (10, 3));
    }

    #[test]
    fn json_collector_gives_up_on_oversized_bodies() {
        let mut collector = JsonCollector::default();
        collector.feed(&vec![b' '; MAX_JSON_CAPTURE_BYTES]);
        collector.feed(b"{}");
        assert!(collector.overflow);
        assert!(collector.parse(ClientKind::Claude).usage.is_none());
    }

    #[test]
    fn claude_roles() {
        let headers = HeaderMap::new();
        let role = |body: Value| infer_role(ClientKind::Claude, "/v1/messages", &headers, &body);
        let tools = json!([{"name":"Read","input_schema":{}}]);

        assert_eq!(
            role(
                json!({"model":"x/opus","tools":tools,"messages":[{"role":"user","content":"hi"}]})
            ),
            RequestRole::Main
        );
        assert_eq!(
            role(
                json!({"model":"x/opus","tools":tools,"messages":[{"role":"user","content":
                "<system-reminder>{\"__SUBAGENT_MARKER__\":{}}</system-reminder> go"}]})
            ),
            RequestRole::Subagent
        );
        assert_eq!(
            role(
                json!({"model":"x/opus","messages":[{"role":"user","content":
                "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools."}]})
            ),
            RequestRole::Compact
        );
        assert_eq!(
            role(json!({"model":"x/opus","messages":[{"role":"user","content":"title?"}]})),
            RequestRole::Background
        );
        assert_eq!(
            role(json!({"model":"claude-haiku-4-5","tools":tools,"messages":[]})),
            RequestRole::Background
        );
    }

    #[test]
    fn codex_roles() {
        let tools = json!({"tools":[{"type":"function","name":"shell"}]});
        let mut headers = HeaderMap::new();
        assert_eq!(
            infer_role(ClientKind::Codex, "/v1/responses", &headers, &tools),
            RequestRole::Main
        );
        assert_eq!(
            infer_role(ClientKind::Codex, "/v1/responses/compact", &headers, &tools),
            RequestRole::Compact
        );
        headers.insert("x-openai-subagent", "review".parse().unwrap());
        assert_eq!(
            infer_role(ClientKind::Codex, "/v1/responses", &headers, &tools),
            RequestRole::Subagent
        );
    }

    #[test]
    fn error_kinds() {
        let upstream = |status| ProxyError::UpstreamError { status, body: None };
        assert_eq!(
            ErrorKind::from_error(&upstream(429)),
            ErrorKind::RateLimited
        );
        assert_eq!(
            ErrorKind::from_error(&upstream(400)),
            ErrorKind::UpstreamRejected
        );
        assert_eq!(
            ErrorKind::from_error(&upstream(502)),
            ErrorKind::UpstreamServer
        );
        assert_eq!(
            ErrorKind::from_error(&ProxyError::ForwardFailed("x".into())),
            ErrorKind::Connection
        );
        assert_eq!(
            ErrorKind::from_error(&ProxyError::InvalidRequest("x".into())),
            ErrorKind::InvalidRequest
        );
    }

    #[test]
    fn truncates_long_error_messages() {
        let long = "错".repeat(MAX_ERROR_MESSAGE_CHARS + 10);
        let out = truncate_chars(&long);
        assert_eq!(out.chars().count(), MAX_ERROR_MESSAGE_CHARS + 1);
    }
}
