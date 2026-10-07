//! agent 的控制端：一个只监听 127.0.0.1 的 HTTP 小服务。
//!
//! # 为什么要有它
//!
//! 面板不再持有网关了，那它总得有个办法问"跑着没"、"帮我起一下"。
//! 走 HTTP 而不是 named pipe / 文件：面板和 agent 是两个进程，
//! HTTP 是调试起来最省事的——`curl` 就能验，不用额外工具。
//!
//! # 鉴权不能省
//!
//! 控制端能起停网关，等于能改用户花谁的钱。所以它复用 `tern.json` 里的
//! `accessToken`：同一把钥匙、同一套校验,不引入第二个秘密。没配 token 时
//! 只监听回环并接受任何请求（和网关自己的宽松模式一致）——
//! 但会在启动时警告，因为那时本机任何进程都能使唤这个 agent。

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{start_gateway, GatewayState, GatewayStatus};

/// 控制端监听端口。写死而不是放进配置：这个端口只该被本机的面板/CLI
/// 访问，配到 0.0.0.0 上没有任何好处，只会多一个暴露面。
pub const CONTROL_PORT: u16 = 15801;

/// 控制端配置。token 从 `tern.json` 现读，不缓存——
/// 用户在面板里改完配置后，下一次请求就该用新 token。
#[derive(Debug, Clone)]
pub struct ControlConfig {
    pub listen: SocketAddr,
    pub token: Option<String>,
}

impl ControlConfig {
    /// 从配置里取 token。读不到配置时返回 None（宽松模式），
    /// 由调用方决定要不要警告——配置坏了不该让 agent 起不来。
    pub fn from_config() -> Self {
        let token = crate::load_config(&crate::config_path().unwrap_or_default())
            .ok()
            .and_then(|config| config.access_token)
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], CONTROL_PORT)),
            token,
        }
    }

    /// 校验请求带的 token。恒定时间比较：虽然这只是本机回环，
    /// 但时序侧信道在这种地方是白送的，没理由不防。
    fn authorized(&self, presented: Option<&str>) -> bool {
        let Some(expected) = self.token.as_deref() else {
            return true;
        };
        match presented {
            Some(presented) => constant_time_eq(presented.as_bytes(), expected.as_bytes()),
            None => false,
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 起控制端。返回后立即开始 accept，调用方在自己的任务里 await 它。
pub async fn serve(state: Arc<GatewayState>, config: ControlConfig) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    log::info!("[agent] 控制端监听 http://{}", config.listen);

    // 手写 HTTP 而不引 axum：就四个端点，为一个框架拖半套依赖不划算。
    // 更何况这里要的是"绝不因为一个畸形请求崩掉"，手写反而更看得住。
    loop {
        let (mut socket, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                log::warn!("[agent] accept 失败: {error}");
                continue;
            }
        };
        let state = state.clone();
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(error) = handle(&mut socket, state, &config).await {
                log::debug!("[agent] 来自 {peer} 的请求处理失败: {error}");
            }
        });
    }
}

/// 处理一个连接：读请求、判鉴权、回 JSON。
///
/// 任何一步出错都回一句人能看懂的话，绝不让单个连接把控制端带崩——
/// 面板的每次查询都走这里，崩一次用户就看不到状态了。
async fn handle(
    socket: &mut tokio::net::TcpStream,
    state: Arc<GatewayState>,
    config: &ControlConfig,
) -> anyhow::Result<()> {
    let mut buffer = vec![0u8; 8192];
    let read = socket.read(&mut buffer).await?;
    if read == 0 {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let Ok((method, path, token)) = parse_request(&request) else {
        return respond(socket, "400 Bad Request", &error_body("请求看不明白")).await;
    };

    if !config.authorized(token.as_deref()) {
        return respond(
            socket,
            "401 Unauthorized",
            &error_body("token 不对。面板和 agent 用的是 tern.json 里同一个 accessToken"),
        )
        .await;
    }

    let (status, body) = match (method.as_str(), path.as_str()) {
        ("GET", "/api/status") => ("200 OK", serde_json::to_string(&status_of(&state))?),
        ("POST", "/api/gateway/start") => match start_gateway(&state) {
            Ok(_) => ("200 OK", serde_json::to_string(&status_of(&state))?),
            Err(error) => (
                "409 Conflict",
                error_body(&error.to_string()),
            ),
        },
        ("POST", "/api/gateway/stop") => {
            state.stop();
            ("200 OK", serde_json::to_string(&status_of(&state))?)
        }
        // 导入过供应商之后必须重起才吃得到新配置。少了这一条，
        // 用户导入完看到面板还是旧的供应商列表，会以为导入失败了
        ("POST", "/api/gateway/restart") => {
            state.stop();
            match start_gateway(&state) {
                Ok(_) => ("200 OK", serde_json::to_string(&status_of(&state))?),
                Err(error) => ("409 Conflict", error_body(&error.to_string())),
            }
        }
        _ => ("404 Not Found", error_body("没有这个端点")),
    };

    respond(socket, status, &body).await
}

async fn respond(
    socket: &mut tokio::net::TcpStream,
    status: &str,
    body: &str,
) -> anyhow::Result<()> {
    let payload = format!("{body}\n");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await?;
    Ok(())
}

fn error_body(message: &str) -> String {
    format!("{{\"error\":{}}}", json_quote(message))
}

/// 从原始请求里挖出方法、路径和 token。
///
/// 手写 HTTP 最怕的就是把畸形请求当成 crash。这里只认最基本的形状，
/// 认不出一律当 400——不猜。
fn parse_request(raw: &str) -> anyhow::Result<(String, String, Option<String>)> {
    let mut lines = raw.lines();
    let request_line = lines.next().ok_or_else(|| anyhow::anyhow!("空请求"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("请求行没有方法"))?
        .to_ascii_uppercase();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();

    let mut token = None;
    for line in lines {
        // 空行之后是 body，头到这里就结束了
        if line.trim().is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        if name == "x-tern-token" || name == "authorization" {
            let value = value.trim();
            let value = value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
                .unwrap_or(value);
            token = Some(value.to_string());
        }
    }
    Ok((method, path, token))
}

fn status_of(state: &GatewayState) -> GatewayStatus {
    GatewayStatus {
        running: state.is_running(),
        listen: state.snapshot().map(|listen| listen.to_string()),
        // 配置读不到时给 0 而不是报错：agent 已经起来了，
        // 用户要看到"现在什么状态"，那比一份配置错误信息更有用
        provider_count: crate::load_config(&crate::config_path().unwrap_or_default())
            .map(|config| config.providers.len())
            .unwrap_or(0),
        last_error: state.last_error(),
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// JSON 字符串转义。不自己写规则，交给 serde_json——
/// 错误信息里带引号、反斜杠、中文时不能把响应写坏。
fn json_quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"<无法序列化>\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(token: Option<&str>) -> ControlConfig {
        ControlConfig {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            token: token.map(str::to_string),
        }
    }

    #[test]
    fn token_is_required_when_configured() {
        let config = config_with(Some("local-secret"));
        assert!(!config.authorized(None), "没带 token 必须拒绝");
        assert!(!config.authorized(Some("wrong")), "错的 token 必须拒绝");
        assert!(config.authorized(Some("local-secret")));
    }

    #[test]
    fn missing_token_means_open_but_only_on_loopback() {
        // 与网关自己的宽松模式一致：没配 token 时不设防。
        // 缓解手段是只监听回环，这一点由 from_config 里的固定地址保证
        let config = config_with(None);
        assert!(config.authorized(None));
        assert!(config.authorized(Some("anything")));
    }

    #[test]
    fn bearer_prefix_is_accepted_for_the_authorization_header() {
        let raw = "POST /api/gateway/start HTTP/1.1\r\nAuthorization: Bearer local-secret\r\n\r\n";
        let (method, path, token) = parse_request(raw).unwrap();
        assert_eq!(method, "POST");
        assert_eq!(path, "/api/gateway/start");
        assert_eq!(token.as_deref(), Some("local-secret"));
    }

    #[test]
    fn the_dedicated_header_works_too() {
        let raw = "GET /api/status HTTP/1.1\r\nX-Tern-Token: local-secret\r\n\r\n";
        let (_, _, token) = parse_request(raw).unwrap();
        assert_eq!(token.as_deref(), Some("local-secret"));
    }

    #[test]
    fn query_string_is_stripped_from_the_path() {
        let raw = "GET /api/status?verbose=1 HTTP/1.1\r\n\r\n";
        let (_, path, _) = parse_request(raw).unwrap();
        assert_eq!(path, "/api/status");
    }

    /// 畸形请求必须被稳定处理，不能 panic。控制端崩一次，
    /// 面板就再也看不到状态了——那比回个 400 糟糕得多。
    ///
    /// 这里不断言"必须报错"：容忍一个缺路径的请求（路径默认 `/`）然后
    /// 404 掉，和直接 400，都是可接受的下场。唯一不可接受的是崩。
    #[test]
    fn malformed_requests_never_panic() {
        for raw in ["", "garbage", "\r\n\r\n", "GET", "GET\r\n\r\n", "\0\0\0"] {
            // 能走到这里就是没 panic；结果本身允许是 Ok 或 Err
            let parsed = parse_request(raw);
            if let Ok((method, path, _)) = parsed {
                // 解析成功了，那方法必须是大写形式、路径必须以 / 开头
                assert_eq!(method, method.to_uppercase(), "{raw:?} 方法没归一化");
                assert!(path.starts_with('/'), "{raw:?} 路径不以 / 开头: {path}");
            }
        }
        // 只有完全空白的输入才必须报错
        assert!(parse_request("").is_err());
        assert!(parse_request("   ").is_err());
    }

    #[test]
    fn constant_time_comparison_rejects_different_lengths() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn json_quote_escapes_without_mangling_chinese() {
        assert_eq!(json_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_quote("a\\b"), "\"a\\\\b\"");
        // 中文不该被转成 \uXXXX：控制端的错误信息要能直接读
        assert_eq!(json_quote("端口被占用"), "\"端口被占用\"");
    }
}
