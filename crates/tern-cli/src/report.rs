//! `tern usage` / `tern price` 的文本输出。界面在阶段 6 做，这里只求信息准确、对得齐。

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use tern_store::{
    Breakdown, BreakdownRow, DayRange, FailureGroup, ModelPrice, RecentRequest, Store, Summary,
    UnpricedModel,
};

use crate::table::Table;

pub fn usage(store: &Store, days: u32, by: Option<&str>, recent: usize) -> anyhow::Result<()> {
    let range = DayRange::last_days(days);
    let summary = store.summary(&range)?;
    let label = if days == 1 {
        "今天".to_string()
    } else {
        format!("最近 {days} 天（{} ~ {}）", range.from, range.to)
    };

    if summary.requests == 0 {
        println!("{label}没有请求记录。");
        println!("用 `tern serve` 启动网关、让 Claude Code / Codex 连上来之后，这里会显示用量。");
        return Ok(());
    }

    println!("{label}");
    print_summary(&summary);

    if summary.unpriced > 0 {
        println!();
        println!("⚠ {} 个请求的模型没有定价，花费未计入：", summary.unpriced);
        print_unpriced(&store.unpriced_models(&range)?);
        println!("  用 `tern price set <模型名> <输入> <输出> [<缓存读> [<缓存写>]]` 补上，历史记录会自动补价");
    }

    let dimensions: Vec<(&str, Breakdown)> = match by {
        Some(name) => vec![(name, breakdown(name))],
        None => vec![
            ("provider", Breakdown::Provider),
            ("model", Breakdown::Model),
            ("role", Breakdown::Role),
        ],
    };
    for (name, dimension) in dimensions {
        let rows = store.breakdown(&range, dimension)?;
        println!();
        println!("按{}", dimension_label(name));
        print_breakdown(&rows, summary.cost);
    }

    let failures = store.failures(&range)?;
    if !failures.is_empty() {
        println!();
        println!("失败（{} 次，不计入上面的模型 token）", summary.failures);
        print_failures(&failures);
    }

    if recent > 0 {
        println!();
        println!("最近 {recent} 条请求");
        print_recent(&store.recent(recent)?);
    }
    Ok(())
}

fn breakdown(name: &str) -> Breakdown {
    match name {
        "model" => Breakdown::Model,
        "role" => Breakdown::Role,
        "client" => Breakdown::Client,
        "day" => Breakdown::Day,
        _ => Breakdown::Provider,
    }
}

fn dimension_label(name: &str) -> &'static str {
    match name {
        "model" => "模型（实际计费的模型，不含失败请求）",
        "role" => "角色",
        "client" => "客户端",
        "day" => "天",
        _ => "供应商",
    }
}

fn print_summary(s: &Summary) {
    let hit = s
        .cache_hit_rate()
        .map_or("-".to_string(), |r| format!("{:.1}%", r * 100.0));
    println!(
        "  花费 {}   请求 {}（失败 {}，中断 {}）",
        money(s.cost),
        s.requests,
        s.failures,
        s.aborted
    );
    println!(
        "  token {}   新鲜输入 {} / 输出 {} / 缓存读 {} / 缓存写 {}",
        tokens(s.total_tokens()),
        tokens(s.fresh_input),
        tokens(s.output),
        tokens(s.cache_read),
        tokens(s.cache_write)
    );
    println!("  缓存命中率 {hit}   缓存省下 {}", money(s.cache_savings));
}

fn print_breakdown(rows: &[BreakdownRow], total_cost: Decimal) {
    let mut table = Table::new([
        "",
        "花费",
        "占比",
        "请求",
        "失败",
        "输入",
        "输出",
        "缓存读",
        "命中率",
    ]);
    for row in rows {
        let s = &row.summary;
        let share = if total_cost.is_zero() {
            "-".to_string()
        } else {
            format!(
                "{:.0}%",
                (s.cost / total_cost * Decimal::from(100))
                    .to_f64()
                    .unwrap_or(0.0)
            )
        };
        let key = if row.key.is_empty() {
            "（未路由）".to_string()
        } else {
            row.key.clone()
        };
        let cost = if s.unpriced > 0 && s.cost.is_zero() {
            "未定价".to_string()
        } else {
            money(s.cost)
        };
        table.row([
            key,
            cost,
            share,
            s.requests.to_string(),
            zero_dash(s.failures),
            tokens(s.fresh_input),
            tokens(s.output),
            tokens(s.cache_read),
            s.cache_hit_rate()
                .map_or("-".to_string(), |r| format!("{:.0}%", r * 100.0)),
        ]);
    }
    table.print("  ");
}

fn print_unpriced(models: &[UnpricedModel]) {
    let mut table = Table::new(["模型", "请求", "token"]);
    for m in models {
        table.row([m.model.clone(), m.requests.to_string(), tokens(m.tokens)]);
    }
    table.print("  ");
}

fn print_failures(groups: &[FailureGroup]) {
    let mut table = Table::new(["原因", "供应商", "状态", "次数", "最近一次"]);
    for g in groups {
        table.row([
            failure_label(&g.error_kind).to_string(),
            g.provider_id.clone().unwrap_or_else(|| "-".into()),
            g.status.to_string(),
            g.count.to_string(),
            g.sample
                .as_deref()
                .map(|s| ellipsize(&s.replace('\n', " "), 60))
                .unwrap_or_default(),
        ]);
    }
    table.print("  ");
}

fn failure_label(kind: &str) -> &str {
    match kind {
        "rate_limited" => "限流",
        "overloaded" => "过载",
        "auth" => "鉴权",
        "timeout" => "超时",
        "connection" => "连接失败",
        "upstream_rejected" => "上游拒绝",
        "upstream_server" => "上游 5xx",
        "invalid_request" => "请求无效",
        "transform" => "协议转换",
        "stream" => "流中断",
        other => other,
    }
}

fn print_recent(rows: &[RecentRequest]) {
    let mut table = Table::new([
        "时间",
        "角色",
        "模型",
        "→ 实际",
        "状态",
        "输入",
        "输出",
        "缓存读",
        "花费",
        "耗时",
    ]);
    for r in rows {
        let time = chrono::DateTime::from_timestamp_millis(r.started_at_ms)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        let actual = r
            .response_model
            .clone()
            .or_else(|| r.upstream_model.clone())
            .unwrap_or_else(|| "-".into());
        let status = match r.outcome.as_str() {
            "success" => r.status.to_string(),
            "aborted" => "中断".into(),
            _ => format!(
                "✗ {} {}",
                r.status,
                r.error_kind.as_deref().map(failure_label).unwrap_or("")
            ),
        };
        table.row([
            time,
            role_label(&r.role).to_string(),
            // 鉴权失败等请求在解析请求体之前就被拒了，没有模型名
            if r.client_model.is_empty() {
                "-".to_string()
            } else {
                ellipsize(&r.client_model, 32)
            },
            ellipsize(&actual, 28),
            status,
            tokens(r.fresh_input),
            tokens(r.output),
            tokens(r.cache_read),
            r.cost.map_or("-".to_string(), money),
            format!("{:.1}s", r.duration_ms as f64 / 1000.0),
        ]);
    }
    table.print("  ");
}

fn role_label(role: &str) -> &str {
    match role {
        "main" => "主对话",
        "subagent" => "子代理",
        "compact" => "压缩",
        "background" => "后台",
        other => other,
    }
}

pub fn prices(store: &Store, model: Option<&str>) {
    let book = store.prices();
    if let Some(model) = model {
        match book.lookup(model) {
            Some(price) => {
                println!(
                    "{model} → {}（来源 {}）",
                    price.model_id,
                    price.source.as_str()
                );
                print_prices(&[price]);
            }
            None => println!(
                "{model} 没有匹配的价格。用 `tern price set {} <输入> <输出>` 添加",
                tern_store::pricing::clean_model_id(model)
            ),
        }
        return;
    }
    let mut all: Vec<&ModelPrice> = book.iter().collect();
    all.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    print_prices(&all);
    let count = |source| all.iter().filter(|p| p.source == source).count();
    println!();
    println!(
        "共 {} 条：内置 {}，models.dev {}，手填 {}。单位：美元 / 百万 token",
        all.len(),
        count(tern_store::PriceSource::Builtin),
        count(tern_store::PriceSource::ModelsDev),
        count(tern_store::PriceSource::User)
    );
}

fn print_prices(prices: &[&ModelPrice]) {
    let mut table = Table::new(["模型", "输入", "输出", "缓存读", "缓存写", "来源"]);
    for p in prices {
        table.row([
            p.model_id.clone(),
            p.input.to_string(),
            p.output.to_string(),
            p.cache_read.to_string(),
            p.cache_write.to_string(),
            p.source.as_str().to_string(),
        ]);
    }
    table.print("");
}

/// 金额保留到分；不足一分但非零时显示 `<$0.01`，免得看起来像没花钱
pub fn money(value: Decimal) -> String {
    if value.is_zero() {
        return "$0.00".into();
    }
    let rounded = value.round_dp(2);
    if rounded.is_zero() {
        return "<$0.01".into();
    }
    format!("${rounded:.2}")
}

/// token 数用 K / M 缩写
pub fn tokens(value: u64) -> String {
    match value {
        0 => "0".into(),
        v if v < 1_000 => v.to_string(),
        v if v < 1_000_000 => format!("{:.1}K", v as f64 / 1e3),
        v if v < 1_000_000_000 => format!("{:.2}M", v as f64 / 1e6),
        v => format!("{:.2}B", v as f64 / 1e9),
    }
}

fn zero_dash(value: u64) -> String {
    if value == 0 {
        "-".into()
    } else {
        value.to_string()
    }
}

fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn money_rounds_to_cents_and_flags_tiny_amounts() {
        let d = |s| Decimal::from_str(s).unwrap();
        assert_eq!(money(d("0")), "$0.00");
        assert_eq!(money(d("0.000001")), "<$0.01");
        assert_eq!(money(d("1.005")), "$1.00");
        assert_eq!(money(d("12.3")), "$12.30");
    }

    #[test]
    fn token_abbreviations() {
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_500), "1.5K");
        assert_eq!(tokens(2_340_000), "2.34M");
        assert_eq!(tokens(3_000_000_000), "3.00B");
    }
}
