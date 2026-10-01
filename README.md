# tern

面向编码 agent 的本地协议翻译网关。Claude Code、Codex 连到 tern，在模型名里写
`供应商/模型`，tern 把请求改写成那家供应商说的协议再转发，响应再转换回来。

```
Claude Code ──Anthropic Messages──┐                ┌── Anthropic 兼容（DeepSeek、Kimi、中转站…）
                                  ├─ tern :15800 ──┼── OpenAI Chat Completions
Codex ───────OpenAI Responses─────┘                ├── OpenAI Responses（含 ChatGPT / xAI 订阅）
                                                   └── Gemini Native（仅 Claude Code）
```

协议转换层取自 [cc-switch](https://github.com/farion1231/cc-switch)（MIT），
同步方式见 [UPSTREAM.md](UPSTREAM.md)；进度与后续计划见 [ROADMAP.md](ROADMAP.md)。

> 当前状态：命令行版可用，还没有界面和用量记录。

## 快速开始

```powershell
cargo build --release -p tern-cli
.\target\release\tern.exe init      # 生成 %APPDATA%\tern\tern.json，含随机 accessToken
notepad $env:APPDATA\tern\tern.json # 把 sk-REPLACE_ME 换成真实 key
.\target\release\tern.exe check     # 校验配置、列出供应商
.\target\release\tern.exe serve     # 启动，Ctrl+C 退出
```

`--config <文件>` 或环境变量 `TERN_CONFIG` 指定其他配置文件；`serve --listen 127.0.0.1:15801`
临时换端口；`RUST_LOG=debug` 看详细日志。订阅类认证（Copilot / ChatGPT / xAI）命令行版暂不支持。

## 路由规则

- `deepseek/deepseek-v4-pro` → 发给 id 为 `deepseek` 的供应商，上游模型名 `deepseek-v4-pro`
- 只按第一个 `/` 切分：`openrouter/anthropic/claude-sonnet-5` 的上游模型名是 `anthropic/claude-sonnet-5`
- 末尾的 `[1m]` 会被剥掉，Anthropic 上游自动补 `context-1m` beta 头
- 前缀不是已知供应商时交给 `defaultProvider`；没配就返回 400 并列出可用供应商

## 供应商配置

```json
{
  "listen": "127.0.0.1:15800",
  "accessToken": "换成你自己的随机串",
  "defaultProvider": "deepseek",
  "providers": [
    {
      "id": "deepseek",
      "name": "DeepSeek",
      "baseUrl": "https://api.deepseek.com/anthropic",
      "apiFormat": "anthropic",
      "auth": { "type": "api_key", "key": "sk-..." }
    },
    {
      "id": "kimi",
      "name": "Kimi",
      "baseUrl": "https://api.moonshot.cn/v1",
      "apiFormat": "openai_chat",
      "auth": { "type": "api_key", "key": "sk-..." }
    }
  ]
}
```

`apiFormat` 是上游说的协议：`anthropic` / `openai_chat` / `openai_responses` / `gemini_native`。
OpenAI 系的 `baseUrl` 可以带 `/v1`，Anthropic 系不带。`baseUrl` 已经是完整端点时加
`"fullUrl": true`。

**安全**：不设 `accessToken` 时，本机任何进程都能通过 tern 使用你的供应商 key。
监听非回环地址时务必设置。

## 接入 Claude Code

```powershell
$env:ANTHROPIC_BASE_URL = "http://127.0.0.1:15800"
$env:ANTHROPIC_AUTH_TOKEN = "<accessToken>"
$env:ANTHROPIC_MODEL = "deepseek/deepseek-v4-pro"
claude
```

## 接入 Codex

`~/.codex/config.toml`：

```toml
model = "kimi/kimi-k3"
model_provider = "tern"

[model_providers.tern]
name = "tern"
base_url = "http://127.0.0.1:15800/v1"
wire_api = "responses"
env_key = "TERN_ACCESS_TOKEN"
```

## 作为库嵌入

```rust
use tern_gateway::{Gateway, GatewayConfig};

let config: GatewayConfig = serde_json::from_str(&std::fs::read_to_string("tern.json")?)?;
Gateway::new(config)?.serve(async { tokio::signal::ctrl_c().await.ok(); }).await?;
```

订阅类供应商（`github_copilot` / `codex_oauth` / `xai_oauth`）需要宿主实现
`TokenProvider` 并通过 `Gateway::with_token_provider` 注入。

## 开发

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets
```

`crates/tern-gateway/src/proxy/` 与上游逐字节一致，不要直接改，也不要整体 `cargo fmt`；
新文件单独 `rustfmt`。
