//! 从 cc-switch 的数据库导入供应商配置。
//!
//! # 为什么值得单独做
//!
//! cc-switch 的用户手上已经有几十个配好的供应商（含 key、倍率、分组）。让用户为了试
//! tern 把这些重录一遍不现实——重录成本比"换个网关"的收益高得多，工具就没人会用。
//! 所以导入不是"锦上添花"，是能不能被采用的先决条件。
//!
//! # 口径
//!
//! cc-switch 把每个供应商存成一份 **agent 配置快照**（`settings_config` 里的 env 键值
//! 对），而 tern 要的是结构化 `ProviderSpec`。转换就发生在这层：
//!
//! - `ANTHROPIC_BASE_URL` → `base_url`；`/v1` 后缀按 OpenAI 约定保留
//! - 凭据取 `ANTHROPIC_AUTH_TOKEN`，没有时退回 `ANTHROPIC_API_KEY`
//! - `apiFormat` 以 `meta.api_format` 为准（用户可改的次要信号），缺省按地址猜：
//!   带 `/v1` 的视为 OpenAI Chat，其余视为 Anthropic（透传最安全）
//! - `costMultiplier`（cc-switch 记在 meta 里，可能是字符串也可能是数字）→
//!   tern 的 `cost_multiplier`；`1` / 空 / 无效一律视为没有
//! - 供应商 id 原样保留：模型名 `供应商/模型` 是用户已经写进环境变量的，改了就断
//!
//! 只读 cc-switch 的库，不写它一个字节。

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use crate::{ApiFormat, Gateway, GatewayConfig, ProviderAuth, ProviderSpec};

/// 默认的 cc-switch 库位置：`~/.cc-switch/cc-switch.db`。
/// cc-switch 自己也用这个路径（见其 `config.rs` 的 `get_app_config_dir`）。
pub fn default_cc_switch_db() -> Result<PathBuf, ImportError> {
    let home = dirs::home_dir().ok_or(ImportError::NoHomeDir)?;
    Ok(home.join(".cc-switch").join("cc-switch.db"))
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("找不到用户主目录，无法定位 ~/.cc-switch/cc-switch.db")]
    NoHomeDir,
    #[error("cc-switch 数据库不存在：{path}。装了 cc-switch 才会有这个文件")]
    DbMissing { path: String },
    #[error("打不开 {path}: {source}")]
    DbOpen {
        path: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("读 {table} 失败（cc-switch 版本可能不兼容）: {source}")]
    DbQuery {
        table: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error("导入的配置没通过网关校验: {0}")]
    GatewayConfig(String),
}

/// 把 SQLite 错误收敛成 ImportError，少写点 map_err
trait DbResult<T> {
    fn db(self, table: &'static str) -> Result<T, ImportError>;
}

impl<T> DbResult<T> for Result<T, rusqlite::Error> {
    fn db(self, table: &'static str) -> Result<T, ImportError> {
        self.map_err(|source| ImportError::DbQuery { table, source })
    }
}

/// 单条转换失败的原因。刻意不用 String：固定的几个枚举值才能被上层统计和测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    NoBaseUrl,
    NoCredentials,
    BadJson,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            SkipReason::NoBaseUrl => "没有 ANTHROPIC_BASE_URL，无法确定上游地址",
            SkipReason::NoCredentials => "没有凭据（ANTHROPIC_AUTH_TOKEN / ANTHROPIC_API_KEY）",
            SkipReason::BadJson => "settings_config 不是合法 JSON",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Deserialize)]
struct CcSwitchProvider {
    id: String,
    name: String,
    settings_config: String,
    meta: String,
}

/// cc-switch 的 `settings_config`：env 键值对 + 少量顶层杂项
#[derive(Debug, Deserialize, Default)]
struct CcSwitchSettingsConfig {
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
struct CcSwitchMeta {
    #[serde(default)]
    api_format: Option<String>,
    /// cc-switch 的 meta 用 camelCase。`rename_all` 一次到位，
    /// 比给单个字段加 alias 更贴合这里的实际数据形状。
    #[serde(default, rename = "costMultiplier", alias = "cost_multiplier")]
    cost_multiplier: Option<Value>,
}

/// 导入结果。`skipped` 单独列出来而不是静默丢弃：用户需要知道哪些没搬过来。
#[derive(Debug, Default)]
pub struct ImportReport {
    pub specs: Vec<ProviderSpec>,
    /// (供应商 id, 没搬过来的原因)
    pub skipped: Vec<(String, SkipReason)>,
}

/// 从 cc-switch 库读出 `app_type` 下的供应商，转成 `ProviderSpec`。
///
/// `app_type` 取 `claude`（Anthropic 协议那套）。codex 那套在 cc-switch 里是
/// 另一份 `config.toml` 快照，格式不同，留给后续。
///
/// 只读 cc-switch 的库，不写它一个字节。
pub fn import_providers(db_path: &PathBuf, app_type: &str) -> Result<ImportReport, ImportError> {
    if !db_path.exists() {
        return Err(ImportError::DbMissing {
            path: db_path.display().to_string(),
        });
    }

    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|source| ImportError::DbOpen {
        path: db_path.display().to_string(),
        source,
    })?;

    let mut stmt = conn
        .prepare(
            "SELECT id, name, settings_config, meta
             FROM providers WHERE app_type = ?1 ORDER BY sort_index, id",
        )
        .db("providers")?;

    let rows = stmt
        .query_map([app_type], |row| {
            Ok(CcSwitchProvider {
                id: row.get(0)?,
                name: row.get(1)?,
                settings_config: row.get(2)?,
                meta: row.get(3)?,
            })
        })
        .db("providers")?;

    let mut report = ImportReport::default();
    for row in rows {
        let row = row.db("providers")?;
        match convert(&row) {
            Ok(spec) => report.specs.push(spec),
            Err(reason) => report.skipped.push((row.id.clone(), reason)),
        }
    }

    // 整批过一次网关自己的校验（id 重复、地址无效等）。
    // 逐个校验只能发现单条的问题，重复 id 要放一起才看得出来。
    Gateway::new(GatewayConfig::new(report.specs.clone()))
        .map_err(|error| ImportError::GatewayConfig(error.to_string()))?;

    Ok(report)
}

/// 单条转换。失败的返回原因码，交由上层汇总。
fn convert(row: &CcSwitchProvider) -> Result<ProviderSpec, SkipReason> {
    let settings: CcSwitchSettingsConfig =
        serde_json::from_str(&row.settings_config).map_err(|_| SkipReason::BadJson)?;
    let meta: CcSwitchMeta = serde_json::from_str(&row.meta).unwrap_or_default();

    let base_url = settings
        .env
        .get("ANTHROPIC_BASE_URL")
        .filter(|url| !url.trim().is_empty())
        .ok_or(SkipReason::NoBaseUrl)?
        .clone();

    // Claude 系默认 AUTH_TOKEN；cc-switch 有少数预设用 API_KEY（见其 apiKeyField）
    let key = settings
        .env
        .get("ANTHROPIC_AUTH_TOKEN")
        .or_else(|| settings.env.get("ANTHROPIC_API_KEY"))
        .filter(|key| !key.trim().is_empty())
        .ok_or(SkipReason::NoCredentials)?
        .clone();

    let api_format =
        parse_api_format(meta.api_format.as_deref()).unwrap_or_else(|| guess_api_format(&base_url));

    let mut spec = ProviderSpec::new(
        row.id.clone(),
        row.name.clone(),
        base_url,
        api_format,
        ProviderAuth::api_key(key),
    );
    spec.cost_multiplier = multiplier_of(&meta.cost_multiplier);

    Ok(spec)
}

/// cc-switch 的 meta.api_format 是字符串，映射到 tern 的枚举。
fn parse_api_format(raw: Option<&str>) -> Option<ApiFormat> {
    match raw?.trim() {
        "anthropic" => Some(ApiFormat::Anthropic),
        "openai_chat" => Some(ApiFormat::OpenaiChat),
        "openai_responses" => Some(ApiFormat::OpenaiResponses),
        "gemini_native" => Some(ApiFormat::GeminiNative),
        _ => None,
    }
}

/// 没写 meta.api_format 时按地址猜。宁可不猜也不猜错：Anthropic 兼容站通常
/// 不带 `/v1`，OpenAI 系按 SDK 约定带。判不出来的一律 anthropic（透传最安全）。
fn guess_api_format(base_url: &str) -> ApiFormat {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/v1") || trimmed.ends_with("/v1beta") {
        ApiFormat::OpenaiChat
    } else {
        ApiFormat::Anthropic
    }
}

/// 倍率：字符串或数字都认，"1" / 空 / 无效一律视为没有（等于没有）。
fn multiplier_of(raw: &Option<Value>) -> Option<String> {
    let text = match raw.as_ref()? {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let is_default = text.is_empty()
        || text == "1"
        || text == "1.0"
        || text.parse::<f64>().is_ok_and(|v| (v - 1.0).abs() < f64::EPSILON);
    (!is_default).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(settings: &str, meta: &str) -> CcSwitchProvider {
        CcSwitchProvider {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            settings_config: settings.into(),
            meta: meta.into(),
        }
    }

    #[test]
    fn converts_anthropic_style_preset() {
        let spec = convert(&row(
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.deepseek.com/anthropic","ANTHROPIC_AUTH_TOKEN":"sk-x"}}"#,
            "{}",
        ))
        .unwrap();
        assert_eq!(spec.id, "deepseek");
        assert_eq!(spec.base_url, "https://api.deepseek.com/anthropic");
        assert_eq!(spec.api_format, ApiFormat::Anthropic);
        assert!(matches!(spec.auth, ProviderAuth::ApiKey { .. }));
    }

    #[test]
    fn falls_back_to_api_key_when_auth_token_absent() {
        let spec = convert(&row(
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com","ANTHROPIC_API_KEY":"sk-y"}}"#,
            "{}",
        ))
        .unwrap();
        assert!(matches!(spec.auth, ProviderAuth::ApiKey { .. }));
    }

    #[test]
    fn meta_api_format_wins_over_url_guess() {
        let spec = convert(&row(
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com/v1","ANTHROPIC_AUTH_TOKEN":"k"}}"#,
            r#"{"api_format":"openai_chat"}"#,
        ))
        .unwrap();
        assert_eq!(spec.api_format, ApiFormat::OpenaiChat);
    }

    #[test]
    fn v1_suffix_is_guessed_as_openai_chat() {
        let spec = convert(&row(
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.moonshot.cn/v1","ANTHROPIC_AUTH_TOKEN":"k"}}"#,
            "{}",
        ))
        .unwrap();
        assert_eq!(spec.api_format, ApiFormat::OpenaiChat);
    }

    #[test]
    fn cost_multiplier_accepts_string_and_drops_the_default() {
        let with = convert(&row(
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com","ANTHROPIC_AUTH_TOKEN":"k"}}"#,
            r#"{"costMultiplier":"0.3"}"#,
        ))
        .unwrap();
        assert_eq!(with.cost_multiplier.as_deref(), Some("0.3"));

        for default_meta in [
            r#"{"costMultiplier":"1"}"#,
            r#"{"costMultiplier":1}"#,
            r#"{"costMultiplier":1.0}"#,
            r#"{"costMultiplier":""}"#,
            "{}",
        ] {
            let spec = convert(&row(
                r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com","ANTHROPIC_AUTH_TOKEN":"k"}}"#,
                default_meta,
            ))
            .unwrap();
            assert_eq!(spec.cost_multiplier, None, "{default_meta}");
        }
    }

    #[test]
    fn missing_base_url_is_reported() {
        assert_eq!(
            convert(&row(r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"k"}}"#, "{}")).unwrap_err(),
            SkipReason::NoBaseUrl
        );
    }

    #[test]
    fn missing_credentials_is_reported() {
        assert_eq!(
            convert(&row(
                r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com"}}"#,
                "{}"
            ))
            .unwrap_err(),
            SkipReason::NoCredentials
        );
    }

    #[test]
    fn broken_json_is_reported() {
        assert_eq!(
            convert(&row("not json", "{}")).unwrap_err(),
            SkipReason::BadJson
        );
    }

    #[test]
    fn skip_reasons_read_as_sentences() {
        assert!(SkipReason::NoBaseUrl.to_string().contains("ANTHROPIC_BASE_URL"));
        assert!(SkipReason::NoCredentials.to_string().contains("凭据"));
        assert!(SkipReason::BadJson.to_string().contains("JSON"));
    }

    #[test]
    fn missing_db_path_is_an_error_not_an_empty_list() {
        let missing = std::env::temp_dir().join("tern-does-not-exist.db");
        let error = import_providers(&missing, "claude").unwrap_err();
        assert!(error.to_string().contains("不存在"), "{error}");
    }
}
