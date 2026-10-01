//! 命令行参数解析。只有三个子命令，手写比引入 clap 轻。

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

pub const USAGE: &str = "\
tern：面向编码 agent 的本地协议翻译网关

用法:
  tern serve [--config <文件>] [--listen <地址:端口>]   启动网关，Ctrl+C 退出
  tern init  [--config <文件>] [--force]               生成样例配置（含随机 accessToken）
  tern check [--config <文件>]                         校验配置并列出供应商

选项:
  -c, --config <文件>   配置文件，默认 %APPDATA%\\tern\\tern.json，也可用环境变量 TERN_CONFIG
      --listen <地址>   覆盖配置里的监听地址，如 127.0.0.1:15801
      --force           init 时覆盖已存在的配置文件
  -h, --help            显示帮助
  -V, --version         显示版本

日志级别用 RUST_LOG 调整，如 RUST_LOG=debug";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Serve {
        config: Option<PathBuf>,
        listen: Option<SocketAddr>,
    },
    Init {
        config: Option<PathBuf>,
        force: bool,
    },
    Check {
        config: Option<PathBuf>,
    },
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let Some(sub) = args.next() else {
        return Ok(Command::Help);
    };
    let sub = sub
        .into_string()
        .map_err(|s| format!("无法识别的子命令 {}", s.to_string_lossy()))?;

    let mut config = None;
    let mut listen = None;
    let mut force = false;

    match sub.as_str() {
        "-h" | "--help" | "help" => return Ok(Command::Help),
        "-V" | "--version" => return Ok(Command::Version),
        "serve" | "init" | "check" => {}
        other => return Err(format!("未知子命令 {other}")),
    }

    while let Some(arg) = args.next() {
        // 路径参数可能不是合法 UTF-8，值保持 OsString；开关名一定是 ASCII
        let flag = arg.to_string_lossy().into_owned();
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

        match (sub.as_str(), name.as_str()) {
            (_, "-h" | "--help") => return Ok(Command::Help),
            (_, "-c" | "--config") => config = Some(PathBuf::from(value("文件路径")?)),
            ("serve", "--listen") => {
                let raw = value("监听地址")?;
                let raw = raw.to_string_lossy();
                listen =
                    Some(raw.parse().map_err(|_| {
                        format!("--listen 地址无效: {raw}（形如 127.0.0.1:15800）")
                    })?);
            }
            ("init", "--force") if inline.is_none() => force = true,
            _ => return Err(format!("{sub} 不支持参数 {flag}")),
        }
    }

    Ok(match sub.as_str() {
        "serve" => Command::Serve { config, listen },
        "init" => Command::Init { config, force },
        _ => Command::Check { config },
    })
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
    fn serve_accepts_config_and_listen_in_both_forms() {
        assert_eq!(
            p(&["serve", "-c", "a.json", "--listen=127.0.0.1:1"]),
            Ok(Command::Serve {
                config: Some("a.json".into()),
                listen: Some("127.0.0.1:1".parse().unwrap()),
            })
        );
        assert_eq!(
            p(&["serve", "--config=b.json", "--listen", "0.0.0.0:2"]),
            Ok(Command::Serve {
                config: Some("b.json".into()),
                listen: Some("0.0.0.0:2".parse().unwrap()),
            })
        );
    }

    #[test]
    fn init_force_and_check() {
        assert_eq!(
            p(&["init", "--force"]),
            Ok(Command::Init {
                config: None,
                force: true
            })
        );
        assert_eq!(p(&["check"]), Ok(Command::Check { config: None }));
    }

    #[test]
    fn rejects_unknown_or_misplaced_flags() {
        for args in [
            &["serve", "--force"][..],
            &["check", "--listen", "127.0.0.1:1"],
            &["init", "--force=yes"],
            &["serve", "--listen", "localhost"],
            &["serve", "--config"],
            &["start"],
        ] {
            assert!(p(args).is_err(), "{args:?}");
        }
    }
}
