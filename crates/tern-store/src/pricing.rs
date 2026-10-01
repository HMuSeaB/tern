//! 模型定价：价格表三层叠加（内置 < models.dev < 用户手填），按模型名模糊匹配，
//! 用 `rust_decimal` 计算成本避免浮点误差。
//!
//! 模型名匹配规则取自 cc-switch `services/usage_stats.rs` 的
//! `model_pricing_candidates` / `should_try_pricing_prefix_match`。

use std::collections::HashMap;
use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tern_gateway::TokenCounts;

use crate::StoreError;

const BUILTIN_JSON: &str = include_str!("builtin_pricing.json");

/// 价格来源，优先级从低到高
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    Builtin,
    ModelsDev,
    User,
}

impl PriceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PriceSource::Builtin => "builtin",
            PriceSource::ModelsDev => "models_dev",
            PriceSource::User => "user",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "builtin" => Some(PriceSource::Builtin),
            "models_dev" => Some(PriceSource::ModelsDev),
            "user" => Some(PriceSource::User),
            _ => None,
        }
    }
}

/// 每百万 token 的美元价格
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPrice {
    /// 归一化后的模型名（小写、无供应商前缀）
    pub model_id: String,
    pub display_name: String,
    pub input: Decimal,
    pub output: Decimal,
    pub cache_read: Decimal,
    pub cache_write: Decimal,
    pub source: PriceSource,
}

impl ModelPrice {
    pub fn parse(
        model_id: &str,
        display_name: &str,
        [input, output, cache_read, cache_write]: [&str; 4],
        source: PriceSource,
    ) -> Result<Self, StoreError> {
        let model_id = clean_model_id(model_id);
        if is_placeholder_model(&model_id) {
            return Err(StoreError::InvalidPrice(format!(
                "模型名无效: {model_id:?}"
            )));
        }
        let price = |label: &str, value: &str| -> Result<Decimal, StoreError> {
            let value = value.trim();
            let parsed = if value.is_empty() {
                Decimal::ZERO
            } else {
                Decimal::from_str(value).map_err(|e| {
                    StoreError::InvalidPrice(format!(
                        "{model_id} 的{label}价格 {value:?} 无效: {e}"
                    ))
                })?
            };
            if parsed.is_sign_negative() {
                return Err(StoreError::InvalidPrice(format!(
                    "{model_id} 的{label}价格不能为负"
                )));
            }
            Ok(parsed.normalize())
        };
        Ok(Self {
            display_name: if display_name.trim().is_empty() {
                model_id.clone()
            } else {
                display_name.trim().to_string()
            },
            input: price("输入", input)?,
            output: price("输出", output)?,
            cache_read: price("缓存读", cache_read)?,
            cache_write: price("缓存写", cache_write)?,
            model_id,
            source,
        })
    }
}

/// 一次计价的结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    /// 含供应商倍率
    pub total: Decimal,
    /// 缓存读按"新鲜输入价 − 缓存读价"折算的节省金额，含倍率
    pub cache_savings: Decimal,
}

pub fn calculate(tokens: &TokenCounts, price: &ModelPrice, multiplier: Decimal) -> Cost {
    let million = Decimal::from(1_000_000u32);
    let part = |count: u64, per_million: Decimal| Decimal::from(count) * per_million / million;
    let base = part(tokens.fresh_input, price.input)
        + part(tokens.output, price.output)
        + part(tokens.cache_read, price.cache_read)
        + part(tokens.cache_write, price.cache_write);
    let saved_per_million = (price.input - price.cache_read).max(Decimal::ZERO);
    Cost {
        total: base * multiplier,
        cache_savings: part(tokens.cache_read, saved_per_million) * multiplier,
    }
}

/// 入库用的整数纳美元（1e-9 USD）：SQL 里求和不丢精度
pub fn to_nano_usd(value: Decimal) -> i64 {
    use rust_decimal::prelude::ToPrimitive;
    (value * Decimal::from(1_000_000_000u64))
        .round()
        .to_i64()
        .unwrap_or(i64::MAX)
}

/// 合并后的价格表
#[derive(Debug, Clone, Default)]
pub struct PriceBook {
    prices: HashMap<String, ModelPrice>,
}

#[derive(Deserialize)]
struct BuiltinFile {
    models: Vec<BuiltinEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuiltinEntry {
    id: String,
    name: String,
    input: String,
    output: String,
    cache_read: String,
    cache_write: String,
}

/// 内置价格表（取自 cc-switch 的种子数据，见 `builtin_pricing.json`）
pub fn builtin_prices() -> Vec<ModelPrice> {
    let file: BuiltinFile =
        serde_json::from_str(BUILTIN_JSON).expect("builtin_pricing.json 必须是合法 JSON");
    file.models
        .iter()
        .map(|e| {
            ModelPrice::parse(
                &e.id,
                &e.name,
                [&e.input, &e.output, &e.cache_read, &e.cache_write],
                PriceSource::Builtin,
            )
            .unwrap_or_else(|err| panic!("内置价格 {} 无效: {err}", e.id))
        })
        .collect()
}

impl PriceBook {
    /// 后加入的同名价格只有来源优先级不低于已有的才覆盖
    pub fn new(prices: impl IntoIterator<Item = ModelPrice>) -> Self {
        let mut book = Self::default();
        for price in prices {
            book.insert(price);
        }
        book
    }

    pub fn insert(&mut self, price: ModelPrice) {
        match self.prices.get(&price.model_id) {
            Some(existing) if existing.source > price.source => {}
            _ => {
                self.prices.insert(price.model_id.clone(), price);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.prices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ModelPrice> {
        self.prices.values()
    }

    /// 精确匹配候选名，再按家族规则做前缀匹配（取最短的那个）
    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        let candidates = model_candidates(model);
        if let Some(price) = candidates.iter().find_map(|c| self.prices.get(c)) {
            return Some(price);
        }
        candidates
            .iter()
            .filter(|c| should_try_prefix_match(c))
            .find_map(|candidate| {
                let prefix = format!("{candidate}-");
                self.prices
                    .values()
                    .filter(|p| p.model_id.starts_with(&prefix))
                    .min_by(|a, b| {
                        a.model_id
                            .len()
                            .cmp(&b.model_id.len())
                            .then_with(|| a.model_id.cmp(&b.model_id))
                    })
            })
    }
}

pub(crate) fn is_placeholder_model(model_id: &str) -> bool {
    let normalized = model_id.trim().to_ascii_lowercase();
    normalized.is_empty() || matches!(normalized.as_str(), "unknown" | "null" | "none")
}

/// 去掉 `provider/` 前缀、`:tag` 后缀和 `[1m]`，统一小写
pub fn clean_model_id(model_id: &str) -> String {
    let normalized = model_id
        .rsplit_once('/')
        .map_or(model_id, |(_, rest)| rest)
        .split(':')
        .next()
        .unwrap_or(model_id)
        .trim()
        .replace('@', "-")
        .to_ascii_lowercase();
    normalized
        .strip_suffix("[1m]")
        .unwrap_or(&normalized)
        .trim()
        .to_string()
}

/// 按优先级排列的查价候选名
pub fn model_candidates(model_id: &str) -> Vec<String> {
    let cleaned = clean_model_id(model_id);
    if is_placeholder_model(&cleaned) {
        return Vec::new();
    }

    let mut candidates: Vec<String> = Vec::new();
    let mut queue = vec![cleaned];
    while let Some(candidate) = queue.pop() {
        if candidate.is_empty() || candidates.contains(&candidate) {
            continue;
        }
        candidates.push(candidate.clone());

        let derived = [
            strip_known_namespace(&candidate),
            strip_claude_non_anthropic_prefix(&candidate),
            strip_bedrock_version_suffix(&candidate),
            strip_date_suffix(&candidate),
            strip_reasoning_effort_suffix(&candidate),
            (candidate.starts_with("claude-") && candidate.contains('.'))
                .then(|| candidate.replace('.', "-")),
        ];
        queue.extend(derived.into_iter().flatten());
    }
    candidates
}

fn strip_known_namespace(model_id: &str) -> Option<String> {
    if let Some(pos) = model_id.rfind("claude-") {
        if pos > 0 {
            return Some(model_id[pos..].to_string());
        }
    }
    [
        "openai.",
        "anthropic.",
        "google.",
        "moonshot.",
        "moonshotai.",
        "bedrock.",
        "global.",
    ]
    .iter()
    .find_map(|marker| model_id.strip_prefix(marker).map(str::to_string))
}

/// 一些中转站给非 Anthropic 模型加 `claude-` 前缀骗过客户端校验
fn strip_claude_non_anthropic_prefix(model_id: &str) -> Option<String> {
    const NON_ANTHROPIC_MARKERS: &[&str] = &[
        "abab",
        "ark-code",
        "arctic",
        "astron",
        "codex",
        "command-r",
        "deepseek",
        "doubao",
        "ernie",
        "gemini",
        "gemma",
        "glm",
        "gpt",
        "grok",
        "hermes",
        "hy3",
        "hunyuan",
        "jamba",
        "kimi",
        "lfm",
        "llama",
        "longcat",
        "mercury",
        "mimo",
        "minimax",
        "mistral",
        "mixtral",
        "moonshot",
        "nemotron",
        "nova-",
        "openai",
        "qianfan",
        "qwen",
        "seed-",
        "solar",
        "stepfun",
    ];
    let rest = model_id.strip_prefix("claude-")?;
    NON_ANTHROPIC_MARKERS
        .iter()
        .any(|marker| rest.starts_with(marker))
        .then(|| rest.to_string())
}

fn strip_bedrock_version_suffix(model_id: &str) -> Option<String> {
    let (base, suffix) = model_id.rsplit_once("-v")?;
    (!base.is_empty() && !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
        .then(|| base.to_string())
}

/// `-2025-08-07`、`-20250807`，以及月日合法的 6 位 `-YYMMDD`
fn strip_date_suffix(model_id: &str) -> Option<String> {
    let bytes = model_id.as_bytes();
    if bytes.len() > 11 {
        let start = bytes.len() - 11;
        let s = &bytes[start..];
        let is_iso_date = s[0] == b'-'
            && s[1..5].iter().all(u8::is_ascii_digit)
            && s[5] == b'-'
            && s[6..8].iter().all(u8::is_ascii_digit)
            && s[8] == b'-'
            && s[9..11].iter().all(u8::is_ascii_digit);
        if is_iso_date {
            return Some(model_id[..start].to_string());
        }
    }

    let (base, suffix) = model_id.rsplit_once('-')?;
    if base.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if suffix.len() == 8 {
        return Some(base.to_string());
    }
    if suffix.len() == 6 {
        let month: u32 = suffix[2..4].parse().unwrap_or(0);
        let day: u32 = suffix[4..6].parse().unwrap_or(0);
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            return Some(base.to_string());
        }
    }
    None
}

fn strip_reasoning_effort_suffix(model_id: &str) -> Option<String> {
    ["-minimal", "-low", "-medium", "-high", "-xhigh"]
        .iter()
        .find_map(|suffix| model_id.strip_suffix(suffix))
        .filter(|stripped| !stripped.is_empty())
        .map(str::to_string)
}

fn should_try_prefix_match(model_id: &str) -> bool {
    let dash_count = model_id.matches('-').count();
    if model_id.starts_with("claude-") {
        return dash_count >= 3;
    }
    if ["o1", "o3", "o4", "o5"]
        .iter()
        .any(|prefix| model_id.starts_with(prefix))
    {
        return dash_count >= 1;
    }
    [
        "gpt-",
        "gemini-",
        "deepseek-",
        "qwen-",
        "glm-",
        "kimi-",
        "minimax-",
    ]
    .iter()
    .any(|prefix| model_id.starts_with(prefix))
        && dash_count >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(value: &str) -> Decimal {
        Decimal::from_str(value).unwrap()
    }

    fn price(id: &str, values: [&str; 4]) -> ModelPrice {
        ModelPrice::parse(id, "", values, PriceSource::User).unwrap()
    }

    #[test]
    fn builtin_table_loads_and_covers_common_models() {
        let book = PriceBook::new(builtin_prices());
        assert!(book.len() > 150, "{}", book.len());
        let opus = book.lookup("claude-opus-4-8").unwrap();
        assert_eq!((opus.input, opus.output), (d("5"), d("25")));
    }

    #[test]
    fn lookup_normalizes_provider_prefix_suffixes_and_dots() {
        let book = PriceBook::new([
            price("claude-sonnet-4-6", ["3", "15", "0.3", "3.75"]),
            price("gpt-5.6", ["5", "30", "0.5", "6.25"]),
            price("deepseek-v4-pro", ["0.4", "0.9", "0.004", "0"]),
        ]);
        for model in [
            "relay/claude-sonnet-4-6[1m]",
            "anthropic.claude-sonnet-4-6-20260217-v1:0",
            "CLAUDE-SONNET-4.6",
        ] {
            assert_eq!(
                book.lookup(model).map(|p| p.model_id.as_str()),
                Some("claude-sonnet-4-6"),
                "{model}"
            );
        }
        assert_eq!(book.lookup("gpt-5.6-xhigh").unwrap().model_id, "gpt-5.6");
        assert_eq!(
            book.lookup("claude-deepseek-v4-pro").unwrap().model_id,
            "deepseek-v4-pro"
        );
        assert!(book.lookup("step-5-preview").is_none());
        assert!(book.lookup("unknown").is_none());
    }

    #[test]
    fn prefix_match_picks_shortest_family_member() {
        let book = PriceBook::new([
            price("claude-haiku-4-5-20251001", ["1", "5", "0.1", "1.25"]),
            price("claude-haiku-4-5-20251001-extended", ["9", "9", "9", "9"]),
        ]);
        assert_eq!(
            book.lookup("claude-haiku-4-5").unwrap().model_id,
            "claude-haiku-4-5-20251001"
        );
        // 太短的名字不做前缀匹配，避免 claude-haiku 命中任意版本
        assert!(book.lookup("claude-haiku").is_none());
    }

    #[test]
    fn higher_priority_sources_win_regardless_of_order() {
        let mut builtin = price("kimi-k3", ["3", "15", "0.3", "0"]);
        builtin.source = PriceSource::Builtin;
        let user = price("kimi-k3", ["1", "2", "0.1", "0"]);
        let a = PriceBook::new([builtin.clone(), user.clone()]);
        let b = PriceBook::new([user, builtin]);
        assert_eq!(a.lookup("kimi-k3").unwrap().input, d("1"));
        assert_eq!(b.lookup("kimi-k3").unwrap().input, d("1"));
    }

    #[test]
    fn cost_and_cache_savings() {
        let p = price("m", ["3", "15", "0.3", "3.75"]);
        let tokens = TokenCounts {
            fresh_input: 1000,
            output: 500,
            cache_read: 200,
            cache_write: 100,
        };
        let cost = calculate(&tokens, &p, Decimal::ONE);
        // 0.003 + 0.0075 + 0.00006 + 0.000375
        assert_eq!(cost.total, d("0.010935"));
        // 200 × (3 − 0.3) / 1M
        assert_eq!(cost.cache_savings, d("0.00054"));

        let relay = calculate(&tokens, &p, d("0.3"));
        assert_eq!(relay.total, d("0.0032805"));
        assert_eq!(to_nano_usd(relay.total), 3_280_500);
    }

    #[test]
    fn rejects_bad_prices() {
        assert!(ModelPrice::parse("m", "", ["-1", "0", "0", "0"], PriceSource::User).is_err());
        assert!(ModelPrice::parse("m", "", ["abc", "0", "0", "0"], PriceSource::User).is_err());
        assert!(ModelPrice::parse("", "", ["1", "1", "0", "0"], PriceSource::User).is_err());
        let ok = ModelPrice::parse("Org/M:free", "", ["1.50", "", "", ""], PriceSource::User);
        let ok = ok.unwrap();
        assert_eq!((ok.model_id.as_str(), ok.input), ("m", d("1.5")));
    }
}
