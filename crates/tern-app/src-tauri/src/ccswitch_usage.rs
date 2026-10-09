//! 从 cc-switch 的库导入历史用量。
//!
//! # 为什么需要
//!
//! tern 是自己开始记的，历史都在 cc-switch 的 `proxy_request_logs` 里（本人 17136 条）。
//! 不导进来的话面板只有"从装上 tern 起"的数据，用户想看的"这个月一共花了多少"答不了。
//!
//! # 四个逐列核对出来的陷阱
//!
//! 这四个都是我对着真实库 `PRAGMA table_info` + 抽样看出来的，每一个都会让导入的
//! 数字错，所以写在这里而不是代码里：
//!
//! 1. **`provider_id = '_session'` 的行必须跳过。** 那些是 cc-switch 从 Claude Code
//!    的 session 日志反推的（`data_source = 'session_log'`），它的 `input_tokens`
//!    是"未命中缓存的新输入"——而 proxy 行的 `input_tokens` 是 Anthropic 原生口径
//!    （不含 cache_read）。两种口径混在一列里，按同一条 SQL 求"新增输入"就会偏低。
//!
//! 2. **`input_token_semantics = 2` 表示 `input_tokens` 不含缓存读。** 这正好和 tern
//!    的 `fresh_input` 同义，所以**直接搬、不要相加**。相加等于把缓存读算两遍。
//!
//! 3. **`created_at` 是 Unix 秒，tern 用毫秒。** 差 1000 倍，不转会全部落到 1970 年。
//!
//! 4. **大部分行没有成本值**（17136 条里只有 90 条有 `total_cost_usd`）。所以不能
//!    搬 `total_cost_usd`，必须按 `tern_store` 自己的价格表重算——否则导入的历史
//!    九成是 0 元，趋势图会是一条平线。重算用的是同一个 `PriceBook`，
//!    口径和 tern 自己记的请求一致。
//!
//! # 幂等
//!
//! cc-switch 的 `request_id` 在它的库里唯一（17137 行 / 17137 个不同值）。tern 的
//! `requests` 表已有 `(provider_id, message_id)` 唯一索引，所以拿 `request_id` 当
//! `message_id` 就能天然去重——重复导入不会翻倍。

use std::path::Path;

use serde::Serialize;

use crate::error::{AppError, Result};
use tern_gateway::ErrorKind;

/// 一次导入的结果。
#[derive(Debug, Serialize, Clone)]
pub struct ImportOutcome {
    /// 库路径，出错时用户看得见找的是哪个文件
    pub db_path: String,
    /// 读到的总行数
    pub total: usize,
    /// 实际写进去的行数（= total - 跳过 - 重复）
    pub imported: usize,
    /// 因 `_session` 口径不同而跳过的
    pub skipped_session: usize,
    /// 因已存在（重复导入）而跳过的
    pub skipped_duplicate: usize,
    /// 有 token 但没查到价格的行数。不为 0 时面板会提示"成本偏低"
    pub unpriced: usize,
    /// 最早 / 最晚一条的时间，Unix 毫秒
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
}

/// 从 cc-switch 的库读一行转换后的中间形态。
///
/// 单独一个结构体而不是直接塞 `UsageEvent`：转换里要判断"这行该不该要"，
/// 而 `UsageEvent` 是所有字段必填的。先读成可选、过滤完再 construction。
#[derive(Debug, Clone)]
struct RawRow {
    request_id: String,
    provider_id: String,
    model: String,
    request_model: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_write: u64,
    status_code: u16,
    error_message: Option<String>,
    session_id: Option<String>,
    is_streaming: bool,
    cost_multiplier: Option<String>,
    created_at_secs: i64,
    /// `input_tokens` 是否不含缓存读。true = Anthropic 原生口径
    input_excludes_cache: bool,
}

/// 从 cc-switch 导入历史用量。
///
/// `db` 是源库（只读打开），`store` 是目标库（写方）。两个库都先由调用方打开，
/// 这里只管搬——这样测试能拿临时库跑，不必碰用户真实的文件。
pub fn import_from_cc_switch(db: &Path, store: &tern_store::Store) -> Result<ImportOutcome> {
    if !db.exists() {
        return Err(AppError::Config(format!(
            "cc-switch 数据库不存在：{}。装了 cc-switch 才会有这个文件",
            db.display()
        )));
    }

    // 只读打开源库。加 no_mutex：单线程顺序读，不需要连接级的互斥
    let source = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| AppError::Config(format!("打不开 {}: {e}", db.display())))?;

    // 先探一下表在不在。库可能是新版 / 旧版的 cc-switch，表名不同的话
    // 给一句"认不出这个库"比一个 SQL 报错有用
    let has_table: bool = source
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='proxy_request_logs'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;
    if !has_table {
        return Err(AppError::Config(format!(
            "{} 里没有 proxy_request_logs 表，认不出这个 cc-switch 库",
            db.display()
        )));
    }

    // 一次读完再写。17136 条 × 每列一个 bind 在这个量级下不值得分页，
    // 而分批写会让"导到一半失败"留下半份历史——那种状态比全失败更难查。
    let mut statement = source
        .prepare(
            "SELECT request_id, provider_id, model, request_model,
                    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                    status_code, error_message, session_id, is_streaming, cost_multiplier,
                    created_at, input_token_semantics
             FROM proxy_request_logs
             ORDER BY created_at ASC",
        )
        .map_err(|e| AppError::Config(format!("读 {} 失败: {e}", db.display())))?;

    let rows = statement
        .query_map([], |row| {
            Ok(RawRow {
                request_id: row.get(0)?,
                provider_id: row.get(1)?,
                model: row.get(2)?,
                request_model: row.get(3)?,
                input_tokens: row.get::<_, i64>(4)?.max(0) as u64,
                output_tokens: row.get::<_, i64>(5)?.max(0) as u64,
                cache_read: row.get::<_, i64>(6)?.max(0) as u64,
                cache_write: row.get::<_, i64>(7)?.max(0) as u64,
                status_code: row.get::<_, i64>(8)?.max(0) as u16,
                error_message: row.get(9)?,
                session_id: row.get(10)?,
                is_streaming: row.get::<_, i64>(11)? != 0,
                cost_multiplier: row.get(12)?,
                created_at_secs: row.get(13)?,
                // 缺省按 Anthropic 原生口径：老数据没这个字段，
                // 而那正是"input_tokens 不含缓存读"的意思
                input_excludes_cache: row.get::<_, i64>(14).unwrap_or(2) == 2,
            })
        })
        .map_err(|e| AppError::Config(format!("逐行读 {} 失败: {e}", db.display())))?;

    let mut outcome = ImportOutcome {
        db_path: db.display().to_string(),
        total: 0,
        imported: 0,
        skipped_session: 0,
        skipped_duplicate: 0,
        unpriced: 0,
        from_ms: None,
        to_ms: None,
    };

    // 供应商倍率表。cc-switch 的 cost_multiplier 在**行上**，tern 的在**供应商上**——
    // 所以先按 provider_id 收一份，写进去之前 set 一次
    let mut multipliers: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // query_map 的 Item 已经是 rusqlite::Result<RawRow>，这里只丢坏行。
    // 坏行通常是某列为 NULL 而 Rust 侧不收——那种行导不进来也不该让整批失败
    let batch: Vec<RawRow> = rows.filter_map(|row| row.ok()).collect();
    outcome.total = batch.len();

    for raw in &batch {
        // 陷阱 1：session_log 口径不同，跳过
        if raw.provider_id == "_session" {
            outcome.skipped_session += 1;
            continue;
        }
        if let Some(multiplier) = raw.cost_multiplier.as_ref() {
            if !multiplier.trim().is_empty() {
                multipliers.insert(raw.provider_id.clone(), multiplier.trim().to_string());
            }
        }
    }

    // 倍率只在**导入期间**生效，导入完还原——它是进程级状态，
    // 留着会让网关后续的真实请求用错倍率
    let multiplier_errors =
        store.set_multipliers(multipliers.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    for message in &multiplier_errors {
        log::warn!("[ccswitch-usage] {message}");
    }

    for raw in &batch {
        if raw.provider_id == "_session" {
            continue;
        }
        // 陷阱 3：秒 -> 毫秒
        let started_at_ms = raw.created_at_secs.saturating_mul(1000);

        // insert 自己会判 (provider_id, message_id) 唯一——重复导入走 Duplicate
        match store.insert(&to_event(raw, started_at_ms)) {
            Ok(tern_store::Inserted::Row { pricing_model, .. }) => {
                outcome.imported += 1;
                if pricing_model.is_none() {
                    outcome.unpriced += 1;
                }
                outcome.from_ms = Some(
                    outcome
                        .from_ms
                        .map_or(started_at_ms, |v| v.min(started_at_ms)),
                );
                outcome.to_ms = Some(
                    outcome
                        .to_ms
                        .map_or(started_at_ms, |v| v.max(started_at_ms)),
                );
            }
            // 幂等的那一分支：同一个 request_id 已经导过了
            Ok(tern_store::Inserted::Duplicate) => outcome.skipped_duplicate += 1,
            // 单行写坏不阻断整批：17136 条里有一条字段异常，不该让前面 17135 条
            // 也跟着回滚。记日志继续走。
            Err(error) => {
                log::warn!("[ccswitch-usage] 跳过 {}: {error}", raw.request_id);
            }
        }
    }

    if !multipliers.is_empty() {
        // 还原成"everything 1"，让网关回到配置里的真实倍率
        store.set_multipliers(std::iter::empty::<(&str, &str)>());
    }

    log::info!(
        "[ccswitch-usage] 导入完成：{} 条读入、{} 条写入、{} 条跳过（口径不同）、{} 条重复",
        outcome.total,
        outcome.imported,
        outcome.skipped_session,
        outcome.skipped_duplicate
    );
    Ok(outcome)
}

/// 一行 cc-switch 的记录 → tern 的 `UsageEvent`。
///
/// `input_excludes_cache` 为真时四个桶直接对应。为假（极少见的老数据）时
/// `input_tokens` **已经含**缓存读，直接搬会把缓存读算两遍——所以这里把它从
/// 输入里扣掉，只留真正的新输入。扣到负数按 0 算，那种行本身就是坏的。
fn to_event(raw: &RawRow, started_at_ms: i64) -> tern_gateway::UsageEvent {
    use tern_gateway::{ClientKind, Outcome, RequestRole, RouteKind, TokenCounts, UsageEvent};

    let (client, error_kind) = match (raw.status_code, &raw.error_message) {
        (code, _) if (200..300).contains(&code) => (ClientKind::Claude, None),
        (401 | 403, _) => (ClientKind::Claude, Some(ErrorKind::Auth)),
        (429, _) => (ClientKind::Claude, Some(ErrorKind::RateLimited)),
        (408 | 504, _) => (ClientKind::Claude, Some(ErrorKind::Timeout)),
        (code, Some(message)) => (ClientKind::Claude, Some(classify_upstream(code, message))),
        (code, None) => (ClientKind::Claude, Some(classify_status(code))),
    };

    let failed = !(200..300).contains(&raw.status_code);

    // 口径归一。semantics != 2 的老数据里 input_tokens 含缓存读，
    // 不减掉就会让"新增输入"虚高、缓存命中率偏低——两个都是用户判断
    // "缓存帮我省了多少"的依据，错了很难发现。
    let fresh_input = if raw.input_excludes_cache {
        raw.input_tokens
    } else {
        raw.input_tokens.saturating_sub(raw.cache_read)
    };
    let has_usage = fresh_input + raw.output_tokens + raw.cache_read + raw.cache_write > 0;

    UsageEvent {
        started_at_ms,
        client,
        endpoint: "/v1/messages".into(),
        // cc-switch 的 provider_id 是 UUID，而 tern 导入时保留了同一套 id
        // （实测 38 个完全对得上），所以直接搬即可归属到正确的供应商
        provider_id: Some(raw.provider_id.clone()),
        route_kind: Some(RouteKind::Fallback),
        // pricing_model 留给 store 自己挑：它优先用 response_model → upstream_model
        client_model: if raw.request_model.trim().is_empty() {
            raw.model.clone()
        } else {
            raw.request_model.clone()
        },
        upstream_model: None,
        // cc-switch 的 model 列是上游回显的，相当于 response_model
        response_model: Some(raw.model.clone()),
        role: RequestRole::Main,
        session_id: raw
            .session_id
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        stream: raw.is_streaming,
        status: raw.status_code,
        outcome: if failed {
            Outcome::Failed
        } else {
            Outcome::Success
        },
        error_kind,
        error_message: raw.error_message.clone(),
        tokens: has_usage.then_some(TokenCounts {
            fresh_input,
            output: raw.output_tokens,
            cache_read: raw.cache_read,
            cache_write: raw.cache_write,
        }),
        // message_id 用 request_id：tern 的 (provider_id, message_id) 唯一索引
        // 靠它做幂等，重复导入不会把历史翻倍
        message_id: Some(raw.request_id.clone()),
        first_token_ms: None,
        duration_ms: 0,
    }
}

fn classify_upstream(code: u16, message: &str) -> ErrorKind {
    let lower = message.to_lowercase();
    if lower.contains("rate limit") || lower.contains("429") {
        ErrorKind::RateLimited
    } else if lower.contains("timeout") || lower.contains("timed out") {
        ErrorKind::Timeout
    } else if lower.contains("insufficient") || lower.contains("quota") || lower.contains("balance")
    {
        ErrorKind::RateLimited
    } else if code >= 500 {
        ErrorKind::UpstreamServer
    } else {
        ErrorKind::UpstreamRejected
    }
}

fn classify_status(code: u16) -> ErrorKind {
    match code {
        0 => ErrorKind::Connection,
        408 | 504 => ErrorKind::Timeout,
        code if code >= 500 => ErrorKind::UpstreamServer,
        _ => ErrorKind::UpstreamRejected,
    }
}

// ---------------------------------------------------------------------------
// tauri 命令
// ---------------------------------------------------------------------------

/// 默认的 cc-switch 库位置。与 `ccswitch_import` 同一口径。
fn default_db() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".cc-switch").join("cc-switch.db"))
        .unwrap_or_else(|| std::path::PathBuf::from(".cc-switch").join("cc-switch.db"))
}

/// 先看一眼会导入什么，不写盘。
///
/// 和 [`import_cc_switch_usage`] 分开：导入是**覆盖用量库**级别的操作，
/// 用户得先看见"会搬多少条、覆盖哪段日期"再决定。这个函数只读源库。
///
/// # 为什么是 async
///
/// 同步的 tauri 命令跑在主线程，而 WebView 的 IPC 也在主线程——跑起来
/// **整个界面冻结**。这里要开一个 11 MB 的库、跑两次全表扫描加一次聚合，
/// 实测约 10 ms，但那是空载读数；库里攒到几万条、或磁盘在喘的时候，
/// 几百毫秒到几秒都是可能的。用户什么都不点，界面先死一会儿。
///
/// 丢进 `spawn_blocking` 之后它跑在线程池，主线程该干嘛干嘛。
#[tauri::command]
pub async fn cc_switch_usage_preview() -> Result<ImportOutcome> {
    tauri::async_runtime::spawn_blocking(preview_from_cc_switch)
        .await
        .map_err(|e| AppError::Config(format!("任务失败: {e}")))?
}

/// 探测的本体。拆出来是为了能单测——`spawn_blocking` 的闭包拿不到测试夹具。
fn preview_from_cc_switch() -> Result<ImportOutcome> {
    let db = default_db();
    preview_at(&db)
}

/// 在指定路径上探测。和 [`preview_from_cc_switch`] 分开只为了让测试能注入路径——
/// 真跑的时候走 `~/.cc-switch/cc-switch.db`，那个路径在测试里不可控。
fn preview_at(db: &std::path::Path) -> Result<ImportOutcome> {
    if !db.exists() {
        return Err(AppError::Config(format!(
            "cc-switch 数据库不存在：{}。装了 cc-switch 才会有这个文件",
            db.display()
        )));
    }
    let source = rusqlite::Connection::open_with_flags(
        &db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| AppError::Config(format!("打不开 {}: {e}", db.display())))?;

    let totals = |sql: &str| -> i64 {
        source
            .query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap_or(0)
    };

    let total = totals("SELECT COUNT(*) FROM proxy_request_logs");
    // 口径不同的那些（session_log 反推）不该算进"将导入"
    let skipped = totals("SELECT COUNT(*) FROM proxy_request_logs WHERE provider_id = '_session'");
    let range: (Option<i64>, Option<i64>) = source
        .query_row(
            "SELECT MIN(created_at), MAX(created_at) FROM proxy_request_logs WHERE provider_id != '_session'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((None, None));

    Ok(ImportOutcome {
        db_path: db.display().to_string(),
        total: total.max(0) as usize,
        imported: (total - skipped).max(0) as usize,
        skipped_session: skipped.max(0) as usize,
        skipped_duplicate: 0,
        unpriced: 0,
        // 秒 -> 毫秒，和导入函数同一口径
        from_ms: range.0.map(|v| v.saturating_mul(1000)),
        to_ms: range.1.map(|v| v.saturating_mul(1000)),
    })
}

/// 真正导入。
///
/// 走内嵌网关那份 `Store`（`AppState::shared_store`）：它是写方，
/// 和记账用的同一份，口径不会分叉。
///
/// # 为什么是 async
///
/// 要搬一万七千条，还要逐条重算成本。同步命令跑起来主线程冻结，
/// 用户会以为面板卡死了。`spawn_blocking` 让它跑在线程池。
#[tauri::command]
pub async fn cc_switch_usage_import(
    state: tauri::State<'_, crate::AppState>,
) -> Result<ImportOutcome> {
    // Store 是 Arc，克隆一份进闭包；tauri 的 State 借引用不能跨 await
    let store = state.shared_store().ok_or_else(|| {
        AppError::Config("网关还没启动过，没有可写入的用量库。先启动网关再导入。".into())
    })?;
    tauri::async_runtime::spawn_blocking(move || import_from_cc_switch(&default_db(), &store))
        .await
        .map_err(|e| AppError::Config(format!("任务失败: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探测要把 `_session` 那些剔掉。它们是从 Claude Code 会话日志反推的，
    /// token 口径和代理请求不同，混进"将导入 N 条"会让数字虚高
    #[test]
    fn preview_excludes_session_rows() {
        let (_dir, path) = source_db();
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            let sql = "INSERT INTO proxy_request_logs VALUES (%s)";
            let placeholders = vec!["?"; 27].join(",");
            let sql = sql.replace("%s", &placeholders);
            for (id, provider) in [("r1", "deepseek"), ("r2", "_session"), ("r3", "_session")] {
                conn.execute(
                    &sql,
                    rusqlite::params_from_iter(row(id, provider, (10, 5, 0, 0), 1_700_000_000)),
                )
                .unwrap();
            }
        }
        let outcome = preview_at(&path).unwrap();
        assert_eq!(outcome.total, 3, "库里总共 3 条");
        assert_eq!(outcome.imported, 1, "只有 1 条算得上可导入");
        assert_eq!(outcome.skipped_session, 2);
    }

    /// 日期范围按**秒**转毫秒。cc-switch 的 created_at 是 Unix 秒，
    /// tern 用毫秒，差 1000 倍。不转会全部落到 1970 年
    #[test]
    fn preview_converts_seconds_to_milliseconds() {
        let (_dir, path) = source_db();
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            let sql = "INSERT INTO proxy_request_logs VALUES (%s)"
                .replace("%s", &vec!["?"; 27].join(","));
            conn.execute(
                &sql,
                rusqlite::params_from_iter(row("r1", "deepseek", (10, 5, 0, 0), 1_700_000_000)),
            )
            .unwrap();
        }
        let outcome = preview_at(&path).unwrap();
        assert_eq!(outcome.from_ms, Some(1_700_000_000_000));
        assert_eq!(outcome.to_ms, Some(1_700_000_000_000));
    }

    /// 库不在时要报错，不能返回一个"0 条"的假结果。
    /// 那会让前端显示"没有可导的"，用户以为自己的历史用量已经齐了
    #[test]
    fn preview_reports_a_missing_db_rather_than_claiming_nothing_to_import() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        let error = preview_at(&missing).unwrap_err().to_string();
        assert!(error.contains("不存在"), "{error}");
    }

    /// 造一个只读的 cc-switch 风格源库。
    fn source_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cc-switch.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE proxy_request_logs (
                request_id TEXT, provider_id TEXT, app_type TEXT, model TEXT,
                input_tokens INTEGER, output_tokens INTEGER,
                cache_read_tokens INTEGER, cache_creation_tokens INTEGER,
                input_cost_usd TEXT, output_cost_usd TEXT, cache_read_cost_usd TEXT,
                cache_creation_cost_usd TEXT, total_cost_usd TEXT,
                latency_ms INTEGER, first_token_ms INTEGER, duration_ms INTEGER,
                status_code INTEGER, error_message TEXT, session_id TEXT,
                provider_type TEXT, is_streaming INTEGER, cost_multiplier TEXT,
                created_at INTEGER, request_model TEXT, data_source TEXT,
                pricing_model TEXT, input_token_semantics INTEGER
            )",
        )
        .unwrap();
        (dir, path)
    }

    /// 一行正常数据。按 `proxy_request_logs` 的列顺序给值，
    /// 用 `rusqlite::params!` 而不是手拼 `Vec<Value>`——那个对 `&str`
    /// 没有 From 实现，逐个 to_string() 会让这个夹具看不出在测什么。
    ///
    /// `overrides` 让个别测试改单个字段（比如把 status 改成 429）。
    fn row(id: &str, provider: &str, tokens: (u64, u64, u64, u64), at: i64) -> Vec<String> {
        vec![
            id.into(),                // request_id
            provider.into(),          // provider_id
            "claude".into(),          // app_type
            "claude-opus-4-8".into(), // model（上游回显）
            tokens.0.to_string(),     // input_tokens
            tokens.1.to_string(),     // output_tokens
            tokens.2.to_string(),     // cache_read
            tokens.3.to_string(),     // cache_creation
            "0".into(),               // input_cost
            "0".into(),               // output_cost
            "0".into(),               // cache_read_cost
            "0".into(),               // cache_creation_cost
            "0.01".into(),            // total_cost_usd
            "100".into(),             // 13 latency_ms
            String::new(),            // 14 first_token_ms（NULL）
            "200".into(),             // 15 duration_ms
            "200".into(),             // 16 status_code
            String::new(),            // 17 error_message（NULL）
            "sess-1".into(),          // 18 session_id
            "proxy".into(),           // 19 provider_type
            "1".into(),               // 20 is_streaming
            "1.0".into(),             // 21 cost_multiplier
            at.to_string(),           // 22 created_at（秒）
            "claude-opus-4-8".into(), // 23 request_model
            "proxy".into(),           // 24 data_source
            String::new(),            // 25 pricing_model（NULL）
            "2".into(),               // 26 input_token_semantics
        ]
    }

    /// 29 列，和 `row()` 的长度必须一致。
    /// 列数对不上的话 insert 会报 "table has 29 columns but N values supplied"，
    /// 而这个常量让失败信息指到名字而不是数量。
    const COLUMN_COUNT: usize = 27;

    fn insert(conn: &rusqlite::Connection, values: &[String]) {
        assert_eq!(values.len(), COLUMN_COUNT, "夹具列数对不上");
        let marks = vec!["?"; values.len()].join(",");
        conn.execute(
            &format!("INSERT INTO proxy_request_logs VALUES ({marks})"),
            rusqlite::params_from_iter(values.iter()),
        )
        .unwrap();
    }

    /// 改单个字段的帮手：测试里要把 status 换成 429、data_source 换成
    /// session_log 之类。返回新 Vec，不动原值。
    fn with(values: &[String], index: usize, value: &str) -> Vec<String> {
        let mut out = values.to_vec();
        out[index] = value.into();
        out
    }

    /// **陷阱 1**：`_session` 行必须跳过。
    ///
    /// 它的 `input_tokens` 是"未命中缓存的新输入"，和 proxy 行的 Anthropic 口径
    /// 不是一回事。混进去求"新增输入"会偏低——而那是用户判断"缓存帮我省了多少"的
    /// 依据，错了很难发现。
    #[test]
    fn session_rows_are_skipped() {
        let (_g, path) = source_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        insert(&conn, &row("r1", "p1", (100, 50, 0, 0), 1_700_000_000));
        // session_log 反推的行：provider 固定是 _session，24 列 data_source 标明来源
        let session_row = with(
            &row("r2", "_session", (9999, 9999, 0, 0), 1_700_000_001),
            24,
            "session_log",
        );
        insert(&conn, &session_row);
        drop(conn);

        let store = tern_store::Store::open_in_memory().unwrap();
        let outcome = import_from_cc_switch(&path, &store).unwrap();

        assert_eq!(outcome.total, 2);
        assert_eq!(outcome.skipped_session, 1);
        assert_eq!(outcome.imported, 1, "只有 proxy 那行该进来");
        // 那 9999 token 不该出现在任何统计里
        let summary = store
            .summary(&tern_store::DayRange::last_days(3650))
            .unwrap();
        assert_eq!(summary.fresh_input, 100, "_session 的 token 不该被计入");
    }

    /// **陷阱 2**：四个桶直接对应，**不许把 cache_read 加进 input**。
    ///
    /// `input_token_semantics = 2` 的意思就是"input_tokens 不含缓存读"，
    /// 和 tern 的 `fresh_input` 同义。相加等于把缓存读算两遍，
    /// "新增输入"会虚高，缓存命中率反而偏低。
    #[test]
    fn tokens_map_one_to_one_without_double_counting() {
        let (_g, path) = source_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        insert(
            &conn,
            &row("r1", "p1", (1000, 200, 8000, 500), 1_700_000_000),
        );
        drop(conn);

        let store = tern_store::Store::open_in_memory().unwrap();
        import_from_cc_switch(&path, &store).unwrap();
        let summary = store
            .summary(&tern_store::DayRange::last_days(3650))
            .unwrap();

        assert_eq!(summary.fresh_input, 1000, "input_tokens 就是 fresh_input");
        assert_eq!(summary.output, 200);
        assert_eq!(
            summary.cache_read, 8000,
            "cache_read 单独一桶，不并进 input"
        );
        assert_eq!(summary.cache_write, 500);
        // 四个桶相加即是全部输入。若相加了就会是 1000+8000=9000
        assert_eq!(
            summary.fresh_input + summary.cache_read + summary.cache_write,
            9500,
            "输入总量不该把缓存读算两遍"
        );
    }

    /// **陷阱 3**：`created_at` 是秒，必须 ×1000。
    ///
    /// 不转会全部落到 1970 年，趋势图上表现为"所有数据挤在最左边一天"
    /// 或者干脆查不到——因为 DayRange 是闭区间的近期日期。
    #[test]
    fn timestamps_are_converted_from_seconds_to_millis() {
        let (_g, path) = source_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        insert(&conn, &row("r1", "p1", (10, 5, 0, 0), 1_700_000_000));
        drop(conn);

        let store = tern_store::Store::open_in_memory().unwrap();
        let outcome = import_from_cc_switch(&path, &store).unwrap();
        // 1_700_000_000 秒 ≈ 2023-11-14，转成毫秒就是原样 ×1000
        assert_eq!(outcome.from_ms, Some(1_700_000_000_000));

        let recent = store.recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].started_at_ms, 1_700_000_000_000);
    }

    /// **陷阱 4 + 幂等**：成本按 tern 的价格表重算，且重复导入不翻倍。
    ///
    /// cc-switch 只有 90/17136 条有 `total_cost_usd`。搬它的话九成历史是 0 元，
    /// 趋势图一条平线。所以这里忽略源库的成本列，让 `store.insert` 自己算。
    #[test]
    fn cost_is_recomputed_and_reimport_does_not_duplicate() {
        let (_g, path) = source_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        // claude-opus-4-8 在 tern 的内置价格表里：5 / 25 / 0.5 / 6.25 每百万
        insert(
            &conn,
            &row("req-1", "p1", (1_000_000, 1_000_000, 0, 0), 1_700_000_000),
        );
        drop(conn);

        let store = tern_store::Store::open_in_memory().unwrap();
        let first = import_from_cc_switch(&path, &store).unwrap();
        assert_eq!(first.imported, 1);
        // 1M 输入 × $5/M + 1M 输出 × $25/M = $30
        let summary = store
            .summary(&tern_store::DayRange::last_days(3650))
            .unwrap();
        assert_eq!(summary.cost.to_string(), "30", "成本应由 tern 的价格表算出");

        // 再导一次：request_id 相同，唯一索引挡住
        let second = import_from_cc_switch(&path, &store).unwrap();
        assert_eq!(second.imported, 0, "重复导入不该写入");
        assert_eq!(second.skipped_duplicate, 1);
        let again = store
            .summary(&tern_store::DayRange::last_days(3650))
            .unwrap();
        assert_eq!(again.cost.to_string(), "30", "重复导入不能把成本翻倍");
        assert_eq!(again.requests, 1);
    }

    /// 失败行也要导，但算成 failed。
    ///
    /// 用户看"这个月失败了多少次"和成功次数一样重要——429 撞限流这种，
    /// 只有把失败也搬过来才看得出那家稳不稳。
    #[test]
    fn failures_are_imported_as_failures() {
        let (_g, path) = source_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        // 16 = status_code，17 = error_message
        let failed = with(&row("r1", "p1", (0, 0, 0, 0), 1_700_000_000), 16, "429");
        let failed = with(&failed, 17, "rate limited");
        insert(&conn, &failed);
        drop(conn);

        let store = tern_store::Store::open_in_memory().unwrap();
        import_from_cc_switch(&path, &store).unwrap();
        let summary = store
            .summary(&tern_store::DayRange::last_days(3650))
            .unwrap();
        assert_eq!(summary.requests, 1);
        assert_eq!(summary.failures, 1);

        let groups = store
            .failures(&tern_store::DayRange::last_days(3650))
            .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].error_kind, "rate_limited");
    }

    /// session_id 空串按 None 处理。
    ///
    /// cc-switch 里有空串的 session_id，塞进去会让会话视图多出一个叫 "" 的会话。
    #[test]
    fn a_blank_session_id_becomes_none() {
        let (_g, path) = source_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        // 18 = session_id。给空白，看它会不会造出一个叫 "" 的会话
        insert(
            &conn,
            &with(&row("r1", "p1", (10, 5, 0, 0), 1_700_000_000), 18, "   "),
        );
        drop(conn);

        let store = tern_store::Store::open_in_memory().unwrap();
        import_from_cc_switch(&path, &store).unwrap();
        // 空 session 不进 sessions 视图
        let sessions = store
            .sessions(&tern_store::DayRange::last_days(3650), 10)
            .unwrap();
        assert!(sessions.is_empty(), "空 session_id 不该造出会话");
    }

    /// **老数据口径**：`input_token_semantics != 2` 时 `input_tokens` **含**缓存读。
    ///
    /// 不减掉就把缓存读算两遍——"新增输入"虚高、缓存命中率偏低，两个都是用户
    /// 判断"缓存帮我省了多少"的依据。真实库里 17000 条全是 semantics=2，
    /// 但 cc-switch 早期版本不是，导入器得对两种情况都对。
    #[test]
    fn legacy_rows_subtract_cache_read_from_input() {
        let (_g, path) = source_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        // 26 列 input_token_semantics 置 0（老口径：input 含缓存读）
        let legacy = with(
            &row("r1", "p1", (10_000, 200, 8_000, 0), 1_700_000_000),
            26,
            "0",
        );
        insert(&conn, &legacy);
        drop(conn);

        let store = tern_store::Store::open_in_memory().unwrap();
        import_from_cc_switch(&path, &store).unwrap();
        let summary = store
            .summary(&tern_store::DayRange::last_days(3650))
            .unwrap();

        assert_eq!(
            summary.fresh_input, 2_000,
            "10000 含 8000 缓存读，新输入只有 2000"
        );
        assert_eq!(summary.cache_read, 8_000, "缓存读那一桶照搬");
    }

    /// 扣成负数按 0 算。坏行（input 比 cache_read 还小）不该让导入失败，
    /// 也不该产出负数 token。
    #[test]
    fn a_nonsensical_legacy_row_clamps_to_zero() {
        let (_g, path) = source_db();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let broken = with(&row("r1", "p1", (100, 0, 9_000, 0), 1_700_000_000), 26, "0");
        insert(&conn, &broken);
        drop(conn);

        let store = tern_store::Store::open_in_memory().unwrap();
        import_from_cc_switch(&path, &store).unwrap();
        let summary = store
            .summary(&tern_store::DayRange::last_days(3650))
            .unwrap();
        assert_eq!(summary.fresh_input, 0, "不能出现负 token");
    }

    /// 库不存在时给一句"装了 cc-switch 才会有"，不是 SQL 报错。
    #[test]
    fn a_missing_db_says_so_in_words() {
        let dir = tempfile::tempdir().unwrap();
        let store = tern_store::Store::open_in_memory().unwrap();
        let error = import_from_cc_switch(&dir.path().join("nope.db"), &store)
            .unwrap_err()
            .to_string();
        assert!(error.contains("不存在"), "{error}");
    }

    /// 表不认识时也说清楚，别抛一个 SQL 错。
    #[test]
    fn an_unrecognised_db_says_so_in_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.db");
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE something_else (x INTEGER)")
            .unwrap();

        let store = tern_store::Store::open_in_memory().unwrap();
        let error = import_from_cc_switch(&path, &store)
            .unwrap_err()
            .to_string();
        assert!(error.contains("proxy_request_logs"), "{error}");
    }
}
