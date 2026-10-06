//! 面板 / 应用侧的配置定位。与 `tern-cli` 的 config 口径一致：
//! 同一份 `tern.json`、同一个 `usage.db`，两个入口不打架。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use tern_gateway::{GatewayConfig, ProviderAuth};

const DIR_NAME: &str = "tern";
const FILE_NAME: &str = "tern.json";
pub const DB_FILE_NAME: &str = "usage.db";
/// 与 tern-cli 一致的占位符，导入时要能识别出来
pub const PLACEHOLDER_KEY: &str = "sk-REPLACE_ME";

/// `%APPDATA%\tern\tern.json`（Windows）等
pub fn config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("TERN_CONFIG").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let dir = dirs::config_dir().context("无法确定系统配置目录")?;
    Ok(dir.join(DIR_NAME).join(FILE_NAME))
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
        .join(DB_FILE_NAME)
}

pub fn load(path: &Path) -> Result<GatewayConfig> {
    if !path.exists() {
        anyhow::bail!("配置文件 {} 不存在", path.display());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取配置文件 {} 失败", path.display()))?;
    parse(&text).with_context(|| format!("解析配置文件 {} 失败", path.display()))
}

/// 容忍 BOM：PowerShell 5 的 `Set-Content -Encoding utf8` 会写 BOM，serde_json 不认
pub fn parse(text: &str) -> Result<GatewayConfig> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    Ok(serde_json::from_str(text)?)
}

/// 能启动但大概率用不了的配置，逐条提醒
pub fn warnings(config: &GatewayConfig) -> Vec<String> {
    let mut warnings = Vec::new();
    if config.providers.is_empty() {
        warnings.push("没有配置任何供应商，所有请求都会失败".to_string());
    }
    if config
        .access_token
        .as_deref()
        .is_none_or(|t| t.trim().is_empty())
    {
        warnings.push("未设置 accessToken：本机任何进程都能通过 tern 使用你的供应商 key".to_string());
    }
    for spec in &config.providers {
        match &spec.auth {
            ProviderAuth::ApiKey { key, .. } if key.trim() == PLACEHOLDER_KEY => {
                warnings.push(format!("供应商 {} 的 key 还是样例占位符", spec.id));
            }
            ProviderAuth::ApiKey { key, .. } if key.trim().is_empty() => {
                warnings.push(format!("供应商 {} 的 key 为空", spec.id));
            }
            ProviderAuth::GithubCopilot { .. }
            | ProviderAuth::CodexOauth { .. }
            | ProviderAuth::XaiOauth { .. } => {
                warnings.push(format!(
                    "供应商 {} 使用订阅登录，需要宿主提供 TokenProvider",
                    spec.id
                ));
            }
            _ => {}
        }
    }
    warnings
}

/// 应用第一次启动：生成一份带随机 accessToken 的样例
pub fn write_sample(path: &Path) -> Result<String> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("创建目录 {} 失败", dir.display()))?;
    }
    let token = format!("tern-{}", uuid::Uuid::new_v4().simple());
    let sample = serde_json::json!({
        "listen": "127.0.0.1:15800",
        "accessToken": token,
        "providers": []
    });
    std::fs::write(path, serde_json::to_string_pretty(&sample)? + "\n")
        .with_context(|| format!("写入 {} 失败", path.display()))?;
    Ok(token)
}

/// 首屏判断：有没有可用的配置。文件不存在或 providers 为空都算"还没就绪"。
pub fn is_first_run() -> bool {
    let Ok(path) = config_path() else {
        return true;
    };
    match load(&path) {
        Ok(config) => config.providers.is_empty(),
        Err(_) => true,
    }
}

pub fn sample_value() -> Value {
    serde_json::json!({ "listen": "127.0.0.1:15800", "accessToken": "", "providers": [] })
}
