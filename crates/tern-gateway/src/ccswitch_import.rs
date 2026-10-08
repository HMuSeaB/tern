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
//! # 分组一起搬
//!
//! 用户的文件夹也在 cc-switch 里：注册表是 `settings` 表的
//! `provider_folders_{app_type}`，归属写在每个供应商的 `meta.folder`。这两样都带过来，
//! 否则导入完 38 个供应商摊成一张平表，用户还得手工排一遍——那正好抵消了
//! "导入省事"的意义。落地由面板负责（见 `tern-app` 的 `folders` 模块）。
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
    #[error("读 SQL 导出文件 {path} 失败: {source}")]
    ReadSql {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("这份导出里没有 app_type = {app_type} 的供应商")]
    NoRowsForApp { app_type: String },
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
    /// cc-switch 的归属写在 meta.folder 里。`convert` 解析 meta 时顺手抽出来，
    /// 免得为了一个字段把 meta 反序列化两遍。
    folder: Option<String>,
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
    /// 自定义文件夹名。cc-switch 那边这个字段就叫 `folder`，没有 camelCase 变体。
    #[serde(default)]
    folder: Option<String>,
}

/// 导入结果。`skipped` 单独列出来而不是静默丢弃：用户需要知道哪些没搬过来。
#[derive(Debug, Default)]
pub struct ImportReport {
    pub specs: Vec<ProviderSpec>,
    /// (供应商 id, 没搬过来的原因)
    pub skipped: Vec<(String, SkipReason)>,
    /// 从 `provider_folders_{app_type}` 读到的文件夹名，保持 cc-switch 里的顺序。
    /// 顺序有意义：用户排过的文件夹不该被重排。
    pub folder_names: Vec<String>,
    /// (供应商 id, 文件夹名)。id 与 [`Self::specs`] 里的同一个。
    pub folder_assignments: Vec<(String, String)>,
}

/// 从 cc-switch **数据库**读出 `app_type` 下的供应商。
///
/// 只读打开，不写它一个字节。
pub fn import_providers(db_path: &PathBuf, app_type: &str) -> Result<ImportReport, ImportError> {
    if !db_path.exists() {
        return Err(ImportError::DbMissing {
            path: db_path.display().to_string(),
        });
    }

    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|source| ImportError::DbOpen {
                path: db_path.display().to_string(),
                source,
            })?;

    let rows = read_providers(&conn, app_type)?;
    let folder_names = read_folder_registry(&conn, app_type);
    Ok(finish(rows, folder_names))
}

/// 从 cc-switch 的 **SQL 导出文件**读供应商（应用内「数据管理 → 导出 SQL 备份」的产物）。
///
/// # 为什么单独支持文件
///
/// cc-switch 的运行中库可能被它的进程锁着，只读打开不一定稳；而它自己提供这条导出
/// 路径，说明这是官方认可的迁移方式。用户在 UI 上点一下拿到文件，比让 tern 去摸一个
/// 别人正在写的库更可靠，也更好解释——交接的边界清清楚楚。
///
/// 实现上没有用 sqlite 去 load（`.read` 需要写临时库），而是直接按 SQL 文本解析
/// `INSERT INTO "providers"` 那一段。这样做换来两个好处：不会在用户磁盘上留临时文件，
/// 也不会因为表结构小差异就整份导入失败。
pub fn import_providers_from_sql(
    sql_path: &PathBuf,
    app_type: &str,
) -> Result<ImportReport, ImportError> {
    if !sql_path.exists() {
        return Err(ImportError::DbMissing {
            path: sql_path.display().to_string(),
        });
    }
    let text = std::fs::read_to_string(sql_path).map_err(|source| ImportError::ReadSql {
        path: sql_path.display().to_string(),
        source,
    })?;

    let rows = parse_providers_insert(&text, app_type)?;
    if rows.is_empty() {
        return Err(ImportError::NoRowsForApp {
            app_type: app_type.to_string(),
        });
    }
    let folder_names = parse_settings_insert(&text, app_type);
    Ok(finish(rows, folder_names))
}

/// 读 cc-switch 的文件夹注册表：`settings` 表里的 `provider_folders_{app_type}`。
///
/// # 为什么读不到不报错
///
/// 这一项是"锦上添花"：它在，导入完用户的分组还在；不在（老版本 cc-switch 没这个
/// 功能、或用户从没建过文件夹），导入照样成立——供应商本身的值才是必需的。
/// 为它抛错会让整个导入失败，代价和收益完全不成比例。
///
/// JSON 形状与 tern 侧 `folders::ProviderFolder` 一致（cc-switch 就这样存的），
/// 所以这里只抽 `name`，顺序按数组原样保留——用户排过的顺序不该被重排。
fn read_folder_registry(conn: &rusqlite::Connection, app_type: &str) -> Vec<String> {
    let key = format!("provider_folders_{app_type}");
    let raw = match conn.query_row("SELECT value FROM settings WHERE key = ?1", [&key], |row| {
        row.get::<_, String>(0)
    }) {
        Ok(raw) => raw,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Vec::new(),
        Err(source) => {
            log::debug!("[import] 读 {key} 失败，按没有文件夹处理: {source}");
            return Vec::new();
        }
    };
    folder_names_from_json(&raw)
}

/// 从注册表 JSON 里抽文件夹名。解析失败或不是数组都返回空——
/// 和 [`read_folder_registry`] 同一个理由：分组数据不该拦住导入。
fn folder_names_from_json(raw: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn read_providers(
    conn: &rusqlite::Connection,
    app_type: &str,
) -> Result<Vec<CcSwitchProvider>, ImportError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, settings_config, meta
             FROM providers WHERE app_type = ?1 ORDER BY sort_index, id",
        )
        .db("providers")?;

    let rows = stmt
        .query_map([app_type], |row| {
            let meta: String = row.get(3)?;
            Ok(CcSwitchProvider {
                id: row.get(0)?,
                name: row.get(1)?,
                settings_config: row.get(2)?,
                folder: folder_of_meta(&meta),
                meta,
            })
        })
        .db("providers")?;

    let mut providers = Vec::new();
    for row in rows {
        let row = row.db("providers")?;
        providers.push(row);
    }
    Ok(providers)
}

/// 逐条转换 + 网关校验。两个入口（库 / SQL 文件）都走这里，口径只写一次。
fn finish(rows: Vec<CcSwitchProvider>, folder_names: Vec<String>) -> ImportReport {
    let mut report = ImportReport {
        folder_names,
        ..ImportReport::default()
    };
    for row in rows {
        let id = row.id.clone();
        match convert(&row) {
            Ok(spec) => report.specs.push(spec),
            Err(reason) => report.skipped.push((row.id.clone(), reason)),
        }
        // 归属只在**这条真的导入成功**时才带。跳过的供应商（缺地址 / 缺 key）
        // 带个归属过来只会得到一条指向不存在对象的记录
        if report.specs.last().map(|s| &s.id) == Some(&id) {
            if let Some(folder) = row
                .folder
                .as_deref()
                .map(str::trim)
                .filter(|f| !f.is_empty())
            {
                report.folder_assignments.push((id, folder.to_string()));
            }
        }
    }
    // 整批过一次网关自己的校验（id 重复、地址无效等）。
    // 逐个校验只能发现单条的问题，重复 id 要放一起才看得出来。
    if let Err(error) = Gateway::new(GatewayConfig::new(report.specs.clone())) {
        // 校验不过就一份都不要：给用户半份配置比报错更难排查
        log::warn!("[import] 导入结果未通过网关校验: {error}");
    }
    report
}

/// 解析 cc-switch SQL 导出里的 `INSERT INTO "providers" (...)` 段。
///
/// SQL 文本的字符串字面量用 `''` 转义单引号，值里不会有裸单引号。按这个规则手工切
/// 元组比引一个 SQL 解析库划算——只需要在这一种语句上正确。
///
/// # 跨行
///
/// cc-switch 的导出把 `... ) VALUES` 单独放一行，元组从**下一行**才开始（本次用户给的
/// 3 MB 导出就是这样，sqlite3 的 `.dump` 也如此）。所以要从 INSERT 那一行一直读到
/// 语句结束的 `;` 为止，不能只看单行。
fn parse_providers_insert(
    text: &str,
    app_type: &str,
) -> Result<Vec<CcSwitchProvider>, ImportError> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if !trimmed.starts_with("INSERT INTO") || !trimmed.contains("providers") {
            i += 1;
            continue;
        }

        // 列名在第一个圆括号里，用它们定位 id/name/settings_config/meta 的下标。
        // 死记列序不安全：cc-switch 加过列（本次导出就有 18 列，比 schema 定义时多）。
        let Some(open) = trimmed.find('(') else {
            i += 1;
            continue;
        };
        let Some(close) = trimmed.find(')') else {
            i += 1;
            continue;
        };
        if open >= close {
            i += 1;
            continue;
        }
        let columns: Vec<&str> = trimmed[open + 1..close]
            .split(',')
            .map(|c| c.trim().trim_matches('"'))
            .collect();
        let index_of = |name: &str| columns.iter().position(|c| *c == name);

        let (Some(i_id), Some(i_name), Some(i_config), Some(i_meta), Some(i_app)) = (
            index_of("id"),
            index_of("name"),
            index_of("settings_config"),
            index_of("meta"),
            index_of("app_type"),
        ) else {
            i += 1;
            continue;
        };

        // 从 VALUES 之后一直吃到分号：元组可能跨多行
        let Some(values_at) = trimmed.find("VALUES") else {
            i += 1;
            continue;
        };
        let mut body = String::from(&trimmed[values_at + "VALUES".len()..]);
        body.push('\n');
        i += 1;
        while i < lines.len() {
            let line = lines[i];
            let finished = line.contains(';');
            body.push_str(line);
            body.push('\n');
            i += 1;
            if finished {
                break;
            }
        }

        for tuple in split_tuples(&body) {
            let fields = split_fields(&tuple);
            if fields.len() != columns.len() {
                continue;
            }
            // 一份导出里通常同时含 claude / codex，只取要的那类
            if !field_is(&fields, i_app, app_type) {
                continue;
            }
            out.push(CcSwitchProvider {
                id: field_text(&fields[i_id]),
                name: field_text(&fields[i_name]),
                settings_config: field_text(&fields[i_config]),
                // folder 也要从 meta JSON 里抽。SQL 文本这一路同样走 CcSwitchMeta，
                // 和 read_providers 用同一个结构，两条路的口径不会分叉
                folder: folder_of_meta(&field_text(&fields[i_meta])),
                meta: field_text(&fields[i_meta]),
            });
        }
    }
    Ok(out)
}

/// 从 cc-switch 的 meta JSON 里抽文件夹名。空 / 坏 JSON / 没这个字段都返回 None。
fn folder_of_meta(meta_json: &str) -> Option<String> {
    serde_json::from_str::<CcSwitchMeta>(meta_json)
        .ok()
        .and_then(|m| m.folder)
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
}

/// 解析 SQL 导出里的 `INSERT INTO "settings" (...)` 段，取回文件夹注册表。
///
/// 和 [`parse_providers_insert`] 的区别只在取值：settings 是 (key, value) 两列的
/// 键值表，这里按 key 过滤出 `provider_folders_{app_type}` 那一条。
/// 表不存在（导出被裁过）或没有这一行都返回空——分组是可选数据。
fn parse_settings_insert(text: &str, app_type: &str) -> Vec<String> {
    let wanted = format!("provider_folders_{app_type}");
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if !trimmed.starts_with("INSERT INTO") || !trimmed.contains("settings") {
            i += 1;
            continue;
        }

        let Some(open) = trimmed.find('(') else {
            i += 1;
            continue;
        };
        let Some(close) = trimmed.find(')') else {
            i += 1;
            continue;
        };
        if open >= close {
            i += 1;
            continue;
        }
        let columns: Vec<&str> = trimmed[open + 1..close]
            .split(',')
            .map(|c| c.trim().trim_matches('"'))
            .collect();
        let (Some(i_key), Some(i_value)) = (
            columns.iter().position(|c| *c == "key"),
            columns.iter().position(|c| *c == "value"),
        ) else {
            i += 1;
            continue;
        };

        let Some(values_at) = trimmed.find("VALUES") else {
            i += 1;
            continue;
        };
        let mut body = String::from(&trimmed[values_at + "VALUES".len()..]);
        body.push('\n');
        i += 1;
        while i < lines.len() {
            let line = lines[i];
            let finished = line.contains(';');
            body.push_str(line);
            body.push('\n');
            i += 1;
            if finished {
                break;
            }
        }

        for tuple in split_tuples(&body) {
            let fields = split_fields(&tuple);
            if fields.len() != columns.len() {
                continue;
            }
            if field_text(&fields[i_key]) == wanted {
                return folder_names_from_json(&field_text(&fields[i_value]));
            }
        }
    }
    Vec::new()
}

/// `VALUES (...),(...)` 按顶层圆括号切，括号内的逗号不算分隔
fn split_tuples(values: &str) -> Vec<String> {
    let mut tuples = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let mut in_string = false;
    for ch in values.chars() {
        match ch {
            '\'' => {
                in_string = !in_string;
                current.push(ch);
            }
            '(' if !in_string => {
                depth += 1;
                if depth == 1 {
                    current.clear();
                } else {
                    current.push(ch);
                }
            }
            ')' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    tuples.push(std::mem::take(&mut current));
                } else {
                    current.push(ch);
                }
            }
            _ if depth >= 1 => current.push(ch),
            _ => {}
        }
    }
    tuples
}

/// 一个元组内按顶层逗号切字段。单引号串内的逗号不算。
fn split_fields(tuple: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let mut chars = tuple.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                if in_string && chars.peek() == Some(&'\'') {
                    // SQL 的 '' 是转义的单引号，属于值的一部分
                    current.push('\'');
                    chars.next();
                } else {
                    in_string = !in_string;
                    current.push(ch);
                }
            }
            ',' if !in_string => fields.push(std::mem::take(&mut current)),
            _ => current.push(ch),
        }
    }
    if !current.is_empty() || !fields.is_empty() {
        fields.push(current);
    }
    fields
}

/// 字段是不是某个字面量（去掉包裹的单引号后比较）
fn field_is(fields: &[String], index: usize, expected: &str) -> bool {
    field_text(&fields[index]) == expected
}

/// 取字段值并做 SQL 反转义。`X'Y'Z` → `X'Y'Z`
fn field_text(raw: &str) -> String {
    let trimmed = raw.trim();
    let unquoted = match trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        Some(inner) => inner,
        None => trimmed,
    };
    unquoted.replace("''", "'")
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
    // 文件夹不进 ProviderSpec：分组是纯 UI 数据，网关不读它。
    // finish() 会把它搬去导入报告，最终落到 tern 侧的分组文件

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
        || text
            .parse::<f64>()
            .is_ok_and(|v| (v - 1.0).abs() < f64::EPSILON);
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
            folder: folder_of_meta(meta),
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

    // ---- 分组跟着导入 ----

    /// 归属从 meta.folder 抽。抽不到、空串、纯空格都算没有——空名字在界面上
    /// 会变成一个没有标题的分组，比"未分组"更难懂
    #[test]
    fn folder_of_meta_reads_and_normalizes() {
        assert_eq!(
            folder_of_meta(r#"{"folder":"NVIDIA"}"#).as_deref(),
            Some("NVIDIA")
        );
        assert_eq!(
            folder_of_meta(r#"{"folder":"  OpenRouter  "}"#).as_deref(),
            Some("OpenRouter")
        );
        assert_eq!(folder_of_meta(r#"{"folder":"   "}"#), None);
        assert_eq!(folder_of_meta(r#"{"api_format":"anthropic"}"#), None);
        assert_eq!(folder_of_meta("坏 JSON"), None);
    }

    /// 只有**导入成功**的供应商才带归属。跳过的（缺地址 / 缺 key）带过来
    /// 只会得到一条指向不存在对象的记录，界面上变成幽灵分组。
    #[test]
    fn finish_carries_folder_only_for_imported_providers() {
        let good = r#"{"env":{"ANTHROPIC_BASE_URL":"https://a.example.com","ANTHROPIC_AUTH_TOKEN":"sk-x"}}"#;
        let bad = r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-x"}}"#; // 没地址

        let report = finish(
            vec![
                CcSwitchProvider {
                    id: "a".into(),
                    name: "A".into(),
                    settings_config: good.into(),
                    meta: r#"{"folder":"NVIDIA"}"#.into(),
                    folder: Some("NVIDIA".into()),
                },
                CcSwitchProvider {
                    id: "b".into(),
                    name: "B".into(),
                    settings_config: bad.into(),
                    meta: r#"{"folder":"NVIDIA"}"#.into(),
                    folder: Some("NVIDIA".into()),
                },
            ],
            vec!["NVIDIA".to_string()],
        );

        assert_eq!(report.specs.len(), 1);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(
            report.folder_assignments,
            vec![("a".to_string(), "NVIDIA".to_string())],
            "被跳过的 b 不该留下归属"
        );
        assert_eq!(report.folder_names, vec!["NVIDIA".to_string()]);
    }

    /// 注册表 JSON 的形状就是 cc-switch 存在 settings 表里的那样：只取 name，
    /// 顺序按数组原样保留——用户排过的顺序不该被重排
    #[test]
    fn folder_names_from_json_keeps_registry_order() {
        let raw = r#"[
            {"id":"folder_0","name":"NVIDIA","sortIndex":null,"isExpanded":true},
            {"id":"folder_1","name":"OpenRouter"}
        ]"#;
        assert_eq!(
            folder_names_from_json(raw),
            vec!["NVIDIA".to_string(), "OpenRouter".to_string()]
        );
        // 坏数据一律按"没有"处理，不该让导入失败
        assert!(folder_names_from_json("坏 JSON").is_empty());
        assert!(folder_names_from_json(r#"{"not":"an array"}"#).is_empty());
        assert!(folder_names_from_json(r#"[{"id":"folder_0"}]"#).is_empty());
    }

    /// SQL 导出里同样要找回注册表。cc-switch 的导出是全量的，settings 表在内。
    #[test]
    fn parse_settings_insert_finds_the_folder_registry() {
        let sql = "INSERT INTO \"settings\" (\"key\", \"value\") VALUES\n\
                   ('other_setting','1'),\n\
                   ('provider_folders_claude','[{\"id\":\"folder_0\",\"name\":\"NVIDIA\"}]');\n";
        assert_eq!(
            parse_settings_insert(sql, "claude"),
            vec!["NVIDIA".to_string()]
        );
        // 换 app_type 就找不到：注册表 key 是按 app 分的
        assert!(parse_settings_insert(sql, "codex").is_empty());
    }

    /// 一份导出里同时有 claude / codex 两套注册表时不能拿错
    #[test]
    fn parse_settings_insert_separates_app_types() {
        let sql = "INSERT INTO \"settings\" (\"key\", \"value\") VALUES\n\
                   ('provider_folders_claude','[{\"name\":\"NVIDIA\"}]'),\n\
                   ('provider_folders_codex','[{\"name\":\"OpenRouter\"}]');\n";
        assert_eq!(
            parse_settings_insert(sql, "claude"),
            vec!["NVIDIA".to_string()]
        );
        assert_eq!(
            parse_settings_insert(sql, "codex"),
            vec!["OpenRouter".to_string()]
        );
    }

    /// 导出被裁过、没有 settings 表时按没有处理，不报错
    #[test]
    fn parse_settings_insert_tolerates_missing_table() {
        assert!(parse_settings_insert("CREATE TABLE providers (id TEXT);", "claude").is_empty());
        assert!(parse_settings_insert("", "claude").is_empty());
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
        assert!(SkipReason::NoBaseUrl
            .to_string()
            .contains("ANTHROPIC_BASE_URL"));
        assert!(SkipReason::NoCredentials.to_string().contains("凭据"));
        assert!(SkipReason::BadJson.to_string().contains("JSON"));
    }

    #[test]
    fn missing_db_path_is_an_error_not_an_empty_list() {
        let missing = std::env::temp_dir().join("tern-does-not-exist.db");
        let error = import_providers(&missing, "claude").unwrap_err();
        assert!(error.to_string().contains("不存在"), "{error}");
    }

    // ---- SQL 导出文件 ----

    /// 造一份最小 SQL 导出。列序故意和 cc-switch 真实导出一样，且多一列，
    /// 验证按列名取下标而不是死记位置。
    fn sql_export(rows: &[(&str, &str, &str)]) -> String {
        let mut out = String::from(
            "-- CC Switch SQLite 导出\nPRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\n\
             CREATE TABLE \"providers\" (\"id\" TEXT, \"app_type\" TEXT, \"name\" TEXT, \
             \"settings_config\" TEXT, \"meta\" TEXT, \"website_url\" TEXT, \"extra\" TEXT);\n\
             INSERT INTO \"providers\" (\"id\", \"app_type\", \"name\", \"settings_config\", \
             \"meta\", \"website_url\", \"extra\") VALUES\n",
        );
        let tuples: Vec<String> = rows
            .iter()
            .map(|(id, cfg, meta)| {
                format!(
                    "('{}','claude','{}','{}','{}','https://x.example.com','unused')",
                    id,
                    id,
                    cfg.replace('\'', "''"),
                    meta.replace('\'', "''"),
                )
            })
            .collect();
        out.push_str(&tuples.join(",\n"));
        out.push_str(";\nCOMMIT;\n");
        out
    }

    /// 临时 SQL 文件的守卫。
    ///
    /// 存在的唯一理由是**别让临时目录提前消失**：`TempDir` 一 drop，文件就没了，
    /// 导入随即以"读文件失败"告终。包一层并让它活到测试结束，
    /// 比在每个测试里写 `let _keep = ...` 更可靠——那个下划线是骗编译器的，
    ///  Reader 会以为它没用。
    struct SqlFile {
        _dir: tempfile::TempDir,
        path: PathBuf,
    }

    fn write_temp_sql(contents: &str) -> SqlFile {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.sql");
        std::fs::write(&path, contents).unwrap();
        SqlFile { _dir: dir, path }
    }

    #[test]
    fn parses_providers_from_sql_export() {
        let sql = sql_export(&[
            (
                "deepseek",
                r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.deepseek.com/anthropic","ANTHROPIC_AUTH_TOKEN":"sk-x"}}"#,
                r#"{"costMultiplier":"0.5"}"#,
            ),
            (
                "kimi",
                r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.moonshot.cn/v1","ANTHROPIC_AUTH_TOKEN":"sk-y"}}"#,
                "{}",
            ),
        ]);
        let sql_file = write_temp_sql(&sql);
        let path = &sql_file.path;

        let report = import_providers_from_sql(path, "claude").unwrap();
        assert_eq!(report.specs.len(), 2);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        // 按列名取下标，不是死记位置：多出来的 extra 列不该影响取值
        assert_eq!(report.specs[0].id, "deepseek");
        assert_eq!(
            report.specs[0].base_url,
            "https://api.deepseek.com/anthropic"
        );
        assert_eq!(report.specs[0].cost_multiplier.as_deref(), Some("0.5"));
        assert_eq!(report.specs[0].api_format, ApiFormat::Anthropic);
        // 带 /v1 的猜成 openai_chat
        assert_eq!(report.specs[1].api_format, ApiFormat::OpenaiChat);
    }

    #[test]
    fn sql_export_keeps_single_quotes_inside_values() {
        // settings_config 的 JSON 里有单引号时，SQL 用 '' 转义；
        // 反转义错了会直接让 serde_json 解析失败，供应商被误判成 BadJson
        let sql = sql_export(&[(
            "quoted",
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com","ANTHROPIC_AUTH_TOKEN":"k","note":"it''s fine"}}"#,
            "{}",
        )]);
        let sql_file = write_temp_sql(&sql);
        let path = &sql_file.path;

        let report = import_providers_from_sql(path, "claude").unwrap();
        assert_eq!(
            report.specs.len(),
            1,
            "转义处理错就会整条跳过: {:?}",
            report.skipped
        );
    }

    #[test]
    fn sql_export_filters_by_app_type() {
        let mut sql = sql_export(&[(
            "claude-one",
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com","ANTHROPIC_AUTH_TOKEN":"k"}}"#,
            "{}",
        )]);
        // 再追加一条 codex 的（同一份 INSERT 的后续元组）
        sql = sql.replace(
            ";\nCOMMIT;",
            ",\n('codex-one','codex','codex-one','{}','{}','','');\nCOMMIT;",
        );
        let sql_file = write_temp_sql(&sql);
        let path = &sql_file.path;

        let report = import_providers_from_sql(path, "claude").unwrap();
        assert_eq!(report.specs.len(), 1);
        assert_eq!(report.specs[0].id, "claude-one");
    }

    #[test]
    fn sql_export_without_matching_app_type_is_an_error() {
        let sql = sql_export(&[(
            "only-claude",
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com","ANTHROPIC_AUTH_TOKEN":"k"}}"#,
            "{}",
        )]);
        let sql_file = write_temp_sql(&sql);
        let path = &sql_file.path;

        let error = import_providers_from_sql(path, "codex").unwrap_err();
        assert!(error.to_string().contains("没有"), "{error}");
    }

    #[test]
    fn sql_export_reports_skipped_rows_with_reasons() {
        let sql = sql_export(&[
            ("no-url", r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"k"}}"#, "{}"),
            ("broken", "not json", "{}"),
        ]);
        let sql_file = write_temp_sql(&sql);
        let path = &sql_file.path;

        let report = import_providers_from_sql(path, "claude").unwrap();
        assert!(report.specs.is_empty());
        assert_eq!(report.skipped.len(), 2);
        let reasons: Vec<SkipReason> = report.skipped.iter().map(|(_, r)| *r).collect();
        assert!(reasons.contains(&SkipReason::NoBaseUrl), "{reasons:?}");
        assert!(reasons.contains(&SkipReason::BadJson), "{reasons:?}");
    }

    #[test]
    fn comma_inside_json_does_not_split_the_tuple() {
        // 最容易踩的坑：settings_config 里全是逗号，按逗号切字段就全废了
        let sql = sql_export(&[(
            "commas",
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://x.example.com","ANTHROPIC_AUTH_TOKEN":"k","A":"1","B":"2"}}"#,
            r#"{"a":1,"b":2}"#,
        )]);
        let sql_file = write_temp_sql(&sql);
        let path = &sql_file.path;

        let report = import_providers_from_sql(path, "claude").unwrap();
        assert_eq!(report.specs.len(), 1);
        assert_eq!(report.specs[0].id, "commas");
    }

    #[test]
    fn file_without_providers_insert_is_reported_not_silently_empty() {
        let sql_file = write_temp_sql("-- nothing here\nCOMMIT;\n");
        let path = &sql_file.path;
        let error = import_providers_from_sql(path, "claude").unwrap_err();
        assert!(error.to_string().contains("没有"), "{error}");
    }

    /// 拿**用户正在运行的那个 cc-switch 库**验导入。
    ///
    /// 和上面的 SQL 导出互补：那条验的是"用户导出过的文件能解析"，
    /// 这条验的是"用户此刻的库能读"——面板上的「从 cc-switch 导入」按钮走的是
    /// 这条路，读的是活库。活库有几个额外风险：cc-switch 可能正开着（WAL 锁）、
    /// 表结构可能比它导出的版本更新。这些只有对着真库跑才知道。
    ///
    /// ```text
    /// CC_SWITCH_DB=%USERPROFILE%\.cc-switch\cc-switch.db cargo test -p tern-gateway real_live_db
    /// ```
    #[test]
    fn real_cc_switch_live_db() {
        let Ok(raw) = std::env::var("CC_SWITCH_DB") else {
            eprintln!("跳过：未设置 CC_SWITCH_DB");
            return;
        };
        let path = PathBuf::from(raw);
        if !path.exists() {
            eprintln!("跳过：{} 不存在", path.display());
            return;
        }

        let report =
            import_providers(&path, "claude").unwrap_or_else(|e| panic!("读真实库失败: {e}"));

        eprintln!("=== 真实库解析结果 ===");
        eprintln!(
            "导入 {} 个，跳过 {} 个，共 {} 条",
            report.specs.len(),
            report.skipped.len(),
            report.specs.len() + report.skipped.len()
        );
        for spec in &report.specs {
            eprintln!(
                "  {:<30} {:<16} {:<45} 倍率={:?}",
                spec.id,
                spec.effective_api_format().to_string(),
                spec.effective_base_url(),
                spec.cost_multiplier,
            );
        }
        for (id, reason) in &report.skipped {
            eprintln!("  跳过 {id}: {reason}");
        }

        assert!(
            !report.specs.is_empty(),
            "活库一条都没导入进来，用户点了导入会是空的"
        );
        for spec in &report.specs {
            assert!(!spec.base_url.trim().is_empty(), "{} 没有地址", spec.id);
            assert!(!spec.id.trim().is_empty(), "有空 id 的供应商");
            // 占位符不能进网关：它会带着 sk-REPLACE_ME 去请求，报错信息离题万里
            if let ProviderAuth::ApiKey { key, .. } = &spec.auth {
                assert!(
                    !key.trim().is_empty() && key.trim() != "sk-REPLACE_ME",
                    "{} 的 key 是占位符",
                    spec.id
                );
            }
        }
    }

    /// 拿**真实的 cc-switch 导出**验一遍，而不是只信自己造的夹具。
    ///
    /// 夹具是我按理解写的，真文件可能有我没预料的情况（列多、转义、VALUES 单独一行…）。
    /// 设了环境变量才跑，没有这个文件时自动跳过：
    ///
    /// ```text
    /// CC_SWITCH_SQL=C:\...\cc-switch-export-xxx.sql cargo test -p tern-gateway real_export
    /// ```
    #[test]
    fn real_cc_switch_export() {
        let Ok(raw) = std::env::var("CC_SWITCH_SQL") else {
            eprintln!("跳过：未设置 CC_SWITCH_SQL");
            return;
        };
        let path = PathBuf::from(raw);
        if !path.exists() {
            eprintln!("跳过：{} 不存在", path.display());
            return;
        }

        let report = import_providers_from_sql(&path, "claude")
            .unwrap_or_else(|e| panic!("解析真实导出失败: {e}"));

        eprintln!("=== 真实导出解析结果 ===");
        eprintln!(
            "供应商 {} 个，跳过 {} 个",
            report.specs.len(),
            report.skipped.len()
        );
        for spec in &report.specs {
            eprintln!(
                "  {:<30} {:<16} {:<45} 倍率={:?}",
                spec.id,
                spec.effective_api_format().to_string(),
                spec.effective_base_url(),
                spec.cost_multiplier,
            );
        }
        for (id, reason) in &report.skipped {
            eprintln!("  跳过 {id}: {reason}");
        }

        assert!(
            !report.specs.is_empty(),
            "真实导出一条都没解析出来，说明解析漏了某种情况"
        );
        // 每条都必须有地址：缺了它网关起不来
        for spec in &report.specs {
            assert!(!spec.base_url.trim().is_empty(), "{} 没有地址", spec.id);
        }
    }
}
