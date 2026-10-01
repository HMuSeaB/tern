//! `provider/model` 路由。
//!
//! cc-switch 的路由键是"agent 类型 → 当前供应商"，切供应商要改 agent 配置；
//! tern 让请求体里的 `model` 自己说明去哪：`deepseek/deepseek-v4-pro` 表示
//! 发给 id 为 `deepseek` 的供应商，上游模型名是 `deepseek-v4-pro`。
//!
//! 只按第一个 `/` 切分，`openrouter/anthropic/claude-sonnet-5` 的上游模型名
//! 是 `anthropic/claude-sonnet-5`。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::provider::ProviderSpec;
use crate::proxy::ProxyError;

/// Claude Code 用 `[1m]` 后缀声明 100 万上下文，上游不认这个本地标记
const ONE_M_CONTEXT_MARKER: &str = "[1m]";

/// 模型名是怎么被路由的
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    /// 写了已知供应商前缀：`provider/model`
    Explicit,
    /// 没有可识别的前缀，落到了 `defaultProvider`
    Fallback,
}

impl RouteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteKind::Explicit => "explicit",
            RouteKind::Fallback => "fallback",
        }
    }
}

/// 一次路由的结果
#[derive(Debug, Clone)]
pub struct Route {
    pub provider: Arc<ProviderSpec>,
    pub kind: RouteKind,
    /// 去掉 `provider/` 前缀和 `[1m]` 后缀之后的上游模型名
    pub upstream_model: String,
    /// 原模型名带 `[1m]`，Anthropic 上游需要补 `context-1m` beta 头
    pub one_m_context: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ModelRouter {
    providers: HashMap<String, Arc<ProviderSpec>>,
    /// 保持配置顺序，供 `/v1/models` 等列表使用
    order: Vec<String>,
    /// 模型名没有前缀、或前缀不是已知供应商时的兜底。
    /// Claude Code 的后台小模型请求（haiku）在用户没配时会带原生模型名过来。
    default_provider: Option<String>,
}

impl ModelRouter {
    pub fn new(
        providers: Vec<ProviderSpec>,
        default_provider: Option<String>,
    ) -> Result<Self, ProxyError> {
        let mut map = HashMap::with_capacity(providers.len());
        let mut order = Vec::with_capacity(providers.len());
        for spec in providers {
            let id = spec.id.trim().to_string();
            if id.is_empty() || id.contains('/') {
                return Err(ProxyError::ConfigError(format!(
                    "供应商 id 不能为空或包含 '/': {:?}",
                    spec.id
                )));
            }
            if map.contains_key(&id) {
                return Err(ProxyError::ConfigError(format!("供应商 id 重复: {id}")));
            }
            order.push(id.clone());
            map.insert(id, Arc::new(spec));
        }

        let default_provider = default_provider
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());
        if let Some(id) = &default_provider {
            if !map.contains_key(id) {
                return Err(ProxyError::ConfigError(format!(
                    "默认供应商 {id} 不在供应商列表中"
                )));
            }
        }

        Ok(Self {
            providers: map,
            order,
            default_provider,
        })
    }

    pub fn get(&self, id: &str) -> Option<&Arc<ProviderSpec>> {
        self.providers.get(id)
    }

    /// 按配置顺序遍历供应商
    pub fn providers(&self) -> impl Iterator<Item = &Arc<ProviderSpec>> {
        self.order.iter().filter_map(|id| self.providers.get(id))
    }

    pub fn resolve(&self, model: &str) -> Result<Route, ProxyError> {
        let (model, one_m_context) = strip_one_m_suffix(model.trim());
        if model.is_empty() {
            return Err(ProxyError::InvalidRequest(
                "请求缺少 model 字段".to_string(),
            ));
        }

        if let Some((prefix, rest)) = model.split_once('/') {
            if let Some(provider) = self.providers.get(prefix) {
                let upstream_model = rest.trim();
                if upstream_model.is_empty() {
                    return Err(ProxyError::InvalidRequest(format!(
                        "模型名 {model} 缺少 '/' 之后的上游模型"
                    )));
                }
                return Ok(Route {
                    provider: provider.clone(),
                    kind: RouteKind::Explicit,
                    upstream_model: upstream_model.to_string(),
                    one_m_context,
                });
            }
        }

        // 前缀不是已知供应商：整串交给默认供应商（可能本身就是带 '/' 的上游模型名）
        match self
            .default_provider
            .as_ref()
            .and_then(|id| self.providers.get(id))
        {
            Some(provider) => Ok(Route {
                provider: provider.clone(),
                kind: RouteKind::Fallback,
                upstream_model: model.to_string(),
                one_m_context,
            }),
            None => Err(ProxyError::InvalidRequest(format!(
                "无法路由模型 {model}：请使用 provider/model 形式，可用供应商: {}",
                self.order.join(", ")
            ))),
        }
    }
}

/// 大小写不敏感地剥离末尾的 `[1m]`（与 cc-switch `strip_one_m_suffix_for_upstream` 一致）
fn strip_one_m_suffix(model: &str) -> (&str, bool) {
    let marker = ONE_M_CONTEXT_MARKER.as_bytes();
    let bytes = model.as_bytes();
    if bytes.len() >= marker.len()
        && bytes[bytes.len() - marker.len()..].eq_ignore_ascii_case(marker)
    {
        return (model[..model.len() - marker.len()].trim_end(), true);
    }
    (model, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ApiFormat, ProviderAuth};

    fn spec(id: &str) -> ProviderSpec {
        ProviderSpec::new(
            id,
            id,
            "https://api.example.com",
            ApiFormat::Anthropic,
            ProviderAuth::api_key("k"),
        )
    }

    fn router(default: Option<&str>) -> ModelRouter {
        ModelRouter::new(
            vec![spec("deepseek"), spec("openrouter")],
            default.map(ToString::to_string),
        )
        .unwrap()
    }

    #[test]
    fn splits_on_first_slash_only() {
        let route = router(None)
            .resolve("openrouter/anthropic/claude-sonnet-5")
            .unwrap();
        assert_eq!(route.provider.id, "openrouter");
        assert_eq!(route.upstream_model, "anthropic/claude-sonnet-5");
        assert_eq!(route.kind, RouteKind::Explicit);
        assert!(!route.one_m_context);
    }

    #[test]
    fn strips_one_m_marker_case_insensitively() {
        let route = router(None)
            .resolve(" deepseek/deepseek-v4-pro[1M] ")
            .unwrap();
        assert_eq!(route.upstream_model, "deepseek-v4-pro");
        assert!(route.one_m_context);
    }

    #[test]
    fn unknown_prefix_or_bare_model_falls_back_to_default() {
        let r = router(Some("deepseek"));
        let bare = r.resolve("claude-haiku-4-5").unwrap();
        assert_eq!(bare.provider.id, "deepseek");
        assert_eq!(bare.upstream_model, "claude-haiku-4-5");
        assert_eq!(bare.kind, RouteKind::Fallback);

        let unknown = r.resolve("moonshotai/kimi-k3").unwrap();
        assert_eq!(unknown.provider.id, "deepseek");
        assert_eq!(unknown.upstream_model, "moonshotai/kimi-k3");
    }

    #[test]
    fn unroutable_model_without_default_is_invalid_request() {
        for model in ["claude-haiku-4-5", "unknown/x", "", "deepseek/"] {
            let err = router(None).resolve(model).unwrap_err();
            assert!(
                matches!(err, ProxyError::InvalidRequest(_)),
                "{model}: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_bad_provider_tables() {
        assert!(ModelRouter::new(vec![spec("a"), spec("a")], None).is_err());
        assert!(ModelRouter::new(vec![spec("a/b")], None).is_err());
        assert!(ModelRouter::new(vec![spec("  ")], None).is_err());
        assert!(ModelRouter::new(vec![spec("a")], Some("b".into())).is_err());
    }

    #[test]
    fn providers_iterate_in_config_order() {
        let ids: Vec<_> = router(None).providers().map(|p| p.id.clone()).collect();
        assert_eq!(ids, ["deepseek", "openrouter"]);
    }
}
