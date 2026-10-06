//! 三页的绘制。只读快照，不碰磁盘、不发请求——数据由 `data::Snapshot` 一次备好。

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::data::Snapshot;

const CYAN: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const WARN: Color = Color::Yellow;
const BAD: Color = Color::Red;
const OK: Color = Color::Green;

pub fn draw_today(frame: &mut Frame, area: ratatui::layout::Rect, snapshot: &Snapshot) {
    let Some(today) = snapshot.today.as_ref() else {
        empty_state(frame, area, &snapshot.status_text(), "先跑 `tern serve`，网关会开始记账");
        return;
    };

    let total = today.fresh_input + today.output + today.cache_read + today.cache_write;
    let hit = cache_hit_rate(today);

    let mut lines: Vec<Line> = Vec::new();

    // hero：花费。一天最想知道的就是这个数
    lines.push(Line::from(vec![
        Span::styled("  今天花了 ", Style::default().fg(DIM)),
        Span::styled(
            format!("${:.2}", today.cost),
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            delta_suffix(today.cost, snapshot.yesterday.as_ref().map(|y| y.cost)),
            Style::default().fg(DIM),
        ),
    ]));
    lines.push(Line::from(""));

    lines.push(Line::from(vec![
        Span::styled("  Token ", Style::default().fg(DIM)),
        Span::raw(tokens(total)),
        Span::styled("   ", Style::default()),
        Span::styled("请求 ", Style::default().fg(DIM)),
        Span::raw(format!("{}", today.requests)),
        Span::styled("   ", Style::default()),
        Span::styled("缓存命中 ", Style::default().fg(DIM)),
        Span::raw(format!("{hit:.1}%")),
        Span::styled("   ", Style::default()),
        Span::styled("省下 ", Style::default().fg(DIM)),
        Span::styled(
            format!("${:.2}", today.cache_savings),
            Style::default().fg(OK),
        ),
    ]));

    // 失败与未定价跟在汇总后面：这两个数字不显眼，成本就会被高估/低估
    if today.failures > 0 || today.unpriced > 0 {
        let mut notes = Vec::new();
        if today.failures > 0 {
            notes.push(format!("失败 {}", today.failures));
        }
        if today.unpriced > 0 {
            notes.push(format!("未定价 {}", today.unpriced));
        }
        lines.push(Line::from(Span::styled(
            format!("  （{}，未计入上方成本）", notes.join(" · ")),
            Style::default().fg(DIM),
        )));
    }
    lines.push(Line::from(""));

    // sparkline：最近 30 天
    if !snapshot.spend_series.is_empty() {
        lines.push(Line::from(Span::styled("  近 30 天", Style::default().fg(DIM))));
        lines.push(Line::from(Span::styled(
            format!("  {}", sparkline(&snapshot.spend_series)),
            Style::default().fg(CYAN),
        )));
        lines.push(Line::from(""));
    }

    // 四个桶
    lines.push(Line::from(vec![
        Span::styled("  新增输入 ", Style::default().fg(DIM)),
        Span::raw(pad(tokens(today.fresh_input), 10)),
        Span::styled("  Output ", Style::default().fg(DIM)),
        Span::raw(pad(tokens(today.output), 10)),
        Span::styled("  缓存写 ", Style::default().fg(DIM)),
        Span::raw(pad(tokens(today.cache_write), 10)),
        Span::styled("  缓存读 ", Style::default().fg(DIM)),
        Span::raw(tokens(today.cache_read)),
    ]));
    lines.push(Line::from(""));

    if !snapshot.unpriced.is_empty() {
        let names: Vec<String> = snapshot
            .unpriced
            .iter()
            .take(3)
            .map(|(model, _, tokens)| format!("{model}({})", tokens_short(*tokens)))
            .collect();
        lines.push(Line::from(Span::styled(
            format!("  ! 未定价 {}：{}", snapshot.unpriced.len(), names.join(" ")),
            Style::default().fg(WARN),
        )));
        lines.push(Line::from(Span::styled(
            "    用 `tern price set <模型> <输入价> <输出价>` 补上",
            Style::default().fg(DIM),
        )));
    }
    if !snapshot.failures.is_empty() {
        let total_failed: u64 = snapshot.failures.iter().map(|(_, _, _, n)| n).sum();
        let top = &snapshot.failures[0];
        lines.push(Line::from(Span::styled(
            format!(
                "  ! 今天失败 {} 次，最多：{}（{} {}）",
                total_failed, top.1, top.2, top.3
            ),
            Style::default().fg(BAD),
        )));
        lines.push(Line::from(Span::styled(
            "    失败单独归类，不计入上面的模型分布",
            Style::default().fg(DIM),
        )));
    }
    if !snapshot.recent.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("  最近", Style::default().fg(DIM))));
        for row in snapshot.recent.iter().take(8) {
            let outcome_color = match row.outcome.as_str() {
                "failed" => BAD,
                "aborted" => WARN,
                _ => DIM,
            };
            let outcome_text = match row.outcome.as_str() {
                "failed" => "失败",
                "aborted" => "中断",
                _ => "成功",
            };
            let mut spans = vec![
                Span::styled(format!("  {} ", row.time), Style::default().fg(DIM)),
                Span::styled(format!("{:<6}", row.client), Style::default().fg(DIM)),
                Span::styled(
                    truncate(&row.model, 34),
                    if row.remapped {
                        Style::default().fg(WARN)
                    } else {
                        Style::default()
                    },
                ),
                Span::raw("  "),
                Span::styled(
                    match row.cost {
                        Some(cost) => format!("${cost:.4}"),
                        None => "未定价".to_string(),
                    },
                    Style::default().fg(if row.cost.is_some() { DIM } else { WARN }),
                ),
            ];
            if row.outcome != "success" {
                spans.push(Span::styled(
                    format!("  {outcome_text}"),
                    Style::default().fg(outcome_color),
                ));
            } else {
                spans.push(Span::styled(
                    format!("  {}", tokens_short(row.tokens)),
                    Style::default().fg(DIM),
                ));
            }
            lines.push(Line::from(spans));
        }
    }

    frame.render_widget(Paragraph::new(lines), area);
}

pub fn draw_providers(frame: &mut Frame, area: ratatui::layout::Rect, snapshot: &Snapshot) {
    if snapshot.providers.is_empty() {
        empty_state(frame, area, "还没有供应商", "用 cc-switch 导出一份 SQL，让 tern 导入；或手写 tern.json");
        return;
    }

    let items: Vec<ListItem> = snapshot
        .providers
        .iter()
        .map(|provider| {
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(
                    format!("{:<28}", truncate(&provider.id, 28)),
                    Style::default().add_modifier(if provider.is_default {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<16}", provider.api_format),
                    Style::default().fg(DIM),
                ),
                Span::raw(" "),
                Span::raw(truncate(&provider.base_url, area.width.saturating_sub(60) as usize)),
            ];
            if provider.web_tools_at_risk {
                spans.push(Span::styled("  联网工具失效", Style::default().fg(WARN)));
            }
            if provider.loopback {
                spans.push(Span::styled("  本机地址", Style::default().fg(BAD)));
            }
            if provider.is_default {
                spans.push(Span::styled("  默认", Style::default().fg(CYAN)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let header = Line::from(Span::styled(
        format!("  {:<28} {:<16} 地址", "id", "协议"),
        Style::default().fg(DIM),
    ));
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(DIM))
            .title(header),
    );
    frame.render_widget(list, area);
}

pub fn draw_gateway(frame: &mut Frame, area: ratatui::layout::Rect, snapshot: &Snapshot) {
    let listen = snapshot
        .listen
        .clone()
        .unwrap_or_else(|| "127.0.0.1:15800".into());

    let mut lines = vec![
        Line::from(Span::styled(
            "  把 Claude Code 指到 tern：",
            Style::default().fg(DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("    $env:ANTHROPIC_BASE_URL = \"http://{listen}\""),
            Style::default().fg(CYAN),
        )),
        Line::from(Span::styled(
            "    $env:ANTHROPIC_MODEL = \"供应商/模型\"",
            Style::default().fg(CYAN),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  这个界面只读数据，不转发请求。网关由 `tern serve` 或桌面应用启动。",
            Style::default().fg(DIM),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("  用量库 {}", snapshot.db_path.display()),
            Style::default().fg(DIM),
        )),
        Line::from(Span::styled(
            format!("  配置文件 {}", snapshot.config_path.display()),
            Style::default().fg(DIM),
        )),
    ];

    if let Some(default) = &snapshot.default_provider {
        lines.push(Line::from(Span::styled(
            format!("  默认供应商 {default}"),
            Style::default().fg(DIM),
        )));
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

// ---- 小工具 ----

fn empty_state(frame: &mut Frame, area: ratatui::layout::Rect, title: &str, hint: &str) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {title}"),
            Style::default().fg(WARN),
        )),
        Line::from(""),
        Line::from(Span::styled(format!("  {hint}"), Style::default().fg(DIM))),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn cache_hit_rate(today: &crate::data::DayTotals) -> f64 {
    let input = today.fresh_input + today.cache_read + today.cache_write;
    if input == 0 {
        return 0.0;
    }
    today.cache_read as f64 / input as f64 * 100.0
}

/// 与 CLI 的 `report::tokens` 同一口径：1.23M / 45.6K / 820
fn tokens(n: u64) -> String {
    tokens_short(n)
}

fn tokens_short(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn pad(s: String, width: usize) -> String {
    format!("{s:<width$}")
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    // 中文一个字符占两列，但这里只按字符数截断：
    // 截太准反而要在每处都传宽度，先保证不溢出错乱
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Unicode 方块 sparkline。和 magpie 的 TUI 同一套字符：` ▁▂▃▄▅▆▇█`
fn sparkline(values: &[f64]) -> String {
    const BARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().fold(0.0f64, f64::max);
    if max <= 0.0 {
        return BARS[0].to_string().repeat(values.len());
    }
    values
        .iter()
        .map(|v| {
            let level = ((v / max) * 8.0).round() as usize;
            BARS[level.min(8)]
        })
        .collect()
}

fn delta_suffix(today: f64, yesterday: Option<f64>) -> String {
    let Some(yesterday) = yesterday else {
        return "  昨天没有数据".into();
    };
    if yesterday <= 0.0 {
        return "  昨天 $0".into();
    }
    let change = (today - yesterday) / yesterday * 100.0;
    let arrow = if change >= 0.0 { "+" } else { "" };
    format!("  {arrow}{change:.0}%  较昨日 ${yesterday:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_short_matches_the_cli_report() {
        assert_eq!(tokens(820), "820");
        assert_eq!(tokens(45_600), "45.6K");
        assert_eq!(tokens(1_230_000), "1.23M");
    }

    #[test]
    fn sparkline_scales_to_the_peak() {
        // 全零时不能除零，退化成一片空格
        assert_eq!(sparkline(&[0.0, 0.0]), "  ");
        // 峰值是满格，中间值按比例落在中间某档
        let line = sparkline(&[0.0, 5.0, 10.0]);
        assert_eq!(line.chars().count(), 3);
        assert!(line.starts_with(' '), "{line}");
        assert!(line.ends_with('█'), "{line}");
    }

    #[test]
    fn truncate_never_exceeds_the_column() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        assert_eq!(truncate("任何长度", 0), "");
    }
}
