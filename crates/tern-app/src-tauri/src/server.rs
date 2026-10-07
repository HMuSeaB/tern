//! 面板侧的服务状态：全部转问 `tern-agent`。
//!
//! # 这里曾经自己持有网关
//!
//! 早先这个模块 spawn 一个线程跑 `Gateway::serve`，好处是双击 exe 就能用。
//! 后来否掉了，代价有三个：
//!
//! 1. **内存**。Tauri 带着 webview，实测约 408 MB。为了"看一眼花了多少"
//!    常驻一个浏览器内核不值
//! 2. **关窗口 = 断网**。用户只是想看用量，不想把流量一起关了
//! 3. **状态和窗口绑死**。面板崩了网关跟着没，排查时两件事搅在一起
//!
//! 现在网关归 `tern-agent`（没有窗口的小进程），这里只负责把它的话
//! 翻译成前端要的形状。见 `agent` 模块的文档。
//!
//! # 关窗口不再停网关
//!
//! `main.rs` 里原来在窗口 Destroyed 时调 `shutdown`。那条逻辑随网关一起
//! 搬走了——现在的预期就是"面板关掉、流量继续"。

use serde::Serialize;

use crate::agent::{self, AgentStatus};
use crate::error::Result;

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
    /// agent 的版本。面板出问题时先确认两边是不是同一套
    pub agent_version: Option<String>,
    /// agent 起来了但没在跑网关，还是连 agent 都没起来。
    /// 前端要分开提示：前者催启动，后者多半是可执行文件没跟着一起装
    pub agent_up: bool,
}

fn to_server_status(status: AgentStatus) -> ServerStatus {
    ServerStatus {
        running: status.running,
        listen: status.listen,
        provider_count: status.provider_count,
        last_error: status.last_error,
        agent_version: Some(status.agent_version),
        agent_up: true,
    }
}

/// agent 不在时的占位状态。
///
/// 特意**不返回错误**：面板要能渲染。用户看到的是"网关已停止"加一句
/// "常驻进程不在"，而不是一个空白页加一句看不懂的错。
fn agent_absent() -> ServerStatus {
    ServerStatus {
        running: false,
        listen: None,
        provider_count: crate::config::load(&crate::config::config_path().unwrap_or_default())
            .map(|config| config.providers.len())
            .unwrap_or(0),
        last_error: None,
        agent_version: None,
        agent_up: false,
    }
}

#[tauri::command]
pub fn server_start() -> Result<ServerStatus> {
    Ok(to_server_status(agent::start_gateway()?))
}

#[tauri::command]
pub fn server_stop() -> Result<ServerStatus> {
    Ok(to_server_status(agent::stop_gateway()?))
}

#[tauri::command]
pub fn server_status() -> Result<ServerStatus> {
    match agent::status() {
        Ok(status) => Ok(to_server_status(status)),
        // agent 不在：给出"停了"的状态而不是把错误抛到前端。
        // 这是常态（用户还没启动过），不是异常
        Err(_) => Ok(agent_absent()),
    }
}

/// 导入过供应商后调它：跑着的网关还拿着旧配置。
pub fn restart_after_config_change() -> Result<ServerStatus> {
    Ok(to_server_status(agent::restart_gateway()?))
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
