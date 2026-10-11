//! `provider_usage` 背后那个纯函数 `build_provider_usage` 的验收。
//!
//! 直接 `cargo test -p tern-app` 跑。它决定供应商页"常用地址"的排序，
//! 所以重点验两件事：**时间窗口**（30 天外的不算）和**近期加权**
//! （最近一周的请求比前三周的值钱）。

use rusqlite::Connection;
use tern_app_lib::commands::build_provider_usage;

/// 与 `tern-store::schema::V1` 的 `daily` 表逐字段一致，只留本查询碰到的列。
/// 直接 INSERT daily 而不走 `seed()`：这里要构造的是"几天前"这种任意偏移，
/// 而 seed() 是按单条明细灌的，先造 requests 再让触发器同步太绕。
const SCHEMA: &str = r#"
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
"#;

fn conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn
}

/// 往某一天塞一个供应商的用量。`day` 用 `YYYY-MM-DD`，
/// 由调用方按相对今天的偏移算好——SQL 里的 BETWEEN 是闭区间字符串比较。
fn seed_daily(conn: &Connection, day: &str, provider: &str, requests: i64) {
    conn.execute(
        "INSERT INTO daily (day, client, provider_id, model, role, requests)
         VALUES (?1, 'claude-code', ?2, 'm', 'main', ?3)",
        rusqlite::params![day, provider, requests],
    )
    .unwrap();
}

/// 今天往前第 `ago` 天的 `YYYY-MM-DD`。口径必须和 `commands::local_days_ago`
/// 一致（chrono::Local），否则测试会因时区在半夜跑的时候差一天。
fn day_ago(ago: i64) -> String {
    (chrono::Local::now().date_naive() - chrono::Days::new(u64::try_from(ago).unwrap_or(0)))
        .format("%Y-%m-%d")
        .to_string()
}

#[test]
fn only_counts_the_last_30_days() {
    let conn = conn();
    seed_daily(&conn, &day_ago(0), "hot", 10);
    seed_daily(&conn, &day_ago(29), "warm", 5);
    // 窗口是闭区间 [今天-30, 今天]，所以第 30 天算在内、第 31 天起排除。
    // 这条边界要钉住：写 BETWEEN ?1 AND ?2 而 ?1 取 day_ago(30) 就意味着
    // "最近 30 天"含第 30 天，共 31 个自然日。别改成 > 或者 29。
    // 只给 1 次：它在窗口内但不享受近期加权，用来验边界而不是抢首位
    seed_daily(&conn, &day_ago(30), "stale", 1);
    seed_daily(&conn, &day_ago(31), "too_old", 100);
    seed_daily(&conn, &day_ago(90), "ancient", 100);

    let rows = build_provider_usage(&conn).unwrap();
    let ids: Vec<&str> = rows.iter().map(|r| r.provider_id.as_str()).collect();
    // hot = 10x3（近期），warm = 5，stale = 1（窗口内但不加权）
    assert_eq!(
        ids,
        vec!["hot", "warm", "stale"],
        "30 天前整天在窗口内，31 天起排除: {ids:?}"
    );
    assert!(
        !ids.contains(&"too_old") && !ids.contains(&"ancient"),
        "窗口外的用量不该影响排序: {ids:?}"
    );
}

#[test]
fn recent_week_outweighs_the_three_weeks_before_it() {
    let conn = conn();
    // 前三周 20 次 vs 最近一周 10 次。不加权前者(20)赢，加权三倍后后者(30)赢。
    // 这正是"最近一直在用"和"早先用过一阵"的区别。
    // 取 10 而不是 3：3x3=9 < 20，加权了也翻不过来，那测不出加权的效果。
    seed_daily(&conn, &day_ago(20), "used_to", 20);
    seed_daily(&conn, &day_ago(1), "lately", 10);

    let rows = build_provider_usage(&conn).unwrap();
    assert_eq!(
        rows[0].provider_id, "lately",
        "最近一周的该排前面: {rows:?}"
    );
    assert_eq!(rows[1].provider_id, "used_to");
    // 加权后的实际值：10 x 3 = 30，压过 20
    assert_eq!(rows[0].requests, 30);
    assert_eq!(rows[1].requests, 20);
}

#[test]
fn same_url_on_two_providers_counts_separately() {
    // 真实情况：一个域名下挂两个供应商（用户截图里 www.mcgrox.top 那两个）。
    // 账按 provider_id 记，这里就该是两条；由前端按 group_key 聚合成组。
    let conn = conn();
    seed_daily(&conn, &day_ago(0), "mcgrox_a", 7);
    seed_daily(&conn, &day_ago(0), "mcgrox_b", 3);

    let rows = build_provider_usage(&conn).unwrap();
    assert_eq!(rows.len(), 2, "按供应商分开记，不合到一起: {rows:?}");
    // 都在今天，落在近 7 天里，所以是 7x3 和 3x3
    let find = |id: &str| rows.iter().find(|r| r.provider_id == id).unwrap().requests;
    assert_eq!(find("mcgrox_a"), 21, "7 次 x 近期权重 3");
    assert_eq!(find("mcgrox_b"), 9, "3 次 x 近期权重 3");
}

#[test]
fn empty_provider_rows_are_dropped() {
    // 未路由的请求在 daily 里记成空串（见 tern-store lib.rs 的 unwrap_or("")）。
    // 它不属于任何供应商，参与排序只会把"未分组"顶到前面。
    let conn = conn();
    seed_daily(&conn, &day_ago(0), "", 50);
    seed_daily(&conn, &day_ago(0), "real", 1);

    let rows = build_provider_usage(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider_id, "real");
}

#[test]
fn ties_break_on_id_so_the_order_is_stable() {
    // 用量相同的供应商，顺序必须可复现。按 id 升序是个稳定的规则，
    // 否则每次轮询列表都可能重排，用户刚认准的位置又跑了。
    let conn = conn();
    seed_daily(&conn, &day_ago(0), "zeta", 5);
    seed_daily(&conn, &day_ago(0), "alpha", 5);

    let rows = build_provider_usage(&conn).unwrap();
    assert_eq!(rows[0].provider_id, "alpha");
    assert_eq!(rows[1].provider_id, "zeta");
}

#[test]
fn empty_db_yields_no_rows() {
    let conn = conn();
    assert!(build_provider_usage(&conn).unwrap().is_empty());
}
