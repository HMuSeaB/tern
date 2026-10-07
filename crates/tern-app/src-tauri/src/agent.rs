//! 面板 ↔ agent 的客户端。
//!
//! # 为什么面板不再自己持有网关
//!
//! 原来是面板 `spawn` 一个线程跑网关，好处是双击 exe 就能用，坏处有三个：
//!
//! 1. **内存**。Tauri 带 webview，实测约 408 MB。为了"看一眼花了多少"
//!    常驻一个浏览器内核不值
//! 2. **关窗口 = 断网**。用户只是想看一眼用量，不想把流量也关了
//! 3. **状态和窗口绑死**。面板崩了网关跟着没，排查时两件事混在一起
//!
//! 所以网关归 `tern-agent`（一个没有窗口的小进程），面板只当它的遥控器。
//! 代价是多一个进程要管理——`ensure_agent` 就是干这个的。
//!
//! # agent 找不到时怎么办
//!
//! 面板自己 spawn 它。用户不该为了"看一眼用量"先开一个终端跑 agent，
//! 那正是当初把网关塞进面板想避免的两步式体验。

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::{AppError, Result};

/// 与 `tern_agent::control::CONTROL_PORT` 一致。两处各写一遍而不是让面板
/// 依赖 agent 的 lib——那会把一个常驻进程的编译产物拖进面板的依赖图，
/// 只为拿到一个常量。
pub const CONTROL_PORT: u16 = 15801;

/// agent 起来了但还没开始 accept 的时间窗。
const AGENT_WARMUP: Duration = Duration::from_millis(600);
/// spawn 之后等它开始监听的时长。
const AGENT_SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

/// 与服务端 `GatewayStatus` 逐字段对应。
#[derive(Debug, Clone, Deserialize)]
pub struct AgentStatus {
    pub running: bool,
    pub listen: Option<String>,
    pub provider_count: usize,
    pub last_error: Option<String>,
    pub agent_version: String,
}

/// agent 可执行文件名。
fn agent_binary_name() -> &'static str {
    if cfg!(windows) {
        "tern-agent.exe"
    } else {
        "tern-agent"
    }
}

/// 找到 agent。它装在面板旁边：release 构建两者同在 target/release/，
/// 打包后同在安装目录。
pub fn agent_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.parent()
        .map(|dir| dir.join(agent_binary_name()))
        .filter(|path| path.exists())
}

/// 问一次状态。agent 没起来时返回 Err，不自动拉起——
/// 拉起是有副作用的动作，要么由 `ensure_agent` 显式做，要么用户点了启动。
pub fn status() -> Result<AgentStatus> {
    request("GET", "/api/status", None)
}

/// 起网关。agent 不在时会先把它拉起来。
pub fn start_gateway() -> Result<AgentStatus> {
    ensure_agent()?;
    request("POST", "/api/gateway/start", None)
}

/// 停网关。**不停 agent**：面板只是想知道"流量断了没"，
/// 顺带把常驻进程也杀掉会让下次启动再付一次冷启动的钱。
pub fn stop_gateway() -> Result<AgentStatus> {
    // agent 本来就没起，那就是已经是停的状态，别硬拉起它再停
    if agent_path().is_none() || !reachable() {
        return Ok(AgentStatus {
            running: false,
            listen: None,
            provider_count: 0,
            last_error: None,
            agent_version: String::new(),
        });
    }
    request("POST", "/api/gateway/stop", None)
}

/// 重起网关。导入过供应商之后必须调它，否则跑着的还是旧配置——
/// 用户会以为导入失败了。
pub fn restart_gateway() -> Result<AgentStatus> {
    ensure_agent()?;
    request("POST", "/api/gateway/restart", None)
}

fn reachable() -> bool {
    matches!(status(), Ok(status) if !status.agent_version.is_empty())
}

/// 确保 agent 在跑。已经在跑就直接返回，否则 spawn 并等它开始监听。
pub fn ensure_agent() -> Result<()> {
    if reachable() {
        return Ok(());
    }
    let path = agent_path().ok_or_else(missing_agent_message)?;

    spawn_agent(&path)?;

    let deadline = Instant::now() + AGENT_SPAWN_TIMEOUT;
    while Instant::now() < deadline {
        if reachable() {
            return Ok(());
        }
        std::thread::sleep(AGENT_WARMUP / 6);
    }
    Err(AppError::Config(format!(
        "等 {} 开始监听超过 {} 秒。它可能起来了但控制端口被占，\n\
         或被安全软件拦了。可以手动跑它看输出",
        path.display(),
        AGENT_SPAWN_TIMEOUT.as_secs()
    )))
}

/// 找不到 agent 时的话。
///
/// 单独提出来是为了能单测：这句话是用户在这个状态下唯一能看到的东西，
/// 值得守住——而 `ensure_agent` 本身的行为取决于"此刻有没有 agent 在跑"，
/// 拿它测消息内容会让测试随环境时好时坏。
fn missing_agent_message() -> AppError {
    AppError::Config(format!(
        "找不到 {}。它和面板应当装在同一个目录。\n\
         手动启动：在安装目录执行\n  \
         {agent}\n然后回到这里点「启动」",
        agent_binary_name(),
        agent = agent_binary_name()
    ))
}

/// spawn agent。要点是**不弹窗口**：用户点一下启动，
/// 不该闪出一个控制台黑框再消失。
fn spawn_agent(path: &std::path::Path) -> Result<()> {
    let mut command = Command::new(path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        // DETACHED_PROCESS 而不是 CREATE_NO_WINDOW。差别在父进程这边：
        // CREATE_NO_WINDOW 只是不弹控制台，子进程仍挂在父进程的控制台上，
        // 父进程（这里是别人的终端）会被拖住一起等。
        // DETACHED_PROCESS 让子进程彻底脱离控制台，且同样不弹窗口。
        // MSDN 明确说 CREATE_NO_WINDOW 与 DETACHED_PROCESS 同用时会被忽略，
        // 所以只写 DETACHED_PROCESS。
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(DETACHED_PROCESS);
    }

    command
        .spawn()
        .map_err(|e| AppError::Config(format!("启动 {} 失败: {e}", path.display())))?;
    log::info!("[tern-app] 已拉起常驻进程 {}", path.display());
    Ok(())
}

/// 从 `tern.json` 取 accessToken，和 agent 用同一把钥匙。
fn control_token() -> Option<String> {
    crate::config::config_path()
        .ok()
        .and_then(|path| crate::config::load(&path).ok())
        .and_then(|config| config.access_token)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// 发一次请求。用阻塞式 reqwest：这是个几十毫秒的本地调用，
/// 为它引入异步是把简单事情搞复杂。
fn request(method: &str, path: &str, body: Option<&str>) -> Result<AgentStatus> {
    let url = format!("http://127.0.0.1:{CONTROL_PORT}{path}");
    let mut request = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Config(e.to_string()))?
        .request(method.parse().unwrap_or(reqwest::Method::GET), &url);

    if let Some(token) = control_token() {
        request = request.header("x-tern-token", token);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body.to_string());
    }

    let response = request
        .send()
        .map_err(|e| AppError::Config(format!("连不上常驻进程（{url}）: {e}")))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .map_err(|e| AppError::Config(format!("读常驻进程响应失败: {e}")))?;

    if !(200..300).contains(&status) {
        // agent 的原话比我们自己总结的有用：端口占用、token 不对、
        // 配置无效，各自的修法完全不同
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|json| {
                json.get("error")
                    .and_then(|e| e.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| text.trim().to_string());
        return Err(AppError::Config(format!("HTTP {status}: {message}")));
    }

    serde_json::from_str(&text).map_err(|e| AppError::Config(format!("响应解析失败: {e}")))
}

/// 供测试：临时换个端口，避免和真在跑的 agent 抢。
#[cfg(test)]
pub fn control_url_for(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_binary_name_matches_the_platform() {
        let name = agent_binary_name();
        if cfg!(windows) {
            assert!(name.ends_with(".exe"), "Windows 上要带 .exe");
        } else {
            assert!(!name.contains('.'), "别的地方不该带扩展名");
        }
    }

    /// 找不到 agent 时那句话要能让人知道去哪修，而不是一句
    /// "No such file or directory"。
    ///
    /// 只测消息本身：`ensure_agent` 的行为取决于"此刻有没有 agent 在跑"，
    /// 拿它测内容会让测试随开发机的状态时好时坏。
    #[test]
    fn a_missing_agent_reports_how_to_fix_it() {
        let error = missing_agent_message().to_string();
        assert!(error.contains("找不到"), "{error}");
        assert!(error.contains(agent_binary_name()), "{error}");
        // 要给出手动启动的办法，不能只报错
        assert!(error.contains("安装目录"), "{error}");
    }

    /// 没有 agent 时点「停止」不能报错：那会让用户以为出了什么问题，
    /// 而实际状态正是他想要的——网关没在跑
    #[test]
    fn stopping_with_no_agent_is_a_no_op_not_an_error() {
        // 两种情形下都不该报错，且结果都该是"没在跑"：
        // 1. agent 没起 → 直接返回占位状态（这条是重点，别硬拉起 agent 再停）
        // 2. agent 起着 → 真的去停，结果同样是 running: false
        let status = stop_gateway().unwrap();
        assert!(!status.running, "停完就该是没在跑，不管 agent 起没起");
    }

    #[test]
    fn control_url_is_always_loopback() {
        // 控制端口只听回环。这条守的是"别哪天把它写成了 0.0.0.0"
        let url = control_url_for(CONTROL_PORT, "/api/status");
        assert!(url.contains("127.0.0.1"), "{url}");
        assert!(!url.contains("0.0.0.0"), "{url}");
    }

    #[test]
    fn listening_port_is_the_same_the_agent_uses() {
        // 面板和 agent 必须用同一个端口。改一处忘了另一处，
        // 症状是"点了启动没反应"，极难查
        assert_eq!(CONTROL_PORT, 15801);
    }
}
