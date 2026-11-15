//! 面板 ↔ agent：找到它、拉起它、问它。
//!
//! # 为什么面板不再自己持有网关
//!
//! 原来是面板 `spawn` 一个线程跑网关，好处是双击 exe 就能用，坏处有三个：
//!
//! 1. **内存**。Tauri 带 webview，实测约 408 MB。为了"看一眼花了多少"
//!    常驻一个浏览器内核不值
//! 2. **关窗口 = 断网**。用户只是想看一眼用量，不想把流量也关了
//! 3. **状态和窗口绑死**。面板崩了网关跟着没，排查时两件事混在一起
//!
//! 所以网关归 `tern-agent`（一个没有窗口的小进程），面板只当它的遥控器。
//!
//! # 这个模块现在只剩转发
//!
//! HTTP 客户端和 spawn 都搬进 `tern_agent::client` 了——`tern use` / `tern start`
//! 要在命令行里打到同一组端点，而两端必须行为一致：token 怎么带、超时各给多少、
//! 出错时转述上游原话还是自己总结，任何一处漂移都会表现成"面板点能成、命令行跑
//! 不行"，而这种问题极难查。留在这儿的是 `AppError` 那层转换。

use crate::error::{AppError, Result};

/// 与服务端 `GatewayStatus` 逐字段对应。
pub use tern_agent::client::AgentStatus;

/// 与 `tern_agent::control::CONTROL_PORT` 一致。
///
/// 前端有处提示文案要拼这个端口号，所以得有个能引用的常量。
pub const CONTROL_PORT: u16 = tern_agent::client::CONTROL_PORT;

/// anyhow → AppError。客户端的话原样带上：那是用户在这个状态下
/// 唯一能看到的东西，包一层"操作失败"只会把它盖掉。
fn app(error: anyhow::Error) -> AppError {
    AppError::Config(error.to_string())
}

/// 问一次状态。agent 没起来时返回 Err，不自动拉起。
pub fn status() -> Result<AgentStatus> {
    tern_agent::client::status().map_err(app)
}

/// 起网关。agent 不在时会先把它拉起来。
pub fn start_gateway() -> Result<AgentStatus> {
    tern_agent::client::start_gateway().map_err(app)
}

/// 停网关。**不停 agent**：面板只是想知道"流量断了没"，
/// 顺带把常驻进程也杀掉会让下次启动再付一次冷启动的钱。
pub fn stop_gateway() -> Result<AgentStatus> {
    tern_agent::client::stop_gateway().map_err(app)
}

/// 重起网关。导入过供应商之后必须调它，否则跑着的还是旧配置——
/// 用户会以为导入失败了。
pub fn restart_gateway() -> Result<AgentStatus> {
    tern_agent::client::restart_gateway().map_err(app)
}

/// 让 agent 进程退出。装新版本之前用，见 `tern_agent::client::exit_agent` 的说明。
pub fn exit_agent() -> Result<()> {
    tern_agent::client::exit_agent().map_err(app)
}

/// agent 在不在且能答话。
pub fn reachable() -> bool {
    tern_agent::client::reachable()
}

/// 确保 agent 在跑。已经在跑就直接返回，否则 spawn 并等它开始监听。
pub fn ensure_agent() -> Result<()> {
    tern_agent::client::ensure_agent().map_err(app)
}

/// agent 可执行文件的路径。它装在面板旁边。
pub fn agent_path() -> Option<std::path::PathBuf> {
    tern_agent::client::agent_path()
}

/// 找不到 agent 时的那句话。单独暴露是为了能单测。
pub fn missing_agent_message() -> String {
    tern_agent::client::missing_agent_message()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 找不到 agent 时那句话要能让人知道去哪修，而不是一句
    /// "No such file or directory"。
    ///
    /// 只测消息本身：`ensure_agent` 的行为取决于"此刻有没有 agent 在跑"，
    /// 拿它测内容会让测试随开发机的状态时好时坏。
    #[test]
    fn a_missing_agent_reports_how_to_fix_it() {
        let message = missing_agent_message();
        assert!(message.contains("找不到"), "{message}");
        assert!(message.contains("tern-agent"), "{message}");
    }

    /// 端口号两处必须一致：前端提示文案拼的是面板这个常量，
    /// 而 agent 监听的是 control.rs 那个。漂了的表现是"照着提示做也连不上"。
    #[test]
    fn control_port_matches_the_agent() {
        assert_eq!(CONTROL_PORT, tern_agent::control::CONTROL_PORT);
    }
}
