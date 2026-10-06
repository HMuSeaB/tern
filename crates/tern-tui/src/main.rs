//! `tern-tui` 可执行入口。
//!
//! 真正的逻辑在 lib 里，这样 `tern tui` 能在**当前进程内**接管终端
//! （spawn 子进程会另开窗口或在管道里失败）。

use std::path::PathBuf;

fn main() {
    // --config 优先，其次 TERN_CONFIG，最后 %APPDATA%\tern\tern.json
    let config = std::env::args()
        .skip_while(|arg| arg != "--config")
        .nth(1)
        .map(PathBuf::from);

    if let Err(error) = tern_tui::run(config) {
        // 屏幕可能还在 alternate mode。先尽力还原再报错，
        // 否则用户看到的是花屏而不是这句话。
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        eprintln!("tern: {error}");
        std::process::exit(1);
    }
}
