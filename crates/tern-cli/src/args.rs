//! 命令行参数解析。子命令不多，手写比引入 clap 轻。

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

pub const USAGE: &str = "\
tern：面向编码 agent 的本地协议翻译网关

用法:
  tern serve [--listen <地址:端口>]          启动网关并记录用量，Ctrl+C 退出
  tern init  [--force]                       生成样例配置（含随机 accessToken）
  tern check                                 校验配置并列出供应商
  tern tui  [--config <文件>]                终端界面：看账、供应商、网关状态（约 10 MB）
  tern panel                                 打开桌面面板看详细图表（需要更多内存）
  tern agent                                 起常驻进程：无窗口，持有网关（约几 MB）
  tern import <cc-switch.sql>                从 cc-switch 的 SQL 备份导入供应商
  tern usage [--days <N>] [--from <日期>] [--to <日期>] [--by <维度>] [--recent <N>]
                                             查看用量；维度: provider model role client day
                                             --from/--to 钉死账期（YYYY-MM-DD），与 --days 互斥
  tern price list [<模型名>]                 列出价格（给模型名时显示实际匹配到的那条）
  tern price set <模型名> <输入> <输出> [<缓存读> [<缓存写>]]
                                             手填价格，美元 / 百万 token
  tern price rm <模型名>                     删除手填的价格
  tern price sync [--file <api.json>]        从 models.dev 同步价格

通用选项:
  -c, --config <文件>   配置文件，默认 %APPDATA%\\tern\\tern.json，也可用环境变量 TERN_CONFIG
      --db <文件>       用量数据库，默认与配置文件同目录的 usage.db，也可用环境变量 TERN_DB
  -h, --help            显示帮助
  -V, --version         显示版本

日志级别用 RUST_LOG 调整，如 RUST_LOG=debug";

/// 所有子命令都认的路径参数
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Paths {
    pub config: Option<PathBuf>,
    pub db: Option<PathBuf>,
}

/// `tern usage` 看哪个时间范围。
///
/// 两种互斥的写法：`--days` 是滚动窗口（"最近 7 天"，每天都在漂），
/// `--from` / `--to` 是钉死的账期。后者存在的理由是上游发票按账期开——
/// 拿一个滚动窗口去对一张固定日期的票，永远差几天，而差的那几天恰好是
/// 用户最想弄清的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageWindow {
    /// 截至今天的最近 N 天
    LastDays(u32),
    /// 钉死的闭区间，两端都是规范化的 `YYYY-MM-DD`。`to` 为空表示到今天为止
    Between { from: String, to: Option<String> },
}

impl UsageWindow {
    /// 解析成区间 + 给人看的那行标题。
    ///
    /// 今天几号在这里算而不是在解析参数时算：参数解析不该依赖时钟，
    /// 而 `--to` 缺省时的"今天"只有在真要查的那一刻才有意义。
    pub fn resolve(&self) -> Result<(tern_store::DayRange, String), String> {
        match self {
            Self::LastDays(days) => {
                let range = tern_store::DayRange::last_days(*days);
                // 用区间自比而不是 days == 1：两者目前等价，但前者不会因为
                // last_days 的口径微调而说错话
                let label = if range.from == range.to {
                    "今天".to_string()
                } else {
                    format!("最近 {days} 天（{} ~ {}）", range.from, range.to)
                };
                Ok((range, label))
            }
            Self::Between { from, to } => {
                let to = to.clone().unwrap_or_else(|| {
                    chrono::Local::now()
                        .date_naive()
                        .format("%Y-%m-%d")
                        .to_string()
                });
                // 字符串比大小在这里是安全的：parse_day 保证两端都是零填充的
                // 规范形式。倒挂的区间在 SQLite 里查不出错，只是静默返回空——
                // 而"这个账期没有用量"会被用户当成结论，所以必须挡在门口
                if from > &to {
                    return Err(format!("--from {from} 晚于 --to {to}，区间是空的"));
                }
                Ok((
                    tern_store::DayRange::between(from.clone(), to.clone()),
                    format!("{from} ~ {to}"),
                ))
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Serve {
        paths: Paths,
        listen: Option<SocketAddr>,
    },
    Init {
        paths: Paths,
        force: bool,
    },
    Check {
        paths: Paths,
    },
    Usage {
        paths: Paths,
        window: UsageWindow,
        by: Option<String>,
        recent: usize,
    },
    /// 终端界面。给 SSH / 不想开图形界面的时候用。
    Tui {
        paths: Paths,
    },
    /// 打开桌面面板。单独一个子命令而不是默认动作：它要拉起 webview，
    /// 内存是 TUI 的几十倍，不该被顺手触发。
    ///
    /// 带 `paths`：面板和网关都按 `--config` 找配置，不带着它的话
    /// `tern panel -c 别的文件` 会静默去读默认那份——用户以为改的是 A，实际是 B
    Panel {
        paths: Paths,
    },
    /// 起常驻进程：持有网关，没有窗口。面板和它说同一套 HTTP。
    /// 已经在跑就安静退出——那是正常情况，不是错误。
    Agent {
        paths: Paths,
    },
    /// 从 cc-switch 的 SQL 备份导入供应商。
    ///
    /// 带 `paths`：`-c` 指定的是"导到哪个配置文件"，不带的话会静默写进默认那份——
    /// 用户以为导进了 A，实际落在 B，而 A 还是空的
    Import {
        sql: PathBuf,
        paths: Paths,
    },
    Price {
        paths: Paths,
        action: PriceAction,
    },
    Help,
    Version,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PriceAction {
    List { model: Option<String> },
    Set { model: String, values: Vec<String> },
    Remove { model: String },
    Sync { file: Option<PathBuf> },
}

pub const BREAKDOWNS: &[&str] = &["provider", "model", "role", "client", "day"];

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    let Some(sub) = args.next() else {
        return Ok(Command::Help);
    };
    let sub = sub
        .into_string()
        .map_err(|s| format!("无法识别的子命令 {}", s.to_string_lossy()))?;
    match sub.as_str() {
        "-h" | "--help" | "help" => return Ok(Command::Help),
        "-V" | "--version" => return Ok(Command::Version),
        "serve" | "init" | "check" | "usage" | "price" | "tui" | "panel" | "agent" | "import" => {}
        other => return Err(format!("未知子命令 {other}")),
    }

    // price 的第二级动作
    let action = if sub == "price" {
        match args.peek().map(|a| a.to_string_lossy().into_owned()) {
            Some(a) if matches!(a.as_str(), "list" | "set" | "rm" | "sync") => {
                args.next();
                Some(a)
            }
            Some(a) if a.starts_with('-') => Some("list".to_string()),
            None => Some("list".to_string()),
            Some(other) => {
                return Err(format!("price 没有 {other} 动作（list / set / rm / sync）"))
            }
        }
    } else {
        None
    };
    let scope = action
        .as_deref()
        .map_or(sub.clone(), |a| format!("price {a}"));

    let mut paths = Paths::default();
    let mut listen = None;
    let mut force = false;
    let mut days = 7u32;
    let mut days_given = false;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut by = None;
    let mut recent = 10usize;
    let mut file = None;
    let mut positional: Vec<String> = Vec::new();

    while let Some(arg) = args.next() {
        // 路径参数可能不是合法 UTF-8，值保持 OsString；开关名一定是 ASCII
        let flag = arg.to_string_lossy().into_owned();
        if !flag.starts_with('-') || flag == "-" || is_number(&flag) {
            positional.push(flag);
            continue;
        }
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name.to_string(), Some(value)),
            _ => (flag.clone(), None),
        };
        let mut value = |what: &str| -> Result<OsString, String> {
            match inline {
                Some(v) => Ok(OsString::from(v)),
                None => args.next().ok_or_else(|| format!("{name} 缺少{what}")),
            }
        };
        let number = |raw: OsString, what: &str| -> Result<u64, String> {
            let raw = raw.to_string_lossy();
            raw.parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{what} 必须是正整数: {raw}"))
        };

        match (scope.as_str(), name.as_str()) {
            (_, "-h" | "--help") => return Ok(Command::Help),
            (_, "-c" | "--config") => paths.config = Some(PathBuf::from(value("文件路径")?)),
            (_, "--db") => paths.db = Some(PathBuf::from(value("文件路径")?)),
            ("serve", "--listen") => {
                let raw = value("监听地址")?;
                let raw = raw.to_string_lossy();
                listen =
                    Some(raw.parse().map_err(|_| {
                        format!("--listen 地址无效: {raw}（形如 127.0.0.1:15800）")
                    })?);
            }
            ("init", "--force") if inline.is_none() => force = true,
            ("usage", "--days") => {
                days_given = true;
                days = u32::try_from(number(value("天数")?, "--days")?).unwrap_or(u32::MAX)
            }
            ("usage", "--from") => from = Some(parse_day(&value("日期")?, "--from")?),
            ("usage", "--to") => to = Some(parse_day(&value("日期")?, "--to")?),
            ("usage", "--recent") => {
                // 0 表示不显示最近请求
                let raw = value("条数")?;
                let raw = raw.to_string_lossy();
                recent = raw
                    .parse()
                    .map_err(|_| format!("--recent 必须是非负整数: {raw}"))?;
            }
            ("usage", "--by") => {
                let raw = value("维度")?.to_string_lossy().into_owned();
                if !BREAKDOWNS.contains(&raw.as_str()) {
                    return Err(format!("--by 只支持 {}", BREAKDOWNS.join(" / ")));
                }
                by = Some(raw);
            }
            ("price sync", "--file") => file = Some(PathBuf::from(value("文件路径")?)),
            _ => return Err(format!("{scope} 不支持参数 {flag}")),
        }
    }

    let no_positional = |positional: &[String]| -> Result<(), String> {
        match positional.first() {
            Some(extra) => Err(format!("{scope} 不接受参数 {extra}")),
            None => Ok(()),
        }
    };

    Ok(match sub.as_str() {
        "serve" => {
            no_positional(&positional)?;
            Command::Serve { paths, listen }
        }
        "init" => {
            no_positional(&positional)?;
            Command::Init { paths, force }
        }
        "check" => {
            no_positional(&positional)?;
            Command::Check { paths }
        }
        "usage" => {
            no_positional(&positional)?;
            Command::Usage {
                paths,
                window: usage_window(days, days_given, from, to)?,
                by,
                recent,
            }
        }
        "tui" => {
            no_positional(&positional)?;
            Command::Tui { paths }
        }
        "panel" => {
            no_positional(&positional)?;
            Command::Panel { paths }
        }
        "agent" => {
            no_positional(&positional)?;
            Command::Agent { paths }
        }
        "import" => match positional.as_slice() {
            [sql] => Command::Import {
                sql: PathBuf::from(sql),
                paths,
            },
            _ => return Err("用法: tern import <cc-switch 的 .sql 备份>".into()),
        },
        _ => {
            let action = match action.as_deref() {
                Some("set") => {
                    if !(3..=5).contains(&positional.len()) {
                        return Err(
                            "用法: tern price set <模型名> <输入> <输出> [<缓存读> [<缓存写>]]"
                                .into(),
                        );
                    }
                    let model = positional.remove(0);
                    PriceAction::Set {
                        model,
                        values: positional,
                    }
                }
                Some("rm") => match positional.as_slice() {
                    [model] => PriceAction::Remove {
                        model: model.clone(),
                    },
                    _ => return Err("用法: tern price rm <模型名>".into()),
                },
                Some("sync") => {
                    no_positional(&positional)?;
                    PriceAction::Sync { file }
                }
                _ => {
                    if positional.len() > 1 {
                        return Err("用法: tern price list [<模型名>]".into());
                    }
                    PriceAction::List {
                        model: positional.pop(),
                    }
                }
            };
            Command::Price { paths, action }
        }
    })
}

/// `--days` 和 `--from` / `--to` 是互斥的两条路。
///
/// 同时给就报错，而不是"忽略其中一个"：这是对账用的命令，静默采用一个不是
/// 用户写下的区间，比报错糟得多——用户会拿一份没对上的账单当结论。
fn usage_window(
    days: u32,
    days_given: bool,
    from: Option<String>,
    to: Option<String>,
) -> Result<UsageWindow, String> {
    if days_given && (from.is_some() || to.is_some()) {
        return Err("--days 和 --from / --to 只能选一种：前者是滚动窗口，后者是钉死的账期".into());
    }
    match (from, to) {
        (None, None) => Ok(UsageWindow::LastDays(days)),
        (Some(from), to) => Ok(UsageWindow::Between { from, to }),
        // 只给结尾定不了起点。不猜"那就最近 7 天"——猜错的区间比报错难发现
        (None, Some(to)) => Err(format!("--to {to} 需要同时给 --from：只给结尾定不了起点")),
    }
}

/// 解析 `YYYY-MM-DD`，返回零填充的规范形式。
///
/// 手写而不引 chrono-parser：这台机器 crates.io 常超时，依赖只用本地缓存
/// （和参数解析不用 clap 是同一个理由）。
///
/// 严格校验不是洁癖。日期写错时如果只是"查不到数据"，用户会读成"那个账期
/// 我没有用量"——对账工具最怕的就是这种静默给空。所以 `2026-13-01`、
/// `2026-02-30`、`2026-1-5x` 全都得在门口挡住。
fn parse_day(raw: &OsString, flag: &str) -> Result<String, String> {
    let raw = raw.to_string_lossy();
    let raw = raw.trim();
    let mut parts = raw.split('-');
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("{flag} 要 YYYY-MM-DD 形式: {raw}"));
    };
    // 年份必须四位：少了就没法区分 501 = 0501 还是 501 年，而后面要按字符串比大小
    if year.len() != 4 || !year.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{flag} 的年份要四位数字: {raw}"));
    }
    let number = |part: &str| part.parse::<u32>().ok();
    // 年份走 i32::try_from 而不是 cast_signed：后者 1.87 才稳定，workspace 的
    // MSRV 是 1.85。四位年份不可能溢出，但没必要为一行代码抬高最低支持版本
    let (Some(y), Some(m), Some(d)) = (
        number(year).and_then(|y| i32::try_from(y).ok()),
        number(month),
        number(day),
    ) else {
        return Err(format!("{flag} 要 YYYY-MM-DD 形式: {raw}"));
    };
    chrono::NaiveDate::from_ymd_opt(y, m, d)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .ok_or_else(|| format!("{flag} 的日期不存在: {raw}"))
}

/// `price set m -1 ...` 里的负数要当成位置参数，交给价格校验报错
fn is_number(arg: &str) -> bool {
    arg.strip_prefix('-')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit() || c == '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn no_args_or_help_shows_usage() {
        assert_eq!(p(&[]), Ok(Command::Help));
        assert_eq!(p(&["--help"]), Ok(Command::Help));
        assert_eq!(p(&["serve", "-h"]), Ok(Command::Help));
        assert_eq!(p(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn serve_accepts_config_db_and_listen_in_both_forms() {
        assert_eq!(
            p(&[
                "serve",
                "-c",
                "a.json",
                "--listen=127.0.0.1:1",
                "--db",
                "u.db"
            ]),
            Ok(Command::Serve {
                paths: Paths {
                    config: Some("a.json".into()),
                    db: Some("u.db".into()),
                },
                listen: Some("127.0.0.1:1".parse().unwrap()),
            })
        );
        assert_eq!(
            p(&["serve", "--config=b.json", "--listen", "0.0.0.0:2"]),
            Ok(Command::Serve {
                paths: Paths {
                    config: Some("b.json".into()),
                    db: None,
                },
                listen: Some("0.0.0.0:2".parse().unwrap()),
            })
        );
    }

    #[test]
    fn init_force_and_check() {
        assert_eq!(
            p(&["init", "--force"]),
            Ok(Command::Init {
                paths: Paths::default(),
                force: true
            })
        );
        assert_eq!(
            p(&["check"]),
            Ok(Command::Check {
                paths: Paths::default()
            })
        );
    }

    #[test]
    fn usage_defaults_and_flags() {
        assert_eq!(
            p(&["usage"]),
            Ok(Command::Usage {
                paths: Paths::default(),
                window: UsageWindow::LastDays(7),
                by: None,
                recent: 10,
            })
        );
        assert_eq!(
            p(&["usage", "--days", "30", "--by=model", "--recent", "3"]),
            Ok(Command::Usage {
                paths: Paths::default(),
                window: UsageWindow::LastDays(30),
                by: Some("model".into()),
                recent: 3,
            })
        );
        assert_eq!(
            p(&["usage", "--recent", "0"]),
            Ok(Command::Usage {
                paths: Paths::default(),
                window: UsageWindow::LastDays(7),
                by: None,
                recent: 0,
            })
        );
    }

    #[test]
    fn usage_from_to_pin_a_billing_period() {
        assert_eq!(
            p(&["usage", "--from", "2026-10-01", "--to", "2026-10-31"]),
            Ok(Command::Usage {
                paths: Paths::default(),
                window: UsageWindow::Between {
                    from: "2026-10-01".into(),
                    to: Some("2026-10-31".into()),
                },
                by: None,
                recent: 10,
            })
        );
        // 只给起点：终点留空，resolve() 时填今天
        assert_eq!(
            p(&["usage", "--from=2026-10-01"]),
            Ok(Command::Usage {
                paths: Paths::default(),
                window: UsageWindow::Between {
                    from: "2026-10-01".into(),
                    to: None,
                },
                by: None,
                recent: 10,
            })
        );
    }

    #[test]
    fn usage_rejects_mixing_rolling_window_with_a_pinned_period() {
        // 静默忽略其中一个比报错糟：用户会拿一份没对上的账单当结论
        for args in [
            vec!["usage", "--days", "7", "--from", "2026-10-01"],
            vec!["usage", "--days", "7", "--to", "2026-10-31"],
            vec![
                "usage",
                "--from",
                "2026-10-01",
                "--days",
                "7",
                "--to",
                "2026-10-31",
            ],
        ] {
            assert!(
                p(&args).is_err(),
                "混用 --days 和 --from/--to 应当报错: {args:?}"
            );
        }
    }

    #[test]
    fn usage_to_without_from_has_no_start() {
        assert!(p(&["usage", "--to", "2026-10-31"]).is_err());
    }

    #[test]
    fn usage_rejects_an_inverted_period() {
        let window = UsageWindow::Between {
            from: "2026-10-31".into(),
            to: Some("2026-10-01".into()),
        };
        // 倒挂区间在 SQLite 里不报错，只是静默返回空——那正好是最坏的结果
        assert!(window.resolve().is_err());
    }

    #[test]
    fn usage_dates_must_be_real_and_zero_padded() {
        // 不存在的日期：挡在门口，别让"查不到数据"被读成"这个账期没有用量"
        for bad in ["2026-13-01", "2026-02-30", "2026-00-10", "2026-10-00"] {
            assert!(
                p(&["usage", "--from", bad]).is_err(),
                "{bad} 不是真实存在的日期"
            );
        }
        // 形状不对
        for bad in ["2026-10", "26-10-01", "2026/10/01", "2026-10-01x", ""] {
            assert!(
                p(&["usage", "--from", bad]).is_err(),
                "{bad} 不是 YYYY-MM-DD"
            );
        }
        // 没零填充的也收，但存成规范形式——后面要按字符串比大小
        assert_eq!(
            p(&["usage", "--from", "2026-1-5"]),
            Ok(Command::Usage {
                paths: Paths::default(),
                window: UsageWindow::Between {
                    from: "2026-01-05".into(),
                    to: None,
                },
                by: None,
                recent: 10,
            })
        );
    }

    #[test]
    fn price_actions() {
        let paths = Paths::default;
        assert_eq!(
            p(&["price"]),
            Ok(Command::Price {
                paths: paths(),
                action: PriceAction::List { model: None }
            })
        );
        assert_eq!(
            p(&["price", "list", "relay/claude-opus-5"]),
            Ok(Command::Price {
                paths: paths(),
                action: PriceAction::List {
                    model: Some("relay/claude-opus-5".into())
                }
            })
        );
        assert_eq!(
            p(&["price", "set", "step-5", "0.2", "0.8", "0.04"]),
            Ok(Command::Price {
                paths: paths(),
                action: PriceAction::Set {
                    model: "step-5".into(),
                    values: vec!["0.2".into(), "0.8".into(), "0.04".into()],
                }
            })
        );
        assert_eq!(
            p(&["price", "set", "m", "-1", "2"]),
            Ok(Command::Price {
                paths: paths(),
                action: PriceAction::Set {
                    model: "m".into(),
                    values: vec!["-1".into(), "2".into()],
                }
            })
        );
        assert_eq!(
            p(&["price", "rm", "m"]),
            Ok(Command::Price {
                paths: paths(),
                action: PriceAction::Remove { model: "m".into() }
            })
        );
        assert_eq!(
            p(&["price", "sync", "--file", "api.json", "-c", "t.json"]),
            Ok(Command::Price {
                paths: Paths {
                    config: Some("t.json".into()),
                    db: None
                },
                action: PriceAction::Sync {
                    file: Some("api.json".into())
                }
            })
        );
    }

    #[test]
    fn rejects_unknown_or_misplaced_flags() {
        for args in [
            &["serve", "--force"][..],
            &["check", "--listen", "127.0.0.1:1"],
            &["init", "--force=yes"],
            &["serve", "--listen", "localhost"],
            &["serve", "--config"],
            &["serve", "extra"],
            &["usage", "--days", "0"],
            &["usage", "--by", "week"],
            &["price", "set", "m", "1"],
            &["price", "rm"],
            &["price", "list", "a", "b"],
            &["price", "list", "--file", "x"],
            &["price", "delete", "m"],
            &["start"],
        ] {
            assert!(p(args).is_err(), "{args:?}");
        }
    }

    /// `-c` / `--db` 是通用选项，每个子命令都该把它带下去。
    ///
    /// 这条测试存在的原因：`Panel` / `Agent` / `Import` 曾经没有 `paths` 字段，
    /// flag 解析成功、退出码 0、然后被静默丢掉。那种失败在单元测试里看不见——
    /// 只有断言"解析出来的 Command 里到底有没有那个路径"才能抓住。
    #[test]
    fn every_subcommand_carries_the_paths_it_was_given() {
        // 写成 fn 而不是闭包：闭包的返回类型生命周期推不出来，fn 的省略规则
        // 会把输入的生命期直接传给输出，正是要的
        fn paths_of(command: &Command) -> Option<&Paths> {
            Some(match command {
                Command::Serve { paths, .. }
                | Command::Init { paths, .. }
                | Command::Check { paths }
                | Command::Tui { paths }
                | Command::Panel { paths }
                | Command::Agent { paths }
                | Command::Import { paths, .. }
                | Command::Usage { paths, .. }
                | Command::Price { paths, .. } => paths,
                // 这两个本来就不读配置，不该有 paths
                Command::Help | Command::Version => return None,
            })
        }

        let subcommands: &[&[&str]] = &[
            &["serve"],
            &["init"],
            &["check"],
            &["tui"],
            &["panel"],
            &["agent"],
            &["import", "x.sql"],
            &["usage"],
            &["price", "list"],
        ];

        for args in subcommands {
            for flag in ["--config", "--db"] {
                let mut argv = args.to_vec();
                argv.extend_from_slice(&[flag, "given.json"]);
                let command = p(&argv).unwrap_or_else(|e| panic!("{argv:?} 应当解析成功: {e}"));
                let paths =
                    paths_of(&command).unwrap_or_else(|| panic!("{argv:?} 解析结果里没有 paths"));
                let got = if flag == "--config" {
                    paths.config.as_deref()
                } else {
                    paths.db.as_deref()
                };
                assert_eq!(
                    got,
                    Some(std::path::Path::new("given.json")),
                    "{argv:?} 把 {flag} 丢掉了"
                );
            }
        }
    }
}
