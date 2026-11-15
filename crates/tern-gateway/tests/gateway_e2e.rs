//! 端到端：真实监听的网关 + 本地假上游，走完整 HTTP 链路。

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tern_gateway::{ApiFormat, Gateway, GatewayConfig, ProviderAuth, ProviderSpec, UpstreamProxy};

#[derive(Debug, Clone)]
struct Captured {
    path: String,
    headers: HeaderMap,
    body: Value,
}

/// 假上游：记录收到的请求，按预设返回固定响应
struct MockUpstream {
    addr: SocketAddr,
    captured: Arc<Mutex<Vec<Captured>>>,
}

impl MockUpstream {
    async fn start(status: StatusCode, content_type: &'static str, body: String) -> Self {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let sink = captured.clone();
        let app =
            axum::Router::new().fallback(move |uri: Uri, headers: HeaderMap, bytes: Bytes| {
                let sink = sink.clone();
                let body = body.clone();
                async move {
                    sink.lock().unwrap().push(Captured {
                        path: uri
                            .path_and_query()
                            .map(|p| p.to_string())
                            .unwrap_or_default(),
                        headers,
                        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                    });
                    Response::builder()
                        .status(status)
                        .header("content-type", content_type)
                        .body(axum::body::Body::from(body))
                        .unwrap()
                        .into_response()
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { addr, captured }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn only_request(&self) -> Captured {
        let captured = self.captured.lock().unwrap();
        assert_eq!(captured.len(), 1, "{captured:?}");
        captured[0].clone()
    }
}

async fn start_gateway(config: GatewayConfig) -> String {
    let gateway = Gateway::new(config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(gateway.serve_on(listener, std::future::pending()));
    format!("http://{addr}")
}

fn config(providers: Vec<ProviderSpec>) -> GatewayConfig {
    let mut config = GatewayConfig::new(providers);
    // 测试机可能设了系统代理，假上游在本机
    config.upstream_proxy = UpstreamProxy::Direct;
    config
}

fn spec(id: &str, base_url: String, format: ApiFormat) -> ProviderSpec {
    ProviderSpec::new(
        id,
        id,
        base_url,
        format,
        ProviderAuth::api_key("sk-upstream-key"),
    )
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn chat_completion() -> String {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "deepseek-chat",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": "Hi there" },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 }
    })
    .to_string()
}

#[tokio::test]
async fn claude_client_to_chat_upstream_non_streaming() {
    let upstream = MockUpstream::start(StatusCode::OK, "application/json", chat_completion()).await;
    let gateway = start_gateway(config(vec![spec(
        "deepseek",
        upstream.base_url(),
        ApiFormat::OpenaiChat,
    )]))
    .await;

    let response = client()
        .post(format!("{gateway}/v1/messages?beta=true"))
        .header("x-api-key", "client-placeholder")
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["type"], "message");
    assert_eq!(body["content"][0]["text"], "Hi there");
    assert_eq!(body["stop_reason"], "end_turn");

    let sent = upstream.only_request();
    assert_eq!(sent.path, "/v1/chat/completions");
    assert_eq!(sent.body["model"], "deepseek-chat");
    assert_eq!(sent.headers["authorization"], "Bearer sk-upstream-key");
    // 客户端的占位 key 不能透传给上游
    assert!(sent.headers.get("x-api-key").is_none());
}

#[tokio::test]
async fn claude_client_to_chat_upstream_streaming() {
    let sse = [
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"},"finish_reason":null}]}"#,
        r#"data: {"id":"c1","model":"deepseek-chat","choices":[{"index":0,"delta":{"content":"lo"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
        "data: [DONE]",
    ]
    .join("\n\n")
        + "\n\n";
    let upstream = MockUpstream::start(StatusCode::OK, "text/event-stream", sse).await;
    let gateway = start_gateway(config(vec![spec(
        "deepseek",
        upstream.base_url(),
        ApiFormat::OpenaiChat,
    )]))
    .await;

    let response = client()
        .post(format!("{gateway}/v1/messages"))
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "max_tokens": 64,
            "stream": true,
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let text = response.text().await.unwrap();
    assert!(text.contains("event: message_start"), "{text}");
    assert!(text.contains("\"text\":\"Hel\""), "{text}");
    assert!(text.contains("event: message_stop"), "{text}");

    assert_eq!(
        upstream.only_request().body["stream_options"]["include_usage"],
        true
    );
}

#[tokio::test]
async fn codex_client_to_anthropic_upstream_streaming() {
    let sse = [
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-5","content":[],"stop_reason":null,"usage":{"input_tokens":4,"output_tokens":0}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        r#"event: message_stop
data: {"type":"message_stop"}"#,
    ]
    .join("\n\n")
        + "\n\n";
    let upstream = MockUpstream::start(StatusCode::OK, "text/event-stream", sse).await;
    let gateway = start_gateway(config(vec![spec(
        "relay",
        upstream.base_url(),
        ApiFormat::Anthropic,
    )]))
    .await;

    let response = client()
        .post(format!("{gateway}/v1/responses"))
        .header("authorization", "Bearer codex-placeholder")
        .header("accept", "text/event-stream")
        .header("originator", "codex_cli_rs")
        .header("session_id", "sess-1")
        .json(&json!({
            "model": "relay/claude-sonnet-5[1m]",
            "stream": true,
            "instructions": "You are helpful.",
            "input": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    assert!(text.contains("response.output_text.delta"), "{text}");
    assert!(text.contains("response.completed"), "{text}");

    let sent = upstream.only_request();
    assert_eq!(sent.path, "/v1/messages");
    assert_eq!(sent.body["model"], "claude-sonnet-5");
    assert_eq!(sent.headers["x-api-key"], "sk-upstream-key");
    assert_eq!(sent.headers["anthropic-version"], "2023-06-01");
    assert_eq!(sent.headers["accept"], "application/json");
    assert!(sent.headers["anthropic-beta"]
        .to_str()
        .unwrap()
        .contains("context-1m-2025-08-07"));
    // Codex 指纹头不能泄露给 Anthropic 上游
    assert!(sent.headers.get("originator").is_none());
    assert!(sent.headers.get("session_id").is_none());
    assert!(sent.headers.get("authorization").is_none());
}

#[tokio::test]
async fn codex_client_to_chat_upstream_non_streaming() {
    let upstream = MockUpstream::start(StatusCode::OK, "application/json", chat_completion()).await;
    let gateway = start_gateway(config(vec![spec(
        "deepseek",
        upstream.base_url(),
        ApiFormat::OpenaiChat,
    )]))
    .await;

    let response = client()
        .post(format!("{gateway}/responses"))
        .json(&json!({
            "model": "deepseek/deepseek-chat",
            "input": [{ "role": "user", "content": "hi" }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["object"], "response");
    assert_eq!(body["status"], "completed");
    assert_eq!(upstream.only_request().path, "/v1/chat/completions");
}

#[tokio::test]
async fn anthropic_passthrough_streams_bytes_unchanged() {
    let sse = "event: ping\ndata: {\"type\":\"ping\"}\n\n".to_string();
    let upstream = MockUpstream::start(StatusCode::OK, "text/event-stream", sse.clone()).await;
    let gateway = start_gateway(config(vec![spec(
        "anthropic",
        upstream.base_url(),
        ApiFormat::Anthropic,
    )]))
    .await;

    let response = client()
        .post(format!("{gateway}/v1/messages?beta=true"))
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "interleaved-thinking-2025-05-14")
        .json(&json!({
            "model": "anthropic/claude-opus-5",
            "max_tokens": 64,
            "stream": true,
            "messages": [{ "role": "user", "content": "hello" }]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), sse);

    let sent = upstream.only_request();
    assert_eq!(sent.path, "/v1/messages?beta=true");
    assert_eq!(
        sent.headers["anthropic-beta"],
        "interleaved-thinking-2025-05-14"
    );
    assert_eq!(sent.headers["x-api-key"], "sk-upstream-key");
}

#[tokio::test]
async fn upstream_error_is_reshaped_for_each_client() {
    let error_body = json!({ "base_resp": { "status_code": 1004, "status_msg": "invalid key" } });
    let upstream = MockUpstream::start(
        StatusCode::UNAUTHORIZED,
        "application/json",
        error_body.to_string(),
    )
    .await;
    let gateway = start_gateway(config(vec![spec(
        "minimax",
        upstream.base_url(),
        ApiFormat::OpenaiChat,
    )]))
    .await;

    let codex = client()
        .post(format!("{gateway}/v1/responses"))
        .json(&json!({ "model": "minimax/abab", "input": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(codex.status(), 401);
    let body: Value = codex.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("invalid key"));
    assert_eq!(body["error"]["upstream_status"], 401);

    let claude = client()
        .post(format!("{gateway}/v1/messages"))
        .json(&json!({ "model": "minimax/abab", "max_tokens": 8, "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(claude.status(), 401);
    let body: Value = claude.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "authentication_error");
}

#[tokio::test]
async fn unroutable_model_is_rejected_without_calling_upstream() {
    let upstream = MockUpstream::start(StatusCode::OK, "application/json", chat_completion()).await;
    let gateway = start_gateway(config(vec![spec(
        "deepseek",
        upstream.base_url(),
        ApiFormat::OpenaiChat,
    )]))
    .await;

    let response = client()
        .post(format!("{gateway}/v1/messages"))
        .json(&json!({ "model": "claude-haiku-4-5", "max_tokens": 8, "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("deepseek"));
    assert!(upstream.captured.lock().unwrap().is_empty());
}

#[tokio::test]
async fn access_token_is_enforced_when_configured() {
    let upstream = MockUpstream::start(StatusCode::OK, "application/json", chat_completion()).await;
    let mut config = config(vec![spec(
        "deepseek",
        upstream.base_url(),
        ApiFormat::OpenaiChat,
    )]);
    config.access_token = Some("local-secret".into());
    let gateway = start_gateway(config).await;

    let request = || {
        client()
            .post(format!("{gateway}/v1/messages"))
            .json(&json!({
                "model": "deepseek/deepseek-chat",
                "max_tokens": 8,
                "messages": [{ "role": "user", "content": "hi" }]
            }))
    };

    assert_eq!(request().send().await.unwrap().status(), 401);
    assert_eq!(
        request()
            .header("x-api-key", "wrong")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        request()
            .header("x-api-key", "local-secret")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        request()
            .bearer_auth("local-secret")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    // 网关自己的 token 不能被当作供应商 key 发出去
    let captured = upstream.captured.lock().unwrap();
    assert!(captured
        .iter()
        .all(|c| c.headers["authorization"] == "Bearer sk-upstream-key"));
}

#[tokio::test]
async fn subscription_without_token_provider_fails_with_auth_error() {
    let gateway = start_gateway(config(vec![ProviderSpec::new(
        "chatgpt",
        "ChatGPT",
        "",
        ApiFormat::OpenaiResponses,
        ProviderAuth::CodexOauth { account_id: None },
    )]))
    .await;

    let response = client()
        .post(format!("{gateway}/v1/responses"))
        .json(&json!({ "model": "chatgpt/gpt-5.5", "input": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn models_lists_configured_providers() {
    let gateway = start_gateway(config(vec![
        spec(
            "deepseek",
            "https://a.example".into(),
            ApiFormat::OpenaiChat,
        ),
        spec("kimi", "https://b.example".into(), ApiFormat::Anthropic),
    ]))
    .await;

    let body: Value = client()
        .get(format!("{gateway}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["deepseek/*", "kimi/*"]);
}
