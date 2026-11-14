# tern 轻量外挂插件设计：独立进程

> 状态：设计稿，无代码。基准：`main @ 0d4a25a`（v0.3.7，2026-11-14）。
> 配套：`ROADMAP.md`（定位：参照 magpie 的简洁体验，护城河在用量不在 agent 数）、
> `docs/DECISIONS.md`（分层、key 不进渲染进程、故障转移三条规矩、依赖只用本地缓存）。
>
> 一句话：插件是独立进程，不是库，也不是 JS 运行时。
> tern 还是管协议翻译和用量记账，插件只管“这家服务怎么登录 / 签名 / 发请求”。

## 0. 为什么要插件，为什么要轻量

用户原话的判断是：magpie 生态好（WorkBuddy、豆包反代插件一大堆），
但 cc-switch 一直在用，不想迁移；自己的 tern“一直都不好用”，主打一个轻量。

tern 的定位在 ROADMAP 里已经定了：参照 yetone/magpie 的简洁体验
（agent × 模型单屏、本地网关按 `provider/model` 路由），复用 cc-switch
的协议转换层，甩掉它的包袱；核心卖点是用量面板；明确不追 Magpie 的
50+ agent 适配，护城河在用量不在 agent 数。

插件如果做成 magpie 那样，就把这个定位丢了：

- magpie 的插件跑在 Bun 上，第一次用插件时才下载 Bun，所有插件装进
  一个 Bun 进程（plugin host），宿主和插件之间走 stdin/stdout、
  一行一条 JSON。插件自己登录、列模型、发请求；网关把请求翻译成
  模型说的 API，交给插件的 `fetch`，再把回答流回去。插件还能当
  网关中间件，`onRequest` / `onEvent` / `onResponse` 看到每个请求和响应。
- 这套东西好用，但重：核心要多一个 JS 运行时、多一个宿主进程、
  多一套插件沙箱和升级逻辑。tern 常驻层现在是 `tern-agent`
  工作集 12.4 MB、私有 5.3 MB（DECISIONS §2 实测），面板 Tauri
  约 408 MB 已经是纯遥控器。插件不应该让常驻层变重。

所以这篇设计只做一件事：**让 tern 能用上订阅 / 反代类模型，
同时核心二进制不引入新运行时、不变重、不丢用量。**

## 1. 目标 / 非目标

### 目标

- Claude Code / Codex 通过 tern 用上 WorkBuddy 这类没有官方个人 API
  的订阅服务，以及任何只暴露本地 HTTP 端点的 2API sidecar。
- 插件崩了不拖垮网关，不用时不占内存、不跑进程。
- 用量面板照样能算钱、能分清主对话 / 子代理 / 被换模型 / 失败，
  不因为走了插件就变成一笔糊涂账。
- 插件作者用任何语言都能写，不强制 Rust / JS。

### 非目标（本设计明确不做）

- 不做 magpie 的 Bun plugin host，不在核心里嵌 JS / WASM 运行时。
- 不做 50+ agent 适配，不做插件市场 / 自动更新 / 自动发现并启用。
- 不做网关中间件引擎 v1（`onRequest` 改请求体那种）。以后真需要，
  再单独设计，不塞进 v1。
- 不改 `provider/model` 路由语义，不改用量口径，不改故障转移三条规矩。
- 不自动加载插件，不轮询，不在 cc-switch 擦掉接线后自动写回
  （DECISIONS §2 的用户否决同样适用于插件）。

## 2. 现状：今天已经能用的三条路

写代码之前先说清楚：下面三条路今天已经通了，插件系统是第四条路，
不是唯一一条路。先走通前三条，再决定要不要做第四条。

### 2.1 插件就是一个 ProviderSpec（零代码）

任何暴露本地 HTTP 端点、说 Anthropic / OpenAI Chat / OpenAI Responses
协议的 sidecar，今天就能直接当供应商接进来，不用等插件系统：

```json
{
  "id": "workbuddy",
  "name": "WorkBuddy",
  "baseUrl": "http://127.0.0.1:18080/v1",
  "apiFormat": "openai_chat",
  "auth": { "type": "api_key", "key": "sk-..." }
}
```

模型写 `workbuddy/<上游模型名>` 就行。`router.rs` 只按第一个 `/`
切分，`openrouter/anthropic/claude-sonnet-5` 的上游模型名是
`anthropic/claude-sonnet-5`，所以带斜杠的上游模型名也不会被切坏。
这是判断“要不要为某一家写插件”的第一道筛子：它已经有 HTTP 端点，
就别写插件，直接配 ProviderSpec。

### 2.2 magpie 当 tern 的一个上游（零代码，待实测）

```json
{
  "id": "magpie",
  "name": "Magpie",
  "baseUrl": "http://127.0.0.1:3425",
  "apiFormat": "anthropic",
  "auth": { "type": "api_key", "key": "magpie" }
}
```

模型写 `magpie/workbuddy/<模型>`：tern 按第一个 `/` 切出
`magpie`，上游模型名剩下 `workbuddy/<模型>`，原样发给 magpie
网关，magpie 再按自己的 `provider/model` 路由到它的 WorkBuddy
插件。用量还是 tern 在出口处算，面板照样能看到。

这条路没在用户机器上实测过，先当待验证的思路，不写进验收。
它最大的价值是验证需求：如果只是偶尔用一两个反代模型，
桥接 magpie 就够了，不用造自己的插件运行时。

### 2.3 TokenProvider：插件鉴权的现成缝

`crates/tern-gateway/src/gateway/mod.rs` 里已经有这个缝：

```rust
pub trait TokenProvider: Send + Sync {
    fn token<'a>(
        &'a self,
        spec: &'a ProviderSpec,
    ) -> BoxFuture<'a, Result<ManagedToken, ProxyError>>;
}
```

`GatewayState.tokens: Option<Arc<dyn TokenProvider>>`，
`Gateway::with_token_provider()` 必须在 `serve` / `app` 之前调用。
`gateway/upstream.rs` 的 `resolve_auth_headers()` 里，
`needs_token_provider(spec)` 为真（`GithubCopilot` /
`CodexOauth` / `XaiOauth`）时才去取 token，否则直接走
`ProviderAuth::ApiKey`。

ROADMAP 里这条是没做的：“订阅类 OAuth（`TokenProvider` 已有
trait，token 交换没实现）”。插件的第一步应该先补这个缝，
而不是先造一个新运行时。

## 3. 核心决定：五条

### D1. 插件是独立进程

- 插件是一个可执行文件（任何语言），由 `tern-agent` 按需拉起，
  不是 tern 进程内的线程 / 动态库 / JS 沙箱。
- 插件只在它的供应商被用到时才跑，不用时不启动；闲置 10 分钟
  自动杀掉；崩了只影响它那一家，不影响网关和其他供应商。
- 常驻层还是 `tern-agent`（12.4 MB 级），面板还是纯遥控器。
  插件进程的内存不算进常驻层，关掉面板也不影响插件的生命周期，
  生命周期归 agent 管。

### D2. 插件对 tern 只暴露 HTTP，不暴露私有 stdio 协议

- v1 不学 magpie 的 stdin/stdout 一行一条 JSON。那套协议只有
  host 能看懂，`curl` 调不通，日志里也看不到完整请求。
- 插件是一个本地 HTTP 服务，tern 用现成的 `reqwest::Client`
  （`gateway/mod.rs` 的 `build_client`，已有关闭自动解压、
  代理、超时那套）去调它，和调任何上游供应商走同一条路。
- 插件必须实现三个端点（见 §4），其余一律不认。协议越小，
  插件作者越不用读 tern 的源码。

### D3. 分工：tern 管翻译和记账，插件只管这家服务

```
Claude Code / Codex
  -- Anthropic Messages / OpenAI Responses -->
tern :15800
  -- 按 provider/model 路由，adapter 翻译成上游协议 -->
插件 127.0.0.1:<ephemeral>
  -- 登录 / 签名 / 按这家服务的规矩发请求 -->
真实服务（WorkBuddy / 豆包 2API / …）
  -- 标准 Anthropic / OpenAI 响应，流式 SSE -->
tern：转换回客户端协议 + 出口处记用量
  -->
Claude Code / Codex
```

- 插件不做协议翻译，不解析 Claude Code 的请求体结构，
  只收 tern 已经翻译好的、符合它声明的 `apiFormat` 的请求。
- 插件返回的必须是标准格式，流式必须是 SSE，否则 tern 的
  用量解析（`gateway/usage.rs` 出口处只解析两种客户端协议，
  流式包一层旁路解析）认不出来。那种请求记 0 token、成本 NULL、
  在面板里标成“未定价 / 未解析”，不能悄悄混进模型分布。
  这条和 DECISIONS §5 的“失败必须单独呈现，不能混进模型分布”
  是同一个道理。

### D4. 发现靠 manifest + 显式启用，不自动加载

- 插件放在 `%APPDATA%\tern\plugins\<id>\plugin.toml` + 可执行文件，
  或者在 `tern.json` 的 `plugins[]` 里显式列出。两种只能选其一
  落盘，不两头写。
- 面板里默认全部禁用，用户勾选才启用，和首次运行导入 cc-switch
  时“凭据不能默认勾选、不能折叠”是同一条规矩：把 key / 登录态
  从 A 工具搬到 B 工具，得用户自己决定。
- 不扫描、不轮询、不自动写回接线。插件列表由面板每次打开时
  如实读，显示成未启用，用户自己点一下启用。

### D5. 安全边界：插件只能碰自己的登录态

- 插件以当前用户权限运行，能读自己的 session 文件，
  不能读 `tern.json` 里其他供应商的 key。tern 传给插件的
  只有三样：插件自己的 `plugin.toml`、分配的端口、
  本次请求已经翻译好的请求体。tern 自己的 `accessToken`
  不传给插件。
- key / token 不进日志、不进面板渲染进程。`ProviderAuth` 的
  手写 `Debug` 已经把 key 遮成前 4 后 4（`provider.rs` 的
  `mask_secret`），插件日志沿用同一条：只记后 4 位，不记全文。
- 插件的登录态文件单独放，每个插件一个文件，权限 600，
  不进仓库，不进 `tern.json`。面板 `provider_detail` 不回传
  key 的老规矩（DECISIONS §4）同样适用于插件。

## 4. 插件契约 v1（最小集）

### 4.1 Manifest：`plugin.toml`

```toml
id = "workbuddy"
name = "WorkBuddy"
version = "0.1.0"
description = "腾讯 WorkBuddy 订阅：每日签到积分换模型调用（自用）"

# tern 怎么拉起它。command 是可执行文件，args 原样传。
# tern 不解析 shell，不拼字符串，不走 PATH 查找以外的魔法。
command = "tern-plugin-workbuddy"
args = ["serve"]

# 插件说哪种协议。tern 翻译成这种协议再发过去。
# 取值和 ProviderSpec.apiFormat 一致：
# anthropic / openai_chat / openai_responses / gemini_native
api_format = "openai_chat"

# 插件提供的模型。面板列表和 /v1/models 都从这里来，
# 插件也可以在 /v1/models 里动态返回更多。
models = ["workbuddy/hunyuan-a52b"]

# 可选：插件自己要读的 session 文件相对路径（相对插件目录）。
# tern 不读这个文件，只把它所在目录的权限要求写进文档。
session_file = "session.json"
```

校验规则（启动时就报错，不要等到第一次请求才炸）：

- `id` 不能为空、不能含 `/`、不能和已有供应商 id 重复
  （`ModelRouter::new` 本来就会报“供应商 id 重复”）。
- `command` 必须存在且可执行，找不到时 `tern check` 就报，
  不要等到用户发请求。
- `api_format` 必须是上面四种之一。

### 4.2 插件必须实现的三个端点（全部只听 127.0.0.1）

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` | `/health` | 存活 + 模型列表。`{"ok": true, "models": ["..."]}` |
| `GET` | `/v1/models` | OpenAI 兼容的模型列表。tern 的 `models.rs` 候选规则照样适用：404 / 405 当“这个候选不对”继续试，401 / 429 / 5xx 直接返回，连接失败直接返回。 |
| `POST` | 按 `api_format` 走：`/v1/messages`（Anthropic）、`/v1/chat/completions`（OpenAI Chat）、`/v1/responses`（OpenAI Responses）、`models/*:generateContent`（Gemini Native） | 真正的模型调用。请求体就是 tern 发给普通上游的那个体，响应也必须是那种格式，流式走 SSE。 |

不实现 `/health` 的插件，tern 视为未就绪，不路由请求过去。

### 4.3 启动握手：tern 分端口，插件只管绑

不要让插件自己选端口再把端口打印到 stdout 里，stdout 解析
是最容易出竞态的地方。v1 定死：

1. tern 在启动插件前先占一个 `127.0.0.1:0` 的临时端口，拿到
   实际端口号后立刻释放，再把端口号通过环境变量
   `TERN_PLUGIN_PORT` 和参数 `--port <端口>` 同时传给插件
   （两个都传，插件只用其中一个就行，另一个是防插件作者
   只读了一种）。
2. 插件绑 `127.0.0.1:$TERN_PLUGIN_PORT`，5 秒内 `GET /health`
   必须返回 200，否则 tern 杀掉进程，记一次启动失败，
   面板显示“插件启动超时”，不重试到天荒地老。
3. 插件进程的标准输出 / 标准错误重定向到
   `%APPDATA%\tern\plugins\<id>\logs\`，按天切分，密钥照样遮蔽。

端口被占用的竞态（tern 释放后、插件绑定前的窗口期）：
v1 接受这个小概率，失败时按启动失败处理，用户点一下重试。
不要为这个窗口期引入端口协商协议，那是第二版的事。

### 4.4 生命周期

- 懒启动：第一次有请求路由到这个供应商时才拉起插件，
  之前只读 manifest，不跑进程。
- 闲置回收：10 分钟没有请求就杀掉，下次请求再拉起。
  闲置时间做成可配，但默认 10 分钟不改。
- 崩溃：插件进程退出时，正在飞的请求按上游错误处理，
  熔断器记一次；1 分钟内连续崩 3 次就标成“已停用”，
  不再自动拉起，等用户在面板里手动点“重新启用”。
- 显式路由不转移：`workbuddy/<模型>` 显式指定了插件，
  插件挂了不自动转到默认供应商。这是 DECISIONS §4 的
  故障转移三条规矩之一：显式 `provider/model` 不转移；
  只在上游没给响应头时换；熔断进程内有效、不落盘。
- 热更新：`Gateway::set_providers()` 换供应商列表时，
  顺手把已经不存在的供应商的插件进程和熔断器一起摘掉，
  和现在摘熔断器是同一段代码。

## 5. 接入 tern 的最小改动（按文件）

不重写网关。下面的每一项都是在现有缝上加一小段。

### 5.1 `crates/tern-gateway/src/provider.rs`

加一个插件引用，不动 `ProviderAuth` 的现有变体（v1）：

```rust
// 建议的新字段，放在 ProviderSpec 里，和 client_env 并列：
#[serde(default, skip_serializing_if = "Option::is_none")]
pub plugin: Option<PluginRef>,

pub struct PluginRef {
    pub plugin_id: String,
    // 插件目录，默认 %APPDATA%\tern\plugins\<plugin_id>
    // 不写就是默认目录。
    pub dir: Option<String>,
}
```

为什么不用新的 `ProviderAuth::Plugin` 变体起手：
`ProviderAuth` 现在管的是“上游要什么凭证”，插件的凭证
在插件自己手里，tern 侧不需要知道。先加 `plugin` 字段，
`auth` 保持 `ApiKey { key: "plugin" }` 或 `None` 都行，
等 Step 1 做鉴权外包时再加 `ProviderAuth::PluginCommand`。

`ProviderSpec::effective_base_url()` 对插件供应商要短路：
插件的 `base_url` 在启动前是不知道的（临时端口），
`effective_base_url()` 返回占位，真正地址由 supervisor
在 `send()` 前填。不要在配置里写死一个端口，那个端口
下次启动就不在了。

### 5.2 新增 `crates/tern-gateway/src/plugin.rs`（新模块，约 300 行目标）

只做四件事，不多做：

- `PluginManifest::load(dir) -> Result<_, _>`：读 `plugin.toml`，
  做 §4.1 的校验。
- `PluginSupervisor`：`HashMap<plugin_id, RunningPlugin>`，
  `ensure_running(id) -> SocketAddr`（懒启动 + 健康检查），
  `stop(id)`，`prune(alive_ids)`（给 `set_providers` 用）。
- `RunningPlugin`：`tokio::process::Child` + 端口 + 上次使用时间，
  闲置回收由一个轻量 tokio 任务每分钟扫一次，不另起线程。
- 错误类型复用 `ProxyError`，不要新造一套错误枚举。
  插件启动失败、健康检查失败、崩溃，都映射成
  `UpstreamError` / `ForwardFailed`，面板已经会显示这类错误。

不做：插件市场、自动下载、自动更新、插件间通信、
中间件钩子、WASM / Bun 宿主。

### 5.3 `crates/tern-gateway/src/gateway/mod.rs`

- `GatewayState` 加一个字段：`pub plugins: Option<Arc<PluginSupervisor>>`，
  和 `tokens` / `usage` 并列。没有插件时是 `None`，网关行为
  和现在完全一样。
- `Gateway::with_plugin_supervisor()`，和 `with_token_provider()` /
  `with_usage_sink()` 一个写法，必须在 `serve` / `app` 之前调用。
- `Gateway::set_providers()` 里顺手调 `plugins.prune(alive_ids)`。

### 5.4 `crates/tern-gateway/src/gateway/upstream.rs`

`send()` 里唯一要改的分支：

```rust
// 伪代码，不是最终代码：
let url = if let Some(plugin) = spec.plugin.as_ref() {
    let addr = state.plugins.ensure_running(plugin).await?;
    // prepared.url 里的 host 换成 127.0.0.1:<plugin port>，
    // path 和 query 原样保留。
    rewrite_host(&req.prepared.url, addr)?
} else {
    req.prepared.url.clone()
};
```

其余不动：`build_headers()` 还是走 `resolve_auth_headers()`，
插件供应商的 `auth` 是占位 key，不会把 tern 的 `accessToken`
泄露给插件；`is_streaming()`、`read_error_body()`、
超时和空闲超时照样适用。

### 5.5 `crates/tern-gateway/src/models.rs`、`router.rs`

- `router.rs`：不动。插件供应商的 id 就是路由前缀，
  `workbuddy/<模型>` 和 `magpie/workbuddy/<模型>` 的双前缀
  情况已经由“只按第一个 `/` 切分”覆盖了，加两个单测就行。
- `models.rs`：`fetch_models()` 对插件供应商走同样的候选规则，
  只是 baseUrl 是插件的临时地址。404 / 405 继续试下一个候选，
  401 / 429 / 5xx 直接返回，连接失败直接返回（DECISIONS §7.2）。

### 5.6 `crates/tern-gateway/src/gateway/usage.rs`、`crates/tern-store`

- 不动。插件返回标准格式时，用量解析和现在一样；
  返回非标准格式时，记 0 token、成本 NULL、面板标“未解析”，
  不混进模型分布。
- 失败行照样记 `outcome=failed`、`error_kind`、`status`、
  `provider_id`、`client_model`，和 §5 的“失败必须单独呈现”
  一致。

### 5.7 `crates/tern-cli`、`crates/tern-agent`、`crates/tern-app`

- `tern-cli`：加三个命令，不加就没法在没面板时排错：
  `tern plugin list`、`tern plugin status <id>`、
  `tern plugin logs <id>`。`tern check` 末尾加一段“插件”状态，
  和现在的“联网工具”“故障转移 / 熔断状态”并列。
- `tern-agent`：拥有 `PluginSupervisor`（常驻层），面板不直接
  spawn 插件。面板是纯遥控器，通过 `127.0.0.1:15801` 问 agent。
- `tern-app`（Tauri）：面板加一页“插件”，只做三件事：
  启用 / 禁用、看状态和日志、看模型列表。key 不进渲染进程
  的老规矩不变，插件的 session 文件也不在面板里打开。

### 5.8 依赖：不加

- 不加 Bun / Deno / wasmtime / 新的 HTTP 客户端。
  `tokio::process`、`reqwest`、`serde_json`、`axum` 都已经在了。
- 加依赖前先查 `~/.cargo/registry/cache/*/` 里有没有，
  `cargo ... --offline` 必须能编过（DECISIONS §7.3）。
  这台机器访问 crates.io 经常超时，这条不是建议，是硬约束。

## 6. 第一个插件：为什么先做 WorkBuddy，不先做豆包

### 先做 WorkBuddy

- 需求是真的：WorkBuddy 每天签到和做任务给积分，积分能换模型调用，
  社区里已经有 `workbuddy2api` / `workbuddy2api-panel` 这类转接项目，
  magpie 社区也有 WorkBuddy 插件。用户就是冲着这个来的。
- 但上游已经停更了：`workbuddy2api` 原仓库和下游面板都已停止维护
  或删库，原因是官方收紧了非官方客户端反代通道的特征检测和协议校验，
  加上灰产批量刷号倒卖。所以 tern 的 WorkBuddy 插件必须定位成
  **自用、小范围**，不是公开中转站，不做账号池、不做批量签到。
- 风险要写进插件 README 和面板提示里：反代是灰色地带，
  有封号风险，别拿主力号、别放关键业务。magpie 社区里已经有
  “慎用 magpie 反代，已经被封了好几个号”的帖子，tern 不能假装
  这个风险不存在。

### 不先做豆包

- 豆包 / Seed 的官方路已经通了：火山方舟 Ark 有官方 API，
  `doubao-seed-code` 也有 Claude Code 兼容的 Coding Plan，
  直接配 ProviderSpec 就能用，不需要反代客户端。
- magpie 社区里豆包反而是缺的那一个，有帖子问“magpie 里有
  各种厂商的反代插件唯独没有豆包，很奇怪”。缺的原因大概率
  是额度少、速度慢、容易撞高峰排队，加上国内厂商的风控更敏感。
  先做豆包等于先挑最难、最敏感的那一家。
- 所以顺序是：WorkBuddy 参考插件先跑通，证明插件契约能用；
  豆包以后如果真要做，先走官方 Ark API，不走客户端反代。

### WorkBuddy 参考插件长什么样（骨架，不是完整代码）

语言建议：先用 Python 写参考插件（签到和签名的逻辑改得快，
用户自己就能改），等契约稳定了再考虑要不要用 Rust 重写。
Rust 重写的好处是离线构建和单文件分发，坏处是改一行就要
重新编译，参考插件阶段不值得。

骨架（约 80 行能跑通，不含签到）：

```python
# tern-plugin-workbuddy serve --port $TERN_PLUGIN_PORT
# 1. 读 session.json（用户自己登录后存的 session，不进仓库）
# 2. GET /health -> {"ok": True, "models": [...]}
# 3. GET /v1/models -> OpenAI 兼容列表
# 4. POST /v1/chat/completions -> 给请求补签名 / token，
#    转发到 WorkBuddy 的真实端点，把 SSE 原样流回 tern
# 5. 401 时尝试刷新一次 session，刷新失败就返回 401，
#    不静默重试到封号
```

验收：Claude Code 通过 tern 写 `workbuddy/<模型>` 跑完一次
真实会话，用量面板里能看到花费和 token，失败行单独聚类，
插件进程杀掉后网关还在，下一次请求能重新拉起或报出
清楚的 502，而不是网关一起死。

## 7. 演进路线：三步，不一步到位

### Step 0：本文档（本 PR，只有文档，没有代码）

- 落 `docs/plugins-external-process-design-zh.md`，ROADMAP 里
  加一节链接到本文档，说明“插件”在路线图里的位置。
- 不改任何 `.rs`，不改 `Cargo.toml`，不加依赖。
  验收：`cargo test --workspace --offline` 不用跑也知道还是绿的，
  因为没动代码。

### Step 1：鉴权外包（约 200 行，先做这一步）

- 加 `ProviderAuth::PluginCommand { command: String, args: Vec<String> }`，
  实现一个 `TokenProvider`：每次需要 token 时跑一次命令，
  命令在 stdout 输出一行 JSON `{"token": "...", "account_id": "..."}`，
  超时 5 秒，失败就报 `AuthError`。
- 这一步先解决 ROADMAP 里“订阅类 OAuth token 交换没实现”的
  一半：Copilot / ChatGPT / xAI / WorkBuddy 的登录逻辑可以先
  放在外部命令里，网关只管按需取 token，不用在 Rust 里实现
  每一家的 OAuth。
- 验收：`tern check` 能显示插件命令的 token 是否能取到；
  现有供应商回归不破。

### Step 2：Sidecar 进程（新模块 `plugin.rs`，约 300 行目标）

- 实现 §5.2 的 `PluginSupervisor` + §4 的 manifest / 健康检查 /
  生命周期，`tern plugin list/status/logs` 三个 CLI 命令，
  一个 WorkBuddy 参考插件（Python，先不进仓库主分支，
  放 `tools/` 或单独的示例仓库，由用户定）。
- 验收：见 §8。

### Step 3：面板（最后做）

- 面板加“插件”页：启用 / 禁用、状态、日志、模型列表。
- 不做插件市场，不做自动更新。面板只做遥控器，不直接 spawn 插件。

明确推迟到 v2 以后再讨论的：网关中间件（`onRequest` 改请求体）、
WASM 插件、插件市场、插件自动更新、50+ agent 适配。

## 8. 验收

改 `gateway/handlers.rs` 的转发路径后，除了 `cargo test -p tern-gateway`，
还必须跑 `--test usage_e2e` 和 `--test gateway_e2e`
（DECISIONS §5：任何改动 `RequestMeta` / `UsageEvent` 填充逻辑的
提交都要用 e2e 验证，单测全绿也可能漏 `session_id`）。

- [ ] `cargo test --workspace --offline` 通过（含新增的插件单测）。
- [ ] `cargo clippy --workspace --all-targets` 通过。
- [ ] e2e：插件进程被杀后网关还在，下一次请求能重新拉起或报清楚的 502；
  `usage_e2e` 里 `session_id` 不为 None。
- [ ] `tern check` 能显示插件健康状态；`tern plugin list` 能列出模型。
- [ ] 真实会话：Claude Code + Codex 各跑一次，`workbuddy/<模型>`
  能出回答，用量面板里花费和 token 对得上，失败行单独聚类。
- [ ] 回归：DeepSeek / StepFun 等现有供应商不受影响，
  `provider/model` 显式路由和 `defaultProvider` 兜底行为不变。
- [ ] 真核对（ROADMAP 第 3 步的地基）：`tern usage --from/--to`
  钉死账期后，能和上游账单对上，插件走的账也一样。

## 9. 未决问题（需要用户拍板，本文档不替用户定）

1. 参考插件用什么语言写？Python（改得快）还是 Rust（离线构建、
   单文件分发）？本文档建议 Python 先跑通契约，Rust 以后再说。
2. 端口分配：tern 分配临时端口再传给插件（本文档选的），
   还是插件自己绑 `0` 再把端口打印到 stdout？后者少一次
   释放再绑定的竞态，但 stdout 解析容易出竞态，两害相权取其轻，
   选了前者。
3. 插件凭证放哪？每个插件一个单独的 session 文件（本文档选的），
   还是统一放 `tern.json` 里？统一放的好处是备份方便，坏处是
   插件能读到不属于它的 key。本文档选分开。
4. 要不要做一个“magpie 插件桥”：tern 的插件进程里直接跑
   magpie 的插件？那样能立刻用上 magpie-community 的插件，
   但要把 Bun 也带进来，轻量就丢了。本文档建议不做，
   先用 §2.2 的 magpie 当上游桥接验证需求。
5. 插件目录放哪？`%APPDATA%\tern\plugins\`（本文档选的，
   和 `tern.json` 同目录，好备份）还是 `%LOCALAPPDATA%`？
   Windows 上两者都常见，定一个就行，别两头写。

## 附录 A：`tern.json` 里的插件供应商长什么样

```json
{
  "listen": "127.0.0.1:15800",
  "accessToken": "换成你自己的随机串",
  "defaultProvider": "deepseek",
  "providers": [
    {
      "id": "workbuddy",
      "name": "WorkBuddy",
      "baseUrl": "http://127.0.0.1:0",
      "apiFormat": "openai_chat",
      "auth": { "type": "api_key", "key": "plugin" },
      "plugin": { "pluginId": "workbuddy" }
    }
  ],
  "plugins": [
    { "id": "workbuddy", "enabled": true }
  ]
}
```

注意：`baseUrl` 里的 `127.0.0.1:0` 是占位，真正地址由
supervisor 在启动插件后填。不要在配置里写死一个端口，
那个端口下次启动就不在了。`plugins[]` 里 `enabled: false`
的插件，manifest 读到了也不拉起。

## 附录 B：请求流（文字版时序）

```
1. Claude Code 发 POST /v1/messages，model="workbuddy/hunyuan-a52b"
2. tern router：按第一个 "/" 切出 provider=workbuddy，
   upstream_model="hunyuan-a52b"，kind=explicit
3. tern adapter：按 (客户端协议, 上游协议) 翻译请求体
4. tern plugin supervisor：workbuddy 没跑 -> 读 plugin.toml，
   分一个临时端口，拉起 tern-plugin-workbuddy，
   5 秒内 GET /health 200 才继续
5. tern upstream：POST 到 127.0.0.1:<port>/v1/chat/completions，
   请求头只带插件需要的占位 key，不带 tern 的 accessToken
6. 插件：补签名 / 刷新 token，转发到 WorkBuddy 真实端点，
   SSE 原样流回 tern
7. tern response：转换回 Anthropic Messages，出口处记用量
   （client_model / upstream_model / role / session_id）
8. Claude Code 收到回答；面板“最近请求”里这条标 explicit，
   不是 fallback
```

如果第 4 步插件起不来，第 5 步不发，直接返回 502，
面板“失败”里按（原因，供应商，状态码）聚类，不混进模型分布。
