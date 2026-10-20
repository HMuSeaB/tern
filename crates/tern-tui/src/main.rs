//! `tern-tui` 可执行入口。
//!
//! 真正的逻辑在 lib 里，这样 `tern tui` 能在**当前进程内**接管终端
//! （spawn 子进程会另开窗口或在管道里失败）。

use std::path::PathBuf;

fn main() {
    // 手抠 --config / --db：这个入口是给"直接跑 tern-tui.exe"用的，
    // 走完整参数解析会把 tern-cli 那份依赖整个拖进来。规则与 CLI 一致：
    // 显式参数 > 环境变量 > 配置同目录（后半段在 data.rs 里做）
    let value_of = |flag: &str| -> Option<PathBuf> {
        let mut args = std::env::args();
        while let Some(arg) = args.next() {
            if arg == flag {
                return args.next().map(PathBuf::from);
            }
            // --flag=value 形式
            if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
                return Some(PathBuf::from(rest));
            }
        }
        None
    };

    let config = value_of("--config").or_else(|| {
        std::env::var_os("TERN_CONFIG")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    let db = value_of("--db").or_else(|| {
        std::env::var_os("TERN_DB")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });

    if let Err(error) = tern_tui::run(config, db) {
        // 屏幕可能还在 alternate mode。先尽力还原再报错，
        // 否则用户看到的是花屏而不是这句话。
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        eprintln!("tern: {error}");
        std::process::exit(1);
    }
}
