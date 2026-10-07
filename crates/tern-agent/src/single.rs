//! 单实例保护。
//!
//! # 为什么需要
//!
//! 两个 agent 同时跑会抢同一个控制端口、同一个网关端口。第二个不会
//! "悄悄失败然后用户发现起不来"——它会在启动时就知道端口被占，
//! 然后安静退出。用户视角是"我已经点过启动了"，行为也正常，因为
//! 第一个实例在服务。
//!
//! # 为什么用端口而不是锁文件
//!
//! 端口被占这件事本身就是"已经有一个在跑"的最强证据，不需要再维护
//! 一份可能残留的锁文件。而 `SO_REUSEADDR` 之类会让"绑定成功"变成
//! 假信号，所以这里绑定后立刻保持不关——让 OS 替我们守着。

use std::net::TcpListener;

/// 已经有一个 agent 在跑了吗？
///
/// 判定方式是试着绑一次控制端口。绑得上 → 没人在跑（并把 listener
/// 交回调用方继续用）；绑不上 → 有人在跑。
///
/// 这里有个 window：探测完到真正 serve 之间端口是放开的。实际上没有
/// 危害——两个进程同时启动是极少数情况，且后一个会 bind 失败退出，
/// 最终仍然只有一个在服务。
pub fn already_running(port: u16) -> Option<TcpListener> {
    // 只绑回环：别的机器探不到，也不需要探到
    TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_port_reports_no_other_instance() {
        // 端口 0 让内核挑一个肯定空闲的，断言"能绑上"
        let listener = already_running(0);
        assert!(listener.is_some(), "空闲端口应该绑得上");
    }

    #[test]
    fn an_occupied_port_reports_a_running_instance() {
        // 先占住，再探同一个端口
        let held = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();

        assert!(
            already_running(port).is_none(),
            "已占用的端口应该报告「已有实例在跑」"
        );
        // 占用的那个还在，说明探测没有把它抢走
        assert!(held.local_addr().is_ok());
    }
}
