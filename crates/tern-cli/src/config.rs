//! 配置文件：定位、读取、生成样例、启动前检查。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tern_gateway::{GatewayConfig, ProviderAuth};

const DIR_NAME: &str = "tern";
const FILE_NAME: &str = "tern.json";

/// 样例里的 key 占位符，`serve` 时检测到会提醒
pub const PLACEHOLDER_KEY: &str = "sk-REPLACE_ME";

/// 系统配置目录下的 `tern/tern.json`（Windows 为 `%APPDATA%\tern\tern.json`）
pub fn default_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("无法确定系统配置目录，请用 --config 指定配置文件")?;
    Ok(dir.join(DIR_NAME).join(FILE_NAME))
}

pub fn load(path: &Path) -> Result<GatewayConfig> {
    if !path.exists() {
        bail!(
            "配置文件 {} 不存在，先运行 `tern init` 生成样例",
            path.display()
        );
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取配置文件 {} 失败", path.display()))?;
    parse(&text).with_context(|| format!("解析配置文件 {} 失败", path.display()))
}

pub fn parse(text: &str) -> Result<GatewayConfig> {
    // Windows PowerShell 5 的 `Set-Content -Encoding utf8` 会写 BOM，serde_json 不认
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    Ok(serde_json::from_str(text)?)
}

/// 样例配置：一个 Anthropic 兼容供应商给 Claude Code，一个 Chat 供应商给 Codex。
/// 手写而不是序列化 `GatewayConfig`，只列用户需要改的字段。
pub fn sample(access_token: &str) -> Value {
    json!({
        "listen": "127.0.0.1:15800",
        "accessToken": access_token,
        "defaultProvider": "deepseek",
        "providers": [
            {
                "id": "deepseek",
                "name": "DeepSeek",
                "baseUrl": "https://api.deepseek.com/anthropic",
                "apiFormat": "anthropic",
                "auth": { "type": "api_key", "key": PLACEHOLDER_KEY }
            },
            {
                "id": "kimi",
                "name": "Kimi",
                "baseUrl": "https://api.moonshot.cn/v1",
                "apiFormat": "openai_chat",
                "auth": { "type": "api_key", "key": PLACEHOLDER_KEY }
            }
        ]
    })
}

/// 写入样例配置，返回生成的 access token。已存在且未指定 `force` 时报错。
pub fn write_sample(path: &Path, force: bool) -> Result<String> {
    if path.exists() && !force {
        bail!("配置文件 {} 已存在，加 --force 覆盖", path.display());
    }
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("创建目录 {} 失败", dir.display()))?;
    }

    // uuid v4 取自系统随机源，足够做本机访问凭证
    let token = format!("tern-{}", uuid::Uuid::new_v4().simple());
    let mut text = serde_json::to_string_pretty(&sample(&token))?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("写入 {} 失败", path.display()))?;

    // 文件里有供应商 key，非 Windows 下只留给当前用户（%APPDATA% 本身就是用户私有目录）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("设置 {} 权限失败", path.display()))?;
    }

    Ok(token)
}

/// 能启动但大概率用不了的配置，启动时逐条提醒
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
        warnings
            .push("未设置 accessToken：本机任何进程都能通过 tern 使用你的供应商 key".to_string());
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
                    "供应商 {} 使用订阅登录，命令行版暂不支持，发给它的请求会失败",
                    spec.id
                ));
            }
            _ => {}
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_gateway::{Gateway, ProviderSpec};

    #[test]
    fn sample_is_a_valid_gateway_config() {
        let config = parse(&sample("tok").to_string()).unwrap();
        assert_eq!(config.listen.to_string(), "127.0.0.1:15800");
        assert_eq!(config.access_token.as_deref(), Some("tok"));
        assert_eq!(config.providers.len(), 2);
        Gateway::new(config).expect("样例应能通过网关校验");
    }

    #[test]
    fn parse_tolerates_utf8_bom() {
        let text = format!("\u{feff}{}", sample("tok"));
        assert!(parse(&text).is_ok());
    }

    #[test]
    fn parse_error_reports_position() {
        let err = parse("{\n  \"providers\": [,]\n}").unwrap_err();
        assert!(err.to_string().contains("line 2"), "{err}");
    }

    #[test]
    fn load_missing_file_points_to_init() {
        let dir = tempfile::tempdir().unwrap();
        let err = load(&dir.path().join("nope.json")).unwrap_err();
        assert!(err.to_string().contains("tern init"), "{err}");
    }

    #[test]
    fn write_sample_creates_dirs_and_refuses_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join(FILE_NAME);

        let first = write_sample(&path, false).unwrap();
        assert!(first.starts_with("tern-") && first.len() > 30);
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.access_token.as_deref(), Some(first.as_str()));

        let err = write_sample(&path, false).unwrap_err();
        assert!(err.to_string().contains("--force"), "{err}");
        assert_eq!(
            load(&path).unwrap().access_token.as_deref(),
            Some(first.as_str()),
            "未加 --force 时不能改动已有文件"
        );

        let second = write_sample(&path, true).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn sample_warns_about_placeholder_keys_only() {
        let config = parse(&sample("tok").to_string()).unwrap();
        let warnings = warnings(&config);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().all(|w| w.contains("占位符")));
    }

    #[test]
    fn warns_about_missing_token_and_subscription_auth() {
        let mut config = GatewayConfig::new(vec![
            ProviderSpec::new(
                "copilot",
                "Copilot",
                "",
                Default::default(),
                ProviderAuth::GithubCopilot { account_id: None },
            ),
            ProviderSpec::new(
                "ok",
                "OK",
                "https://api.example.com",
                Default::default(),
                ProviderAuth::api_key("sk-real"),
            ),
        ]);
        config.access_token = Some("  ".into());
        let warnings = warnings(&config);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("accessToken"));
        assert!(warnings[1].contains("copilot"));

        assert_eq!(
            super::warnings(&GatewayConfig::new(vec![])).len(),
            2,
            "空供应商列表 + 无 token"
        );
    }
}
