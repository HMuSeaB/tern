//! tern-gateway：面向编码 agent 的本地协议翻译网关。
//!
//! `proxy/` 下的协议转换层原样取自 cc-switch（MIT，见仓库根目录 LICENSE），
//! 模块路径保持 `crate::proxy::...` 不变，以便继续从上游 cherry-pick 修复。

pub mod adapter;
pub mod gateway;
pub mod provider;
// 转换层里仍有不少函数只被 cc-switch 的故障转移 / 用量 / Copilot 优化器使用。
// 加在这里而不是被搬运的文件里，保持它们与上游逐字节一致。
#[allow(dead_code)]
pub mod proxy;
pub mod router;

pub use gateway::{Gateway, GatewayConfig, ManagedToken, TokenProvider, UpstreamProxy};
pub use provider::{ApiFormat, KeyHeader, ProviderAuth, ProviderSpec};
pub use router::{ModelRouter, Route};
