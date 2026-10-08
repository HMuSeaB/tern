//! tern-agent 的入口：一个没有窗口的常驻进程。
//!
//! # 它长什么样
//!
//! 没有窗口、没有托盘、没有终端界面。它在后台跑，持有网关，
//! 等着被面板或 CLI 问一句"跑着没"。
//!
//! # 怎么退出
//!
//! Ctrl+C 或任务管理器结束进程。停机时会把在途的流补记成 aborted，
//! 再让写入线程把队列里剩下的写完——所以正常退出不会丢用量。
//!
//! # 面板怎么找到它
//!
//! 看 tern-agent.exe 是否在自己的 exe 旁边，是就 spawn 它。
//! spawn 时带 `CREATE_NO_WINDOW`：否则每次启动都会闪一个控制台黑框。

use std::sync::Arc;

use tern_agent::control::{self, ControlConfig};
use tern_agent::single;
use tern_agent::{start_gateway, GatewayState};
fn main() {
    // 默认 info；RUST_LOG 可覆盖，如 RUST_LOG=tern_agent=debug
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_target(false)
        .init();

    if let Err(error) = run() {
        eprintln!("错误: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    // 先探端口再做事：已经有一个在跑就别启动了。
    // 这是正常情况而不是错误——用户可能点了两次「启动」，
    // 第一个已经在服务了，安静退出比报错更接近他的预期
    if single::already_running(control::CONTROL_PORT).is_none() {
        log::info!("[agent] 已有实例在跑，退出");
        return Ok(());
    }

    let control_config = ControlConfig::from_config();
    if control_config.token.is_none() {
        log::warn!(
            "[agent] tern.json 未设置 accessToken：本机任何进程都能起停你的网关。\
             建议填一个"
        );
    }

    let state = Arc::new(GatewayState::default());

    // 起网关失败不阻断 agent：用户可能只是配错了，而这时他仍然需要
    // 一个能问状态、能告诉他哪里错了的东西活着。错误由 /api/status
    // 和启动接口带出去
    match start_gateway(&state) {
        Ok(listen) => log::info!("[agent] 网关已启动 {listen}"),
        Err(error) => {
            log::error!("[agent] 网关启动失败: {error}");
            state.note_error(error.to_string());
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let control_state = state.clone();
        let control_task =
            tokio::spawn(async move { control::serve(control_state, control_config).await });

        // 停机信号：Ctrl+C 或任务管理器。收到就停网关——
        // 在途的流会补记 aborted，写入线程把队列排空
        tokio::select! {
            _ = tokio::signal::ctrl_c() => log::info!("[agent] 收到 Ctrl+C"),
            result = control_task => {
                if let Err(error) = result {
                    log::error!("[agent] 控制端异常退出: {error}");
                }
            }
        }
        state.stop();
        Ok(())
    })
}
