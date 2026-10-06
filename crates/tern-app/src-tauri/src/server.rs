//! 内嵌网关的启停管理。
//!
//! # 为什么要内嵌
//!
//! 用户要的是"点 exe 就跑"。若沿用 `tern serve` 那套，得先开一个终端跑网关、再开应用
//! 看面板——两步都不符合预期，而且网关死了面板还在显示旧数据更难排查。所以应用自己
//! 持有网关：一个开关，启停都在进程内完成。
//!
//! # 停机怎么实现
//!
//! 网关的 `serve` 只在 Ctrl+C 时返回。要让 UI 上的开关真正停掉它，就得给它一个能
//! 触发的信号。做法是 `select!` 一个中止信号 future：停的时候把 signal  pending 的
//! waker 唤醒，`serve` 随即返回、线程自然结束。没有轮询、没有轮转检查。
//!
//! tokio 的 `Notify` 正好干这个：停机方 `notify_waiters`，serve 方 `notified().await`。
//! 用 `waiters` 而不是 `notify_one`，避免"通知在 await 之前就到达"的竞态丢信号——
//! `Notified` future 是持久的，先 `notified()` 再 `notify_waiters()` 也照样醒。

use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::State;
use tokio::sync::Notify;
use tern_gateway::{Gateway, GatewayConfig};
use tern_store::Store;

use crate::error::{AppError, Result};
use crate::AppState;

struct Running {
    listen: SocketAddr,
    shutdown: Arc<Notify>,
}

/// 网关状态。`None` 表示没在跑。
#[derive(Default)]
pub struct ServerState {
    inner: Mutex<Option<Running>>,
    /// 记录"本来在跑、但因为出错退了"，供前端提示
    last_error: Mutex<Option<String>>,
}

impl ServerState {
    fn snapshot(&self) -> Option<SocketAddr> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|r| r.listen)
    }

    /// 停掉网关（若在跑）。窗口关闭时调用，避免留下占着端口的孤儿进程。
    /// 幂等：没在跑时什么都不做。
    pub fn shutdown(&self) {
        if let Some(running) = self.inner.lock().unwrap_or_else(|p| p.into_inner()).take() {
            running.shutdown.notify_waiters();
            log::info!("[tern-app] 随窗口关闭停止网关");
        }
    }
}

/// 给前端的服务状态
#[derive(Debug, Serialize, Clone)]
pub struct ServerStatus {
    pub running: bool,
    /// 监听地址，没运行时为空
    pub listen: Option<String>,
    /// 配置里的供应商数量：没启动也能显示"配了几个"
    pub provider_count: usize,
    /// 启动过一次但后来自己退了，带原因
    pub last_error: Option<String>,
}

/// 探一下端口能不能绑。绑得上再立刻放开，交给真正的 serve。
///
/// 提前探测是为了给出人能懂的错话：axum 的 bind 错误原样抛给用户，
/// 多半只会看到一句 `Address already in use`，不知道该怎么办。
fn ensure_port_free(listen: SocketAddr) -> Result<()> {
    match TcpListener::bind(listen) {
        Ok(_) => Ok(()),
        Err(_) => Err(AppError::PortInUse {
            listen: listen.to_string(),
        }),
    }
}

#[tauri::command]
pub fn server_start(
    state: State<'_, AppState>,
    server: State<'_, ServerState>,
) -> Result<ServerStatus> {
    // 幂等：已经在跑就直接回报状态，不重复起
    if let Some(listen) = server.snapshot() {
        return Ok(status_of(&server, Some(listen)));
    }

    let config_path = crate::config::config_path()?;
    let config = crate::config::load(&config_path)?;
    let db_path = crate::config::db_path_for(&config_path);

    ensure_port_free(config.listen)?;

    let store = Arc::new(Store::open(&db_path).map_err(|e| AppError::Store(e.to_string()))?);
    state.set_shared_store(store.clone(), db_path.clone());

    for error in store.set_multipliers(multipliers_of(&config)) {
        log::warn!("[tern-app] {error}");
    }

    let (recorder, recorder_handle) = tern_store::spawn_recorder(store, log_usage);

    let gateway = Gateway::new(config.clone()).map_err(|e| AppError::Config(e.to_string()))?;
    let gateway = gateway.with_usage_sink(recorder);

    let shutdown = Arc::new(Notify::new());
    let serve_shutdown = shutdown.clone();
    let thread_shutdown = shutdown.clone();
    let thread_name = format!("tern-gateway {}", config.listen);

    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            // 独立线程，不复用 Tauri 的 async runtime：网关要长期跑，
            // 抢 UI 的运行时会把窗口操作拖慢
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::error!("[tern-app] 创建运行时失败: {error}");
                    return;
                }
            };
            let result = runtime.block_on(gateway.serve(async move {
                // 停机信号由 server_stop 唤醒；Ctrl+C 也一并接上，
                // 这样从托盘 / 任务管理器结束进程时能正常补记 aborted
                tokio::select! {
                    _ = serve_shutdown.notified() => log::info!("[tern-app] 收到停止信号"),
                    _ = tokio::signal::ctrl_c() => log::info!("[tern-app] 收到 Ctrl+C"),
                }
            }));
            // 先停 runtime：进行中的流被丢弃时会补记 aborted，
            // 再让写入线程把队列里剩下的写完
            drop(runtime);
            drop(recorder_handle);
            if let Err(error) = result {
                log::error!("[tern-app] 网关退出: {error}");
            }
        })
        .map_err(|e| AppError::Store(format!("启动网关线程失败: {e}")))?;

    *server.inner.lock().unwrap_or_else(|p| p.into_inner()) = Some(Running {
        listen: config.listen,
        shutdown: thread_shutdown,
    });
    *server.last_error.lock().unwrap_or_else(|p| p.into_inner()) = None;

    log::info!("[tern-app] 网关已启动 {}", config.listen);
    Ok(status_of(&server, Some(config.listen)))
}

fn multipliers_of(config: &GatewayConfig) -> impl Iterator<Item = (&str, &str)> {
    config.providers.iter().filter_map(|spec| {
        spec.cost_multiplier
            .as_deref()
            .map(|value| (spec.id.as_str(), value))
    })
}

/// 每条请求一行摘要，与 `tern serve` 的 log_usage 同口径
fn log_usage(event: &tern_gateway::UsageEvent, inserted: &tern_store::Inserted) {
    if matches!(inserted, tern_store::Inserted::Duplicate) {
        return;
    }
    let provider = event.provider_id.as_deref().unwrap_or("-");
    let model = event
        .response_model
        .as_deref()
        .or(event.upstream_model.as_deref())
        .unwrap_or(&event.client_model);
    log::debug!(
        "[usage] {} {} {} ({}) {}ms",
        event.outcome.as_str(),
        provider,
        model,
        event.role.as_str(),
        event.duration_ms
    );
}

/// 停止网关。没在跑时返回 Ok（幂等）。
#[tauri::command]
pub fn server_stop(server: State<'_, ServerState>) -> Result<ServerStatus> {
    let running = server.inner.lock().unwrap_or_else(|p| p.into_inner()).take();
    if let Some(running) = running {
        running.shutdown.notify_waiters();
    }
    Ok(status_of(&server, None))
}

#[tauri::command]
pub fn server_status(server: State<'_, ServerState>) -> Result<ServerStatus> {
    let running = server.snapshot();
    // 配置读不到时不该让整个状态查询失败：进程已经起来了，
    // 用户要看到"有几个供应商"，那比一份配置错误信息更有用
    let provider_count = config_summary().map(|summary| summary.providers.len()).unwrap_or(0);
    Ok(status_of(&server, running).with_provider_count(provider_count))
}

impl ServerStatus {
    fn with_provider_count(mut self, provider_count: usize) -> Self {
        if self.provider_count == 0 {
            self.provider_count = provider_count;
        }
        self
    }
}

fn status_of(server: &ServerState, running: Option<SocketAddr>) -> ServerStatus {
    let last_error = server
        .last_error
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    ServerStatus {
        running: running.is_some(),
        listen: running.map(|listen| listen.to_string()),
        provider_count: 0,
        last_error: running.is_none().then_some(last_error).flatten(),
    }
}

/// 给前端的配置摘要：没启动时也要能显示配了哪些供应商
#[derive(Debug, Serialize, Clone)]
pub struct ConfigSummary {
    pub path: String,
    pub listen: String,
    pub default_provider: Option<String>,
    pub providers: Vec<ProviderSummary>,
    /// 配了但启不动的（key 是占位符之类）
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_format: String,
    /// 第三方网关：联网工具会失效（同 web_tools 的判据）
    pub web_tools_at_risk: bool,
    auth_kind: String,
}

#[tauri::command]
pub fn config_summary() -> Result<ConfigSummary> {
    let path = crate::config::config_path()?;
    let config = crate::config::load(&path)?;
    let warnings = crate::config::warnings(&config);

    let providers = config
        .providers
        .iter()
        .map(|spec| ProviderSummary {
            id: spec.id.clone(),
            name: spec.name.clone(),
            base_url: spec.effective_base_url(),
            api_format: spec.effective_api_format().to_string(),
            web_tools_at_risk: matches!(
                tern_gateway::assess(spec),
                tern_gateway::WebToolsSupport::ThirdParty
            ),
            auth_kind: auth_kind(&spec.auth).to_string(),
        })
        .collect();

    Ok(ConfigSummary {
        path: path.display().to_string(),
        listen: config.listen.to_string(),
        default_provider: config.default_provider.clone(),
        providers,
        warnings,
    })
}

fn auth_kind(auth: &tern_gateway::ProviderAuth) -> &'static str {
    match auth {
        tern_gateway::ProviderAuth::None => "none",
        tern_gateway::ProviderAuth::ApiKey { .. } => "api_key",
        tern_gateway::ProviderAuth::GoogleOauth { .. } => "google_oauth",
        tern_gateway::ProviderAuth::GithubCopilot { .. } => "github_copilot",
        tern_gateway::ProviderAuth::CodexOauth { .. } => "codex_oauth",
        tern_gateway::ProviderAuth::XaiOauth { .. } => "xai_oauth",
    }
}

/// 打开配置所在目录，方便用户手动改
#[tauri::command]
pub fn open_config_dir(app: tauri::AppHandle) -> Result<String> {
    let path = crate::config::config_path()?;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| std::path::Path::new(".").to_path_buf());
    let text = dir.display().to_string();

    // 用 opener 插件在系统文件管理器里打开。失败不致命：
    // 路径已经返回给前端，用户可以自己粘
    let _ = tauri_plugin_opener::OpenerExt::opener(&app)
        .open_path(dir.to_string_lossy().to_string(), None::<&str>);
    Ok(text)
}
