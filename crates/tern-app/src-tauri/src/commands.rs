//! 给前端的查询命令。
//!
//! SQL 口径与 `tern-store::query` 逐字对齐，这样面板和 `tern usage` 命令行
//! 永远对得上。凡是上百万行的明细都走 `daily` 预聚合表，不把明细拉进前端。

use serde::Serialize;
use tauri::State;

use crate::error::Result;
use crate::AppState;

/// 与 `tern_store::query::Summary` 同构，成本给字符串避免 JS 的 float 误差
/// （纳美元是 i64，转 f64 再过 JSON 会在小数位上丢数）
#[derive(Debug, Serialize)]
pub struct SummaryDto {
    pub requests: u64,
    pub failures: u64,
    pub aborted: u64,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: String,
    pub cache_savings: String,
    pub unpriced: u64,
}

/// 面板首屏需要的全部数字。按 cc-switch 使用统计页的密度做：
/// 一个 hero（Token 总量）+ 一组小卡 + 两条醒目提示。
#[derive(Debug, Serialize)]
pub struct PanelDto {
    /// 数据库文件路径，出错时前端可以直接展示
    pub db_path: String,
    /// 今天没有数据时为 true，前端显示引导而不是 0
    pub first_run: bool,
    pub today: SummaryDto,
    /// 昨天同口径，"较昨日"对比用
    pub yesterday: SummaryDto,
    /// 未定价模型：有 token 却没查到价，成本图因此偏低
    pub unpriced_models: Vec<UnpricedDto>,
    /// 失败按（原因, 供应商, 状态码）聚类，不混进模型统计
    pub failures: Vec<FailureDto>,
    /// 最近若干条，给"请求流"折叠区用
    pub recent: Vec<RecentDto>,
}

#[derive(Debug, Serialize)]
pub struct UnpricedDto {
    pub model: String,
    pub requests: u64,
    pub tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct FailureDto {
    pub error_kind: String,
    pub provider_id: Option<String>,
    pub status: u16,
    pub count: u64,
    pub sample: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecentDto {
    pub started_at_ms: i64,
    pub client: String,
    pub provider_id: Option<String>,
    /// 客户端原始模型名
    pub client_model: String,
    /// 上游回显的模型名，被供应商换过模型时和 client_model 不同
    pub response_model: Option<String>,
    pub role: String,
    pub status: u16,
    pub outcome: String,
    pub error_kind: Option<String>,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: Option<String>,
    pub duration_ms: u64,
}

/// `Summary` 的十个聚合列，与 tern-store 的 SUMMARY_COLUMNS 一致
const SUMMARY_COLUMNS: &str = "COALESCE(SUM(requests), 0), COALESCE(SUM(failures), 0),
    COALESCE(SUM(aborted), 0), COALESCE(SUM(fresh_input), 0), COALESCE(SUM(output), 0),
    COALESCE(SUM(cache_read), 0), COALESCE(SUM(cache_write), 0), COALESCE(SUM(cost_nano), 0),
    COALESCE(SUM(savings_nano), 0), COALESCE(SUM(unpriced), 0)";

/// 纳美元 → 十进制字符串。与 `tern_store::query::nano_to_usd` 保持同样精度。
fn nano_to_usd_string(nano: i64) -> String {
    // 手写而不是用 Decimal：这里只需要显示，且不想让面板依赖 rust_decimal 的版本
    let sign = if nano < 0 { "-" } else { "" };
    let nano = nano.unsigned_abs();
    format!("{}{}.{:09}", sign, nano / 1_000_000_000, nano % 1_000_000_000)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

// hero 的 token 总量、缓存命中率由前端自己算（Dto 只搬原始桶），
// 不在 Rust 侧留一份同逻辑的两处实现。

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryDto> {
    let u = |i: usize| -> rusqlite::Result<u64> {
        Ok(u64::try_from(row.get::<_, i64>(i)?).unwrap_or(0))
    };
    Ok(SummaryDto {
        requests: u(0)?,
        failures: u(1)?,
        aborted: u(2)?,
        fresh_input: u(3)?,
        output: u(4)?,
        cache_read: u(5)?,
        cache_write: u(6)?,
        cost: nano_to_usd_string(row.get(7)?),
        cache_savings: nano_to_usd_string(row.get(8)?),
        unpriced: u(9)?,
    })
}

/// 本地日期 `YYYY-MM-DD`，与写入时算 `day` 列的 `tern_store::local_day` 同一时区口径
fn local_days_ago(days: i64) -> String {
    (chrono::Local::now().date_naive() - chrono::Days::new(u64::try_from(days).unwrap_or(0)))
        .format("%Y-%m-%d")
        .to_string()
}

/// 打开数据库并返回首屏全部数据。库不存在时返回错误，前端据此显示引导。
#[tauri::command]
pub fn open_db(state: State<'_, AppState>) -> Result<String> {
    let path = state.db_path();
    state.ensure_open()?;
    Ok(path)
}

#[tauri::command]
pub fn panel_summary(state: State<'_, AppState>) -> Result<PanelDto> {
    state.with_db(|db| {
        db.with_conn(|conn| build_panel(conn, &state.db_path()))
    })
}

/// 面板首屏的全部数据。抽成不依赖 tauri 的普通函数，测试可以直接调用。
pub fn build_panel(conn: &rusqlite::Connection, db_path: &str) -> Result<PanelDto> {
    {
        let today = local_days_ago(0);
        let yesterday = local_days_ago(1);

        let summary = |day: &str| -> rusqlite::Result<SummaryDto> {
            conn.query_row(
                &format!("SELECT {SUMMARY_COLUMNS} FROM daily WHERE day = ?1"),
                [day],
                summary_from_row,
            )
        };
        let today_summary = summary(&today)?;
        let yesterday_summary = summary(&yesterday)?;
        // 有数据才谈"今日"，否则前端显示首次引导
        let first_run = conn.query_row::<i64, _, _>(
            "SELECT COALESCE(SUM(requests), 0) FROM daily",
            [],
            |row| row.get(0),
        )? == 0;

        let unpriced_models = {
            let mut stmt = conn.prepare(
                "SELECT COALESCE(response_model, upstream_model, client_model) AS model,
                        COUNT(*), SUM(fresh_input + output + cache_read + cache_write)
                 FROM requests
                 WHERE day = ?1 AND has_usage = 1 AND cost_nano IS NULL
                 GROUP BY model ORDER BY 3 DESC LIMIT 8",
            )?;
            let rows = stmt.query_map([&today], |row| {
                Ok(UnpricedDto {
                    model: row.get(0)?,
                    requests: u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                    tokens: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let failures = {
            let mut stmt = conn.prepare(
                // 与 tern-store 的 failures() 相同：按（原因, 供应商, 状态码）聚类，
                // 取每组最近一条错误摘要做样本
                "SELECT COALESCE(error_kind, 'unknown'), provider_id, status, COUNT(*),
                        (SELECT r2.error_message FROM requests r2
                         WHERE r2.day = ?1 AND r2.outcome = 'failed'
                           AND COALESCE(r2.error_kind, 'unknown') = COALESCE(r.error_kind, 'unknown')
                           AND r2.provider_id IS r.provider_id AND r2.status = r.status
                         ORDER BY r2.started_at DESC LIMIT 1)
                 FROM requests r
                 WHERE day = ?1 AND outcome = 'failed'
                 GROUP BY 1, 2, 3 ORDER BY 4 DESC LIMIT 6",
            )?;
            let rows = stmt.query_map([&today], |row| {
                Ok(FailureDto {
                    error_kind: row.get(0)?,
                    provider_id: row.get(1)?,
                    status: u16::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
                    count: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                    sample: row.get(4)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let recent = {
            let mut stmt = conn.prepare(
                "SELECT started_at, client, provider_id, client_model, response_model,
                        role, status, outcome, error_kind,
                        fresh_input, output, cache_read, cache_write, cost_nano, duration_ms
                 FROM requests ORDER BY started_at DESC, id DESC LIMIT 12",
            )?;
            let u = |row: &rusqlite::Row<'_>, i: usize| -> rusqlite::Result<u64> {
                Ok(u64::try_from(row.get::<_, i64>(i)?).unwrap_or(0))
            };
            let rows = stmt.query_map([], |row| {
                Ok(RecentDto {
                    started_at_ms: row.get(0)?,
                    client: row.get(1)?,
                    provider_id: row.get(2)?,
                    client_model: row.get(3)?,
                    response_model: row.get(4)?,
                    role: row.get(5)?,
                    status: u16::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
                    outcome: row.get(7)?,
                    error_kind: row.get(8)?,
                    fresh_input: u(row, 9)?,
                    output: u(row, 10)?,
                    cache_read: u(row, 11)?,
                    cache_write: u(row, 12)?,
                    cost: row.get::<_, Option<i64>>(13)?.map(nano_to_usd_string),
                    duration_ms: u(row, 14)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        Ok(PanelDto {
            db_path: db_path.to_string(),
            first_run,
            today: today_summary,
            yesterday: yesterday_summary,
            unpriced_models,
            failures,
            recent,
        })
    }
}
