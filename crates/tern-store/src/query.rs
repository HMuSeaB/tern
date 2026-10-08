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

/// 趋势上的一天。按天拆，可选再按供应商 / 模型拆一层。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrendPoint {
    pub day: String,
    /// 下钻的键（供应商 id / 模型名）。没有维度时为 ""
    pub key: String,
    pub summary: Summary,
}

/// 一次会话的合计。`session_id` 是客户端自带的，网关兜底生成的不入库
/// （那个 ID 每次会话都不同，聚合起来没有意义）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRow {
    pub session_id: String,
    pub client: String,
    /// 第一次请求的时间，Unix 毫秒
    pub started_at_ms: i64,
    /// 最后一次请求的时间
    pub ended_at_ms: i64,
    pub summary: Summary,
    /// 会话里出现过的角色。前端据此标"这次会话里有子代理"
    pub roles: Vec<String>,
}

/// 模型流向的一条边：客户端要的模型 → 实际花钱的模型。
///
/// 这是 ROADMAP 里"谁在花钱"那个问题的直接答案：cc-switch 的真实数据里
/// 212 条 `claude-sonnet-4-6 → claude-opus-5.5` 全是子代理请求被供应商
/// 映射走了，用户在原界面里完全看不出这件事。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelFlow {
    /// 客户端发来的模型名
    pub client_model: String,
    /// 上游回显的模型名；为空表示没拿到（失败请求）
    pub response_model: Option<String>,
    /// 请求数
    pub requests: u64,
    pub summary: Summary,
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

    /// 按天的趋势，可选再按一个维度下钻。
    ///
    /// 返回的是 **(天, 维度键)** 的笛卡尔积，缺的组合补 0 —— 堆叠柱要靠这个
    /// 才能让每个序列都是连续的时间轴。没有维度的调用（`Breakdown::Day`）
    /// 只返回每天一条，`key` 为空串。
    ///
    /// 走 SQL 的 `GROUP BY` 而不是在内存里拼：按天 × 供应商在 30 天窗口下是
    /// 几百行，拼一次的成本远低于 scan。
    pub fn trend(&self, range: &DayRange, by: Breakdown) -> Result<Vec<TrendPoint>> {
        let conn = self.conn();
        let column = by.column();
        let sql = if by == Breakdown::Day {
            // 单层：每天的合计。cost_nano 为 0 的天 SQLite 也会给一行（COALESCE），
            // 但完全没有请求的天不会出现——那些由 days_in 补
            format!(
                "SELECT day, '', {SUMMARY_COLUMNS} FROM daily
                 WHERE day BETWEEN ?1 AND ?2 GROUP BY day"
            )
        } else {
            format!(
                "SELECT day, {column}, {SUMMARY_COLUMNS} FROM daily
                 WHERE day BETWEEN ?1 AND ?2 GROUP BY day, {column}"
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![range.from, range.to], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                summary_from_row(row, 2)?,
            ))
        })?;
        let found: Vec<(String, String, Summary)> = rows.collect::<rusqlite::Result<_>>()?;

        if by == Breakdown::Day {
            // 单层：每天一条，key 空。完全没有请求的天 SQLite 不给行，由 days_in 补
            return Ok(days_in(range)
                .into_iter()
                .map(|day| {
                    let summary = found
                        .iter()
                        .find(|(d, _, _)| *d == day)
                        .map(|(_, _, s)| s.clone())
                        .unwrap_or_default();
                    TrendPoint {
                        day,
                        key: String::new(),
                        summary,
                    }
                })
                .collect());
        }

        // 维度键按**总花费**排：图例的顺序应当和时间范围无关，
        // 不然切个日期范围图例就跳一下
        let mut totals: Vec<(String, Decimal)> = Vec::new();
        for (_, key, summary) in &found {
            if key.is_empty() {
                continue;
            }
            match totals.iter_mut().find(|(k, _)| k == key) {
                Some((_, cost)) => *cost += summary.cost,
                None => totals.push((key.clone(), summary.cost)),
            }
        }
        totals.sort_by(|a, b| b.1.cmp(&a.1));
        let keys: Vec<String> = totals.into_iter().map(|(k, _)| k).collect();

        // 笛卡尔积补齐：缺的组合补 0，堆叠柱的每个序列才是连续的时间轴
        let mut out = Vec::with_capacity(days_in(range).len() * keys.len());
        for day in days_in(range) {
            for key in &keys {
                let summary = found
                    .iter()
                    .find(|(d, k, _)| *d == day && k == key)
                    .map(|(_, _, s)| s.clone())
                    .unwrap_or_default();
                out.push(TrendPoint {
                    day: day.clone(),
                    key: key.clone(),
                    summary,
                });
            }
        }
        Ok(out)
    }

    /// 会话视图：按 `session_id` 聚合，花费多的在前。
    ///
    /// 查 `requests` 而不是 `daily`：`daily` 是**按天 × 供应商 × 模型 × 角色**预聚合的，
    /// 它的粒度里没有 session。而且会话经常跨天（昨天下午开会到今天上午），
    /// 按天聚合会把一次会话拆成两半。
    ///
    /// 只统计拿到响应的：失败行没有"这次干了什么"的意义，混进来会出现
    /// "这次重构花了 $0、0 token"的假会话。
    pub fn sessions(&self, range: &DayRange, limit: usize) -> Result<Vec<SessionRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT session_id,
                    MIN(started_at), MAX(started_at),
                    COUNT(*), SUM(fresh_input), SUM(output),
                    SUM(cache_read), SUM(cache_write),
                    COALESCE(SUM(cost_nano), 0), COALESCE(SUM(savings_nano), 0),
                    SUM(outcome = 'aborted'),
                    SUM(cost_nano IS NULL AND fresh_input + output + cache_read + cache_write > 0),
                    GROUP_CONCAT(DISTINCT role),
                    MIN(client)
             FROM requests
             WHERE day BETWEEN ?1 AND ?2
               AND session_id IS NOT NULL AND session_id != ''
               AND outcome != 'failed'
             GROUP BY session_id
             ORDER BY COALESCE(SUM(cost_nano), 0) DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![range.from, range.to, limit as i64], |row| {
            let u = |i: usize| -> rusqlite::Result<u64> {
                Ok(u64::try_from(row.get::<_, i64>(i)?).unwrap_or(0))
            };
            Ok(SessionRow {
                session_id: row.get(0)?,
                started_at_ms: row.get(1)?,
                ended_at_ms: row.get(2)?,
                summary: Summary {
                    requests: u(3)?,
                    fresh_input: u(4)?,
                    output: u(5)?,
                    cache_read: u(6)?,
                    cache_write: u(7)?,
                    cost: nano_to_usd(row.get(8)?),
                    cache_savings: nano_to_usd(row.get(9)?),
                    aborted: u(10)?,
                    unpriced: u(11)?,
                    failures: 0,
                },
                roles: row
                    .get::<_, Option<String>>(12)?
                    .map(|text| text.split(',').map(str::to_string).collect())
                    .unwrap_or_default(),
                client: row.get(13)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 模型流向：客户端模型 → 实际模型。请求数多的在前，无 token 的沉底。
    ///
    /// 只算拿到响应的：失败请求没有 `response_model`，按它分组会凭空多出
    /// "某模型 12 次 0 token"的边，看起来像流量凭空消失了。
    pub fn model_flow(&self, range: &DayRange) -> Result<Vec<ModelFlow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT client_model, COALESCE(response_model, upstream_model),
                    COUNT(*),
                    SUM(fresh_input), SUM(output), SUM(cache_read), SUM(cache_write),
                    COALESCE(SUM(cost_nano), 0), COALESCE(SUM(savings_nano), 0)
             FROM requests
             WHERE day BETWEEN ?1 AND ?2 AND outcome != 'failed' AND has_usage = 1
             GROUP BY 1, 2
             ORDER BY SUM(fresh_input + output + cache_read + cache_write) DESC",
        )?;
        let rows = stmt.query_map(params![range.from, range.to], |row| {
            let u = |i: usize| -> rusqlite::Result<u64> {
                Ok(u64::try_from(row.get::<_, i64>(i)?).unwrap_or(0))
            };
            Ok(ModelFlow {
                client_model: row.get(0)?,
                response_model: row.get(1)?,
                requests: u(2)?,
                summary: Summary {
                    requests: u(2)?,
                    fresh_input: u(3)?,
                    output: u(4)?,
                    cache_read: u(5)?,
                    cache_write: u(6)?,
                    cost: nano_to_usd(row.get(7)?),
                    cache_savings: nano_to_usd(row.get(8)?),
                    ..Summary::default()
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// 闭区间内的每个日期，旧 → 新。**没有数据的天也列出来**：趋势图缺一天会让人
/// 以为那天没用，而不是"那天没记录"——两者对用户是同一件事，但后者是故障。
fn days_in(range: &DayRange) -> Vec<String> {
    use chrono::NaiveDate;
    let (Ok(from), Ok(to)) = (
        NaiveDate::parse_from_str(&range.from, "%Y-%m-%d"),
        NaiveDate::parse_from_str(&range.to, "%Y-%m-%d"),
    ) else {
        return Vec::new();
    };
    let mut days = Vec::new();
    let mut day = from;
    while day <= to {
        days.push(day.format("%Y-%m-%d").to_string());
        let Some(next) = day.checked_add_signed(chrono::Duration::days(1)) else {
            break;
        };
        day = next;
    }
    days
}
