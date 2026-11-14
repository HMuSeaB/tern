//! 外挂插件的 token 命令（Step 1，见
//! `docs/plugins-external-process-design-zh.md` §7）。
//!
//! 网关只管“按需取 token”，登录 / 刷新逻辑放在外部命令里：
//!
//! - 配置：供应商的 `auth` 写 `{"type": "plugin_command", "command": "...",
//!   "args": [...]}`。命令不经过 shell，参数原样传入。
//! - 契约：每次需要 token 就运行一次命令；命令必须在 5 秒内退出 0，
//!   并在 stdout 输出一个 JSON 对象：
//!   `{"token": "...", "account_id": "..."}`（`account_id` 可选）。
//!   输出不是合法 JSON、token 为空、退出码非 0、超时，一律报 `AuthError`。
//! - 安全：token 永不原样进日志，只记前 4 后 4（和 `ProviderAuth` 的
//!   `Debug` 遮蔽同一条规则）；stderr 只截取前 500 字符放进错误信息，
//!   且命令本身应当只把诊断写进 stderr、绝不把 token 写进去。
//! - 这是通用基础设施，不是某一家服务的登录实现：不实现 WorkBuddy
//!   或任何真实服务的端点 / 签名。

use std::process::Stdio;
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use crate::gateway::{GatewayConfig, ManagedToken, TokenProvider};
use crate::provider::{mask_secret, ProviderAuth, ProviderSpec};
use crate::proxy::ProxyError;

/// 默认超时：命令 5 秒内必须返回 token（设计稿 §7 Step 1）。
pub const DEFAULT_TOKEN_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// stdout 最多读 1 MiB：token 命令的输出本该只有一行 JSON，超了就是异常。
const MAX_STDOUT_BYTES: u64 = 1024 * 1024;
/// stderr 只保留前 64 KiB，错误信息里再截到 500 字符。
const MAX_STDERR_BYTES: u64 = 64 * 1024;
const MAX_STDERR_CHARS_IN_ERROR: usize = 500;

/// `TokenProvider` 的一种：每次需要 token 就运行一次外部命令。
///
/// 无状态，可多线程共享；超时时间可配（默认 5 秒），单测里用短超时。
#[derive(Debug, Clone)]
pub struct CommandTokenProvider {
    timeout: Duration,
}

impl Default for CommandTokenProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandTokenProvider {
    pub fn new() -> Self {
        Self {
            timeout: DEFAULT_TOKEN_COMMAND_TIMEOUT,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }

    /// 配置里有没有 `PluginCommand` 供应商：有才需要接这个 provider，
    /// 没有时网关行为和原来完全一样。
    pub fn needed_for(config: &GatewayConfig) -> bool {
        config
            .providers
            .iter()
            .any(|spec| matches!(spec.auth, ProviderAuth::PluginCommand { .. }))
    }

    async fn fetch(
        &self,
        provider_id: &str,
        command: &str,
        args: &[String],
    ) -> Result<ManagedToken, ProxyError> {
        if command.trim().is_empty() {
            return Err(ProxyError::AuthError(format!(
                "供应商 {provider_id} 的插件 token 命令为空"
            )));
        }
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                ProxyError::AuthError(format!(
                    "供应商 {provider_id} 的插件 token 命令启动失败 ({command})：{error}"
                ))
            })?;
        let mut stdout = child.stdout.take().expect("stdout 已设为 piped");
        let mut stderr = child.stderr.take().expect("stderr 已设为 piped");
        let wait = async {
            let mut out_buf = Vec::new();
            let mut err_buf = Vec::new();
            let mut out_limited = (&mut stdout).take(MAX_STDOUT_BYTES);
            let mut err_limited = (&mut stderr).take(MAX_STDERR_BYTES);
            let (read_out, read_err, status) = tokio::join!(
                out_limited.read_to_end(&mut out_buf),
                err_limited.read_to_end(&mut err_buf),
                child.wait(),
            );
            (read_out.map(|_| out_buf), read_err.map(|_| err_buf), status)
        };
        let (read_out, read_err, status) = match tokio::time::timeout(self.timeout, wait).await {
            Ok(result) => result,
            Err(_) => {
                // 超时：杀掉子进程再等它退出，不留孤儿进程
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(ProxyError::AuthError(format!(
                    "供应商 {provider_id} 的插件 token 命令超时（{}s）",
                    self.timeout.as_secs()
                )));
            }
        };
        let (stdout_bytes, stderr_bytes, status) = match (read_out, read_err, status) {
            (Ok(out), Ok(err), Ok(status)) => (out, err, status),
            (Err(error), _, _) | (_, Err(error), _) => {
                return Err(ProxyError::AuthError(format!(
                    "供应商 {provider_id} 的插件 token 命令读取输出失败：{error}"
                )));
            }
            (_, _, Err(error)) => {
                return Err(ProxyError::AuthError(format!(
                    "供应商 {provider_id} 的插件 token 命令等待退出失败：{error}"
                )));
            }
        };
        if !status.success() {
            let stderr_text = String::from_utf8_lossy(&stderr_bytes);
            let stderr_text: String = stderr_text
                .chars()
                .take(MAX_STDERR_CHARS_IN_ERROR)
                .collect();
            return Err(ProxyError::AuthError(format!(
                "供应商 {provider_id} 的插件 token 命令退出码 {}：{}",
                status.code().map_or("-".to_string(), |c| c.to_string()),
                stderr_text.trim(),
            )));
        }
        let stdout_text = String::from_utf8_lossy(&stdout_bytes);
        let parsed: TokenOutput = serde_json::from_str(stdout_text.trim()).map_err(|error| {
            ProxyError::AuthError(format!(
                "供应商 {provider_id} 的插件 token 命令输出不是合法 JSON：{error}"
            ))
        })?;
        let token = parsed.token.trim().to_string();
        if token.is_empty() {
            return Err(ProxyError::AuthError(format!(
                "供应商 {provider_id} 的插件 token 命令返回的 token 为空"
            )));
        }
        log::info!(
            "[PluginToken] 供应商 {provider_id} 已取到 token {}",
            mask_secret(&token)
        );
        Ok(ManagedToken {
            token,
            account_id: parsed.account_id.filter(|id| !id.trim().is_empty()),
        })
    }
}

impl TokenProvider for CommandTokenProvider {
    fn token<'a>(
        &'a self,
        spec: &'a ProviderSpec,
    ) -> BoxFuture<'a, Result<ManagedToken, ProxyError>> {
        Box::pin(async move {
            let (command, args) = match &spec.auth {
                ProviderAuth::PluginCommand { command, args } => (command, args),
                _ => {
                    return Err(ProxyError::AuthError(format!(
                        "供应商 {} 不是 plugin_command 认证，无法用 token 命令取 token",
                        spec.id
                    )));
                }
            };
            self.fetch(&spec.id, command, args).await
        })
    }
}

/// token 命令在 stdout 输出的 JSON 形状。
#[derive(Debug, Deserialize)]
struct TokenOutput {
    token: String,
    #[serde(default)]
    account_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ApiFormat;

    fn spec(command: &str, args: Vec<String>) -> ProviderSpec {
        ProviderSpec::new(
            "plugin-test",
            "Plugin Test",
            "https://api.example.com",
            ApiFormat::OpenaiChat,
            ProviderAuth::PluginCommand {
                command: command.to_string(),
                args,
            },
        )
    }

    fn echo_json(json: &str) -> (String, Vec<String>) {
        // python3 -c 是单测里最可移植的“打印一行 JSON”办法：
        // Windows / Linux 都有 python3，sh -c 在 Windows 上没有。
        // JSON 走 argv[1]，不用在 -c 代码里拼引号。
        (
            "python3".to_string(),
            vec![
                "-c".to_string(),
                "import sys; sys.stdout.write(sys.argv[1])".to_string(),
                json.to_string(),
            ],
        )
    }

    // ManagedToken 故意没派生 Debug（派生了日志里就可能打印明文 token），
    // 所以单测不用 unwrap / unwrap_err，统一走这两个 match 助手。
    async fn fetch_ok(provider: &CommandTokenProvider, spec: &ProviderSpec) -> ManagedToken {
        match provider.token(spec).await {
            Ok(managed) => managed,
            Err(error) => panic!("期望取到 token，实际失败：{error:?}"),
        }
    }

    async fn fetch_err(provider: &CommandTokenProvider, spec: &ProviderSpec) -> ProxyError {
        match provider.token(spec).await {
            Ok(_) => panic!("期望 AuthError，实际取到了 token"),
            Err(error) => error,
        }
    }

    #[tokio::test]
    async fn happy_path_returns_token_and_account_id() {
        let (command, args) = echo_json(r#"{"token":"tok-abcdef-123456","account_id":"acc-1"}"#);
        let provider = CommandTokenProvider::new();
        let managed = fetch_ok(&provider, &spec(&command, args)).await;
        assert_eq!(managed.token, "tok-abcdef-123456");
        assert_eq!(managed.account_id.as_deref(), Some("acc-1"));
    }

    #[tokio::test]
    async fn account_id_is_optional() {
        let (command, args) = echo_json(r#"{"token":"tok-only"}"#);
        let provider = CommandTokenProvider::new();
        let managed = fetch_ok(&provider, &spec(&command, args)).await;
        assert_eq!(managed.token, "tok-only");
        assert_eq!(managed.account_id, None);
    }

    #[tokio::test]
    async fn invalid_json_is_auth_error_without_token() {
        let (command, args) = echo_json("not json at all");
        let provider = CommandTokenProvider::new();
        let error = fetch_err(&provider, &spec(&command, args)).await;
        assert!(matches!(error, ProxyError::AuthError(_)), "{error:?}");
        assert!(!format!("{error:?}").contains("not json"), "{error:?}");
    }

    #[tokio::test]
    async fn empty_token_is_auth_error() {
        let (command, args) = echo_json(r#"{"token":"   "}"#);
        let provider = CommandTokenProvider::new();
        let error = fetch_err(&provider, &spec(&command, args)).await;
        assert!(matches!(error, ProxyError::AuthError(_)), "{error:?}");
    }

    #[tokio::test]
    async fn nonzero_exit_reports_stderr_truncated() {
        let provider = CommandTokenProvider::new();
        let error = fetch_err(
            &provider,
            &spec(
                "python3",
                vec![
                    "-c".to_string(),
                    "import sys; sys.stderr.write('boom\\n'); sys.exit(3)".to_string(),
                ],
            ),
        )
        .await;
        let message = format!("{error:?}");
        assert!(matches!(error, ProxyError::AuthError(_)), "{message}");
        assert!(message.contains('3'), "{message}");
        assert!(message.contains("boom"), "{message}");
    }

    #[tokio::test]
    async fn missing_command_is_auth_error() {
        let provider = CommandTokenProvider::new();
        let error = fetch_err(
            &provider,
            &spec("definitely-not-a-real-command-xyz", vec![]),
        )
        .await;
        assert!(matches!(error, ProxyError::AuthError(_)), "{error:?}");
    }

    #[tokio::test]
    async fn timeout_kills_slow_command() {
        let provider = CommandTokenProvider::with_timeout(Duration::from_millis(200));
        let started = std::time::Instant::now();
        let error = fetch_err(
            &provider,
            &spec(
                "python3",
                vec!["-c".to_string(), "import time; time.sleep(30)".to_string()],
            ),
        )
        .await;
        assert!(matches!(error, ProxyError::AuthError(_)), "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "超时必须在 5 秒内返回，实际 {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn non_plugin_command_spec_is_auth_error() {
        let provider = CommandTokenProvider::new();
        let other = ProviderSpec::new(
            "plain",
            "Plain",
            "https://api.example.com",
            ApiFormat::OpenaiChat,
            ProviderAuth::api_key("k"),
        );
        let error = fetch_err(&provider, &other).await;
        assert!(matches!(error, ProxyError::AuthError(_)), "{error:?}");
    }

    #[test]
    fn needed_for_only_when_plugin_command_present() {
        let mut config = GatewayConfig::new(vec![ProviderSpec::new(
            "plain",
            "Plain",
            "https://api.example.com",
            ApiFormat::OpenaiChat,
            ProviderAuth::api_key("k"),
        )]);
        assert!(!CommandTokenProvider::needed_for(&config));
        config.providers.push(spec("cmd", vec![]));
        assert!(CommandTokenProvider::needed_for(&config));
    }
}
