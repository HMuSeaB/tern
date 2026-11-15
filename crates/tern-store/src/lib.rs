//! tern-store：用量明细的 SQLite 存储与计价。
//!
//! - [`Store`]：打开 / 迁移数据库，写入 [`UsageEvent`]，查询聚合
//! - [`Recorder`]：实现网关的 [`UsageSink`]，在后台线程落库，不阻塞请求路径
//! - [`pricing`]：价格表（内置 < models.dev < 用户手填）与成本计算

pub mod models_dev;
pub mod pricing;
mod query;
mod schema;

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::TimeZone;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use tern_gateway::{Outcome, UsageEvent, UsageSink};

pub use pricing::{ModelPrice, PriceBook, PriceSource};
pub use query::{
    Breakdown, BreakdownRow, DayRange, FailureGroup, ModelFlow, RecentRequest, SessionRow, Summary,
    TrendPoint, UnpricedModel,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("数据库错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("数据库版本 {found} 比当前程序支持的 {supported} 新，请升级 tern")]
    SchemaTooNew { found: usize, supported: usize },
    #[error("价格无效: {0}")]
    InvalidPrice(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// 写入结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inserted {
    Row {
        id: i64,
        /// 计价用的模型名；有 token 但没查到价时为空
        pricing_model: Option<String>,
        cost: Option<Decimal>,
    },
    /// 同一供应商的同一条消息已经记过
    Duplicate,
}

pub struct Store {
    conn: Mutex<Connection>,
    prices: RwLock<Arc<PriceBook>>,
    /// 供应商 id → 成本倍率
    multipliers: RwLock<HashMap<String, Decimal>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| StoreError::Other(format!("创建目录 {} 失败: {e}", dir.display())))?;
        }
        let conn = Connection::open(path)?;
        // WAL：网关写入的同时，`tern usage` 等进程可以并发读
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// 只读打开。给观察方用（面板、`tern usage` 只读模式）。
    ///
    /// 与 [`Store::open`] 的区别是刻意的，三条都不能少：
    /// - 不 `create_dir_all`、不建库：库不存在时应当报错，而不是造一个空的
    /// - 不 `journal_mode=WAL`：写模式是创建者定的，观察方无权改
    /// - 不迁移：迁移是写方的事。读者面对更新的 schema 会由 [`StoreError::SchemaTooNew`]
    ///   拦住，那正是想要的提示
    pub fn open_readonly(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(3))?;
        Self::init_readonly(conn)
    }

    fn init_readonly(conn: Connection) -> Result<Self> {
        // 刻意不调用 schema::migrate：迁移是写方的事。只读打开一个 schema 比自己
        // 新的库时，查询照样能跑（用的都是 V1 就有的列），要不要提示"库太新"
        // 由调用方决定。
        let store = Self {
            conn: Mutex::new(conn),
            prices: RwLock::new(Arc::new(PriceBook::default())),
            multipliers: RwLock::new(HashMap::new()),
        };
        store.reload_prices()?;
        Ok(store)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        schema::migrate(&mut conn)?;
        let store = Self {
            conn: Mutex::new(conn),
            prices: RwLock::new(Arc::new(PriceBook::default())),
            multipliers: RwLock::new(HashMap::new()),
        };
        store.reload_prices()?;
        Ok(store)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn prices(&self) -> Arc<PriceBook> {
        self.prices
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// 设置供应商倍率。值是十进制字符串，无效的报错并忽略该项。
    pub fn set_multipliers<'a>(
        &self,
        entries: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<String> {
        let mut map = HashMap::new();
        let mut errors = Vec::new();
        for (provider, raw) in entries {
            match Decimal::from_str(raw.trim()) {
                Ok(value) if !value.is_sign_negative() => {
                    map.insert(provider.to_string(), value.normalize());
                }
                _ => errors.push(format!(
                    "供应商 {provider} 的 costMultiplier {raw:?} 无效，按 1 计"
                )),
            }
        }
        *self.multipliers.write().unwrap_or_else(|p| p.into_inner()) = map;
        errors
    }

    fn multiplier(&self, provider: Option<&str>) -> Decimal {
        provider
            .and_then(|id| {
                self.multipliers
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(id)
                    .copied()
            })
            .unwrap_or(Decimal::ONE)
    }

    // -----------------------------------------------------------------------
    // 写入
    // -----------------------------------------------------------------------

    pub fn insert(&self, event: &UsageEvent) -> Result<Inserted> {
        let day = local_day(event.started_at_ms);
        let multiplier = self.multiplier(event.provider_id.as_deref());
        let prices = self.prices();

        // 上游回显的模型优先：中转站常把请求模型再映射一次，实际计费的是回显的那个。
        // 不退回客户端模型名——那是别名，按它计价会把真实模型的 token 按错误价格固化。
        let (pricing_model, cost) = match &event.tokens {
            Some(tokens) => {
                let found = [
                    event.response_model.as_deref(),
                    event.upstream_model.as_deref(),
                ]
                .into_iter()
                .flatten()
                .find_map(|model| prices.lookup(model));
                match found {
                    Some(price) => (
                        Some(price.model_id.clone()),
                        Some(pricing::calculate(tokens, price, multiplier)),
                    ),
                    None => (None, None),
                }
            }
            None => (None, None),
        };
        let tokens = event.tokens.unwrap_or_default();
        let daily_model = pricing_model
            .clone()
            .or_else(|| event.response_model.clone())
            .or_else(|| event.upstream_model.clone())
            .unwrap_or_else(|| event.client_model.clone());
        // 只有带 token 的行参与去重：失败行没有消息 ID
        let message_id = event.tokens.and(event.message_id.as_deref());

        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "INSERT OR IGNORE INTO requests (
                started_at, day, client, endpoint, provider_id, route_kind,
                client_model, upstream_model, response_model, pricing_model, role, session_id,
                stream, status, outcome, error_kind, error_message,
                fresh_input, output, cache_read, cache_write, has_usage,
                cost_nano, savings_nano, cost_multiplier, message_id, first_token_ms, duration_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                      ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)",
            params![
                event.started_at_ms,
                day,
                event.client.as_str(),
                event.endpoint,
                event.provider_id,
                event.route_kind.map(|k| k.as_str()),
                event.client_model,
                event.upstream_model,
                event.response_model,
                pricing_model,
                event.role.as_str(),
                event.session_id,
                event.stream,
                event.status,
                event.outcome.as_str(),
                event.error_kind.map(|k| k.as_str()),
                event.error_message,
                to_i64(tokens.fresh_input),
                to_i64(tokens.output),
                to_i64(tokens.cache_read),
                to_i64(tokens.cache_write),
                event.tokens.is_some(),
                cost.map(|c| pricing::to_nano_usd(c.total)),
                cost.map(|c| pricing::to_nano_usd(c.cache_savings)),
                multiplier.to_string(),
                message_id,
                event.first_token_ms.map(to_i64),
                to_i64(event.duration_ms),
            ],
        )?;
        if changed == 0 {
            log::warn!(
                "[Store] 重复的消息 {} (provider={})，忽略",
                message_id.unwrap_or("-"),
                event.provider_id.as_deref().unwrap_or("-")
            );
            return Ok(Inserted::Duplicate);
        }
        let id = tx.last_insert_rowid();

        tx.execute(
            "INSERT INTO daily (day, client, provider_id, model, role,
                requests, failures, aborted, fresh_input, output, cache_read, cache_write,
                cost_nano, savings_nano, unpriced)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (day, client, provider_id, model, role) DO UPDATE SET
                requests = requests + 1,
                failures = failures + excluded.failures,
                aborted = aborted + excluded.aborted,
                fresh_input = fresh_input + excluded.fresh_input,
                output = output + excluded.output,
                cache_read = cache_read + excluded.cache_read,
                cache_write = cache_write + excluded.cache_write,
                cost_nano = cost_nano + excluded.cost_nano,
                savings_nano = savings_nano + excluded.savings_nano,
                unpriced = unpriced + excluded.unpriced",
            params![
                day,
                event.client.as_str(),
                event.provider_id.as_deref().unwrap_or(""),
                daily_model,
                event.role.as_str(),
                event.outcome == Outcome::Failed,
                event.outcome == Outcome::Aborted,
                to_i64(tokens.fresh_input),
                to_i64(tokens.output),
                to_i64(tokens.cache_read),
                to_i64(tokens.cache_write),
                cost.map_or(0, |c| pricing::to_nano_usd(c.total)),
                cost.map_or(0, |c| pricing::to_nano_usd(c.cache_savings)),
                event.tokens.is_some() && cost.is_none(),
            ],
        )?;
        tx.commit()?;

        Ok(Inserted::Row {
            id,
            pricing_model,
            cost: cost.map(|c| c.total),
        })
    }

    // -----------------------------------------------------------------------
    // 价格
    // -----------------------------------------------------------------------

    /// 重新合并内置价格与库里的覆盖
    pub fn reload_prices(&self) -> Result<()> {
        let overrides = self.price_overrides()?;
        let book = PriceBook::new(pricing::builtin_prices().into_iter().chain(overrides));
        *self.prices.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(book);
        Ok(())
    }

    fn price_overrides(&self) -> Result<Vec<ModelPrice>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT model_id, source, display_name, input, output, cache_read, cache_write
             FROM prices",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                [
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ],
            ))
        })?;
        let mut prices = Vec::new();
        for row in rows {
            let (id, source, name, values) = row?;
            let Some(source) = PriceSource::parse(&source) else {
                continue;
            };
            let values = values.each_ref().map(String::as_str);
            match ModelPrice::parse(&id, &name, values, source) {
                Ok(price) => prices.push(price),
                Err(e) => log::warn!("[Store] 跳过无效价格 {id}: {e}"),
            }
        }
        Ok(prices)
    }

    /// 写入价格覆盖（`source` 必须是 models_dev 或 user），并给之前未定价的明细补价
    pub fn upsert_prices(&self, prices: &[ModelPrice]) -> Result<usize> {
        let now = chrono::Utc::now().timestamp();
        {
            let mut conn = self.conn();
            let tx = conn.transaction()?;
            for price in prices {
                if price.source == PriceSource::Builtin {
                    return Err(StoreError::InvalidPrice("内置价格不能写入数据库".into()));
                }
                tx.execute(
                    "INSERT INTO prices (model_id, source, display_name, input, output,
                        cache_read, cache_write, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT (model_id, source) DO UPDATE SET
                        display_name = excluded.display_name, input = excluded.input,
                        output = excluded.output, cache_read = excluded.cache_read,
                        cache_write = excluded.cache_write, updated_at = excluded.updated_at",
                    params![
                        price.model_id,
                        price.source.as_str(),
                        price.display_name,
                        price.input.to_string(),
                        price.output.to_string(),
                        price.cache_read.to_string(),
                        price.cache_write.to_string(),
                        now,
                    ],
                )?;
            }
            tx.commit()?;
        }
        self.reload_prices()?;
        self.reprice_unpriced()
    }

    /// 删除用户手填的价格
    pub fn delete_user_price(&self, model_id: &str) -> Result<bool> {
        let removed = self.conn().execute(
            "DELETE FROM prices WHERE model_id = ?1 AND source = 'user'",
            [pricing::clean_model_id(model_id)],
        )? > 0;
        self.reload_prices()?;
        Ok(removed)
    }

    /// 给有 token 但没定价的明细补价，返回补上的行数。
    ///
    /// 已经定过价的行不重算：价格表更新不应改写历史账单。
    pub fn reprice_unpriced(&self) -> Result<usize> {
        let prices = self.prices();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        /// id, response_model, upstream_model, 四个 token 桶, 倍率
        type Pending = (i64, Option<String>, Option<String>, [i64; 4], String);
        let pending: Vec<Pending> = {
            let mut stmt = tx.prepare(
                "SELECT id, response_model, upstream_model,
                        fresh_input, output, cache_read, cache_write, cost_multiplier
                 FROM requests WHERE has_usage = 1 AND cost_nano IS NULL",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    [row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?],
                    row.get(7)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };

        let mut updated = 0;
        for (id, response_model, upstream_model, counts, multiplier) in pending {
            let Some(price) = [response_model.as_deref(), upstream_model.as_deref()]
                .into_iter()
                .flatten()
                .find_map(|model| prices.lookup(model))
            else {
                continue;
            };
            let tokens = tern_gateway::TokenCounts {
                fresh_input: from_i64(counts[0]),
                output: from_i64(counts[1]),
                cache_read: from_i64(counts[2]),
                cache_write: from_i64(counts[3]),
            };
            let multiplier = Decimal::from_str(&multiplier).unwrap_or(Decimal::ONE);
            let cost = pricing::calculate(&tokens, price, multiplier);
            tx.execute(
                "UPDATE requests SET pricing_model = ?2, cost_nano = ?3, savings_nano = ?4
                 WHERE id = ?1",
                params![
                    id,
                    price.model_id,
                    pricing::to_nano_usd(cost.total),
                    pricing::to_nano_usd(cost.cache_savings),
                ],
            )?;
            updated += 1;
        }
        if updated > 0 {
            // 补价会改变 daily 的模型键（从上游模型名变成计价模型名），整表重建最简单
            rebuild_daily(&tx)?;
        }
        tx.commit()?;
        Ok(updated)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

/// 从明细重算按天聚合
fn rebuild_daily(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM daily;
         INSERT INTO daily (day, client, provider_id, model, role,
            requests, failures, aborted, fresh_input, output, cache_read, cache_write,
            cost_nano, savings_nano, unpriced)
         SELECT day, client, COALESCE(provider_id, ''),
                COALESCE(pricing_model, response_model, upstream_model, client_model), role,
                COUNT(*), SUM(outcome = 'failed'), SUM(outcome = 'aborted'),
                SUM(fresh_input), SUM(output), SUM(cache_read), SUM(cache_write),
                COALESCE(SUM(cost_nano), 0), COALESCE(SUM(savings_nano), 0),
                SUM(has_usage = 1 AND cost_nano IS NULL)
         FROM requests
         GROUP BY 1, 2, 3, 4, 5;",
    )?;
    Ok(())
}

/// Unix 毫秒 → 本机时区的日期
pub(crate) fn local_day(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(chrono::Local::now)
        .format("%Y-%m-%d")
        .to_string()
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn from_i64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 后台写入
// ---------------------------------------------------------------------------

enum Message {
    Event(Box<UsageEvent>),
    Stop,
}

/// 网关的 [`UsageSink`]：事件进队列，后台线程逐条落库
pub struct Recorder {
    tx: Mutex<mpsc::Sender<Message>>,
}

impl UsageSink for Recorder {
    fn record(&self, event: UsageEvent) {
        let sent = self
            .tx
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .send(Message::Event(Box::new(event)));
        if sent.is_err() {
            log::warn!("[Store] 写入线程已停止，丢弃一条用量记录");
        }
    }
}

/// 写入线程的句柄。`stop` 会写完队列里剩下的事件再返回。
pub struct RecorderHandle {
    tx: mpsc::Sender<Message>,
    thread: Option<JoinHandle<()>>,
}

impl RecorderHandle {
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        let _ = self.tx.send(Message::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for RecorderHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// 启动写入线程。`on_insert` 在每条写入后调用（命令行用它打一行摘要日志）。
pub fn spawn_recorder(
    store: Arc<Store>,
    on_insert: impl Fn(&UsageEvent, &Inserted) + Send + 'static,
) -> (Arc<Recorder>, RecorderHandle) {
    let (tx, rx) = mpsc::channel::<Message>();
    let thread = std::thread::Builder::new()
        .name("tern-usage".into())
        .spawn(move || {
            for message in rx {
                match message {
                    Message::Event(event) => match store.insert(&event) {
                        Ok(inserted) => on_insert(&event, &inserted),
                        Err(e) => log::error!("[Store] 写入用量失败: {e}"),
                    },
                    Message::Stop => break,
                }
            }
        })
        .expect("创建用量写入线程失败");
    (
        Arc::new(Recorder {
            tx: Mutex::new(tx.clone()),
        }),
        RecorderHandle {
            tx,
            thread: Some(thread),
        },
    )
}
