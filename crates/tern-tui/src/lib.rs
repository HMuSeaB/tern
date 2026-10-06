//! tern 的终端界面。
//!
//! # 为什么有这一个
//!
//! Tauri 面板实测 408 MB（主进程 31 MB + 6 个 WebView2 进程 377 MB）。"比 Electron
//! 省"是对的，比起终端仍是数量级差距——而网关平时就该静静跑着，不该为了看一眼账
//! 常驻一份 Chromium。
//!
//! 所以拆成两层：这一个常驻（约 10 MB），详细图表交给 `tern panel` 按需开。
//! magpie 也是这个路子（托盘面板 / `magpie tui` 常驻，完整窗口按需开）。
//!
//! # 界面
//!
//! 三页，`1`/`2`/`3` 或 `Tab` 切换，`q` 退出：
//!
//! - **今日**：花了多少、命中率、四个 token 桶、sparkline、未定价与失败提示
//! - **供应商**：一行一个，标出第三方网关（联网工具会失效）和本机回环地址（套了两层网关）
//! - **网关**：监听状态、给 Claude Code 的环境变量照抄即用

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;

mod data;
mod pages;

pub use data::Snapshot;
use data::Snapshot as Snap;
use pages::{draw_gateway, draw_providers, draw_today};

/// 数据刷新间隔。网关自己会写库，这里只负责看，不需要紧跟每个请求。
const REFRESH: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Today,
    Providers,
    Gateway,
}

impl Page {
    fn next(self) -> Self {
        match self {
            Page::Today => Page::Providers,
            Page::Providers => Page::Gateway,
            Page::Gateway => Page::Today,
        }
    }

    fn prev(self) -> Self {
        match self {
            Page::Today => Page::Gateway,
            Page::Providers => Page::Today,
            Page::Gateway => Page::Providers,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Today => "今日",
            Page::Providers => "供应商",
            Page::Gateway => "网关",
        }
    }
}

/// 跑起来，占用当前终端。返回时终端已还原。
pub fn run(config_path: Option<PathBuf>) -> io::Result<()> {
    let mut terminal = setup_terminal()?;
    let result = event_loop(&mut terminal, config_path.as_deref());
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(out))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}

/// 出错时把终端还原。单独一个函数是因为事件循环里任何一步都可能失败，
/// 那时屏幕还在 alternate mode，不还原用户看到的就是花屏。
fn restore_quietly() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    config_path: Option<&Path>,
) -> io::Result<()> {
    let mut page = Page::Today;
    let mut snapshot = Snap::load(config_path);
    let mut last_refresh = Instant::now();

    loop {
        terminal.draw(|frame| render(frame, page, &snapshot))?;

        // 到期就重新读库。用 poll 的 timeout 而不是单独起计时线程：
        // 事件和刷新都在一个循环里，不存在谁等谁。
        let timeout = REFRESH.saturating_sub(last_refresh.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                // Windows 上按下和抬起都会报一次，只认按下，否则一次按键翻两页
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => page = page.next(),
                    KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => page = page.prev(),
                    KeyCode::Char('1') => page = Page::Today,
                    KeyCode::Char('2') => page = Page::Providers,
                    KeyCode::Char('3') => page = Page::Gateway,
                    KeyCode::Char('r') => {
                        snapshot = Snap::load(config_path);
                        last_refresh = Instant::now();
                    }
                    _ => {}
                }
            }
        }
        if last_refresh.elapsed() >= REFRESH {
            snapshot = Snap::load(config_path);
            last_refresh = Instant::now();
        }
    }
}

fn render(frame: &mut Frame, page: Page, snapshot: &Snapshot) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    draw_header(frame, chunks[0], page, snapshot);
    match page {
        Page::Today => draw_today(frame, chunks[1], snapshot),
        Page::Providers => draw_providers(frame, chunks[1], snapshot),
        Page::Gateway => draw_gateway(frame, chunks[1], snapshot),
    }
    draw_footer(frame, chunks[2]);
}

fn draw_header(frame: &mut Frame, area: Rect, current: Page, snapshot: &Snapshot) {
    let mut spans = vec![Span::styled(
        " tern ",
        Style::default().fg(Color::Cyan).bold(),
    )];

    for (index, page) in [Page::Today, Page::Providers, Page::Gateway]
        .into_iter()
        .enumerate()
    {
        let label = format!(" {} {} ", index + 1, page.title());
        spans.push(Span::styled(
            label,
            if page == current {
                Style::default().fg(Color::Black).bg(Color::Cyan).bold()
            } else {
                Style::default().fg(Color::Gray)
            },
        ));
    }

    // 状态靠右：一眼看到"今天花了多少"或"为什么没数据"
    let status = snapshot.status_text();
    let width = area.width as usize;
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let pad = width.saturating_sub(used + status.chars().count() + 1);
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(status, Style::default().fg(Color::DarkGray)));

    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(
        ratatui::widgets::Paragraph::new(Line::from(spans)).block(block),
        area,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect) {
    let line = Line::from(Span::styled(
        " 1-3/Tab 切页   r 刷新   q 退出",
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(ratatui::widgets::Paragraph::new(line), area);
}
