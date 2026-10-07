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
  tern usage [--days <N>] [--by <维度>] [--recent <N>]
                                             查看用量；维度: provider model role client day
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
        days: u32,
        by: Option<String>,
        recent: usize,
    },
    /// 终端界面。给 SSH / 不想开图形界面的时候用。
    Tui { paths: Paths },
    /// 打开桌面面板。单独一个子命令而不是默认动作：它要拉起 webview，
    /// 内存是 TUI 的几十倍，不该被顺手触发。
    Panel,
    /// 起常驻进程：持有网关，没有窗口。面板和它说同一套 HTTP。
    /// 已经在跑就安静退出——那是正常情况，不是错误。
    Agent,
    /// 从 cc-switch 的 SQL 备份导入供应商
    Import { sql: PathBuf },
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
                days = u32::try_from(number(value("天数")?, "--days")?).unwrap_or(u32::MAX)
            }
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
                days,
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
            Command::Panel
        }
        "agent" => {
            no_positional(&positional)?;
            Command::Agent
        }
        "import" => match positional.as_slice() {
            [sql] => Command::Import {
                sql: PathBuf::from(sql),
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
                days: 7,
                by: None,
                recent: 10,
            })
        );
        assert_eq!(
            p(&["usage", "--days", "30", "--by=model", "--recent", "3"]),
            Ok(Command::Usage {
                paths: Paths::default(),
                days: 30,
                by: Some("model".into()),
                recent: 3,
            })
        );
        assert_eq!(
            p(&["usage", "--recent", "0"]),
            Ok(Command::Usage {
                paths: Paths::default(),
                days: 7,
                by: None,
                recent: 0,
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
}
