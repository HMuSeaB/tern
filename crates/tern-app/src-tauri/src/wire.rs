//! 把 Claude Code 的流量接到 tern 上。
//!
//! # 不做这个，tern 就只是个没人访问的网关
//!
//! 用户在用的 `~/.claude/settings.json` 里，`env.ANTHROPIC_BASE_URL` 指向的是
//! cc-switch 的本地代理（`http://127.0.0.1:5000`，旁边的 `PROXY_MANAGED` 占位符
//! 就是它接管的证据）。tern 监听 15800，两个端口互不相干——网关起得再好，
//! 请求也全绕过去。这才是"装了不会用"的真正原因。
//!
//! 所以这里提供一个开关：点一下，把 `env.ANTHROPIC_BASE_URL` 改成 tern 的地址，
//! 并按需写入 `ANTHROPIC_AUTH_TOKEN`。再点一下恢复原样。
//!
//! # 为什么写 settings.json 而不是 settings.local.json
//!
//! 用户级只读 `settings.json` 这一个文件。`settings.local.json` 是**项目级**
//! `.claude/settings.local.json` 的迁移路径，放家目录下没人读。
//! 实测判据记录在 `permissions.rs` 的模块注释里，同一套结论。
//!
//! # cc-switch 会把接线擦掉
//!
//! 它每次切供应商都用供应商的 `settings_config` 整份重写 settings.json
//! （`sanitize_claude_settings_for_live` 只剥 `api_format` 这类内部字段）。
//! 所以"已接入"随时可能变成"未接入"，且不是用户点的。
//!
//! 对策分两层：
//! 1. **如实显示**：`wire_status` 每次重新读文件，被擦了就是未接入，
//!    用户看见的是一个可以一键恢复的开关，不是一个说谎的绿色指示灯
//! 2. **原值存在 tern 自己那儿**：`%APPDATA%\tern\claude-wire-backup.json`，
//!    不放进 settings.json——放那儿同样会被 cc-switch 擦掉，按住原值就没意义了
//!
//! # 只碰自己写的那两个键
//!
//! 读-改-写，只动 `env.ANTHROPIC_BASE_URL` 和 `env.ANTHROPIC_AUTH_TOKEN`。
//! settings.json 里 cc-switch 的模型别名、用户自己的 `includeCoAuthoredBy`
//! 之类一律原样带回。测试守住这一条。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{AppError, Result};

const BASE_URL_KEY: &str = "ANTHROPIC_BASE_URL";
const AUTH_TOKEN_KEY: &str = "ANTHROPIC_AUTH_TOKEN";
const BACKUP_FILE: &str = "claude-wire-backup.json";

/// 给前端的接线状态
#[derive(Debug, Serialize, Clone)]
pub struct WireStatus {
    /// settings.json 的 `env.ANTHROPIC_BASE_URL` 是否正指向 tern
    pub wired: bool,
    /// 文件里现在的值。没有就是没配（理论上 cc-switch 会配上）
    pub current_base_url: Option<String>,
    /// 如果接入，它该变成的值
    pub tern_base_url: String,
    /// 是否已写入 `ANTHROPIC_AUTH_TOKEN`
    pub token_written: bool,
    /// tern 自己有没有配 accessToken。没配的话接入前必须先提醒用户
    pub token_configured: bool,
    /// 接入前的原值。断开时要还原
    pub replaced: Vec<(String, String)>,
    /// 网关在不在跑。没跑的时候接线成功也没流量，UI 要催启动
    pub running: bool,
    /// 配置文件路径，方便用户自己去看
    pub settings_path: String,
}

/// 接入前的原值快照。单独存一份而不塞进 settings.json：
/// cc-switch 重写那个文件时不会带上它，塞进去等于没存。
#[derive(Debug, Default, Serialize, Deserialize)]
struct Backup {
    #[serde(default)]
    replaced_env: Vec<(String, String)>,
}

/// 探一下接线是否真的通。
#[derive(Debug, Serialize, Clone)]
pub struct ProbeResult {
    /// TCP 连上了、HTTP 也回了
    pub reachable: bool,
    pub http_status: Option<u16>,
    /// 给人看的一句话。失败时把上游的原话带上，别自己编
    pub message: String,
    /// 实际发出的模型名
    pub model: String,
}

/// 网关在哪跑、在不在跑。接线写的就是这个地址，所以由 agent 现问现取——
/// 用户改过配置或临时换过端口时，接进去的必须是他实际会用的那个。
#[derive(Debug, Clone)]
pub struct GatewayEndpoint {
    pub listen: std::net::SocketAddr,
    pub running: bool,
}

impl GatewayEndpoint {
    /// 问 agent。agent 不在时退回配置里的 listen，
    /// 让"网关还没起就接入"仍然可行——那时只能用配置值。
    pub fn from_agent() -> Result<Self> {
        let status = crate::agent::status();
        match status {
            Ok(status) => {
                let listen = status
                    .listen
                    .as_deref()
                    .and_then(|text| text.parse().ok())
                    .or_else(|| configured_listen().ok());
                Ok(Self {
                    listen: listen.ok_or_else(|| {
                        AppError::Config("拿不到网关监听地址：agent 没报，配置里也没有".into())
                    })?,
                    running: status.running,
                })
            }
            Err(_) => Ok(Self {
                listen: configured_listen()?,
                running: false,
            }),
        }
    }
}

fn configured_listen() -> Result<std::net::SocketAddr> {
    Ok(crate::config::load(&crate::config::config_path()?)?.listen)
}

#[tauri::command]
pub fn wire_status() -> Result<WireStatus> {
    let dir = crate::permissions::claude_dir()?;
    status_at(&dir, &GatewayEndpoint::from_agent()?)
}

fn status_at(claude_dir: &Path, gateway: &GatewayEndpoint) -> Result<WireStatus> {
    let tern_base_url = format!("http://{}", gateway.listen);
    let settings = read_settings(&claude_dir.join("settings.json"))?;

    let env = settings.pointer("/env").and_then(Value::as_object);
    let current_base_url = env
        .and_then(|e| e.get(BASE_URL_KEY))
        .and_then(Value::as_str)
        .map(str::to_string);
    let token_written = env
        .and_then(|e| e.get(AUTH_TOKEN_KEY))
        .and_then(Value::as_str)
        .is_some();

    let backup = read_backup(claude_dir)?;
    let token_configured = crate::config::load(&crate::config::config_path()?)
        .map(|c| {
            c.access_token
                .as_deref()
                .is_some_and(|t| !t.trim().is_empty())
        })
        .unwrap_or(false);

    Ok(WireStatus {
        wired: current_base_url.as_deref() == Some(tern_base_url.as_str()),
        current_base_url,
        tern_base_url,
        token_written,
        token_configured,
        replaced: backup.replaced_env,
        running: gateway.running,
        settings_path: claude_dir.join("settings.json").display().to_string(),
    })
}

/// 接入：把 base_url 指到 tern，并按需写入 accessToken。
#[tauri::command]
pub fn wire_enable() -> Result<WireStatus> {
    let dir = crate::permissions::claude_dir()?;
    let gateway = GatewayEndpoint::from_agent()?;
    let tern_base_url = format!("http://{}", gateway.listen);

    let config = crate::config::load(&crate::config::config_path()?)?;
    let token = config
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);

    if token.is_none() {
        // 不阻断：用户可能就是想图省事。但这句话要说清楚——
        // 没 token 的网关等于本机任何人都能借他的供应商 key 用
        log::warn!("[tern-app] 未设置 accessToken 就接入，本机任何进程都能使用供应商 key");
    }

    let mut replaced = Vec::new();
    {
        let path = dir.join("settings.json");
        let mut root = read_settings(&path)?;
        let env = ensure_env(root.as_object_mut().ok_or_else(|| {
            AppError::Config("settings.json 顶层不是对象".into())
        })?)?;

        for key in [BASE_URL_KEY, AUTH_TOKEN_KEY] {
            if let Some(old) = env.get(key).and_then(Value::as_str) {
                replaced.push((key.to_string(), old.to_string()));
            }
        }
        env.insert(BASE_URL_KEY.to_string(), Value::String(tern_base_url));
        if let Some(token) = &token {
            env.insert(
                AUTH_TOKEN_KEY.to_string(),
                Value::String(token.clone()),
            );
        }

        write_settings(&path, &root)?;
    }

    write_backup(&dir, &Backup { replaced_env: replaced })?;
    status_at(&dir, &GatewayEndpoint::from_agent()?)
}

/// 断开：删掉自己写的键，有备份就还原。
#[tauri::command]
pub fn wire_disable() -> Result<WireStatus> {
    let dir = crate::permissions::claude_dir()?;
    let backup = read_backup(&dir)?;

    {
        let path = dir.join("settings.json");
        let mut root = read_settings(&path)?;
        let env = ensure_env(root.as_object_mut().ok_or_else(|| {
            AppError::Config("settings.json 顶层不是对象".into())
        })?)?;

        for key in [BASE_URL_KEY, AUTH_TOKEN_KEY] {
            match backup.replaced_env.iter().find(|(k, _)| k == key) {
                Some((_, old)) => {
                    env.insert(key.to_string(), Value::String(old.clone()));
                }
                // 原本就没有这个键：删掉而不是塞个空串，空串会让 Claude Code
                // 以为配了 base_url 然后连一个空地址
                None => {
                    env.remove(key);
                }
            }
        }
        write_settings(&path, &root)?;
    }

    // 还原过了，备份就没用了；留着只会让下次接入时误以为要还原旧值
    let _ = std::fs::remove_file(backup_path(&dir));
    status_at(&dir, &GatewayEndpoint::from_agent()?)
}

/// 发一条真实请求验整条链路。
///
/// 为什么值得花用户的钱：这个产品唯一的价值主张就是"记下你花了多少"，
/// 而"记下"的前提是请求真的流经过它。一条真实请求同时证明了
/// 接线、鉴权、路由、上游连通、记账五件事——比任何健康检查都实在。
///
/// 所以它是个**显式按钮**，不进自动流程，文案里写清会花一次调用的钱。
#[tauri::command]
pub async fn wire_probe() -> Result<ProbeResult> {
    let dir = crate::permissions::claude_dir()?;
    let listen = GatewayEndpoint::from_agent()?.listen;
    let token = crate::config::load(&crate::config::config_path()?)
        .ok()
        .and_then(|c| c.access_token)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    // 用用户自己的模型名去问：和 Claude Code 实际发出的请求同一条路。
    // 自己挑一个模型名的话，第三方网关不认就 404，那种失败说明不了任何事
    let model = client_model(&dir)?;

    let claude_dir = dir.clone();
    // reqwest 的同步 API 会阻塞，tauri 命令跑在 async runtime 上，
    // 阻塞它会把窗口操作一起拖住
    tauri::async_runtime::spawn_blocking(move || {
        send_probe(&listen, token.as_deref(), &model, &claude_dir)
    })
    .await
    .map_err(|e| AppError::Config(format!("探测任务失败: {e}")))?
}

fn send_probe(
    listen: &std::net::SocketAddr,
    token: Option<&str>,
    model: &str,
    _claude_dir: &Path,
) -> Result<ProbeResult> {
    let url = format!("http://{listen}/v1/messages");
    let mut request = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| AppError::Config(e.to_string()))?
        .post(&url)
        .json(&serde_json::json!({
            "model": model,
            "max_tokens": 16,
            "messages": [{ "role": "user", "content": "ping" }]
        }));
    if let Some(token) = token {
        request = request.header("x-api-key", token);
    }

    let response = request
        .send()
        .map_err(|e| AppError::Config(format!("连不上 {url}: {e}")))?;
    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();

    if (200..300).contains(&status) {
        return Ok(ProbeResult {
            reachable: true,
            http_status: Some(status),
            message: "通了。这条请求已记入用量，刷新面板就能看到。".to_string(),
            model: model.to_string(),
        });
    }

    // 上游的原话比我们自己总结的有用：模型名不对、key 失效、超额，
    // 各自的修法完全不同
    let detail = first_error_message(&body).unwrap_or_else(|| truncate(&body, 300));
    Ok(ProbeResult {
        reachable: false,
        http_status: Some(status),
        message: format!("HTTP {status}：{detail}"),
        model: model.to_string(),
    })
}

/// 用户实际会发出去的模型名：优先 `ANTHROPIC_MODEL`，其次 SONNET，
/// 都没有就给个常见值。读的是 settings.json 的 env，
/// 也就是 Claude Code 自己会用的那个。
fn client_model(claude_dir: &Path) -> Result<String> {
    let settings = read_settings(&claude_dir.join("settings.json"))?;
    let env = settings.pointer("/env").and_then(Value::as_object);
    for key in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    ] {
        if let Some(model) = env
            .and_then(|e| e.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            return Ok(model.to_string());
        }
    }
    Ok("claude-sonnet-4-6".to_string())
}

/// 从 Anthropic 风格的错误体里挖 message。挖不到就退回原文截断。
fn first_error_message(body: &str) -> Option<String> {
    let json: Value = serde_json::from_str(body).ok()?;
    let message = json
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| json.pointer("/message").and_then(Value::as_str))?;
    Some(truncate(message, 300))
}

fn truncate(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept}…")
}

fn backup_path(claude_dir: &Path) -> PathBuf {
    claude_dir.join(BACKUP_FILE)
}

fn read_backup(claude_dir: &Path) -> Result<Backup> {
    let path = backup_path(claude_dir);
    if !path.exists() {
        return Ok(Backup::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| AppError::Config(e.to_string()))?;
    // 备份读不出来不能当初次接入处理——那会丢掉用户原来的 base_url，
    // 断开时再也回不去
    serde_json::from_str(&text).map_err(|e| {
        AppError::Config(format!(
            "{} 解析失败，为保住原值暂不改动接线: {e}",
            path.display()
        ))
    })
}

fn write_backup(claude_dir: &Path, backup: &Backup) -> Result<()> {
    std::fs::create_dir_all(claude_dir).map_err(|e| AppError::Config(e.to_string()))?;
    let text = serde_json::to_string_pretty(backup).map_err(|e| AppError::Config(e.to_string()))?;
    std::fs::write(backup_path(claude_dir), text + "\n")
        .map_err(|e| AppError::Config(e.to_string()))
}

/// 读 settings.json。容忍 BOM 与空文件，不存在就是空对象。
fn read_settings(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let text = std::fs::read_to_string(path).map_err(|e| AppError::Config(e.to_string()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(text)
        .map_err(|e| AppError::Config(format!("{} 解析失败: {e}", path.display())))
}

/// 拿到底层 JSON 里 `env` 那个对象，没有就建一个。
///
/// `env` 已存在但不是对象时**报错**而不是覆盖：那说明有人（用户或别的工具）
/// 把它写成了别的形状，默默替换会丢东西。
fn ensure_env(root: &mut Map<String, Value>) -> Result<&mut Map<String, Value>> {
    let env = root
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    if !env.is_object() {
        return Err(AppError::Config("settings.json 的 env 已存在且不是对象".into()));
    }
    env.as_object_mut().ok_or_else(|| {
        AppError::Config("settings.json 的 env 已存在且不是对象".into())
    })
}

/// 落盘前留一份备份，只留第一次的。
fn backup_once(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".tern-bak");
    let backup = PathBuf::from(backup);
    if backup.exists() {
        return Ok(());
    }
    std::fs::copy(path, &backup).map_err(|e| AppError::Config(e.to_string()))?;
    Ok(())
}

fn write_settings(path: &Path, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::Config(e.to_string()))?;
    }
    backup_once(path)?;
    let text = serde_json::to_string_pretty(root).map_err(|e| AppError::Config(e.to_string()))?;
    std::fs::write(path, text + "\n").map_err(|e| AppError::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().to_path_buf();
        (d, p)
    }

    /// cc-switch 接管后的真实形态：env 里BASE_URL 指向它自己的端口，
    /// 外加一堆模型别名。接入时这些一个字都不能丢
    const CC_SWITCH_SHAPED: &str = r#"{
      "env": {
        "ANTHROPIC_AUTH_TOKEN": "PROXY_MANAGED",
        "ANTHROPIC_BASE_URL": "http://127.0.0.1:5000",
        "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-4-6[1M]",
        "CLAUDE_CODE_SUBAGENT_MODEL": "step-5-preview[1M]"
      },
      "includeCoAuthoredBy": false
    }"#;

    /// 不碰 ServerState，直接测文件那半边——纯函数，没有运行时依赖。
    /// 接入 = 改 settings.json + 写备份，正好是纯文件操作。
    fn enable_in(dir: &Path, tern_url: &str, token: Option<&str>) -> Result<()> {
        let path = dir.join("settings.json");
        let mut root = read_settings(&path)?;
        let mut replaced = Vec::new();
        {
            let env = ensure_env(
                root.as_object_mut()
                    .ok_or_else(|| AppError::Config("顶层不是对象".into()))?,
            )?;
            for key in [BASE_URL_KEY, AUTH_TOKEN_KEY] {
                if let Some(old) = env.get(key).and_then(Value::as_str) {
                    replaced.push((key.to_string(), old.to_string()));
                }
            }
            env.insert(BASE_URL_KEY.to_string(), Value::String(tern_url.to_string()));
            if let Some(token) = token {
                env.insert(AUTH_TOKEN_KEY.to_string(), Value::String(token.to_string()));
            }
        }
        write_settings(&path, &root)?;
        write_backup(dir, &Backup { replaced_env: replaced })
    }

    fn disable_in(dir: &Path) -> Result<()> {
        let backup = read_backup(dir)?;
        let path = dir.join("settings.json");
        let mut root = read_settings(&path)?;
        {
            let env = ensure_env(
                root.as_object_mut()
                    .ok_or_else(|| AppError::Config("顶层不是对象".into()))?,
            )?;
            for key in [BASE_URL_KEY, AUTH_TOKEN_KEY] {
                match backup.replaced_env.iter().find(|(k, _)| k == key) {
                    Some((_, old)) => {
                        env.insert(key.to_string(), Value::String(old.clone()));
                    }
                    None => {
                        env.remove(key);
                    }
                }
            }
        }
        write_settings(&path, &root)?;
        let _ = std::fs::remove_file(backup_path(dir));
        Ok(())
    }

    #[test]
    fn enabling_only_changes_the_two_owned_keys() {
        let (_g, dir) = dir();
        std::fs::write(dir.join("settings.json"), CC_SWITCH_SHAPED).unwrap();

        enable_in(&dir, "http://127.0.0.1:15800", Some("tern-secret")).unwrap();

        let json = read_settings(&dir.join("settings.json")).unwrap();
        assert_eq!(json["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:15800");
        assert_eq!(json["env"]["ANTHROPIC_AUTH_TOKEN"], "tern-secret");
        // cc-switch 的东西一个字都没动
        assert_eq!(
            json["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            "claude-sonnet-4-6[1M]"
        );
        assert_eq!(json["env"]["CLAUDE_CODE_SUBAGENT_MODEL"], "step-5-preview[1M]");
        assert_eq!(json["includeCoAuthoredBy"], false);
    }

    #[test]
    fn disabling_restores_the_original_values() {
        let (_g, dir) = dir();
        std::fs::write(dir.join("settings.json"), CC_SWITCH_SHAPED).unwrap();
        enable_in(&dir, "http://127.0.0.1:15800", Some("tern-secret")).unwrap();

        disable_in(&dir).unwrap();

        let json = read_settings(&dir.join("settings.json")).unwrap();
        assert_eq!(json["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:5000");
        assert_eq!(json["env"]["ANTHROPIC_AUTH_TOKEN"], "PROXY_MANAGED");
        // 模型别名和无关字段还在
        assert_eq!(
            json["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"],
            "claude-sonnet-4-6[1M]"
        );
        assert_eq!(json["includeCoAuthoredBy"], false);
        // 备份清掉了，否则下次接入会拿着它当"原值"还原
        assert!(!backup_path(&dir).exists());
    }

    /// 用户 settings.json 里本来没有 AUTH_TOKEN：接入后再断开，这个键应该消失，
    /// 而不是留个空串——空串会被 Claude Code 当成"配了"，然后连一个空地址
    #[test]
    fn disabling_removes_keys_that_did_not_exist_before() {
        let (_g, dir) = dir();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:5000"}}"#,
        )
        .unwrap();

        enable_in(&dir, "http://127.0.0.1:15800", Some("tern-secret")).unwrap();
        disable_in(&dir).unwrap();

        let json = read_settings(&dir.join("settings.json")).unwrap();
        let env = json["env"].as_object().unwrap();
        assert_eq!(env["ANTHROPIC_BASE_URL"], "http://127.0.0.1:5000");
        assert!(
            env.get("ANTHROPIC_AUTH_TOKEN").is_none(),
            "原本没有的键不该留空串"
        );
    }

    /// cc-switch 把接线擦了之后，备份还在 tern 自己这儿，
    /// 断开时仍能还原出 cc-switch 原来的值
    #[test]
    fn backup_survives_cc_switch_rewriting_settings() {
        let (_g, dir) = dir();
        std::fs::write(dir.join("settings.json"), CC_SWITCH_SHAPED).unwrap();
        enable_in(&dir, "http://127.0.0.1:15800", Some("tern-secret")).unwrap();

        // 模拟 cc-switch 切供应商：整份重写，tern 的痕迹全没了
        std::fs::write(
            dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:5000",
                       "ANTHROPIC_AUTH_TOKEN":"PROXY_MANAGED"},
                 "includeCoAuthoredBy":false}"#,
        )
        .unwrap();

        let backup = read_backup(&dir).unwrap();
        // 顺序无关：这只是个查找表
        assert!(backup
            .replaced_env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_AUTH_TOKEN" && v == "PROXY_MANAGED"));
        assert!(backup
            .replaced_env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_BASE_URL" && v == "http://127.0.0.1:5000"));
    }

    /// 备份坏了就拒绝改接线：宁可不动，也不能把用户原来的 base_url 弄丢
    #[test]
    fn a_corrupt_backup_blocks_changes() {
        let (_g, dir) = dir();
        std::fs::write(backup_path(&dir), "{ 不是 json").unwrap();

        let error = read_backup(&dir).unwrap_err();
        assert!(error.to_string().contains("暂不改动接线"), "{error}");
        assert!(disable_in(&dir).is_err());
    }

    #[test]
    fn env_that_is_not_an_object_is_refused() {
        let (_g, dir) = dir();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"env":"http://127.0.0.1:5000"}"#,
        )
        .unwrap();

        let mut root = read_settings(&dir.join("settings.json")).unwrap();
        let error = ensure_env(root.as_object_mut().unwrap()).unwrap_err();
        assert!(error.to_string().contains("不是对象"), "{error}");
        assert!(enable_in(&dir, "http://127.0.0.1:15800", None).is_err());
    }

    /// 没有 settings.json 也要能接入：目录建出来，从空对象开始
    #[test]
    fn enabling_from_nothing_creates_the_file() {
        let (_g, dir) = dir();
        enable_in(&dir, "http://127.0.0.1:15800", Some("tern-secret")).unwrap();

        let json = read_settings(&dir.join("settings.json")).unwrap();
        assert_eq!(json["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:15800");
    }

    /// 断开后又接入，不能拿着上次的备份去"还原"
    #[test]
    fn backup_is_cleared_so_a_second_cycle_starts_clean() {
        let (_g, dir) = dir();
        std::fs::write(dir.join("settings.json"), CC_SWITCH_SHAPED).unwrap();
        enable_in(&dir, "http://127.0.0.1:15800", Some("t1")).unwrap();
        disable_in(&dir).unwrap();
        enable_in(&dir, "http://127.0.0.1:15800", Some("t2")).unwrap();

        let backup = read_backup(&dir).unwrap();
        // 第二次的原值就是第一次还原回去的那个，不是更早的历史。顺序无关
        assert!(backup
            .replaced_env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_AUTH_TOKEN" && v == "PROXY_MANAGED"));
        assert!(backup
            .replaced_env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_BASE_URL" && v == "http://127.0.0.1:5000"));
    }

    #[test]
    fn client_model_prefers_what_claude_code_itself_would_send() {
        let (_g, dir) = dir();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_MODEL":"claude-opus-4-8[1M]",
                       "ANTHROPIC_DEFAULT_SONNET_MODEL":"claude-sonnet-4-6[1M]"}}"#,
        )
        .unwrap();

        assert_eq!(client_model(&dir).unwrap(), "claude-opus-4-8[1M]");
    }

    #[test]
    fn client_model_falls_back_when_no_env_is_configured() {
        let (_g, dir) = dir();
        std::fs::write(dir.join("settings.json"), r#"{"includeCoAuthoredBy":false}"#).unwrap();
        assert_eq!(client_model(&dir).unwrap(), "claude-sonnet-4-6");
    }

    #[test]
    fn error_message_is_dug_out_of_an_anthropic_shaped_body() {
        let body = r#"{"error":{"type":"invalid_request_error","message":"model not found"}}"#;
        assert_eq!(first_error_message(body).unwrap(), "model not found");
        // 非 JSON 或没有 message 字段时不给假信息
        assert!(first_error_message("upstream is down").is_none());
        assert!(first_error_message(r#"{"error":{}}"#).is_none());
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        // 中文一个 3 字节：按字节截会把汉字劈成乱码
        assert_eq!(truncate("中文测试", 2), "中文…");
        assert_eq!(truncate("ab", 5), "ab");
    }
}
