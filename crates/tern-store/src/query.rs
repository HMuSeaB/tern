//! 聚合查询。趋势 / 占比走 `daily` 预聚合表；失败聚类、未定价、最近请求查明细。

use rusqlite::{params, Row};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::{local_day, Result, Store};

/// 闭区间日期范围（本地日期 `YYYY-MM-DD`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRange {
    pub from: String,
    pub to: String,
}

impl DayRange {
    /// 截至今天的最近 `days` 天（含今天）
    pub fn last_days(days: u32) -> Self {
        let today = chrono::Local::now().date_naive();
        let from = today - chrono::Days::new(u64::from(days.max(1) - 1));
        Self {
            from: from.format("%Y-%m-%d").to_string(),
            to: today.format("%Y-%m-%d").to_string(),
        }
    }

    pub fn today() -> Self {
        Self::last_days(1)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub requests: u64,
    pub failures: u64,
    pub aborted: u64,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost: Decimal,
    pub cache_savings: Decimal,
    /// 有 token 但没定价的请求数
    pub unpriced: u64,
}

impl Summary {
    pub fn total_tokens(&self) -> u64 {
        self.fresh_input + self.output + self.cache_read + self.cache_write
    }

    /// 缓存读占全部输入的比例；没有输入时为空
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let input = self.fresh_input + self.cache_read + self.cache_write;
        (input > 0).then(|| self.cache_read as f64 / input as f64)
    }
}

/// 按哪个维度拆分
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakdown {
    Provider,
    Model,
    Role,
    Client,
    Day,
}

impl Breakdown {
    fn column(self) -> &'static str {
        match self {
            Breakdown::Provider => "provider_id",
            Breakdown::Model => "model",
            Breakdown::Role => "role",
            Breakdown::Client => "client",
            Breakdown::Day => "day",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BreakdownRow {
    pub key: String,
    pub summary: Summary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnpricedModel {
    pub model: String,
    pub requests: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FailureGroup {
    pub error_kind: String,
    pub provider_id: Option<String>,
    pub status: u16,
    pub count: u64,
    /// 最近一次的错误摘要
    pub sample: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecentRequest {
    pub started_at_ms: i64,
    pub client: String,
    pub provider_id: Option<String>,
    pub client_model: String,
    pub upstream_model: Option<String>,
    pub response_model: Option<String>,
    pub role: String,
    pub status: u16,
    pub outcome: String,
    pub error_kind: Option<String>,
    pub fresh_input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// 未定价为空
    pub cost: Option<Decimal>,
    pub duration_ms: u64,
}

const SUMMARY_COLUMNS: &str = "COALESCE(SUM(requests), 0), COALESCE(SUM(failures), 0),
    COALESCE(SUM(aborted), 0), COALESCE(SUM(fresh_input), 0), COALESCE(SUM(output), 0),
    COALESCE(SUM(cache_read), 0), COALESCE(SUM(cache_write), 0), COALESCE(SUM(cost_nano), 0),
    COALESCE(SUM(savings_nano), 0), COALESCE(SUM(unpriced), 0)";

fn summary_from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<Summary> {
    let u = |i: usize| -> rusqlite::Result<u64> {
        Ok(u64::try_from(row.get::<_, i64>(offset + i)?).unwrap_or(0))
    };
    Ok(Summary {
        requests: u(0)?,
        failures: u(1)?,
        aborted: u(2)?,
        fresh_input: u(3)?,
        output: u(4)?,
        cache_read: u(5)?,
        cache_write: u(6)?,
        cost: nano_to_usd(row.get(offset + 7)?),
        cache_savings: nano_to_usd(row.get(offset + 8)?),
        unpriced: u(9)?,
    })
}

pub(crate) fn nano_to_usd(nano: i64) -> Decimal {
    Decimal::new(nano, 9).normalize()
}

impl Store {
    pub fn summary(&self, range: &DayRange) -> Result<Summary> {
        let conn = self.conn();
        let sql = format!("SELECT {SUMMARY_COLUMNS} FROM daily WHERE day BETWEEN ?1 AND ?2");
        Ok(conn.query_row(&sql, params![range.from, range.to], |row| {
            summary_from_row(row, 0)
        })?)
    }

    /// 按维度拆分，花费高的在前（花费相同按请求数）
    pub fn breakdown(&self, range: &DayRange, by: Breakdown) -> Result<Vec<BreakdownRow>> {
        let conn = self.conn();
        let column = by.column();
        let order = if by == Breakdown::Day {
            "key ASC"
        } else {
            "SUM(cost_nano) DESC, SUM(requests) DESC, key ASC"
        };
        let sql = format!(
            "SELECT {column} AS key, {SUMMARY_COLUMNS} FROM daily
             WHERE day BETWEEN ?1 AND ?2 GROUP BY key ORDER BY {order}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![range.from, range.to], |row| {
            Ok(BreakdownRow {
                key: row.get(0)?,
                summary: summary_from_row(row, 1)?,
            })
        })?;
        let rows: Vec<BreakdownRow> = rows.collect::<rusqlite::Result<_>>()?;
        if by != Breakdown::Model {
            return Ok(rows);
        }
        // 模型分布只算拿到响应的请求：失败行没有响应模型，按请求模型分组会显示成
        // "某模型 N 次 0 token"，看起来像被偷换了模型。失败另由 `failures` 聚类呈现。
        Ok(rows
            .into_iter()
            .filter_map(|mut row| {
                row.summary.requests -= row.summary.failures;
                row.summary.failures = 0;
                (row.summary.requests > 0).then_some(row)
            })
            .collect())
    }

    /// 有 token 却没查到价格的模型，按 token 量排序
    pub fn unpriced_models(&self, range: &DayRange) -> Result<Vec<UnpricedModel>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(response_model, upstream_model, client_model) AS model,
                    COUNT(*), SUM(fresh_input + output + cache_read + cache_write) AS tokens
             FROM requests
             WHERE day BETWEEN ?1 AND ?2 AND has_usage = 1 AND cost_nano IS NULL
             GROUP BY model ORDER BY tokens DESC",
        )?;
        let rows = stmt.query_map(params![range.from, range.to], |row| {
            Ok(UnpricedModel {
                model: row.get(0)?,
                requests: row.get::<_, i64>(1)? as u64,
                tokens: row.get::<_, i64>(2)? as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 失败按（原因, 供应商, 状态码）聚类，次数多的在前
    pub fn failures(&self, range: &DayRange) -> Result<Vec<FailureGroup>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(error_kind, 'unknown'), provider_id, status, COUNT(*),
                    (SELECT r2.error_message FROM requests r2
                     WHERE r2.day BETWEEN ?1 AND ?2 AND r2.outcome = 'failed'
                       AND COALESCE(r2.error_kind, 'unknown') = COALESCE(r.error_kind, 'unknown')
                       AND r2.provider_id IS r.provider_id AND r2.status = r.status
                     ORDER BY r2.started_at DESC LIMIT 1)
             FROM requests r
             WHERE day BETWEEN ?1 AND ?2 AND outcome = 'failed'
             GROUP BY 1, 2, 3 ORDER BY 4 DESC",
        )?;
        let rows = stmt.query_map(params![range.from, range.to], |row| {
            Ok(FailureGroup {
                error_kind: row.get(0)?,
                provider_id: row.get(1)?,
                status: row.get(2)?,
                count: row.get::<_, i64>(3)? as u64,
                sample: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<RecentRequest>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT started_at, client, provider_id, client_model, upstream_model,
                    response_model, role, status, outcome, error_kind,
                    fresh_input, output, cache_read, cache_write, cost_nano, duration_ms
             FROM requests ORDER BY started_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            let u = |i: usize| -> rusqlite::Result<u64> {
                Ok(u64::try_from(row.get::<_, i64>(i)?).unwrap_or(0))
            };
            Ok(RecentRequest {
                started_at_ms: row.get(0)?,
                client: row.get(1)?,
                provider_id: row.get(2)?,
                client_model: row.get(3)?,
                upstream_model: row.get(4)?,
                response_model: row.get(5)?,
                role: row.get(6)?,
                status: row.get(7)?,
                outcome: row.get(8)?,
                error_kind: row.get(9)?,
                fresh_input: u(10)?,
                output: u(11)?,
                cache_read: u(12)?,
                cache_write: u(13)?,
                cost: row.get::<_, Option<i64>>(14)?.map(nano_to_usd),
                duration_ms: u(15)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 今天的本地日期，测试和 CLI 显示用
    pub fn today() -> String {
        local_day(chrono::Utc::now().timestamp_millis())
    }
}
