//! 本地网关：axum 服务 + 按 `provider/model` 路由的转发层。
//!
//! 对应 cc-switch 的 `server.rs` / `handlers.rs` / `forwarder.rs` / `response_processor.rs`，
//! 但不从上游搬运：那几个文件和 Tauri、SQLite、托盘状态缠在一起。tern 的版本只做
//! 一件事——收到请求、路由到供应商、改写请求、转换响应。
//!
//! 用量只产出事件（见 [`usage`]），存储由宿主通过 [`UsageSink`] 接入。
//!
//! 暂不包含（后续阶段）：故障转移与熔断、Copilot 动态端点 / 模型解析、
//! 原生 Anthropic 上游的请求头大小写保持（cc-switch 用 hyper 原始写入实现）。

mod aggregate;
mod errors;
mod handlers;
mod response;
mod upstream;
pub mod usage;

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::routing::{get, post};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::provider::{ProviderAuth, ProviderSpec};
use crate::proxy::providers::codex_chat_history::CodexChatHistoryStore;
use crate::proxy::providers::gemini_shadow::GeminiShadowStore;
use crate::proxy::ProxyError;
use crate::resilience::{Breakers, ResilienceConfig};
use crate::router::ModelRouter;
pub use usage::UsageSink;

/// 上游请求走哪个代理
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "url", rename_all = "snake_case")]
pub enum UpstreamProxy {
    /// 跟随 `HTTP(S)_PROXY` / `ALL_PROXY` 环境变量
    #[default]
    System,
    /// 直连
    Direct,
    /// 指定代理，支持 http / https / socks5 / socks5h
    Url(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConfig {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    pub providers: Vec<ProviderSpec>,
    /// 模型名没有可识别的 `provider/` 前缀时使用
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// 设置后，客户端必须在 `Authorization: Bearer` 或 `x-api-key` 里带上它。
    /// 不设置时任何能连到监听地址的进程都能借用供应商的 key。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default)]
    pub upstream_proxy: UpstreamProxy,
    /// 非流式请求的整体超时；流式请求用它限制等待响应头的时间
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// 流式响应两个数据块之间的最长间隔，0 表示不限制
    #[serde(default = "default_stream_idle_timeout_secs")]
    pub stream_idle_timeout_secs: u64,
    /// 故障转移与熔断。缺省开启：默认那家挂了就转下一家，失败到阈值熔断
    #[serde(default)]
    pub resilience: ResilienceConfig,
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 15800))
}

fn default_request_timeout_secs() -> u64 {
    600
}

fn default_stream_idle_timeout_secs() -> u64 {
    300
}

impl GatewayConfig {
    pub fn new(providers: Vec<ProviderSpec>) -> Self {
        Self {
            listen: default_listen(),
            providers,
            default_provider: None,
            access_token: None,
            upstream_proxy: UpstreamProxy::default(),
            request_timeout_secs: default_request_timeout_secs(),
            stream_idle_timeout_secs: default_stream_idle_timeout_secs(),
            resilience: ResilienceConfig::default(),
        }
    }
}

/// 订阅类供应商（Copilot / ChatGPT / xAI）的动态 token
#[derive(Clone)]
pub struct ManagedToken {
    pub token: String,
    /// Codex OAuth 需要以 `ChatGPT-Account-Id` 头发送
    pub account_id: Option<String>,
}

/// 由宿主（桌面应用）实现：登录、刷新、多账号选择都在宿主里，网关只按需取 token
pub trait TokenProvider: Send + Sync {
    fn token<'a>(
        &'a self,
        spec: &'a ProviderSpec,
    ) -> BoxFuture<'a, Result<ManagedToken, ProxyError>>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Timeouts {
    pub request: Duration,
    /// `Duration::ZERO` 表示不限制
    pub stream_idle: Duration,
}

pub(crate) struct GatewayState {
    router: RwLock<Arc<ModelRouter>>,
    pub client: reqwest::Client,
    pub access_token: Option<String>,
    pub timeouts: Timeouts,
    pub tokens: Option<Arc<dyn TokenProvider>>,
    pub usage: Option<Arc<dyn UsageSink>>,
    /// Codex → Chat 时补全工具调用历史
    pub chat_history: Arc<CodexChatHistoryStore>,
    /// Claude → Gemini 时保存思维签名
    pub gemini_shadow: Arc<GeminiShadowStore>,
    /// 故障转移与熔断。每个供应商一个熔断器，**进程内有效、不落盘**
    pub breakers: Breakers,
    pub resilience: ResilienceConfig,
}

impl GatewayState {
    pub fn router(&self) -> Arc<ModelRouter> {
        self.router
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// 网关句柄。`Clone` 很便宜，共享同一份状态。
#[derive(Clone)]
pub struct Gateway {
    state: Arc<GatewayState>,
    listen: SocketAddr,
}

impl Gateway {
    pub fn new(config: GatewayConfig) -> Result<Self, ProxyError> {
        let router = ModelRouter::new(config.providers, config.default_provider)?;
        let client = build_client(&config.upstream_proxy)?;
        let access_token = config
            .access_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());

        Ok(Self {
            state: Arc::new(GatewayState {
                router: RwLock::new(Arc::new(router)),
                client,
                access_token,
                timeouts: Timeouts {
                    request: Duration::from_secs(config.request_timeout_secs.max(1)),
                    stream_idle: Duration::from_secs(config.stream_idle_timeout_secs),
                },
                tokens: None,
                usage: None,
                chat_history: Arc::new(CodexChatHistoryStore::default()),
                gemini_shadow: Arc::new(GeminiShadowStore::default()),
                breakers: Breakers::default(),
                resilience: config.resilience,
            }),
            listen: config.listen,
        })
    }

    /// 接入订阅 token 来源。必须在 `serve` / `app` 之前调用。
    pub fn with_token_provider(mut self, tokens: Arc<dyn TokenProvider>) -> Self {
        match Arc::get_mut(&mut self.state) {
            Some(state) => state.tokens = Some(tokens),
            None => log::warn!("[Gateway] 网关已在运行，忽略 token provider"),
        }
        self
    }

    /// 接入用量记录。必须在 `serve` / `app` 之前调用。
    pub fn with_usage_sink(mut self, sink: Arc<dyn UsageSink>) -> Self {
        match Arc::get_mut(&mut self.state) {
            Some(state) => state.usage = Some(sink),
            None => log::warn!("[Gateway] 网关已在运行，忽略 usage sink"),
        }
        self
    }

    /// 热更新供应商列表，进行中的请求继续用旧表
    pub fn set_providers(
        &self,
        providers: Vec<ProviderSpec>,
        default_provider: Option<String>,
    ) -> Result<(), ProxyError> {
        let router = Arc::new(ModelRouter::new(providers, default_provider)?);
        // 顺手把不存在的供应商的熔断器摘掉。不摘的话进程活得越久里面的死条目
        // 越多，用户删过的供应商的失败计数一直占着内存不释放
        let alive: Vec<String> = router.providers().map(|spec| spec.id.clone()).collect();
        self.state.breakers.retain(&alive);
        *self
            .state
            .router
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = router;
        Ok(())
    }

    /// 当前各供应商的熔断状态。给面板和 `tern check` 显示"哪几家被摘了"。
    pub async fn breaker_states(&self) -> Vec<(String, String)> {
        self.state
            .breakers
            .snapshot(&(&self.state.resilience).into())
            .await
            .into_iter()
            .map(|(id, state)| (id, state.to_string()))
            .collect()
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen
    }

    /// axum 应用，便于嵌入到宿主自己的服务里或在测试中直接驱动
    pub fn app(&self) -> axum::Router {
        axum::Router::new()
            .route("/health", get(handlers::health))
            .route("/v1/models", get(handlers::models))
            .route("/models", get(handlers::models))
            .route("/v1/messages", post(handlers::claude_messages))
            .route("/v1/messages/count_tokens", post(handlers::claude_messages))
            .route("/v1/responses", post(handlers::codex_responses))
            .route("/responses", post(handlers::codex_responses))
            .route("/v1/responses/compact", post(handlers::codex_responses))
            .route("/responses/compact", post(handlers::codex_responses))
            // 请求体常带大段上下文和内联图片；与 cc-switch 一致放到 200 MB
            .layer(axum::extract::DefaultBodyLimit::max(200 * 1024 * 1024))
            .with_state(self.state.clone())
    }

    /// 绑定配置里的地址并一直运行，直到 `shutdown` 完成
    pub async fn serve(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), ProxyError> {
        let listener = tokio::net::TcpListener::bind(self.listen)
            .await
            .map_err(|e| ProxyError::BindFailed(format!("{}: {e}", self.listen)))?;
        self.serve_on(listener, shutdown).await
    }

    /// 在已绑定的 listener 上运行（测试里用 `127.0.0.1:0` 取随机端口）
    pub async fn serve_on(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), ProxyError> {
        if let Ok(addr) = listener.local_addr() {
            log::info!("[Gateway] 监听 http://{addr}");
            if !addr.ip().is_loopback() && self.state.access_token.is_none() {
                log::warn!(
                    "[Gateway] 监听在非回环地址 {addr} 且未设置 accessToken，\
                     局域网内任何人都能使用你的供应商 key"
                );
            }
        }
        axum::serve(listener, self.app())
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(|e| ProxyError::Internal(format!("网关异常退出: {e}")))
    }
}

/// 与 cc-switch `http_client::build_client` 一致：关闭自动解压，
/// 由网关按 content-encoding 自行处理，避免 reqwest 改写 accept-encoding
fn build_client(proxy: &UpstreamProxy) -> Result<reqwest::Client, ProxyError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .tcp_keepalive(Duration::from_secs(60))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd();

    builder = match proxy {
        UpstreamProxy::System => builder,
        UpstreamProxy::Direct => builder.no_proxy(),
        UpstreamProxy::Url(url) => {
            let proxy = reqwest::Proxy::all(url.trim()).map_err(|e| {
                ProxyError::ConfigError(format!(
                    "上游代理地址无效 ({}): {e}",
                    crate::proxy::http_client::mask_url(url)
                ))
            })?;
            builder.proxy(proxy)
        }
    };

    builder
        .build()
        .map_err(|e| ProxyError::Internal(format!("创建 HTTP 客户端失败: {e}")))
}

/// 订阅类认证需要 token provider
pub(crate) fn needs_token_provider(spec: &ProviderSpec) -> bool {
    matches!(
        spec.auth,
        ProviderAuth::GithubCopilot { .. }
            | ProviderAuth::CodexOauth { .. }
            | ProviderAuth::XaiOauth { .. }
    )
}
