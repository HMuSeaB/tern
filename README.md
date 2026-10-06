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

> 当前状态：命令行版可用，带用量记录；还没有界面。

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

## 联网工具（WebSearch / WebFetch）会失效

切到第三方网关后，Claude Code 的 `WebSearch` 和 `WebFetch` 可能**完全不可用**。这两个工具
不走 `ANTHROPIC_BASE_URL` 消息通道，是客户端自己发起的独立能力，请求不经过 tern——所以
协议转换做得再完美也覆盖不到，这不是网关 bug，也不需要网关修。

`tern serve` 启动时和 `tern check` 会标出哪些供应商属于第三方网关，以及怎么处理：

```powershell
tern check     # 末尾的「联网工具」段落列出需注意的供应商
```

成因与三种处理方式（禁用 / 跳过域名校验 / 切回官方）见
`docs/guides/web-tools-on-third-party-gateways-zh.md`（另有 `-en` / `-ja` 版）。

## 用量与价格

`tern serve` 把每个请求（含失败、中途断开）记到配置文件同目录的 `usage.db`，`--db` 或 `TERN_DB` 可改。

```powershell
tern usage                      # 最近 7 天：花费、缓存命中率、缓存省下的钱，按供应商 / 模型 / 角色拆分
tern usage --days 1 --by model  # 今天，按实际计费的模型
tern price list relay/claude-opus-5   # 看某个模型名实际匹配到哪条价格
tern price set step-5-preview 0.2 0.8 0.04   # 手填：输入 输出 [缓存读 [缓存写]]，美元 / 百万 token
tern price sync                 # 从 models.dev 同步；网络不通时 --file api.json
```

中转站按折扣计费时，在供应商上加 `"costMultiplier": "0.3"`。没有定价的模型照样记 token，
`tern usage` 会列出来；补上价格后历史记录自动补价。

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
