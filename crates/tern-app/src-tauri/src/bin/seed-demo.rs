//! 造一份真实结构的用量数据，给 tern-app 面板做验收。
//!
//! 走的是 `tern_store::Store::insert` 这条**真实写入路径**（和网关记账同一套代码），
//! 所以面板的 SQL 面对的是真行，不是我手抄的 fixture。
//!
//! 用法：`cargo run -p tern-app --bin seed-demo -- <db路径>`

use std::path::Path;

use tern_gateway::{
    ClientKind, ErrorKind, Outcome, RequestRole, RouteKind, TokenCounts, UsageEvent,
};
use tern_store::Store;

/// 造一条请求记录。多数字段固定，只暴露面板关心的那几个维度。
/// 参数偏多但每个都对应 UsageEvent 的一个真实字段，收成结构体反而更难读。
#[allow(clippy::too_many_arguments)]
fn event(
    started_at_ms: i64,
    client: ClientKind,
    client_model: &str,
    response_model: &str,
    role: RequestRole,
    outcome: Outcome,
    tokens: Option<TokenCounts>,
    failure: Option<(ErrorKind, &str, u16)>,
) -> UsageEvent {
    let (error_kind, error_message, status) = match failure {
        Some((kind, message, status)) => (Some(kind), Some(message.to_string()), status),
        None => (None, None, 200),
    };
    UsageEvent {
        started_at_ms,
        client,
        endpoint: "/v1/messages".to_string(),
        provider_id: Some("relay".to_string()),
        route_kind: Some(RouteKind::Explicit),
        client_model: client_model.to_string(),
        upstream_model: Some(client_model.to_string()),
        response_model: Some(response_model.to_string()),
        role,
        session_id: Some("sess-demo".to_string()),
        stream: true,
        status,
        outcome,
        error_kind,
        error_message,
        tokens,
        message_id: Some(format!("msg-{started_at_ms}")),
        first_token_ms: Some(180),
        duration_ms: 2_400,
    }
}

/// 成功请求的简写
fn ok(
    started_at_ms: i64,
    client: ClientKind,
    client_model: &str,
    response_model: &str,
    role: RequestRole,
    tokens: TokenCounts,
) -> UsageEvent {
    event(
        started_at_ms,
        client,
        client_model,
        response_model,
        role,
        Outcome::Success,
        Some(tokens),
        None,
    )
}

/// 失败请求的简写：没有 token（上游没给出用量），单独聚类用
fn failed(
    started_at_ms: i64,
    client: ClientKind,
    client_model: &str,
    role: RequestRole,
    kind: ErrorKind,
    message: &str,
    status: u16,
) -> UsageEvent {
    event(
        started_at_ms,
        client,
        client_model,
        client_model,
        role,
        Outcome::Failed,
        None,
        Some((kind, message, status)),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo-usage.db".to_string());
    let db = Path::new(&db);
    if db.exists() {
        std::fs::remove_file(db)?;
    }
    let store = Store::open(db)?;

    // 定价：这三条都查得到价，另一条故意不设（用来验"未定价"提示）
    store.upsert_prices(&[
        tern_store::ModelPrice::parse(
            "claude-opus-5.5",
            "",
            ["3.75", "18.75", "0.375", "4.687"],
            tern_store::PriceSource::User,
        )?,
        tern_store::ModelPrice::parse(
            "claude-sonnet-5",
            "",
            ["3", "15", "0.3", "3.75"],
            tern_store::PriceSource::User,
        )?,
        tern_store::ModelPrice::parse(
            "gpt-5-codex",
            "",
            ["1.25", "10", "0.125", "1.25"],
            tern_store::PriceSource::User,
        )?,
    ])?;

    let now = chrono::Utc::now().timestamp_millis();
    let minute = 60_000;

    // 主对话：claude-sonnet-4-6 被供应商映射成 claude-opus-5.5（真实数据里的坑）
    store.insert(&ok(
        now,
        ClientKind::Claude,
        "claude-sonnet-4-6",
        "claude-opus-5.5",
        RequestRole::Main,
        TokenCounts {
            fresh_input: 1_200,
            output: 840,
            cache_read: 42_000,
            cache_write: 3_600,
        },
    ))?;
    // 子代理成功
    store.insert(&ok(
        now - minute,
        ClientKind::Claude,
        "claude-sonnet-4-6",
        "claude-sonnet-5",
        RequestRole::Subagent,
        TokenCounts {
            fresh_input: 2_100,
            output: 1_240,
            cache_read: 11_000,
            cache_write: 700,
        },
    ))?;
    // 未定价：有 token、查不到价
    store.insert(&ok(
        now - 2 * minute,
        ClientKind::Claude,
        "claude-sonnet-4-6",
        "step-5-preview",
        RequestRole::Subagent,
        TokenCounts {
            fresh_input: 5_000,
            output: 900,
            cache_read: 0,
            cache_write: 0,
        },
    ))?;
    // codex 后台任务，中途断开：仍有输入 / 输出计费
    store.insert(&event(
        now - 3 * minute,
        ClientKind::Codex,
        "gpt-5-codex",
        "gpt-5-codex",
        RequestRole::Background,
        Outcome::Aborted,
        Some(TokenCounts {
            fresh_input: 3_400,
            output: 420,
            cache_read: 0,
            cache_write: 0,
        }),
        None,
    ))?;
    // 子代理并发撞上限：429，必须单独聚类、不能混进模型分布
    for i in 0..3 {
        store.insert(&failed(
            now - (4 + i) * minute,
            ClientKind::Claude,
            "claude-sonnet-4-6",
            RequestRole::Subagent,
            ErrorKind::RateLimited,
            "gateway_concurrency_limit",
            429,
        ))?;
    }

    println!("已写入 {}", db.display());
    println!("(数据走的是 Store::insert 真写入路径，与网关记账同一套代码)");
    Ok(())
}
