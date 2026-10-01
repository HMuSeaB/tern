//! tern-gateway：面向编码 agent 的本地协议翻译网关。
//!
//! `proxy/` 下的协议转换层原样取自 cc-switch（MIT，见仓库根目录 LICENSE），
//! 模块路径保持 `crate::proxy::...` 不变，以便继续从上游 cherry-pick 修复。

pub mod provider;
// 调用方（forwarder / handlers）尚未重写，转换层里大量函数暂时无人引用。
// 加在这里而不是被搬运的文件里，保持它们与上游逐字节一致。
#[allow(dead_code)]
pub mod proxy;
