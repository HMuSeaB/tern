//! 故障转移与熔断（ROADMAP T+4 / 阶段 8）。
//!
//! # 要解决的问题
//!
//! 用户配了 3 家中转站，第一家挂了（key 失效、上游 529、DNS 挂了）。此前的行为是
//! 请求直接失败——用户看到一句"请求失败"，然后自己切一家。而"切一家"正是这个产品
//! 最常用的操作，凭什么要他手动做?
//!
//! 所以：**默认供应商连续失败就把请求转给下一家**；失败到阈值就熔断一段时间，
//! 不再往那家发（省得每次都等一个超时）。
//!
//! # 三个不能破的规矩
//!
//! 1. **显式指定 `provider/model` 的请求不转移**。用户写 `deepseek/xxx` 就是要
//!    deepseek，悄悄转给别家比报错更糟——那会让"我明明指定了"变成假话。
//! 2. **不重试已经产生输出的请求**。流式响应发了一半再换一家重发，用户会看到两段
//!    拼在一起的回答。只在上游**连响应头都没给**的时候才值得换。
//! 3. **熔断是进程内的，不落盘**。它是"这家现在不太行"的一个短期判断，重启就该重新
//!    试——把上次的失败记到下次启动，会让用户面对一个他没法理解的"为什么这家不能
//!    用"。（用量库里的失败统计是另一件事，那个要长久留着。）
//!
//! # 为什么不用 cc-switch 那套
//!
//! cc-switch 的熔断挂在 `AppProxyConfig`（按 app 存一份配置：`claude` 和 `codex`
//! 各自一套阈值）。tern 的供应商是**中立**的，不按 agent 分，所以阈值属于网关整体
//! 而不是某个 app。熔断器本身（`proxy/circuit_breaker.rs`）原样搬了过来，那部分是
//! 纯算法、与配置来源无关。

use std::collections::HashMap;
use std::sync::Arc;

use crate::proxy::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, CircuitState};
use crate::router::ModelRouter;
use serde::{Deserialize, Serialize};

/// 熔断阈值。默认值照 cc-switch 实测收敛出来的那套，见
/// `CircuitBreakerConfig::default`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResilienceConfig {
    /// 总开关。关掉之后回到"失败就报错"的老行为——用户要自己诊断时可以关
    #[serde(default = "default_true")]
    pub failover_enabled: bool,
    /// 连续失败多少次后熔断
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    /// 半开状态下成功多少次后恢复
    #[serde(default = "default_success_threshold")]
    pub success_threshold: u32,
    /// 熔断后多久试探一次（秒）
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    /// 错误率阈值 0.0-1.0
    #[serde(default = "default_error_rate")]
    pub error_rate_threshold: f64,
    /// 计算错误率前至少要多少请求。少了不判——刚发两个都失败不该熔断
    #[serde(default = "default_min_requests")]
    pub min_requests: u32,
}

fn default_true() -> bool {
    true
}
fn default_failure_threshold() -> u32 {
    4
}
fn default_success_threshold() -> u32 {
    2
}
fn default_timeout() -> u64 {
    60
}
fn default_error_rate() -> f64 {
    0.6
}
fn default_min_requests() -> u32 {
    10
}

impl Default for ResilienceConfig {
    fn default() -> Self {
        Self {
            failover_enabled: true,
            failure_threshold: default_failure_threshold(),
            success_threshold: default_success_threshold(),
            timeout_seconds: default_timeout(),
            error_rate_threshold: default_error_rate(),
            min_requests: default_min_requests(),
        }
    }
}

impl From<&ResilienceConfig> for CircuitBreakerConfig {
    fn from(config: &ResilienceConfig) -> Self {
        Self {
            failure_threshold: config.failure_threshold,
            success_threshold: config.success_threshold,
            timeout_seconds: config.timeout_seconds,
            error_rate_threshold: config.error_rate_threshold,
            min_requests: config.min_requests,
        }
    }
}

/// 每个供应商一个熔断器。
///
/// `RwLock<HashMap>` 而不是 DashMap：供应商数量是个位数到几十，读远多于写，
/// 且持有锁的时间里只有一次哈希查找。
#[derive(Default)]
pub struct Breakers {
    inner: std::sync::RwLock<HashMap<String, Arc<CircuitBreaker>>>,
}

impl Breakers {
    /// 取或建。并发第一次访问同一个供应商时可能建两次，后建的被丢弃——
    /// 那之前的计数会丢。听起来是个 bug，实际上窗口极小（只在进程启动后第一次
    /// 请求某家时），且丢的是"再熔断一次"的成本，不是正确性。
    fn breaker(&self, id: &str, config: &CircuitBreakerConfig) -> Arc<CircuitBreaker> {
        if let Some(found) = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
        {
            return found;
        }
        let breaker = Arc::new(CircuitBreaker::new(config.clone()));
        self.inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(id.to_string())
            .or_insert(breaker)
            .clone()
    }

    /// 某家现在能不能用。Open 且超时未到 → false。
    pub async fn is_available(&self, id: &str, config: &CircuitBreakerConfig) -> bool {
        self.breaker(id, config).is_available().await
    }

    pub async fn state(&self, id: &str, config: &CircuitBreakerConfig) -> CircuitState {
        self.breaker(id, config).get_state().await
    }

    pub async fn record_success(&self, id: &str, config: &CircuitBreakerConfig) {
        // 不是 HalfOpen 放行的就没有名额要还，第二个参数恒为 false。
        // 这里的用法是"路由阶段只看可用性、请求阶段不占名额"，见 handlers 里的注释
        self.breaker(id, config).record_success(false).await;
    }

    pub async fn record_failure(&self, id: &str, config: &CircuitBreakerConfig) {
        self.breaker(id, config).record_failure(false).await;
    }

    /// 删掉已经不存在的供应商。`set_providers` 之后调，否则内存里会一直留着
    /// 用户删过的那几家的计数——重启才清，等于一个只涨不消的泄漏。
    pub fn retain(&self, alive: &[String]) {
        let mut inner = self.inner.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.retain(|id, _| alive.iter().any(|keep| keep == id));
    }

    /// 当前所有熔断器的快照。给面板 / `tern check` 显示"哪几家被摘了"。
    pub async fn snapshot(&self, config: &CircuitBreakerConfig) -> Vec<(String, CircuitState)> {
        let ids: Vec<String> = {
            let inner = self.inner.read().unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.keys().cloned().collect()
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let state = self.state(&id, config).await;
            out.push((id, state));
        }
        // 只按 id 排：CircuitState 没实现 Ord 也不需要。这里的用途是给人看
        // "哪几家被摘了"，顺序稳定就行，按状态排反而看不出谁是谁
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// 一次请求可以按顺序尝试的供应商列表。
///
/// # 谁会进来
///
/// - ** fallback 路由**（模型名没带前缀）：从默认供应商开始，按配置顺序把所有可用的
///   过一遍。这是故障转移的主场景
/// - **显式路由**（`deepseek/xxx`）：只有 deepseek 一家。规矩 1：用户指定了就是指定了
///
/// # 谁会出去
///
/// 熔断中的（`is_available` 为 false）。全都熔断时退回原列表——全摘了让请求直接失败，
/// 不如让用户看到一个真实的上游错误，那至少告诉他"不是 tern 的问题"。
pub async fn failover_chain(
    router: &ModelRouter,
    provider_id: &str,
    explicit: bool,
    breakers: &Breakers,
    config: &ResilienceConfig,
) -> Vec<Arc<crate::provider::ProviderSpec>> {
    let circuit = CircuitBreakerConfig::from(config);
    let ordered: Vec<Arc<crate::provider::ProviderSpec>> = if explicit || !config.failover_enabled {
        router.providers_with(provider_id)
    } else {
        // 默认那家排第一，其余按配置顺序跟上
        let mut chain = router.providers_with(provider_id);
        let rest: Vec<_> = router
            .providers()
            .filter(|spec| spec.id != provider_id)
            .cloned()
            .collect();
        chain.extend(rest);
        chain
    };

    if !config.failover_enabled {
        return ordered;
    }

    let mut usable = Vec::with_capacity(ordered.len());
    let mut all_open = true;
    for spec in ordered {
        if breakers.is_available(&spec.id, &circuit).await {
            all_open = false;
            usable.push(spec);
        }
    }
    // 全熔断时放开：见上面"谁会出去"
    if all_open {
        return router.providers_with(provider_id);
    }
    usable
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ApiFormat, ProviderAuth, ProviderSpec};

    fn spec(id: &str) -> ProviderSpec {
        ProviderSpec::new(
            id,
            id,
            format!("https://{id}.example.com"),
            ApiFormat::Anthropic,
            ProviderAuth::api_key("k"),
        )
    }

    /// ModelRouter::new 内部会把每个 spec 包成 Arc。
    fn router_of(ids: &[&str]) -> ModelRouter {
        ModelRouter::new(ids.iter().map(|id| spec(id)).collect(), Some(ids[0].into())).unwrap()
    }

    #[test]
    fn defaults_match_the_cc_switch_tuned_values() {
        let config = ResilienceConfig::default();
        // 这四个数是从 cc-switch 实际使用里收敛出来的，别随手改：
        // threshold 4 = 连炸 4 次才摘，太低会把偶发的 429 当成挂
        assert_eq!(config.failure_threshold, 4);
        assert_eq!(config.success_threshold, 2);
        assert_eq!(config.timeout_seconds, 60);
        assert!(config.failover_enabled);
    }

    /// 配置缺字段时要有能用的默认值 + camelCase 要和 Rust 字段对得上。
    /// 用户手改 tern.json 时只写一部分是常态，解析失败等于整个网关起不来。
    #[test]
    fn a_partial_config_deserializes() {
        let config: ResilienceConfig =
            serde_json::from_str(r#"{"failureThreshold": 2}"#).unwrap();
        assert_eq!(config.failure_threshold, 2);
        assert_eq!(config.success_threshold, 2, "没给的字段用默认值");
        assert!(config.failover_enabled);
    }

    #[tokio::test]
    async fn an_open_breaker_drops_the_provider_from_the_chain() {
        let breakers = Breakers::default();
        let config = ResilienceConfig {
            // 阈值 1：一次失败就熔断
            failure_threshold: 1,
            timeout_seconds: 3600,
            ..ResilienceConfig::default()
        };
        let circuit = CircuitBreakerConfig::from(&config);
        let id = "dead";
        assert!(breakers.is_available(id, &circuit).await, "刚开始是通的");
        breakers.record_failure(id, &circuit).await;
        assert!(
            !breakers.is_available(id, &circuit).await,
            "失败到阈值后应当摘掉"
        );
    }

    /// 半开探测成功要把状态关回去。摘了就再也不摘回来，比"偶发失败"更糟——
    /// 用户会因为一次抖动永久失去一家供应商。
    #[tokio::test]
    async fn successes_in_half_open_close_the_breaker() {
        let breakers = Breakers::default();
        let config = ResilienceConfig {
            failure_threshold: 1,
            success_threshold: 1,
            timeout_seconds: 0,
            ..ResilienceConfig::default()
        };
        let circuit = CircuitBreakerConfig::from(&config);
        breakers.record_failure("p", &circuit).await;
        assert_eq!(breakers.state("p", &circuit).await, CircuitState::Open);
        // timeout 0 → 下次查询就进半开
        assert!(breakers.is_available("p", &circuit).await);
        assert_eq!(breakers.state("p", &circuit).await, CircuitState::HalfOpen);
        breakers.record_success("p", &circuit).await;
        assert_eq!(breakers.state("p", &circuit).await, CircuitState::Closed);
    }

    /// retain 之后被删的供应商不再占内存。不删的话进程活得越久里面的死条目越多。
    #[tokio::test]
    async fn retain_drops_providers_that_no_longer_exist() {
        let breakers = Breakers::default();
        let config = ResilienceConfig::default();
        let circuit = CircuitBreakerConfig::from(&config);
        breakers.record_failure("gone", &circuit).await;
        assert_eq!(breakers.snapshot(&circuit).await.len(), 1);
        breakers.retain(&["alive".to_string()]);
        assert!(
            breakers.snapshot(&circuit).await.is_empty(),
            "被删的供应商不该留下计数"
        );
    }

    /// 显式路由不转移。用户写 `deepseek/xxx` 就是要 deepseek，
    /// 悄悄转给别家比报错更糟——那会让"我明明指定了"变成假话。
    #[tokio::test]
    async fn an_explicit_route_never_fails_over() {
        let router = router_of(&["a", "b"]);
        let breakers = Breakers::default();
        let config = ResilienceConfig {
            failure_threshold: 1,
            timeout_seconds: 3600,
            ..ResilienceConfig::default()
        };
        let circuit = CircuitBreakerConfig::from(&config);
        // 把 a 熔断，但显式指定它的时候照样该只有它
        breakers.record_failure("a", &circuit).await;

        let explicit = failover_chain(&router, "a", true, &breakers, &config).await;
        assert_eq!(explicit.len(), 1);
        assert_eq!(explicit[0].id, "a", "显式指定不转移");
    }

    /// fallback 路由要转移：默认那家不行就换下一家。
    #[tokio::test]
    async fn a_fallback_route_moves_to_the_next_provider() {
        let router = router_of(&["a", "b"]);
        let breakers = Breakers::default();
        let config = ResilienceConfig {
            failure_threshold: 1,
            timeout_seconds: 3600,
            ..ResilienceConfig::default()
        };
        let circuit = CircuitBreakerConfig::from(&config);
        breakers.record_failure("a", &circuit).await;

        let chain = failover_chain(&router, "a", false, &breakers, &config).await;
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].id, "b", "a 挂了应当转到 b");
    }

    /// 全熔断时不该把请求全部拒掉：放开让用户看到真实的上游错误。
    /// 全摘了只会得到一句"都不可用"，而那对他的下一步没有任何帮助。
    #[tokio::test]
    async fn when_everything_is_open_the_chain_falls_back_to_the_original() {
        let router = router_of(&["a"]);
        let breakers = Breakers::default();
        let config = ResilienceConfig {
            failure_threshold: 1,
            timeout_seconds: 3600,
            ..ResilienceConfig::default()
        };
        let circuit = CircuitBreakerConfig::from(&config);
        breakers.record_failure("a", &circuit).await;

        let chain = failover_chain(&router, "a", false, &breakers, &config).await;
        assert_eq!(chain.len(), 1, "全熔断也要放一个出去，别让请求无声失败");
        assert_eq!(chain[0].id, "a");
    }

    /// 关掉开关时回到老行为：不转移、不熔断。
    #[tokio::test]
    async fn disabling_failover_restores_the_single_provider_behaviour() {
        let router = router_of(&["a", "b"]);
        let breakers = Breakers::default();
        let config = ResilienceConfig {
            failover_enabled: false,
            ..ResilienceConfig::default()
        };
        let chain = failover_chain(&router, "a", false, &breakers, &config).await;
        assert_eq!(chain.len(), 1, "关掉之后只走默认那家");
        assert_eq!(chain[0].id, "a");
    }
}
