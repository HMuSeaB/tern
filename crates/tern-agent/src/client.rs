//! 控制端的客户端：面板和 CLI 共用。
//!
//! # 为什么从 tern-app 挪进来
//!
//! 这份代码原本只在面板里。`tern use` / `tern start` 要在命令行里打到同一组端点，
//! 而两端必须**行为一致**——token 怎么带、超时各给多少、出错时转述上游原话还是自己
//! 总结，任何一处漂移都会表现成"面板点能成、命令行跑不行"，而这种问题极难查。
//!
//! 放这儿而不是新建 crate：`tern-cli` 和 `tern-app` 都已经依赖 `tern-agent`
//! （CLI 用 `single::already_running` 和 `CONTROL_PORT`），而 reqwest 也早就经由
//! tern-gateway 在这条依赖树里了——常驻进程不会因此变大。
//!
//! # 顺带把 spawn 也收进来了
//!
//! `tern-cli` 的 `launch_agent` 原本自己抄了一份"找兄弟 exe + 无窗口拉起"。
//! 搬过来之后两边共用，`tern agent` 和面板的「启动」走的是同一条路。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use serde::Deserialize;

pub use crate::control::CONTROL_PORT;

/// agent 起来了但还没开始 accept 的时间窗。
const AGENT_WARMUP: Duration = Duration::from_millis(600);
/// spawn 之后等它开始监听的时长。
const AGENT_SPAWN_TIMEOUT: Duration = Duration::from_secs(10);
/// 只问状态的超时。
///
/// 问的是本机回环接口，健康时几毫秒就回。等 15 秒没有任何意义——而托盘刷新
/// 会调它，等满 15 秒意味着界面冻 15 秒。
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);
/// 让 agent 退出的超时。同样不等：它僵住的话安装器会用 taskkill 收尾。
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);
/// 起停网关的超时。比问状态长：它要等网关真正 listen 或真正停下。
const ACTION_TIMEOUT: Duration = Duration::from_secs(15);

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
pub fn agent_binary_name() -> &'static str {
    if cfg!(windows) {
        "tern-agent.exe"
    } else {
        "tern-agent"
    }
}

/// 找到 agent。它装在调用方旁边：release 构建两者同在 target/release/，
/// 打包后同在安装目录。
pub fn agent_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.parent()
        .map(|dir| dir.join(agent_binary_name()))
        .filter(|path| path.exists())
}

/// 找不到 agent 时的那句话。
///
/// 单独提出来是为了能单测：这句话是用户在这个状态下唯一能看到的东西，
/// 值得守住——而 `ensure_agent` 本身的行为取决于"此刻有没有 agent 在跑"，
/// 拿它测消息内容会让测试随环境时好时坏。
pub fn missing_agent_message() -> String {
    format!(
        "找不到 {agent}。它和 tern 应当装在同一个目录。\n\
         手动启动：在安装目录执行\n  \
         {agent}\n然后它会一直在后台持有网关",
        agent = agent_binary_name()
    )
}

/// 问一次状态。agent 没起来时返回 Err，不自动拉起——
/// 拉起是有副作用的动作，要么由 `ensure_agent` 显式做，要么用户点了启动。
pub fn status() -> anyhow::Result<AgentStatus> {
    request("GET", "/api/status", None, STATUS_TIMEOUT)
}

/// 起网关。agent 不在时会先把它拉起来。
pub fn start_gateway() -> anyhow::Result<AgentStatus> {
    ensure_agent()?;
    request("POST", "/api/gateway/start", None, ACTION_TIMEOUT)
}

/// 停网关。**不停 agent**：调用方通常只是想知道"流量断了没"，
/// 顺带把常驻进程也杀掉会让下次启动再付一次冷启动的钱。
pub fn stop_gateway() -> anyhow::Result<AgentStatus> {
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
    request("POST", "/api/gateway/stop", None, ACTION_TIMEOUT)
}

/// 重起网关。改过配置之后必须调它，否则跑着的还是旧配置——
/// 用户会以为改动没生效。
pub fn restart_gateway() -> anyhow::Result<AgentStatus> {
    ensure_agent()?;
    request("POST", "/api/gateway/restart", None, ACTION_TIMEOUT)
}

/// 让 agent 进程退出。
///
/// # 什么时候用
///
/// 装新版本之前。Windows 上覆盖不了正在运行的 exe，而 agent 是常驻进程
/// （关窗口不停它，那是设计）。不先让它退出，安装器会报
/// `Can't write: ...\tern-agent.exe` 然后整个升级失败。
///
/// # 为什么不用 taskkill
///
/// `taskkill /F` 是硬杀，在途的请求补不了 aborted、写入队列没排空就没了。
/// 走 HTTP 让 agent 自己收尾，用量不丢。
///
/// agent 本来没起时**不报错**：那正是想要的状态。但也别为它特意拉起一个
/// 再杀——所以这里直接问，连不上就当已经退出。
pub fn exit_agent() -> anyhow::Result<()> {
    if !reachable() {
        return Ok(());
    }
    match request("POST", "/api/agent/exit", None, EXIT_TIMEOUT) {
        Ok(_) => {
            log::info!("[agent-client] 常驻进程已退出");
            Ok(())
        }
        // 响应回来之前进程就没了，连接被掐断是正常的。看结果不看错误
        Err(error) => {
            log::info!("[agent-client] 常驻进程退出（连接随进程结束）: {error}");
            Ok(())
        }
    }
}

/// agent 在不在且能答话。只看版本号是不是空的——那是最便宜的一次往返。
pub fn reachable() -> bool {
    matches!(status(), Ok(status) if !status.agent_version.is_empty())
}

/// 确保 agent 在跑。已经在跑就直接返回，否则 spawn 并等它开始监听。
pub fn ensure_agent() -> anyhow::Result<()> {
    if reachable() {
        return Ok(());
    }
    let path = agent_path().ok_or_else(|| anyhow::anyhow!(missing_agent_message()))?;

    spawn_agent(&path)?;

    let deadline = Instant::now() + AGENT_SPAWN_TIMEOUT;
    while Instant::now() < deadline {
        if reachable() {
            return Ok(());
        }
        std::thread::sleep(AGENT_WARMUP / 6);
    }
    anyhow::bail!(
        "等 {} 开始监听超过 {} 秒。它可能起来了但控制端口被占，\n\
         或被安全软件拦了。可以手动跑它看输出",
        path.display(),
        AGENT_SPAWN_TIMEOUT.as_secs()
    )
}

/// spawn agent。要点是**不弹任何窗口**。
///
/// 关键在于 `DETACHED_PROCESS`：它告诉系统这个子进程不需要控制台。
/// 少了它，当调用方是从资源管理器启动的（GUI 程序、自身没有控制台），
/// 系统会为了这个子进程**临时分配一个控制台**——用户看到一个黑色 cmd
/// 窗口闪一下。加上之后分配不发生，窗口也就不出现。
///
/// 顺带 `CREATE_NEW_PROCESS_GROUP`：不把 agent 拉进调用方自己的进程组，
/// 否则调用方退出时 Ctrl+C 一类的事件会连带波及它——而 agent 本来就该
/// 比调用方活得久。
///
/// 代价：agent 没有控制台，stdout/stderr 无处可去。日志走 RUST_LOG 重定向；
/// 但要知道这意味着**看不到它的即时输出了**，出问题时得手动跑 `tern-agent` 看。
///
/// MSDN 明确说 CREATE_NO_WINDOW 与 DETACHED_PROCESS 同用时前者被忽略，
/// 所以这里只写后两个。
pub fn spawn_agent(path: &Path) -> anyhow::Result<()> {
    let mut command = Command::new(path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    command
        .spawn()
        .map_err(|e| anyhow::anyhow!("启动 {} 失败: {e}", path.display()))?;
    log::info!("[agent-client] 已拉起常驻进程 {}", path.display());
    Ok(())
}

/// 从 `tern.json` 取 accessToken，和 agent 用同一把钥匙。
fn control_token() -> Option<String> {
    let path = crate::config_path().ok()?;
    let config = crate::load_config(&path).ok()?;
    config
        .access_token
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// 共用一个阻塞客户端。
///
/// 每次 `Client::builder().build()` 都会新建一套连接池，而 `status()` 是托盘
/// 高频调用的那个——那点开销每问一次都要付一遍。reqwest 的阻塞 Client 是
/// `Send + Sync`，放静态量里安全。
static CONTROL_CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        // 兜底超时。真正生效的是按请求覆盖的那个，见 request()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new())
});

/// 发一次请求。用阻塞式 reqwest：这是个几十毫秒的本地调用，
/// 为它引入异步是把简单事情搞复杂。
///
/// `timeout` 按请求覆盖：调用方自己知道能等多久（问状态的和起网关的完全不是
/// 一回事），而超时正是冻住界面的唯一来源，必须逐个收窄。
fn request(
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> anyhow::Result<AgentStatus> {
    let url = format!("http://127.0.0.1:{CONTROL_PORT}{path}");
    let mut request = CONTROL_CLIENT
        .request(method.parse().unwrap_or(reqwest::Method::GET), &url)
        .timeout(timeout);

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
        .map_err(|e| anyhow::anyhow!("连不上常驻进程（{url}）: {e}"))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .map_err(|e| anyhow::anyhow!("读常驻进程响应失败: {e}"))?;

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
        anyhow::bail!("HTTP {status}: {message}");
    }

    Ok(serde_json::from_str(&text)?)
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
        let message = missing_agent_message();
        assert!(message.contains("找不到"), "{message}");
        assert!(message.contains(agent_binary_name()), "{message}");
    }

    #[test]
    fn control_url_points_at_the_loopback_control_port() {
        // 只验证拼装规则：真发请求会让测试依赖"此刻有没有 agent 在跑"
        assert_eq!(
            format!("http://127.0.0.1:{CONTROL_PORT}/api/status"),
            format!("http://127.0.0.1:15801/api/status")
        );
    }

    /// 没有 agent 时「停止」不能报错：那会让用户以为出了什么问题，
    /// 而实际状态正是他想要的——网关没在跑。
    ///
    /// 这条守的是"别硬把 agent 拉起再停"。拉起是有副作用的动作，
    /// 为了停一个已经停着的网关去起一个常驻进程，说不通。
    #[test]
    fn stopping_with_no_agent_is_a_no_op_not_an_error() {
        // 两种情形下都不该报错，且结果都该是"没在跑"：
        // 1. agent 没起 → 直接返回占位状态（这条是重点）
        // 2. agent 起着 → 真的去停，结果同样是 running: false
        let status = stop_gateway().unwrap();
        assert!(!status.running, "停完就该是没在跑，不管 agent 起没起");
    }

    /// 控制端口只听回环。这条守的是"别哪天把它写成了 0.0.0.0"——
    /// 那等于让本机上任何进程都能起停你的网关。
    #[test]
    fn control_port_is_loopback_only() {
        let url = format!("http://127.0.0.1:{CONTROL_PORT}/api/status");
        assert!(url.contains("127.0.0.1"), "{url}");
        assert!(!url.contains("0.0.0.0"), "{url}");
    }
}
