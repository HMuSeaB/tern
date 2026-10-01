use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;

use rust_decimal::Decimal;
use tern_gateway::{
    ClientKind, ErrorKind, Outcome, RequestRole, RouteKind, TokenCounts, UsageEvent, UsageSink,
};
use tern_store::{
    spawn_recorder, Breakdown, DayRange, Inserted, ModelPrice, PriceSource, Store, StoreError,
};

fn d(value: &str) -> Decimal {
    Decimal::from_str(value).unwrap()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn success(provider: &str, client_model: &str, upstream: &str, tokens: TokenCounts) -> UsageEvent {
    UsageEvent {
        started_at_ms: now_ms(),
        client: ClientKind::Claude,
        endpoint: "/v1/messages".into(),
        provider_id: Some(provider.into()),
        route_kind: Some(RouteKind::Explicit),
        client_model: client_model.into(),
        upstream_model: Some(upstream.into()),
        response_model: None,
        role: RequestRole::Main,
        session_id: Some("sess-1".into()),
        stream: true,
        status: 200,
        outcome: Outcome::Success,
        error_kind: None,
        error_message: None,
        tokens: Some(tokens),
        message_id: None,
        first_token_ms: Some(300),
        duration_ms: 1200,
    }
}

fn failure(provider: &str, client_model: &str, status: u16, kind: ErrorKind) -> UsageEvent {
    UsageEvent {
        outcome: Outcome::Failed,
        status,
        error_kind: Some(kind),
        error_message: Some(format!("HTTP {status}")),
        tokens: None,
        first_token_ms: None,
        ..success(provider, client_model, client_model, TokenCounts::default())
    }
}

fn tokens(fresh_input: u64, output: u64, cache_read: u64, cache_write: u64) -> TokenCounts {
    TokenCounts {
        fresh_input,
        output,
        cache_read,
        cache_write,
    }
}

fn user_price(id: &str, values: [&str; 4]) -> ModelPrice {
    ModelPrice::parse(id, "", values, PriceSource::User).unwrap()
}

#[test]
fn prices_success_and_keeps_failures_out_of_model_stats() {
    let store = Store::open_in_memory().unwrap();
    // claude-opus-4-8 在内置价格表里：5 / 25 / 0.5 / 6.25
    let inserted = store
        .insert(&success(
            "relay",
            "relay/claude-opus-4-8",
            "claude-opus-4-8",
            tokens(1000, 500, 10_000, 0),
        ))
        .unwrap();
    let Inserted::Row {
        pricing_model,
        cost,
        ..
    } = inserted
    else {
        panic!("{inserted:?}")
    };
    assert_eq!(pricing_model.as_deref(), Some("claude-opus-4-8"));
    // 0.005 + 0.0125 + 0.005
    assert_eq!(cost, Some(d("0.0225")));

    for _ in 0..8 {
        store
            .insert(&failure(
                "relay",
                "claude-sonnet-4-6",
                429,
                ErrorKind::RateLimited,
            ))
            .unwrap();
    }

    let today = DayRange::today();
    let summary = store.summary(&today).unwrap();
    assert_eq!(summary.requests, 9);
    assert_eq!(summary.failures, 8);
    assert_eq!(summary.cost, d("0.0225"));
    // 10000 × (5 − 0.5) / 1M
    assert_eq!(summary.cache_savings, d("0.045"));
    assert_eq!(summary.unpriced, 0);
    let rate = summary.cache_hit_rate().unwrap();
    assert!((rate - 10_000.0 / 11_000.0).abs() < 1e-9);

    // 模型分布不含失败行，不会冒充"sonnet 用了 8 次 0 token"
    let by_model = store.breakdown(&today, Breakdown::Model).unwrap();
    let keys: Vec<&str> = by_model.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["claude-opus-4-8"]);
    assert_eq!(by_model[0].summary.requests, 1);
    // 供应商维度保留失败数，用来看错误率
    let by_provider = store.breakdown(&today, Breakdown::Provider).unwrap();
    assert_eq!(
        (
            by_provider[0].summary.requests,
            by_provider[0].summary.failures
        ),
        (9, 8)
    );

    let failures = store.failures(&today).unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].error_kind, "rate_limited");
    assert_eq!((failures[0].status, failures[0].count), (429, 8));
    assert_eq!(failures[0].sample.as_deref(), Some("HTTP 429"));
}

#[test]
fn response_model_wins_over_client_alias_for_pricing() {
    let store = Store::open_in_memory().unwrap();
    // 子代理请求 sonnet，中转站映射到 opus：按回显的 opus 计价，而不是 sonnet
    let mut event = success(
        "relay",
        "claude-sonnet-4-6",
        "claude-sonnet-4-6",
        tokens(1_000_000, 0, 0, 0),
    );
    event.response_model = Some("claude-opus-4-8".into());
    event.role = RequestRole::Subagent;
    store.insert(&event).unwrap();

    let summary = store.summary(&DayRange::today()).unwrap();
    assert_eq!(summary.cost, d("5"));
    let roles = store
        .breakdown(&DayRange::today(), Breakdown::Role)
        .unwrap();
    assert_eq!(roles[0].key, "subagent");
}

#[test]
fn unpriced_models_are_flagged_and_repriced_later() {
    let store = Store::open_in_memory().unwrap();
    store
        .insert(&success(
            "step",
            "step/step-5-preview",
            "step-5-preview",
            tokens(2_000_000, 1_000_000, 0, 0),
        ))
        .unwrap();

    let today = DayRange::today();
    let summary = store.summary(&today).unwrap();
    assert_eq!((summary.unpriced, summary.cost), (1, Decimal::ZERO));
    let unpriced = store.unpriced_models(&today).unwrap();
    assert_eq!(unpriced.len(), 1);
    assert_eq!(unpriced[0].model, "step-5-preview");
    assert_eq!(unpriced[0].tokens, 3_000_000);

    let repriced = store
        .upsert_prices(&[user_price("step-5-preview", ["0.2", "0.8", "0.04", "0"])])
        .unwrap();
    assert_eq!(repriced, 1);
    let summary = store.summary(&today).unwrap();
    assert_eq!((summary.unpriced, summary.cost), (0, d("1.2")));
    assert!(store.unpriced_models(&today).unwrap().is_empty());
    let by_model = store.breakdown(&today, Breakdown::Model).unwrap();
    assert_eq!(by_model[0].key, "step-5-preview");

    // 已定价的历史账单不随价格表变化
    store
        .upsert_prices(&[user_price("step-5-preview", ["9", "9", "9", "9"])])
        .unwrap();
    assert_eq!(store.summary(&today).unwrap().cost, d("1.2"));
}

#[test]
fn user_price_overrides_builtin_and_can_be_deleted() {
    let store = Store::open_in_memory().unwrap();
    let builtin = store.prices().lookup("claude-opus-4-8").unwrap().clone();
    assert_eq!(builtin.source, PriceSource::Builtin);

    store
        .upsert_prices(&[user_price("claude-opus-4-8", ["1", "1", "0", "0"])])
        .unwrap();
    assert_eq!(
        store.prices().lookup("claude-opus-4-8").unwrap().source,
        PriceSource::User
    );

    assert!(store.delete_user_price("CLAUDE-OPUS-4-8").unwrap());
    assert_eq!(store.prices().lookup("claude-opus-4-8").unwrap(), &builtin);

    let builtin_write = store.upsert_prices(&[builtin]);
    assert!(matches!(builtin_write, Err(StoreError::InvalidPrice(_))));
}

#[test]
fn provider_multiplier_applies_to_cost_and_savings() {
    let store = Store::open_in_memory().unwrap();
    let errors = store.set_multipliers([("relay", "0.3"), ("bad", "abc")]);
    assert_eq!(errors.len(), 1);
    store
        .insert(&success(
            "relay",
            "relay/claude-opus-4-8",
            "claude-opus-4-8",
            tokens(1_000_000, 0, 1_000_000, 0),
        ))
        .unwrap();
    let summary = store.summary(&DayRange::today()).unwrap();
    // (5 + 0.5) × 0.3
    assert_eq!(summary.cost, d("1.65"));
    // 1M × (5 − 0.5) × 0.3
    assert_eq!(summary.cache_savings, d("1.35"));
}

#[test]
fn duplicate_message_ids_are_ignored_per_provider() {
    let store = Store::open_in_memory().unwrap();
    let mut event = success("a", "a/m", "claude-opus-4-8", tokens(10, 10, 0, 0));
    event.message_id = Some("msg_1".into());
    assert!(matches!(
        store.insert(&event).unwrap(),
        Inserted::Row { .. }
    ));
    assert_eq!(store.insert(&event).unwrap(), Inserted::Duplicate);

    // 不同供应商可能复用 envelope id，不算重复
    event.provider_id = Some("b".into());
    assert!(matches!(
        store.insert(&event).unwrap(),
        Inserted::Row { .. }
    ));
    assert_eq!(store.summary(&DayRange::today()).unwrap().requests, 2);
}

#[test]
fn breakdown_by_day_and_provider() {
    let store = Store::open_in_memory().unwrap();
    let mut old = success("a", "a/m", "claude-opus-4-8", tokens(1_000_000, 0, 0, 0));
    old.started_at_ms -= 2 * 24 * 3600 * 1000;
    store.insert(&old).unwrap();
    store
        .insert(&success(
            "b",
            "b/m",
            "claude-opus-4-8",
            tokens(2_000_000, 0, 0, 0),
        ))
        .unwrap();

    let week = DayRange::last_days(7);
    let days = store.breakdown(&week, Breakdown::Day).unwrap();
    assert_eq!(days.len(), 2);
    assert!(days[0].key < days[1].key, "按日期升序");

    let providers = store.breakdown(&week, Breakdown::Provider).unwrap();
    assert_eq!(providers[0].key, "b", "花费高的在前");
    assert_eq!(providers[0].summary.cost, d("10"));
    assert_eq!(store.summary(&DayRange::today()).unwrap().requests, 1);
}

#[test]
fn recent_requests_newest_first() {
    let store = Store::open_in_memory().unwrap();
    store
        .insert(&success("a", "a/x", "claude-opus-4-8", tokens(1, 1, 0, 0)))
        .unwrap();
    store
        .insert(&failure("a", "a/y", 502, ErrorKind::UpstreamServer))
        .unwrap();
    let recent = store.recent(10).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].client_model, "a/y");
    assert_eq!(recent[0].outcome, "failed");
    assert!(recent[0].cost.is_none());
    assert!(recent[1].cost.is_some());
}

#[test]
fn file_database_reopens_and_rejects_newer_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("usage.db");
    {
        let store = Store::open(&path).unwrap();
        store
            .insert(&success("a", "a/m", "claude-opus-4-8", tokens(1, 1, 0, 0)))
            .unwrap();
        store.set_meta("k", "v").unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.summary(&DayRange::today()).unwrap().requests, 1);
        assert_eq!(store.meta("k").unwrap().as_deref(), Some("v"));
    }

    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    assert!(matches!(
        Store::open(&path),
        Err(StoreError::SchemaTooNew { found: 99, .. })
    ));
}

#[test]
fn recorder_writes_in_background_and_flushes_on_stop() {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let seen = Arc::new(Mutex::new(0));
    let counter = seen.clone();
    let (recorder, handle) = spawn_recorder(store.clone(), move |_, _| {
        *counter.lock().unwrap() += 1;
    });
    for i in 0..20 {
        let mut event = success("a", "a/m", "claude-opus-4-8", tokens(1, 1, 0, 0));
        event.message_id = Some(format!("msg_{i}"));
        recorder.record(event);
    }
    handle.stop();
    assert_eq!(*seen.lock().unwrap(), 20);
    assert_eq!(store.summary(&DayRange::today()).unwrap().requests, 20);

    // 线程停了之后再记录不会 panic
    recorder.record(success("a", "a/m", "m", tokens(1, 1, 0, 0)));
}
