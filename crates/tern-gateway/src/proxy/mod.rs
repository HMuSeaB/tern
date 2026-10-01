//! 协议转换与转发基础设施（取自 cc-switch `src-tauri/src/proxy/`）
//!
//! 只收录不依赖 cc-switch 数据库 / Tauri / 设置的模块。
//! forwarder、handlers、provider_router 等胶水层会在第三阶段按
//! `provider/model` 路由模型重写，不从上游搬运。

pub mod body_filter;
pub mod cache_injector;
pub mod circuit_breaker;
pub(crate) mod content_encoding;
pub mod copilot_optimizer;
pub mod error;
pub mod error_mapper;
pub mod gemini_url;
pub mod http_client;
pub mod hyper_client;
pub(crate) mod json_canonical;
pub mod log_codes;
pub mod providers;
pub mod session;
pub(crate) mod sse;
pub(crate) mod switch_lock;
pub mod thinking_budget_rectifier;
pub mod thinking_optimizer;
pub mod thinking_rectifier;
pub(crate) mod tool_media;
pub(crate) mod types;
pub mod usage;

#[allow(unused_imports)]
pub use circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerStats, CircuitState,
};
#[allow(unused_imports)]
pub use error::ProxyError;
#[allow(unused_imports)]
pub use session::{extract_session_id, SessionIdResult, SessionIdSource};
#[allow(unused_imports)]
pub use types::{ProxyConfig, ProxyServerInfo, ProxyStatus};

#[allow(unused_imports)]
pub(crate) use types::*;
