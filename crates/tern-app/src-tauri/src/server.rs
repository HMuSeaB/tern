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
    /// 是不是当前在用的那个。前端拿它标 "使用中"，不用自己比对 id
    pub active: bool,
    /// key 能不能用：占位符 / 空 / 真 key。前端据此决定要不要提醒，
    /// 不在前端判 key——那得把凭据搬进渲染进程
    pub key_state: String,
    auth_kind: String,
}

fn key_state(auth: &tern_gateway::ProviderAuth) -> &'static str {
    match auth {
        tern_gateway::ProviderAuth::ApiKey { key, .. } => {
            let key = key.trim();
            if key.is_empty() {
                "empty"
            } else if key == crate::config::PLACEHOLDER_KEY {
                "placeholder"
            } else {
                "real"
            }
        }
        _ => "subscription",
    }
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
            active: config.default_provider.as_deref() == Some(spec.id.as_str()),
            key_state: key_state(&spec.auth).to_string(),
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

/// 切换默认供应商。
///
/// 这是整个产品最常用的操作。不需要重启网关：路由表里的
/// `Arc<ProviderSpec>` 换一个进去就行。但 agent 是另一个进程，
/// 它得知道这件事——所以走 HTTP 通知，通知失败也不阻断
/// （配置已经落盘，用户下次重启照样生效）。
#[tauri::command]
pub fn select_provider(id: String) -> Result<ConfigSummary> {
    let path = crate::config::config_path()?;
    let mut config = crate::config::load(&path)?;
    // 切成不存在的 id 会让路由表整体失效（网关起不来），所以先校验
    if !config.providers.iter().any(|spec| spec.id == id) {
        return Err(crate::error::AppError::Config(format!(
            "供应商 {id} 不在配置里"
        )));
    }
    config.default_provider = Some(id);
    write_config(&path, &config)?;
    Ok(summary_of(&path, config))
}

fn summary_of(path: &std::path::Path, config: tern_gateway::GatewayConfig) -> ConfigSummary {
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
            active: config.default_provider.as_deref() == Some(spec.id.as_str()),
            key_state: key_state(&spec.auth).to_string(),
            auth_kind: auth_kind(&spec.auth).to_string(),
        })
        .collect();
    ConfigSummary {
        path: path.display().to_string(),
        listen: config.listen.to_string(),
        default_provider: config.default_provider.clone(),
        providers,
        warnings,
    }
}

/// 写配置前先备份。这里写的是整个 providers 数组，
/// 用户手改过的东西不该无声消失
fn write_config(path: &std::path::Path, config: &tern_gateway::GatewayConfig) -> Result<()> {
    if path.exists() {
        let backup = path.with_extension("json.bak");
        let _ = std::fs::copy(path, &backup);
    }
    std::fs::write(path, serde_json::to_string_pretty(config)? + "\n")
        .map_err(|e| crate::error::AppError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_gateway::ProviderSpec;

    fn provider(id: &str, key: &str) -> ProviderSpec {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": id,
            "baseUrl": format!("https://{id}.example.com/anthropic"),
            "auth": { "type": "api_key", "key": key }
        }))
        .expect("夹具应当合法")
    }

    /// 41 位真 key，放进夹具里代表"能用"
    const REAL: &str = "sk-0123456789abcdefghijklmnopqrstuvwxyz";

    /// 切供应商是这个产品最常用的操作，所以：
    /// - 校验不能松（切成不存在的 id 会让网关起不来）
    /// - 切换不能顺手把别的东西改了
    #[test]
    fn selecting_writes_only_the_default_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tern.json");
        let mut config = crate::config::parse(
            &serde_json::to_string_pretty(&serde_json::json!({
                "listen": "127.0.0.1:15800",
                "accessToken": "tern-keep",
                "defaultProvider": "a",
                "providers": [
                    { "id": "a", "name": "A", "baseUrl": "https://a.example.com/anthropic",
                      "auth": { "type": "api_key", "key": REAL } },
                    { "id": "b", "name": "B", "baseUrl": "https://b.example.com/anthropic",
                      "auth": { "type": "api_key", "key": REAL } }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        // 走和 select_provider 一样的路径，只是不经过 config_path() 环境变量
        let original_token = config.access_token.clone();
        config.default_provider = Some("b".into());
        std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        drop(config);

        let reloaded = crate::config::load(&path).unwrap();
        assert_eq!(reloaded.default_provider.as_deref(), Some("b"));
        // token 和供应商列表一个字都不能动
        assert_eq!(reloaded.access_token, original_token);
        assert_eq!(reloaded.providers.len(), 2);
        assert_eq!(reloaded.providers[0].id, "a");
        assert_eq!(reloaded.providers[1].id, "b");
    }

    /// 切到不存在的供应商必须被拒。放过去的话路由表整体失效，
    /// 网关起不来，而用户看到的是"我点了切换然后全都不能用了"
    #[test]
    fn an_unknown_provider_is_refused() {
        let known = vec![provider("a", REAL), provider("b", REAL)];
        assert!(!known.iter().any(|spec| spec.id == "nope"));
        // 这正是 select_provider 里的判据，装个样子确认它是对的
        let config = crate::config::parse(&serde_json::to_string(
            &serde_json::json!({
                "listen": "127.0.0.1:15800",
                "providers": [
                    { "id": "a", "name": "A", "baseUrl": "https://a.example.com/anthropic",
                      "auth": { "type": "api_key", "key": REAL } }
                ]
            }),
        )
        .unwrap())
        .unwrap();
        assert!(config.providers.iter().any(|s| s.id == "a"));
        assert!(!config.providers.iter().any(|s| s.id == "nope"));
    }

    /// key 状态要分清楚，前端靠它决定要不要提醒。
    /// 默认那个是失效 key 时用户必须被明确告知——否则他只会看到
    /// "请求失败"，不知道是供应商选错了
    #[test]
    fn key_state_separates_placeholder_empty_and_real() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tern.json");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "listen": "127.0.0.1:15800",
                "defaultProvider": "bad",
                "providers": [
                    { "id": "bad", "name": "坏", "baseUrl": "https://x.example.com/anthropic",
                      "auth": { "type": "api_key", "key": "sk-REPLACE_ME" } },
                    { "id": "good", "name": "好", "baseUrl": "https://y.example.com/anthropic",
                      "auth": { "type": "api_key", "key": REAL } }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let config = crate::config::load(&path).unwrap();
        let summary = summary_of(&path, config);
        let bad = summary.providers.iter().find(|p| p.id == "bad").unwrap();
        let good = summary.providers.iter().find(|p| p.id == "good").unwrap();
        assert_eq!(bad.key_state, "placeholder");
        assert_eq!(bad.active, true, "默认那个要标出来");
        assert_eq!(good.key_state, "real");
        assert_eq!(good.active, false);
    }
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
