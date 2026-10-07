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
    } else if !has_usable_provider(config) {
        // 和上面的"一个都没有"分开说：这里的数量不为零，
        // 用户会以为配好了，实际一个都用不了
        warnings.push("配了供应商，但 key 全是占位符或空——请求都会失败，建议从 cc-switch 导入".to_string());
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

/// 首屏判断：有没有可用的供应商。
///
/// **只看数量是不够的**：样例配置自带两个 `sk-REPLAC_ME` 的占位供应商，
/// 那时候 providers 非空、`first_run` 为 false，导入向导就不出现了——
/// 而那两个供应商一个请求都发不出去。用户点遍界面也找不到导入入口，
/// 只能对着两个假供应商干瞪眼。
///
/// 所以判据改成"有没有一个真能用的"：key 不是占位符、不是空。
/// 一个能用的都没有，就当首次运行处理，把导入向导给他。
pub fn is_first_run() -> bool {
    let Ok(path) = config_path() else {
        return true;
    };
    match load(&path) {
        Ok(config) => !has_usable_provider(&config),
        Err(_) => true,
    }
}

/// 至少有一个供应商拿着真 key。
pub fn has_usable_provider(config: &GatewayConfig) -> bool {
    config.providers.iter().any(|spec| match &spec.auth {
        ProviderAuth::ApiKey { key, .. } => {
            let key = key.trim();
            !key.is_empty() && key != PLACEHOLDER_KEY
        }
        // 订阅登录（OAuth）在宿主提供 TokenProvider 之前也用不了，
        // 但那种情况用户得先跑宿主，不该在这儿把他拦成"首次运行"
        _ => true,
    })
}

pub fn sample_value() -> Value {
    serde_json::json!({ "listen": "127.0.0.1:15800", "accessToken": "", "providers": [] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_gateway::ProviderSpec;

    fn spec_with_key(key: &str) -> ProviderSpec {
        let json = serde_json::json!({
            "id": "p1",
            "name": "P1",
            "baseUrl": "https://api.example.com/anthropic",
            "auth": { "type": "api_key", "key": key }
        });
        serde_json::from_value(json).expect("夹具应当合法")
    }

    fn config_of(providers: Vec<ProviderSpec>) -> GatewayConfig {
        let mut config = GatewayConfig::new(providers);
        config.access_token = Some("tern-test".into());
        config
    }

    /// 样例配置那两个占位供应商必须让 is_first_run 报 true。
    /// 报成 false 的后果是导入向导不出现，用户没有任何入口去导入真供应商
    #[test]
    fn placeholder_providers_still_count_as_first_run() {
        let config = config_of(vec![
            spec_with_key(PLACEHOLDER_KEY),
            spec_with_key(PLACEHOLDER_KEY),
        ]);
        assert!(!has_usable_provider(&config));
    }

    #[test]
    fn an_empty_key_is_not_usable() {
        assert!(!has_usable_provider(&config_of(vec![spec_with_key("")])));
        // 空白字符也不行：用户在编辑器里手改出来的常见残留
        assert!(!has_usable_provider(&config_of(vec![spec_with_key("   ")])));
    }

    #[test]
    fn a_real_key_makes_it_not_first_run() {
        let config = config_of(vec![
            spec_with_key(PLACEHOLDER_KEY),
            spec_with_key("sk-real-key"),
        ]);
        assert!(has_usable_provider(&config));
    }

    /// 订阅登录不拦：那种情况得先跑宿主，不该把用户打成"首次运行"
    #[test]
    fn subscription_auth_counts_as_usable() {
        let json = serde_json::json!({
            "id": "cp", "name": "Copilot", "baseUrl": "https://api.githubcopilot.com",
            "auth": { "type": "github_copilot" }
        });
        let spec: ProviderSpec = serde_json::from_value(json).unwrap();
        assert!(has_usable_provider(&config_of(vec![spec])));
    }

    #[test]
    fn no_providers_at_all_is_not_usable() {
        assert!(!has_usable_provider(&config_of(Vec::new())));
    }

    /// 提醒要分情况说："一个都没有"和"有但都是占位符"是两回事，
    /// 后者用户会以为自己配好了
    #[test]
    fn warnings_distinguish_placeholder_only_from_empty() {
        let all_placeholder = warnings(&config_of(vec![spec_with_key(PLACEHOLDER_KEY)]));
        assert!(
            all_placeholder.iter().any(|w| w.contains("占位符")),
            "{all_placeholder:?}"
        );

        let none = warnings(&config_of(Vec::new()));
        assert!(none.iter().any(|w| w.contains("没有配置任何供应商")), "{none:?}");
    }
}
