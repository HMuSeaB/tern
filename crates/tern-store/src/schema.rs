//! 表结构与迁移。用 `PRAGMA user_version` 记版本，迁移只能追加不能改。

use rusqlite::Connection;

use crate::StoreError;

/// 版本 1：请求明细、按天预聚合、价格覆盖
const V1: &str = r#"
CREATE TABLE requests (
    id              INTEGER PRIMARY KEY,
    -- 收到请求的时间，Unix 毫秒（UTC）
    started_at      INTEGER NOT NULL,
    -- 本地日期 YYYY-MM-DD，按天聚合用；写入时按本机时区算好
    day             TEXT    NOT NULL,
    client          TEXT    NOT NULL,              -- claude / codex
    endpoint        TEXT    NOT NULL,
    provider_id     TEXT,                          -- 路由失败时为空
    route_kind      TEXT,                          -- explicit / fallback
    client_model    TEXT    NOT NULL,              -- 客户端原样发来的
    upstream_model  TEXT,                          -- 路由后实际发给上游的
    response_model  TEXT,                          -- 上游回显的
    -- 计价用的模型名（upstream_model → response_model 里先查到价的那个）
    pricing_model   TEXT,
    role            TEXT    NOT NULL,              -- main / subagent / compact / background
    session_id      TEXT,
    stream          INTEGER NOT NULL,
    status          INTEGER NOT NULL,
    outcome         TEXT    NOT NULL,              -- success / aborted / failed
    error_kind      TEXT,
    error_message   TEXT,
    -- 四个桶互斥：fresh_input 不含任何缓存（OpenAI 口径入库前已扣除）
    fresh_input     INTEGER NOT NULL DEFAULT 0,
    output          INTEGER NOT NULL DEFAULT 0,
    cache_read      INTEGER NOT NULL DEFAULT 0,
    cache_write     INTEGER NOT NULL DEFAULT 0,
    has_usage       INTEGER NOT NULL DEFAULT 0,
    -- 成本以纳美元（1e-9 USD）存整数，SQL 求和不丢精度；未定价为 NULL，不是 0
    cost_nano       INTEGER,
    savings_nano    INTEGER,
    cost_multiplier TEXT    NOT NULL DEFAULT '1',
    message_id      TEXT,
    first_token_ms  INTEGER,
    duration_ms     INTEGER NOT NULL
);
CREATE INDEX idx_requests_started ON requests(started_at);
CREATE INDEX idx_requests_day ON requests(day);
CREATE INDEX idx_requests_session ON requests(session_id) WHERE session_id IS NOT NULL;
-- 重试、SSE 聚合兜底可能把同一条消息记两次
CREATE UNIQUE INDEX idx_requests_message
    ON requests(provider_id, message_id) WHERE message_id IS NOT NULL;

-- 按天预聚合，面板的趋势 / 占比查询不扫明细。与 requests 在同一事务里维护。
CREATE TABLE daily (
    day             TEXT    NOT NULL,
    client          TEXT    NOT NULL,
    provider_id     TEXT    NOT NULL,              -- 路由失败记为 ''
    model           TEXT    NOT NULL,              -- 计价模型，没有则上游模型，再没有则客户端模型
    role            TEXT    NOT NULL,
    requests        INTEGER NOT NULL DEFAULT 0,
    failures        INTEGER NOT NULL DEFAULT 0,
    aborted         INTEGER NOT NULL DEFAULT 0,
    fresh_input     INTEGER NOT NULL DEFAULT 0,
    output          INTEGER NOT NULL DEFAULT 0,
    cache_read      INTEGER NOT NULL DEFAULT 0,
    cache_write     INTEGER NOT NULL DEFAULT 0,
    cost_nano       INTEGER NOT NULL DEFAULT 0,
    savings_nano    INTEGER NOT NULL DEFAULT 0,
    -- 有 token 但没定价的请求数，面板据此提示"未定价"
    unpriced        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, client, provider_id, model, role)
);

-- 用户手填与 models.dev 同步的价格；内置价格在代码里，不入库
CREATE TABLE prices (
    model_id        TEXT    NOT NULL,
    source          TEXT    NOT NULL,              -- models_dev / user
    display_name    TEXT    NOT NULL,
    input           TEXT    NOT NULL,              -- 每百万 token 美元，十进制字符串
    output          TEXT    NOT NULL,
    cache_read      TEXT    NOT NULL,
    cache_write     TEXT    NOT NULL,
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (model_id, source)
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

const MIGRATIONS: &[&str] = &[V1];

pub(crate) fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let current: usize = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > MIGRATIONS.len() {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: MIGRATIONS.len(),
        });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index + 1)?;
        tx.commit()?;
        log::info!("[Store] 数据库升级到版本 {}", index + 1);
    }
    Ok(())
}
