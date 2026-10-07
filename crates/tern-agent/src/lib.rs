//! tern-agent：常驻进程，持有网关。
//!
//! # 为什么要有它
//!
//! 原来网关长在 Tauri 面板里：双击 exe → 面板起来 → 网关起来 → 关窗口 →
//! 网关跟着停。三个后果：
//!
//! 1. **内存**。Tauri 带着 webview，实测约 408 MB。用户要的是"记用量"，
//!    不是永远开着一个浏览器内核
//! 2. **不能后台**。面板关了流量就断，"常驻"名不副实
//! 3. **生命周期绑死**。关窗口 = 断网，用户只是想看一眼花了多少
//!
//! 所以拆开：agent 是个没有窗口的小进程，只做"持有网关 + 回答状态"，
//! 内存约几 MB；面板想看的时候再拉起来，看完关掉，网关不受影响。
//!
//! # 它刻意不做什么
//!
//! **不碰 Claude Code 的配置文件。** 曾经想过让它在检测到 cc-switch 把
//! 接线擦掉时自动写回去，否掉了：用户可能正故意在用别的代理，这时候
//! "帮忙"就是跟他打架。他被擦掉的接线由面板如实显示成未接入，
//! 他自己点一下恢复——比一个猜错意图的后台进程靠谱。
//!
//! 同理没有轮询、没有文件监听。agent 只在被问到的时候说话。
//!
//! # 它有多大
//!
//! 实测（Windows、debug 构建、网关起着）：工作集 12.4 MB、私有内存 5.3 MB。
//! 对比 Tauri 面板实测的约 408 MB——差着一个数量级，这才是拆开的意义。
//! debug 未经优化，release 会更小；但差距不在这个量级上。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::Notify;

pub mod control;
pub mod single;

pub use control::ControlConfig;

/// 配置文件位置。与 tern-cli / tern-app 同一套规则：环境变量优先，
/// 否则系统配置目录。三个入口必须是同一个文件，否则状态会分叉。
pub fn config_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("TERN_CONFIG").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("无法确定系统配置目录"))?;
    Ok(dir.join("tern").join("tern.json"))
}

/// 与 CLI 的 resolve_db 同规则：环境变量优先，否则配置同目录
pub fn db_path_for(config: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("TERN_DB").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    config
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("usage.db")
}

struct Running {
    listen: SocketAddr,
    shutdown: Arc<Notify>,
}

/// agent 掌握的网关状态。`None` = 没在跑。
///
/// 用 `Mutex` 而不是 async 原语：这里的临界区极短（取出/放入一个 Option），
/// 不值得为它引入 await 点。 Poison 时照常用 `into_inner`——
/// 持锁线程 panic 了也不代表状态本身坏了。
#[derive(Default)]
pub struct GatewayState {
    inner: Mutex<Option<Running>>,
    /// 启动过一次但自己退了，把原因留在这里给面板显示
    last_error: Mutex<Option<String>>,
}

impl GatewayState {
    fn snapshot(&self) -> Option<SocketAddr> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|r| r.listen)
    }

    pub fn is_running(&self) -> bool {
        self.snapshot().is_some()
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// 记下一条启动失败的原因。给面板显示用——用户要看到
    /// "为什么没跑起来"，而不是一个不亮的灯
    pub fn note_error(&self, message: String) {
        *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(message);
    }

    /// 停掉网关（若在跑）。幂等。
    ///
    /// 只发信号不 join 线程：`serve` 返回前可能有进行中的流要补记 aborted，
    /// 等它自己收尾比在这里阻塞控制端好——控制端还要继续回答别人的请求。
    pub fn stop(&self) {
        if let Some(running) = self.inner.lock().unwrap_or_else(|p| p.into_inner()).take() {
            running.shutdown.notify_waiters();
            log::info!("[agent] 已通知网关停止");
        }
    }

    fn set_running(&self, running: Running) {
        *self.inner.lock().unwrap_or_else(|p| p.into_inner()) = Some(running);
        *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// 给外部的状态快照。字段名与面板 UI 直接对应，别随意改。
#[derive(Debug, Serialize, Clone)]
pub struct GatewayStatus {
    pub running: bool,
    pub listen: Option<String>,
    /// 没启动也能显示"配了几个"：用户常先看这个判断要不要启动
    pub provider_count: usize,
    pub last_error: Option<String>,
    /// agent 自己的版本，面板出问题时先看两边是不是一套
    pub agent_version: String,
}

pub struct Agent {
    pub state: Arc<GatewayState>,
    pub control: ControlConfig,
}

/// 起网关。端口被占用时给出人能懂的话，而不是把 axum 的 bind 错误原样抛出去。
pub fn start_gateway(state: &Arc<GatewayState>) -> anyhow::Result<SocketAddr> {
    if let Some(listen) = state.snapshot() {
        return Ok(listen);
    }

    let config_path = config_path()?;
    let config = load_config(&config_path)?;
    let db_path = db_path_for(&config_path);

    // 提前探一次端口：bind 失败时 axum 只给 "Address already in use"，
    // 用户不知道该怎么办
    ensure_port_free(config.listen)?;

    let store = Arc::new(tern_store::Store::open(&db_path)?);
    let multipliers = config.providers.iter().filter_map(|spec| {
        spec.cost_multiplier
            .as_deref()
            .map(|value| (spec.id.as_str(), value))
    });
    for error in store.set_multipliers(multipliers) {
        log::warn!("[agent] {error}");
    }

    let (recorder, recorder_handle) = tern_store::spawn_recorder(store, log_usage);

    // 网关的 config 要 move 进线程，listen 得先抄出来
    let listen = config.listen;
    let gateway = tern_gateway::Gateway::new(config)?.with_usage_sink(recorder);

    let shutdown = Arc::new(Notify::new());
    let serve_shutdown = shutdown.clone();
    let thread_name = format!("tern-gateway {listen}");

    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            // 独立线程 + 独立运行时，不复用控制端的：网关要长期跑，
            // 抢控制端的运行时会把 /api/status 一起拖慢
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::error!("[agent] 创建运行时失败: {error}");
                    return;
                }
            };
            let result = runtime.block_on(gateway.serve(async move {
                // Ctrl+C 也接上：从任务管理器结束进程时能正常补记 aborted
                tokio::select! {
                    _ = serve_shutdown.notified() => log::info!("[agent] 收到停止信号"),
                    _ = tokio::signal::ctrl_c() => log::info!("[agent] 收到 Ctrl+C"),
                }
            }));
            // 先停 runtime：进行中的流被丢弃时会补记 aborted，
            // 再让写入线程把队列里剩下的写完
            drop(runtime);
            drop(recorder_handle);
            if let Err(error) = result {
                log::error!("[agent] 网关退出: {error}");
            }
        })?;

    state.set_running(Running { listen, shutdown });
    log::info!("[agent] 网关已启动 {listen}");
    Ok(listen)
}

/// 读配置。provider 数量在启动失败时也要能报，所以单独包一层。
pub fn load_config(path: &Path) -> anyhow::Result<tern_gateway::GatewayConfig> {
    if !path.exists() {
        anyhow::bail!("配置文件 {} 不存在，先运行 tern init", path.display());
    }
    let text = std::fs::read_to_string(path)?;
    // 容忍 BOM：PowerShell 5 的 Set-Content -Encoding utf8 会写，serde_json 不认。
    // 用 as_str 而不是把 text move 进去——下面 parse 还要用它
    let text = text.strip_prefix('\u{feff}').unwrap_or(text.as_str());
    Ok(serde_json::from_str(text)?)
}

fn ensure_port_free(listen: SocketAddr) -> anyhow::Result<()> {
    match std::net::TcpListener::bind(listen) {
        Ok(_) => Ok(()),
        Err(_) => anyhow::bail!(
            "端口 {listen} 已被占用。另一个 tern 或 cc-switch 可能正在运行；\n\
             关掉它，或在配置里把 listen 改成别的端口"
        ),
    }
}

/// 每条请求一行摘要，与 `tern serve` 同口径
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
