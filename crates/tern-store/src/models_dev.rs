//! 解析 models.dev 的 `api.json`，挑出文本模型的价格。
//!
//! 取自 cc-switch `src/lib/modelsDevPricing.ts` 的 `flattenModels` / `toModelPricing`；
//! 下载由调用方负责（网关 crate 里已有 reqwest，这里不再引入 HTTP 依赖）。

use std::collections::{HashMap, HashSet};

use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;

use crate::pricing::{clean_model_id, ModelPrice, PriceSource};

pub const API_URL: &str = "https://models.dev/api.json";

#[derive(Debug, Deserialize)]
struct Provider {
    #[serde(default)]
    models: HashMap<String, Model>,
}

#[derive(Debug, Default, Deserialize)]
struct Model {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    cost: Option<ModelCost>,
    #[serde(default)]
    modalities: Option<Modalities>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ModelCost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct Modalities {
    #[serde(default)]
    output: Vec<String>,
}

const NON_TEXT_MARKERS: &[&str] = &[
    "audio",
    "deprecated",
    "embedding",
    "image",
    "moderation",
    "realtime",
    "transcribe",
    "tts",
    "video",
];

fn is_text_model(model_id: &str, model: &Model) -> bool {
    if model
        .status
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("deprecated"))
    {
        return false;
    }
    if let Some(modalities) = &model.modalities {
        let outputs: Vec<String> = modalities
            .output
            .iter()
            .map(|m| m.to_ascii_lowercase())
            .collect();
        if !outputs.is_empty()
            && (!outputs.iter().any(|m| m == "text")
                || outputs
                    .iter()
                    .any(|m| matches!(m.as_str(), "audio" | "image" | "video")))
        {
            return false;
        }
    }
    let searchable = format!("{model_id} {}", model.name.as_deref().unwrap_or("")).to_lowercase();
    !NON_TEXT_MARKERS.iter().any(|m| searchable.contains(m))
}

/// models.dev 的价格是浮点数（每百万 token 美元），保留 6 位小数转成 Decimal
fn price(value: Option<f64>) -> Decimal {
    value
        .filter(|v| v.is_finite() && *v > 0.0 && *v < 1e12)
        .and_then(Decimal::from_f64)
        .map(|d| d.round_dp(6).normalize())
        .unwrap_or(Decimal::ZERO)
}

/// 解析 `api.json`。
///
/// 同一个模型名在多家（官方 + 各路转售）都有时，`prefer` 里列出的 models.dev
/// 供应商优先（通常是官方：`anthropic`、`openai`、`deepseek`…），其次取发布日期最新的。
pub fn parse(json: &str, prefer: &[&str]) -> Result<Vec<ModelPrice>, serde_json::Error> {
    let data: HashMap<String, Provider> = serde_json::from_str(json)?;
    let prefer: HashSet<&str> = prefer.iter().copied().collect();

    struct Candidate<'a> {
        preferred: bool,
        release_date: &'a str,
        provider: &'a str,
        price: ModelPrice,
    }

    let mut best: HashMap<String, Candidate> = HashMap::new();
    for (provider_id, provider) in &data {
        for (model_id, model) in &provider.models {
            if !is_text_model(model_id, model) {
                continue;
            }
            let Some(cost) = &model.cost else { continue };
            if cost.input.is_none() && cost.output.is_none() {
                continue;
            }
            let id = clean_model_id(model_id);
            if id.is_empty() {
                continue;
            }
            let candidate = Candidate {
                preferred: prefer.contains(provider_id.as_str()),
                release_date: model.release_date.as_deref().unwrap_or(""),
                provider: provider_id,
                price: ModelPrice {
                    display_name: model.name.clone().unwrap_or_else(|| model_id.clone()),
                    input: price(cost.input),
                    output: price(cost.output),
                    cache_read: price(cost.cache_read),
                    cache_write: price(cost.cache_write),
                    model_id: id.clone(),
                    source: PriceSource::ModelsDev,
                },
            };
            let better = match best.get(&id) {
                None => true,
                Some(current) => {
                    (
                        candidate.preferred,
                        candidate.release_date,
                        current.provider,
                    ) > (current.preferred, current.release_date, candidate.provider)
                }
            };
            if better {
                best.insert(id, candidate);
            }
        }
    }

    let mut prices: Vec<ModelPrice> = best.into_values().map(|c| c.price).collect();
    prices.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    Ok(prices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    const FIXTURE: &str = r#"{
      "anthropic": { "id": "anthropic", "models": {
        "claude-opus-5": { "name": "Claude Opus 5", "release_date": "2026-05-01",
          "cost": { "input": 5, "output": 25, "cache_read": 0.5, "cache_write": 6.25 },
          "modalities": { "input": ["text","image"], "output": ["text"] } }
      }},
      "some-reseller": { "models": {
        "anthropic/claude-opus-5": { "name": "Opus (resold)", "release_date": "2026-06-01",
          "cost": { "input": 4, "output": 20 } },
        "stepfun/step-5-preview": { "name": "Step 5 Preview", "release_date": "2026-09-01",
          "cost": { "input": 0.2, "output": 0.8, "cache_read": 0.04 } },
        "gpt-image-2": { "cost": { "input": 5, "output": 40 },
          "modalities": { "output": ["image"] } },
        "text-embedding-4": { "cost": { "input": 0.02 } },
        "old-model": { "status": "deprecated", "cost": { "input": 1, "output": 1 } },
        "free-model": { "cost": {} },
        "precise": { "cost": { "input": 0.123456789, "output": 1e13 } }
      }}
    }"#;

    #[test]
    fn keeps_text_models_and_prefers_official_provider() {
        let prices = parse(FIXTURE, &["anthropic"]).unwrap();
        let ids: Vec<&str> = prices.iter().map(|p| p.model_id.as_str()).collect();
        assert_eq!(ids, ["claude-opus-5", "precise", "step-5-preview"]);

        let opus = &prices[0];
        assert_eq!(opus.display_name, "Claude Opus 5");
        assert_eq!(opus.input, Decimal::from(5));
        assert_eq!(opus.cache_write, Decimal::from_str("6.25").unwrap());
        assert_eq!(opus.source, PriceSource::ModelsDev);

        let precise = &prices[1];
        assert_eq!(precise.input, Decimal::from_str("0.123457").unwrap());
        assert_eq!(precise.output, Decimal::ZERO, "离谱的价格当作 0");
    }

    #[test]
    fn without_preference_newest_release_wins() {
        let prices = parse(FIXTURE, &[]).unwrap();
        assert_eq!(prices[0].display_name, "Opus (resold)");
    }

    #[test]
    fn rejects_non_json() {
        assert!(parse("<html>", &[]).is_err());
    }
}
