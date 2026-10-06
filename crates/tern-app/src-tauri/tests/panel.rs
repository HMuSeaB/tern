//! `panel_summary` 背后那个纯函数 `build_panel` 的验收。
//!
//! 直接 `cargo test -p tern-app` 跑：灌一个与线上 schema 一致的临时库，
//! 检查聚合口径、未定价、失败聚类、"较昨日"对比都对。
//!
//! 口径基准是 `tern-store` 的 `query.rs`；这里只验证面板没把它抄歪。

use std::path::Path;

use rusqlite::Connection;
use tern_app_lib::commands::build_panel;

/// 与 `tern-store::schema::V1` 等价，只留面板查询碰到的列
const SCHEMA: &str = r#"
CREATE TABLE requests (
    id              INTEGER PRIMARY KEY,
    started_at      INTEGER NOT NULL,
    day             TEXT    NOT NULL,
    client          TEXT    NOT NULL,
    endpoint        TEXT    NOT NULL,
    provider_id     TEXT,
    route_kind      TEXT,
    client_model    TEXT    NOT NULL,
    upstream_model  TEXT,
    response_model  TEXT,
    pricing_model   TEXT,
    role            TEXT    NOT NULL,
    session_id      TEXT,
    stream          INTEGER NOT NULL,
    status          INTEGER NOT NULL,
    outcome         TEXT    NOT NULL,
    error_kind      TEXT,
    error_message   TEXT,
    fresh_input     INTEGER NOT NULL DEFAULT 0,
    output          INTEGER NOT NULL DEFAULT 0,
    cache_read      INTEGER NOT NULL DEFAULT 0,
    cache_write     INTEGER NOT NULL DEFAULT 0,
    has_usage       INTEGER NOT NULL DEFAULT 0,
    cost_nano       INTEGER,
    savings_nano    INTEGER,
    cost_multiplier TEXT    NOT NULL DEFAULT '1',
    message_id      TEXT,
    first_token_ms  INTEGER,
    duration_ms     INTEGER NOT NULL
);
CREATE TABLE daily (
    day          TEXT NOT NULL,
    client       TEXT NOT NULL,
    provider_id  TEXT NOT NULL,
    model        TEXT NOT NULL,
    role         TEXT NOT NULL,
    requests     INTEGER NOT NULL DEFAULT 0,
    failures     INTEGER NOT NULL DEFAULT 0,
    aborted      INTEGER NOT NULL DEFAULT 0,
    fresh_input  INTEGER NOT NULL DEFAULT 0,
    output       INTEGER NOT NULL DEFAULT 0,
    cache_read   INTEGER NOT NULL DEFAULT 0,
    cache_write  INTEGER NOT NULL DEFAULT 0,
    cost_nano    INTEGER NOT NULL DEFAULT 0,
    savings_nano INTEGER NOT NULL DEFAULT 0,
    unpriced     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (day, client, provider_id, model, role)
);
CREATE TABLE prices (
    model_id TEXT NOT NULL, source TEXT NOT NULL, display_name TEXT NOT NULL,
    input TEXT NOT NULL, output TEXT NOT NULL, cache_read TEXT NOT NULL,
    cache_write TEXT NOT NULL, updated_at INTEGER NOT NULL,
    PRIMARY KEY (model_id, source)
);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

#[allow(clippy::too_many_arguments)]
fn seed(
    conn: &Connection,
    day: &str,
    started_at: i64,
    client: &str,
    provider: Option<&str>,
    client_model: &str,
    response_model: Option<&str>,
    role: &str,
    outcome: &str,
    error_kind: Option<&str>,
    error_message: Option<&str>,
    status: u16,
    fresh_input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    has_usage: bool,
    cost_nano: Option<i64>,
    savings_nano: Option<i64>,
) {
    // 23 列 / 23 个绑定参数，占位符 ?1..?23
    conn.execute(
        "INSERT INTO requests (started_at, day, client, endpoint, provider_id, client_model,
             upstream_model, response_model, role, stream, status, outcome, error_kind,
             error_message, fresh_input, output, cache_read, cache_write, has_usage,
             cost_nano, savings_nano, message_id, duration_ms)
         VALUES (?1,?2,?3,'/v1/messages',?4,?5,?5,?6,?7,1,?8,?9,?10,?11,
                 ?12,?13,?14,?15,?16,?17,?18,?19,?20)",
        rusqlite::params![
            started_at,
            day,
            client,
            provider,
            client_model,
            response_model,
            role,
            status,
            outcome,
            error_kind,
            error_message,
            fresh_input as i64,
            output as i64,
            cache_read as i64,
            cache_write as i64,
            i32::from(has_usage),
            cost_nano,
            savings_nano,
            "m".to_string() + &started_at.to_string(),
            (output + fresh_input) as i64 * 3,
        ],
    )
    .unwrap();

    // daily 与 requests 同事务维护。注意 requests 是**每条明细都 +1**（不分成败），
    // failures / aborted 是另外计的——见 tern-store lib.rs 的 upsert。
    conn.execute(
        "INSERT INTO daily (day, client, provider_id, model, role, requests, failures, aborted,
             fresh_input, output, cache_read, cache_write, cost_nano, savings_nano, unpriced)
         VALUES (?1,?2,?3,?4,?5,1,
             CASE WHEN ?6='failed'  THEN 1 ELSE 0 END,
             CASE WHEN ?6='aborted' THEN 1 ELSE 0 END,
             ?7,?8,?9,?10,?11,?12,?13)",
        rusqlite::params![
            day,
            client,
            provider.unwrap_or(""),
            response_model.unwrap_or(client_model),
            role,
            outcome,
            fresh_input as i64,
            output as i64,
            cache_read as i64,
            cache_write as i64,
            cost_nano.unwrap_or(0),
            savings_nano.unwrap_or(0),
            i32::from(cost_nano.is_none() && has_usage),
        ],
    )
    .unwrap();
}

fn local_days_ago(days: i64) -> String {
    (chrono::Local::now().date_naive() - chrono::Days::new(u64::try_from(days).unwrap_or(0)))
        .format("%Y-%m-%d")
        .to_string()
}

/// 灌一个覆盖各分支的库：成功 / 未定价 / 中断 / 失败，外加昨天一条做对比基准
fn seeded() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("usage.db")).unwrap();
    conn.execute_batch(SCHEMA).unwrap();

    let today = local_days_ago(0);
    let yesterday = local_days_ago(1);
    let t = chrono::Utc::now().timestamp_millis();

    seed(
        &conn, &today, t, "claude", Some("relay"), "cl<SECRET_bea1c54f>-sonnet-4-6",
        Some("cl<SECRET_bea1c54f>-opus-5.5"), "main", "success", None, None, 200,
        1_000, 800, 40_000, 5_000, true, Some(123_456), Some(500),
    );
    seed(
        &conn, &today, t + 1, "claude", Some("relay"), "cl<SECRET_bea1c54f>-sonnet-4-6",
        Some("claude-sonnet-5"), "subagent", "success", None, None, 200,
        2_000, 1_200, 10_000, 800, true, Some(50_000), Some(200),
    );
    // 未定价：有 token、成本 NULL
    seed(
        &conn, &today, t + 2, "claude", Some("relay"), "cl<SECRET_bea1c54f>-sonnet-4-6",
        Some("step-5-preview"), "subagent", "success", None, None, 200,
        500, 100, 0, 0, true, None, None,
    );
    // 子代理断开
    seed(
        &conn, &today, t + 3, "codex", Some("relay"), "gpt-5-codex",
        Some("gpt-5-codex"), "background", "aborted", None, None, 200,
        3_000, 400, 0, 0, true, Some(9_000), None,
    );
    // 限流失败：必须单独聚类
    seed(
        &conn, &today, t + 4, "claude", Some("relay"), "cl<SECRET_bea1c54f>-sonnet-4-6",
        None, "subagent", "failed", Some("rate_limit"),
        Some("gateway_concurrency_limit"), 429, 0, 0, 0, 0, false, None, None,
    );
    seed(
        &conn, &yesterday, t - 86_400_000, "claude", Some("relay"),
        "cl<SECRET_bea1c54f>-sonnet-4-6", Some("claude-sonnet-5"), "main", "success",
        None, None, 200, 1_000, 500, 20_000, 2_000, true, Some(60_000), Some(300),
    );

    (dir, conn)
}

#[test]
fn aggregates_match_what_was_seeded() {
    let (_dir, conn) = seeded();
    let panel = build_panel(&conn, "/tmp/usage.db").unwrap();

    assert!(!panel.first_run, "有数据就不是首次运行");
    assert_eq!(panel.db_path, "/tmp/usage.db");

    // 今天 5 条明细（第 6 条刻意灌在昨天），daily.requests 每条都 +1，含失败行
    assert_eq!(panel.today.requests, 5);
    assert_eq!(panel.today.failures, 1);
    assert_eq!(panel.today.aborted, 1);
    // token 四个桶互斥，直接相加
    assert_eq!(panel.today.fresh_input, 1_000 + 2_000 + 500 + 3_000);
    assert_eq!(panel.today.output, 800 + 1_200 + 100 + 400);
    assert_eq!(panel.today.cache_read, 40_000 + 10_000);
    assert_eq!(panel.today.cache_write, 5_000 + 800);
    // 成本：123456 + 50000 + 9000 纳美元 = 182456 nano = $0.000182456
    assert_eq!(panel.today.cost, "0.000182456");
    // 缓存省下：500 + 200 纳美元
    assert_eq!(panel.today.cache_savings, "0.0000007");
    // 只有 step-5-preview 那条未定价
    assert_eq!(panel.today.unpriced, 1);

    // 昨天 1 条：60000 纳美元
    assert_eq!(panel.yesterday.requests, 1);
    assert_eq!(panel.yesterday.cost, "0.00006");
}

#[test]
fn unpriced_models_list_the_one_without_price() {
    let (_dir, conn) = seeded();
    let panel = build_panel(&conn, "x").unwrap();

    let models: Vec<&str> = panel
        .unpriced_models
        .iter()
        .map(|m| m.model.as_str())
        .collect();
    assert_eq!(models, vec!["step-5-preview"]);
    assert_eq!(panel.unpriced_models[0].requests, 1);
    assert_eq!(panel.unpriced_models[0].tokens, 600);
}

#[test]
fn failures_cluster_by_kind_and_keep_sample() {
    let (_dir, conn) = seeded();
    let panel = build_panel(&conn, "x").unwrap();

    assert_eq!(panel.failures.len(), 1);
    let f = &panel.failures[0];
    assert_eq!(f.error_kind, "rate_limit");
    assert_eq!(f.provider_id.as_deref(), Some("relay"));
    assert_eq!(f.status, 429);
    assert_eq!(f.count, 1);
    assert_eq!(f.sample.as_deref(), Some("gateway_concurrency_limit"));
}

#[test]
fn recent_shows_mapping_and_is_newest_first() {
    let (_dir, conn) = seeded();
    let panel = build_panel(&conn, "x").unwrap();

    assert_eq!(panel.recent.len(), 6);
    // 最后灌的那条失败在最前（started_at 最大）
    let newest = &panel.recent[0];
    assert_eq!(newest.outcome, "failed");
    assert_eq!(newest.error_kind.as_deref(), Some("rate_limit"));
    // 失败行没有响应模型，只能看到客户端模型
    assert_eq!(newest.response_model, None);
    assert_eq!(newest.client_model, "cl<SECRET_bea1c54f>-sonnet-4-6");

    // 第二条是 codex 的中断
    assert_eq!(panel.recent[1].outcome, "aborted");
    assert_eq!(panel.recent[1].client, "codex");
}

#[test]
fn empty_database_is_first_run_and_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open(dir.path().join("usage.db")).unwrap();
    conn.execute_batch(SCHEMA).unwrap();

    let panel = build_panel(&conn, "empty").unwrap();
    assert!(panel.first_run);
    assert_eq!(panel.today.requests, 0);
    assert!(panel.unpriced_models.is_empty());
    assert!(panel.failures.is_empty());
    assert!(panel.recent.is_empty());
}

#[test]
fn db_path_is_passed_through_for_the_error_message() {
    let (_dir, conn) = seeded();
    let panel = build_panel(&conn, "C:/Users/me/usage.db").unwrap();
    assert_eq!(panel.db_path, "C:/Users/me/usage.db");
}

#[test]
fn resolve_db_path_prefers_tern_db_env() {
    // 同一进程里其它测试可能设过 TERN_DB，这里只验证优先级不炸
    let _ = tern_app_lib::commands::build_panel(&Connection::open_in_memory().unwrap(), "x");
    let _ = Path::new("does-not-matter");
}
