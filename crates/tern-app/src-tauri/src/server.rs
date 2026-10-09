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

/// 面板自己点的启停。带 `AppHandle` 是为了顺手把托盘菜单刷了——
/// 不刷的话用户刚在面板里停了网关，托盘上还写着"停止网关"。
#[tauri::command]
pub fn server_start(app: tauri::AppHandle) -> Result<ServerStatus> {
    let status = start_gateway_now()?;
    crate::tray::refresh(&app);
    Ok(status)
}

#[tauri::command]
pub fn server_stop(app: tauri::AppHandle) -> Result<ServerStatus> {
    let status = stop_gateway_now()?;
    crate::tray::refresh(&app);
    Ok(status)
}

#[tauri::command]
pub fn server_status() -> Result<ServerStatus> {
    Ok(server_status_now())
}

// ---- 不带 tauri 命令层的版本：托盘菜单和面板共用 ----

/// 问一次状态。agent 不在时给"停了"而不是报错——那是常态（用户还没启动过），
/// 不是异常，抛错会让托盘刷新变成一片红色日志。
pub fn server_status_now() -> ServerStatus {
    match agent::status() {
        Ok(status) => to_server_status(status),
        Err(_) => agent_absent(),
    }
}

pub fn start_gateway_now() -> Result<ServerStatus> {
    Ok(to_server_status(agent::start_gateway()?))
}

pub fn stop_gateway_now() -> Result<ServerStatus> {
    Ok(to_server_status(agent::stop_gateway()?))
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
    /// 归在哪个自定义文件夹；None = 未分组
    pub folder: Option<String>,
    /// 规范化后的请求地址，"按地址归类"的分组键。
    /// Rust 侧算而不是前端算：两边口径必须一致，而前端算的话就得再抄一份
    /// `folders::normalize_url`，抄歪了分组就对不上
    pub group_key: String,
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
    Ok(summary_of(&path, config))
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

    // 切到哪家,就把那家的模型映射写给 Claude Code。
    // 只在**已接线**时做:没接线说明用户没让 tern 接管 settings.json,
    // 往那个文件里写模型是越权。失败不阻断切换——配置已经落盘了,
    // 那才是要紧的;模型下次接线时会补上
    if let Err(error) = apply_provider_model_env(&config) {
        log::warn!("[server] 切换后应用模型映射失败: {error}");
    }

    // 网关在跑时重新加载配置，确保路由表与当前选择一致。
    // 失败不阻断切换本身：配置已经落盘
    if let Err(error) = restart_after_config_change() {
        log::warn!("[server] 切换默认供应商后重起网关失败: {error}");
    }
    Ok(summary_of(&path, config))
}

/// 把当前默认供应商的 `client_env` 写进 `~/.claude/settings.json`。
///
/// # 为什么单独一个函数
///
/// `select_provider` 已经够长了,而这件事有自己的失败模式(文件不在、被别的
/// 进程占用、JSON 形状不对),单独拆出来日志能说清是哪一步出的问题。
///
/// # 覆盖规则
///
/// **该供应商配了的档位一律以它为准,没配的保留现值。**
/// 理由是这些映射本来就是用户当初为这家逐个挑的(cc-switch 里 39 家,
/// 每家的 Opus/Sonnet 档都不一样),切到 StepFun 却还用着上一家的 Opus 档,
/// 等于"切了但没完全切"。而它没配的档位(多数第三方站只配 Opus/Sonnet,
/// 不配 Haiku)保留现值,免得把用户手工指定的那个抹成空。
///
/// 这与 `model.rs::set_model` 的"已有值不覆盖"刻意不同:那边是用户主动选一个
/// 模型,这边是"这家配套的档位组合",语义不是一回事。
fn apply_provider_model_env(config: &tern_gateway::GatewayConfig) -> crate::error::Result<()> {
    // 没接线就不碰用户文件
    if !crate::wire::wire_status()?.wired {
        return Ok(());
    }
    let Some(active) = config
        .providers
        .iter()
        .find(|spec| Some(&spec.id) == config.default_provider.as_ref())
    else {
        return Ok(());
    };
    if active.client_env.is_empty() {
        return Ok(());
    }
    let claude_dir = crate::model::claude_dir()?;
    crate::model::apply_env(&claude_dir, &active.client_env)?;
    log::info!(
        "[server] 已把「{}」的 {} 个模型键写给 Claude Code",
        active.name,
        active.client_env.len()
    );
    Ok(())
}

/// 拉一个供应商的模型列表（「获取模型列表」按钮）。
///
/// key 从配置里现读，不进返回值也不进日志。用阻塞客户端，
/// 所以由 tauri 丢到 blocking 线程池——否则一个 15 秒的上游超时
/// 会把窗口操作一起拖住。
#[tauri::command]
pub async fn fetch_provider_models(id: String) -> Result<Vec<String>> {
    let config = crate::config::load(&crate::config::config_path()?)?;
    let spec = config
        .providers
        .iter()
        .find(|spec| spec.id == id)
        .ok_or_else(|| crate::error::AppError::Config(format!("供应商 {id} 不在配置里")))?
        .clone();

    let (base_url, api_key, full_url) = match &spec.auth {
        tern_gateway::ProviderAuth::ApiKey { key, .. } => (
            spec.effective_base_url(),
            key.trim().to_string(),
            spec.full_url,
        ),
        // 订阅登录（OAuth）的 key 在宿主手里，面板拿不到，
        // 硬要问只会得到 401。说清楚比给个假列表好
        _ => {
            return Err(crate::error::AppError::Config(
                "这个供应商用订阅登录，模型列表得由宿主提供，面板拉不到".into(),
            ))
        }
    };

    tauri::async_runtime::spawn_blocking(move || {
        tern_gateway::models::fetch_models(&base_url, &api_key, full_url, None)
            .map(|models| models.into_iter().map(|m| m.id).collect())
            .map_err(crate::error::AppError::Config)
    })
    .await
    .map_err(|e| crate::error::AppError::Config(format!("任务失败: {e}")))?
}

pub(crate) fn summary_of(
    path: &std::path::Path,
    config: tern_gateway::GatewayConfig,
) -> ConfigSummary {
    let warnings = crate::config::warnings(&config);
    // 分组数据是另一个文件。读不到就是空表，供应商照样全列——分组不该挡住列表
    let folders = crate::folders::read();
    let folder_of = |id: &str| -> Option<String> {
        // 归属值里的空串按未分组算：文件被手改过是常态
        folders
            .assignments
            .get(id)
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
    };

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
            folder: folder_of(&spec.id),
            group_key: crate::folders::group_key(&spec.effective_base_url()),
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
/// 用户手改过的东西不该无声消失。
///
/// `pub(crate)`：`providers` 模块写的是同一个文件，备份规则分叉会让
/// `.bak` 时新时旧
pub(crate) fn write_config(
    path: &std::path::Path,
    config: &tern_gateway::GatewayConfig,
) -> Result<()> {
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

    /// 切到不存在的供应商必须被拒。
    ///
    /// 放过去的话路由表里留下一个指向空气的 default_provider，
    /// 网关起不来（`ModelRouter::new` 会校验默认供应商存在），
    /// 而用户看到的是"我点了切换然后全都不能用了"——比切换前更糟。
    #[test]
    fn an_unknown_provider_is_refused() {
        let providers = [provider("a", REAL), provider("b", REAL)];
        // 这正是 select_provider 的第一道判据
        assert!(providers.iter().any(|spec| spec.id == "b"));
        assert!(!providers.iter().any(|spec| spec.id == "nope"));

        // 而一个真实的配置里，网关侧的要求也是同一个：默认供应商必须在列表里
        let config = crate::config::parse(
            &serde_json::to_string(&serde_json::json!({
                "listen": "127.0.0.1:15800",
                "defaultProvider": "ghost",
                "providers": [
                    { "id": "a", "name": "A", "baseUrl": "https://a.example.com/anthropic",
                      "auth": { "type": "api_key", "key": REAL } }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        // 配置能解析，但拿着它建网关必须失败——这就是"放过去会怎样"的证据
        assert!(tern_gateway::Gateway::new(config).is_err());
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
        assert!(bad.active, "默认那个要标出来");
        assert_eq!(good.key_state, "real");
        assert!(!good.active);
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
