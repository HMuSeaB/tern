//! 用量采集端到端：真实监听的网关 + 假上游，检查 `UsageSink` 收到的事件。

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::StatusCode;
use axum::response::Response;
use bytes::Bytes;
use serde_json::{json, Value};
use tern_gateway::{
    ApiFormat, ClientKind, ErrorKind, Gateway, GatewayConfig, Outcome, ProviderAuth, ProviderSpec,
    RequestRole, RouteKind, TokenCounts, UpstreamProxy, UsageEvent, UsageSink,
};

#[derive(Default)]
struct Recorder(Mutex<Vec<UsageEvent>>);

impl UsageSink for Recorder {
    fn record(&self, event: UsageEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl Recorder {
    /// 事件在响应体结束 / 被丢弃时才发出，可能比客户端读完略晚
    async fn wait_one(&self) -> UsageEvent {
        for _ in 0..100 {
            {
                let events = self.0.lock().unwrap();
                if !events.is_empty() {
                    assert_eq!(events.len(), 1, "{events:#?}");
                    return events[0].clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("没有收到用量事件");
    }
}

/// 假上游：固定状态码和 content-type，响应体按给定分块发出，分块之间可停顿
async fn upstream(
    status: StatusCode,
    content_type: &'static str,
    chunks: Vec<String>,
    pause: Duration,
) -> String {
    let app = axum::Router::new().fallback(move || {
        let chunks = chunks.clone();
        async move {
            let stream = async_stream::stream! {
                for (i, chunk) in chunks.into_iter().enumerate() {
                    if i > 0 && !pause.is_zero() {
                        tokio::time::sleep(pause).await;
                    }
                    yield Ok::<Bytes, Infallible>(Bytes::from(chunk));
                }
            };
            Response::builder()
                .status(status)
                .header("content-type", content_type)
                .body(Body::from_stream(stream))
                .unwrap()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn gateway(
    base_url: &str,
    format: ApiFormat,
    default_provider: bool,
) -> (String, Arc<Recorder>) {
    let mut config = GatewayConfig::new(vec![ProviderSpec::new(
        "up",
        "Upstream",
        base_url,
        format,
        ProviderAuth::api_key("sk-upstream-key"),
    )]);
    config.upstream_proxy = UpstreamProxy::Direct;
    if default_provider {
        config.default_provider = Some("up".into());
    }
    let recorder = Arc::new(Recorder::default());
    let gateway = Gateway::new(config)
        .unwrap()
        .with_usage_sink(recorder.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(gateway.serve_on(listener, std::future::pending()));
    (format!("http://{addr}"), recorder)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn tools() -> Value {
    json!([{ "name": "Read", "description": "read", "input_schema": { "type": "object" } }])
}

#[tokio::test]
async fn claude_streaming_from_chat_records_fresh_input_and_cache() {
    // Chat 的 prompt_tokens 含缓存命中，转换层会扣成 Anthropic 的 fresh input
    let sse = [
        r#"data: {"id":"c1","model":"deepseek-v4-pro","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"},"finish_reason":null}]}"#,
        r#"data: {"id":"c1","model":"deepseek-v4-pro","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":80}}}"#,
        "data: [DONE]",
    ]
    .map(|line| format!("{line}\n\n"))
    .to_vec();
    let base = upstream(
        StatusCode::OK,
        "text/event-stream",
        sse,
        Duration::from_millis(30),
    )
    .await;
    let (gw, recorder) = gateway(&base, ApiFormat::OpenaiChat, false).await;

    let text = client()
        .post(format!("{gw}/v1/messages"))
        .header("x-claude-code-session-id", "sess-42")
        .json(&json!({
            "model": "up/deepseek-v4-pro",
            "max_tokens": 64,
            "stream": true,
            "tools": tools(),
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(text.contains("message_stop"), "{text}");

    let event = recorder.wait_one().await;
    assert_eq!(event.client, ClientKind::Claude);
    assert_eq!(event.outcome, Outcome::Success);
    assert_eq!(event.status, 200);
    assert_eq!(event.provider_id.as_deref(), Some("up"));
    assert_eq!(event.route_kind, Some(RouteKind::Explicit));
    assert_eq!(event.client_model, "up/deepseek-v4-pro");
    assert_eq!(event.upstream_model.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(event.role, RequestRole::Main);
    assert_eq!(event.session_id.as_deref(), Some("sess-42"));
    assert!(event.stream);
    assert_eq!(
        event.tokens,
        Some(TokenCounts {
            fresh_input: 20,
            output: 5,
            cache_read: 80,
            cache_write: 0,
        })
    );
    assert!(event.first_token_ms.is_some());
    assert!(event.duration_ms >= event.first_token_ms.unwrap());
}

#[tokio::test]
async fn codex_non_streaming_subtracts_cache_from_responses_input() {
    let body = json!({
        "id": "resp_1",
        "object": "response",
        "status": "completed",
        "model": "gpt-5.6",
        "output": [],
        "usage": {
            "input_tokens": 1000,
            "output_tokens": 40,
            "input_tokens_details": { "cached_tokens": 900 }
        }
    });
    let base = upstream(
        StatusCode::OK,
        "application/json",
        vec![body.to_string()],
        Duration::ZERO,
    )
    .await;
    let (gw, recorder) = gateway(&base, ApiFormat::OpenaiResponses, true).await;

    let response = client()
        .post(format!("{gw}/v1/responses"))
        .header("x-openai-subagent", "review")
        .json(&json!({ "model": "gpt-5.6", "input": "hi", "tools": [{"type":"function","name":"shell"}] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.bytes().await.unwrap();

    let event = recorder.wait_one().await;
    assert_eq!(event.client, ClientKind::Codex);
    assert_eq!(event.route_kind, Some(RouteKind::Fallback));
    assert_eq!(event.role, RequestRole::Subagent);
    assert_eq!(event.response_model.as_deref(), Some("gpt-5.6"));
    assert_eq!(event.message_id.as_deref(), Some("resp_1"));
    assert_eq!(event.tokens.unwrap().fresh_input, 100);
    assert_eq!(event.tokens.unwrap().cache_read, 900);
    assert!(!event.stream);
    assert!(event.first_token_ms.is_none());
}

#[tokio::test]
async fn upstream_rate_limit_is_a_failure_without_tokens() {
    let error = json!({"error":{"type":"rate_limit_error","message":"gateway_concurrency_limit"}});
    let base = upstream(
        StatusCode::TOO_MANY_REQUESTS,
        "application/json",
        vec![error.to_string()],
        Duration::ZERO,
    )
    .await;
    let (gw, recorder) = gateway(&base, ApiFormat::Anthropic, false).await;

    let response = client()
        .post(format!("{gw}/v1/messages"))
        .json(&json!({
            "model": "up/claude-sonnet-4-6",
            "max_tokens": 8,
            "messages": [{ "role": "user", "content": "<system-reminder>{\"__SUBAGENT_MARKER__\":{}}</system-reminder>go" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);

    let event = recorder.wait_one().await;
    assert_eq!(event.outcome, Outcome::Failed);
    assert_eq!(event.status, 429);
    assert_eq!(event.error_kind, Some(ErrorKind::RateLimited));
    assert!(event
        .error_message
        .as_deref()
        .unwrap()
        .contains("gateway_concurrency_limit"));
    assert_eq!(event.role, RequestRole::Subagent);
    assert!(event.tokens.is_none(), "失败请求不能有 token");
    assert!(event.response_model.is_none());
}

#[tokio::test]
async fn unroutable_and_unauthorized_requests_are_recorded() {
    let (gw, recorder) = gateway("http://127.0.0.1:1", ApiFormat::Anthropic, false).await;
    let response = client()
        .post(format!("{gw}/v1/messages"))
        .json(&json!({ "model": "claude-haiku-4-5", "max_tokens": 8, "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);

    let event = recorder.wait_one().await;
    assert_eq!(event.error_kind, Some(ErrorKind::InvalidRequest));
    assert_eq!(event.client_model, "claude-haiku-4-5");
    assert!(event.provider_id.is_none());
    assert_eq!(event.role, RequestRole::Background);
}

#[tokio::test]
async fn count_tokens_is_not_recorded() {
    let base = upstream(
        StatusCode::OK,
        "application/json",
        vec![r#"{"input_tokens":12}"#.into()],
        Duration::ZERO,
    )
    .await;
    let (gw, recorder) = gateway(&base, ApiFormat::Anthropic, false).await;
    let response = client()
        .post(format!("{gw}/v1/messages/count_tokens"))
        .json(&json!({ "model": "up/claude-opus-5", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.bytes().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(recorder.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn client_disconnect_mid_stream_is_recorded_as_aborted() {
    let start = r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_9","type":"message","role":"assistant","model":"claude-opus-5","content":[],"usage":{"input_tokens":50,"cache_read_input_tokens":1000}}}

"#;
    let delta = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n";
    let mut chunks = vec![start.to_string()];
    chunks.extend(std::iter::repeat_n(delta.to_string(), 50));
    let base = upstream(
        StatusCode::OK,
        "text/event-stream",
        chunks,
        Duration::from_millis(100),
    )
    .await;
    let (gw, recorder) = gateway(&base, ApiFormat::Anthropic, false).await;

    let mut response = client()
        .post(format!("{gw}/v1/messages"))
        .json(&json!({
            "model": "up/claude-opus-5",
            "max_tokens": 64,
            "stream": true,
            "tools": tools(),
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .send()
        .await
        .unwrap();
    // 读到 message_start 就断开
    let first = response.chunk().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("message_start"));
    drop(response);

    let event = recorder.wait_one().await;
    assert_eq!(event.outcome, Outcome::Aborted);
    assert_eq!(event.message_id.as_deref(), Some("msg_9"));
    // 已经发生的输入 / 缓存读照样计费
    let tokens = event.tokens.unwrap();
    assert_eq!((tokens.fresh_input, tokens.cache_read), (50, 1000));
}
