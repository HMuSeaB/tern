# tern — 决策与架构

> 从 `~/.claude/projects/D--4rchive-Code-cc-switch/memory/` 的 12 份散装记忆合并而来
> （2026-10-29）。**读这一份就够了**，不要再去找原来那 12 个文件。
>
> 配套：`docs/STATUS.md` 管"现在在哪"，`ROADMAP.md` 管"为什么这么做"，
> 本文管"这些判断的完整来历和不能违反的规矩"。

## 目录

1. [从 cc-switch 分出来的理由](#1-从-cc-switch-分出来的理由)
2. [分层：谁常驻、谁按需开](#2-分层谁常驻谁按需开)
3. [外壳定向：保切换、推前缀](#3-外壳定向保切换推前缀)
4. [面板的四条硬规矩](#4-面板的四条硬规矩)
5. [用量口径为什么必须分开记失败](#5-用量口径为什么必须分开记失败)
6. [容易踩的坑（全是只在打包/上线时才炸的）](#6-容易踩的坑)
7. [上游依赖的三条硬约束](#7-上游依赖的三条硬约束)
8. [一直没解决的：真核对](#8-一直没解决的真核对)

---

## 1. 从 cc-switch 分出来的理由

**时间**：2026-10-01。起因是用户看了 `yetone/magpie`（Go + Wails，agent×模型
单屏，本地网关按 `provider/model` 路由），觉得 cc-switch 臃肿。

用户的原话判断：「更多只是参考了 ccswitch」「还需要改写」。方向定为**不做全 TUI、
不重写内核**，只做两件事：让显式路由成为推荐路径，把用量护城河提到首屏。

### 搬了什么、没搬什么（`src-tauri/src/proxy/`，66 文件）

- **48 个文件可直搬**（约 1.4 MB，含 750 个测试）：`transform*` / `streaming*` /
  `sse` / `circuit_breaker` / `thinking_*` / 各 OAuth auth 模块。
  `error.rs` 只有一个 `DatabaseError` 变体要改名。
- **薄耦合**：adapter 依赖 `Provider{settings_config: Value, meta}`；`transform_codex_chat`
  只依赖 `CodexChatReasoningConfig`。
- **必须重写**：`forwarder.rs`（5026 行，通过 tauri `AppHandle` 取 OAuth state）、
  `handlers.rs`（按路径决定 `AppType`）、`provider_router.rs`（从 DB 读当前供应商）、
  `usage/logger` 与 `response_processor`（写 DB）、`failover_switch`（托盘）。
- **核心架构差异**：cc-switch 的路由键是 `AppType → 当前供应商`；tern 改成
  请求体 `model` 字段 `provider/model` → 供应商。

### 搬运的纪律

搬进来的纯转换文件**逐字节与上游一致**，本地只改写 `proxy/mod.rs`、
`providers/mod.rs`、`usage/mod.rs`、`provider.rs`，差异记录在 `UPSTREAM.md`。
`dead_code` 警告用 `lib.rs` 上的模块级 `allow` 压掉，**不改被搬运的文件**。
理由：以后能从上游 cherry-pick 转换层的修复。

各阶段提交：`38703c1`（阶段 1）→ `64eeb13`（阶段 2）→ `3dfa37e`（阶段 3）→
`2f93ed7`（阶段 4）→ `7badcc4`（阶段 5）。

---

## 2. 分层：谁常驻、谁按需开

这一条推翻过两次，现在的形态是第三版。

**第一版**：配置和看账绑死在一个 Tauri 窗口里，为了看账得常驻 400 MB。

**第二版（2026-10-06）**：用户实测 `tern-app` 内存 **408 MB**
（主进程 31 MB + 6 个 WebView2 渲染进程 377 MB）。当时决定按「方案 1」拆成
TUI 常驻 + 面板按需开。TUI 参照 magpie 的 `magpie tui`（Bubble Tea/lipgloss 的
Unicode 方块 sparkline `▁▂▃▄▅▆▇█` 和八格配额仪表）。

> 顺带纠正当时的错误判断：**magpie 不是 TUI**。它是 Go（`go.mod`，
> `CGO_ENABLED=0`，`modernc.org/sqlite`），**三套 UI 并存**——Wails v3 桌面壳
> + Bubble Tea TUI + `magpie web`。用户截图是 Web UI 那套。判据：TUI 页签是
> `agents/providers/routing/usage/sessions/library`（6 个，**无 Gateway**），
> GUI 导航有 Gateway。它 Docker 用 `-tags nogui` 裁掉 GUI，同一二进制两种形态。

**第三版（2026-10-07，现行）**：用户原话「这个终端其实后台不需要显示吧，只有我
要显示的时候显示面板」。TUI 会占一个终端窗口，这条直接否了它。常驻层改成
**headless agent**：

| 层 | 内存 | 职责 |
|---|---:|---|
| `tern-agent` | 工作集 12.4 MB / 私有 5.3 MB（debug 实测） | 持有网关；控制端 HTTP `127.0.0.1:15801`（`/api/status`、`/api/gateway/start\|stop\|restart`） |
| `tern panel`（Tauri） | 约 408 MB | 纯遥控器，`server.rs` 不再 spawn 网关线程，全转问 agent。**关窗口不再停网关** |
| `tern tui` | 约 12 MB | 仍可用，但不再是常驻层 |

`tern-agent` 的细节：鉴权复用 `tern.json` 的 accessToken，**恒定时间比较**；
单实例靠"绑一下控制端口"判定，第二个实例安静退出码 0。

**用户追加的一条关键否决**：**不要检测 cc-switch 擦掉接线后自动写回，也不要轮询。**
理由是用户可能正故意在用别的代理，"帮忙"就是跟他打架；5 秒轮询也是白耗。
被擦掉的接线由面板每次打开时如实读，显示成未接入，用户自己点一下恢复。

---

## 3. 外壳定向：保切换、推前缀

**时间**：2026-10-20。起因是一篇 Magpie 对比分析（存于 `D:\4rchive\library\wechat\`）。

三条病灶，全在外壳，都局部：

1. **招牌路由没被走到**。`router.rs` 的显式路由设计是对的，但 GUI 主推路径是
   「切供应商 → 写 `default_provider` → 把 `client_env` 写进 `~/.claude/settings.json`」，
   而 `client_env` 是从 cc-switch 原样照搬的**裸模型名**。客户端发裸名 → 无 `/` →
   走 fallback。**所以 tern 的招牌功能只有手改 env 的人才用得上。**
2. **护城河被扣在桌底**。`model_flow`（抓子代理 sonnet→opus 被供应商换掉）、
   `role`、失败不混统计、缓存省钱——这些 cc-switch 和 Magpie 都没有，
   此前全在折叠的二级视图。
3. **既视感**：视觉只是皮；根子是"选一家、写裸名、default 兜底"这条 cc-switch
   路径还在主位上。

**定的方向**：**不重写**，外壳「保切换，推前缀」（保留切换交互，同时把
`provider/model` 提为推荐路径，**不砍 `client_env`**），视觉用 B 版工作台
（`design/b-workbench.html`）。**明确不追 Magpie 的 50+ agent 适配**——
护城河在用量不在 agent 数。

### 这条判断的复用处

以后动 tern 外壳时，先问一句：**这条改动让显式路由更容易走到，还是更容易绕开它？**
任何新增的"切一家"快捷方式，都要同时给出 `供应商/模型名` 的写法。

---

## 4. 面板的四条硬规矩

T+1~T+4 于 2026-10-08 提交（`1cebd25` / `5169c99` / `9b7766e` / `3f3a448`）。
**下次改面板前必须先读这四条。**

**tab 化**：离屏页用 `inert` 禁焦点但**不从 DOM 摘掉**。摘掉的话 `usePanel`
的轮询会随页卸载，翻回「用量」先看空页再等下一次 tick；分组展开态、搜索词也会丢。
代价是四页同时在布局里，所以每页必须 `inert`，否则 Tab 键跑进看不见的内容。

**只读查询复用**：`tern_store::Store::open_readonly()`（不建库、不迁移、不设 WAL）。
此前 `tern-app` 的 `build_panel` **手抄了一份** `SUMMARY_COLUMNS`，和
`tern-store::query` 的逐字对齐只靠注释维持——这是那次重构的主要动机。
**规则：新查询一律加在 `tern-store`，面板只调方法。**

**key 不进渲染进程**：`provider_detail` 没有 key 字段，编辑框永远空着、
留空 = 不修改。代价是改 key 要整段重填；不这么做的代价是凭据进了 webview 拿不回来。

**故障转移三条规矩**：显式 `provider/model` **不转移**；只在上游**没给响应头**时换
（流式发一半换家会拼出两段回答）；熔断进程内有效、不落盘。阈值属于网关整体
而非某个 app——照搬 cc-switch 的 `AppProxyConfig` 会要求为 claude/codex 各配一份。

---

## 5. 用量口径为什么必须分开记失败

**用户对 cc-switch 用量页的具体不满（2026-10-01 查库证实）**：模型统计里冒出
「claude-sonnet-4-6 8 次 0 token」，其实是 8 条子代理请求被上游 429
`gateway_concurrency_limit` 拒了，失败行没有响应 model 就退回请求模型；
而成功的 212 条 sonnet 请求被供应商映射成了 opus-5.5。

所以 tern 面板**必须**把失败和模型分布分开展示，并记 `client_model` /
`upstream_model` / `role`（主对话 / 子代理 / haiku）。这条不是洁癖，是
"钱花在了不是你要的模型上而日志里一条错都没有"这个场景唯一的暴露口。

**视觉**：用户只用过 OpenRouter，要求自创配色和卡片层叠，不要照搬。

### e2e 抓得住单测抓不住的东西

`crates/tern-gateway/tests/usage_e2e.rs` 覆盖入库字段的完整链路
（HTTP 进来 → 路由 → 转换 → 记账落库）。它抓到过：拆 `send_to` 时漏了
`meta.session_id = ..`，1019 条单测全绿，e2e 立刻报 `session_id: None`。

**怎么用**：改 `gateway/handlers.rs` 的转发路径后，除了
`cargo test -p tern-gateway`，**还必须跑** `--test usage_e2e` 和
`--test gateway_e2e`。更一般地：任何改动 `RequestMeta` / `UsageEvent` 填充逻辑的
提交都要用 e2e 验证。

---

## 6. 容易踩的坑

这一节全是**只在打包或上线时才炸、本地 `cargo run` 完全正常**的一类问题。

### Tauri 的 Rust crate 与 npm 包版本必须一致

`tauri build` 会硬校验，不一致直接退出：

```
Found version mismatched Tauri packages.
tauri (v2.11.6) : @tauri-apps/api (v2.12.1)
```

根因：`[workspace.dependencies]` 里写 `tauri = { version = "2" }` 时，cargo 解析到
**已缓存的最高 2.x**，不一定和 `pnpm-lock.yaml` 对齐。两边各自演化，只在打包时暴露。

对策：workspace 里钉死 `version = "=2.12.1"`（`=` 精确），再
`cargo update -p tauri --precise 2.12.1` 同步 lock。npm 侧看
`crates/tern-app/package.json` 里的 `@tauri-apps/api` / `plugin-opener` / `cli`。
**升降级任一侧时两边一起改。**

### workspace 里 bundle 落在根 `target/`

`src-tauri/target/` 只存在于单 crate 项目。当 `src-tauri` 属于 workspace
（根 `Cargo.toml` 的 `members` 里有它）时，bundle 被提升到
`<workspace 根>/target/release/`：

```
target/release/tern-app.exe
target/release/bundle/nsis/tern_0.1.0_x64-struct.exe
```

判据：CI 日志里 `Built application at:` 和 `Finished N bundle at:` 两行给的是绝对
路径，照抄即可。`.github/workflows/release.yml` 因此**同时扫两个位置**。
tern 第一次发布白跑了一轮 13 分钟的构建才发现，而安装包本身是好的（8.9 MB）。

---

## 7. 上游依赖的三条硬约束

### 7.1 `~/.claude/settings.json` 只认这一份，且被 cc-switch 整份托管

**用户级 Claude Code 只读 `~/.claude/settings.json`。** 放
`settings.local.json` 里的任何东西都不会被应用。

实测判据（用临时 `CLAUDE_CONFIG_DIR` 隔离，不碰真实配置）：
- 对照组：`settings.json` 的 `env.ANTHROPIC_BASE_URL` 指死端口 → `claude -p` 报
  `ConnectionRefused`，证明该文件确实被读
- 实验组：`settings.json` 仍指死端口、`settings.local.json` 指活端口 → 依旧
  `ConnectionRefused`

二进制里 `legacy settings.local.json` 那串错误信息指的是**项目级**
`.claude/settings.local.json` 的迁移路径，与用户级无关。

⚠ headless `-p` 模式会自动放行工具调用，**不能**用它测 permissions
（拿 deny 规则当探针会得假阴性）。

**cc-switch 整份托管**：`services/provider/live.rs` 的
`sanitize_claude_settings_for_live` 只剥 `api_format` / `apiFormat` /
`openrouter_compat_mode` / `openrouterCompatMode` 四个内部字段，其余原样覆盖；
而 `write_claude_live` 写入的是**供应商的 settings_config**。所以每次切供应商，
`~/.claude/settings.json` 除这四个字段外的**所有内容**（permissions、hooks、
statusLine、用户自己的字段）全部被重写。

**推论**：任何工具往这个文件写的持久状态都会被 cc-switch 下一次切换擦掉。
要么如实显示并被擦后重写，要么常驻监听重抢，**没有第三条路**。
（这也是冲突风险的来源——tern 的 `claude_settings.rs` 写的是同一个文件。）

cc-switch 的库在 `~/.cc-switch/cc-switch.db`（不在 `%APPDATA%`，
`AppData/Roaming/com.ccswitch.desktop` 里只有窗口状态）。

### 7.2 中转站没有统一的模型端点，靠候选规则试

规则在 `crates/tern-gateway/src/models.rs`（`fetch_models` +
`build_models_url_candidates`），31 个测试。原规则来自 cc-switch 的
`services/model_fetch.rs`：

1. override 非空 → 只用它
2. baseURL 已以版本段 `/v{N}` 结尾（`/v1`、智谱 `/api/coding/paas/v4`）→
   `{base}/models`，**不能再补 `/v1`**（否则 `/v4/v1/models` → 404）；版本不是 v1
   时再追加 `{base}/v1/models` 兜底
3. 否则 → `{base}/v1/models`
4. 命中已知 Anthropic 兼容后缀 → 剥离后加 `{root}/v1/models`、`{root}/models`

已知后缀**按长度降序**（最长优先，否则 `/anthropic` 会提前吃掉 `/api/anthropic`）：
`/api/claudecode`、`/api/anthropic`、`/apps/anthropic`、`/api/coding`、
`/claudecode`、`/anthropic`、`/step_plan`、`/coding`、`/claude`

错误处理：**404 / 405 当"这个候选不对"继续试**；401 / 429 / 5xx 直接返回
（鉴权失败换候选没用）；连接失败（DNS / 超时 / 拒绝）直接返回
（继续试只是把等待时间乘以候选数）。

**Why**：中转站模型名千奇百怪（`step-3.5-flash-2603`、`kimi-k2.5`），让用户猜只会
得到一句 404；上游基本都有 OpenAI 兼容的 `/models`，问它就行。

### 7.3 依赖只用本地缓存里有的版本

这台机器访问 crates.io 经常超时。**加依赖前先查
`~/.cargo/registry/cache/*/` 里有没有**，`cargo ... --offline` 必须能编过。
这就是不用 clap、不用 chrono-parser 的原因。

---

## 8. 一直没解决的真核对

**2026-10-07 做过一次端到端验收**（临时 `TERN_CONFIG` / `TERN_DB` 沙箱，
没动现有 `tern.json`）：

通过：导入（38 个供应商、0 跳过，面板读活库那条路同样 38/0）→ 鉴权（不带
accessToken 请求 `/v1/models` 得 401）→ 路由（前缀与 defaultProvider 兜底都通）
→ 转发（请求真的到了 `api.deepseek.com`）→ 报错质量（上游 401 完整带回，
含掩码 key 后缀、provider id、模型、`upstream_status`，可以直接抄给用户看）
→ 记账（失败请求也完整落库：`outcome=failed`、`status=401`、`error_kind=auth`、
`duration_ms=272`、`provider_id`、`client_model` 都有）。

**当时唯一的坏消息**：DeepSeek 的 key 被 DeepSeek 自己拒收（401 invalid）。
比对过指纹，SQL 导出（10-06）和活库（当天 13:03）里是**同一把**
（sha256 前 12 位 `7bc2537da300`，len 35），所以不是导出过期。

⚠ 38 个供应商里有 3 个 baseUrl 是 `http://0.0.0.0:4000` / `127.0.0.1:*`，
那些是本地别的代理，连上去不会有意义。

**2026-10-29 更新**：用户确认 **StepFun 上游一直可用**，实测
`api.stepfun.com/step_plan/v1/messages` + `step-5-preview` 返回 **HTTP 200**。
所以「缺一家能用的 key」这个外部条件**已经解除**——真核对现在只是执行问题。
口径与检查表见 `docs/guides/reconciling-with-upstream-billing-zh.md`。
