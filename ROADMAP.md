# 路线图

## 定位

参照 [yetone/magpie](https://github.com/yetone/magpie) 的简洁体验（agent × 模型单屏、本地网关按
`provider/model` 路由），复用 cc-switch 的协议转换层，甩掉它的包袱（按 agent 存配置快照、
切供应商要改 agent 配置、十几个 app 类型）。

- MVP 只支持 Claude Code 和 Codex 两个客户端
- **核心卖点是用量面板**：花了多少钱、花在哪、缓存省了多少，一眼看清
- 差异化：保留 cc-switch fork 里的供应商文件夹与智能分组

## 已完成

| 阶段 | 提交 | 内容 |
|---|---|---|
| 1 | `38703c1` | 从 cc-switch 搬 47 个纯转换文件，与上游逐字节一致 |
| 2 | `64eeb13` | 中立的 `ProviderSpec`；`adapter::prepare_request` 按（客户端协议, 上游协议）改写请求 |
| 3 | `3dfa37e` | `provider/model` 路由、axum 转发层、流式/非流式响应转换、按客户端协议改写错误；单测 951 + e2e 10 |

## 阶段 4：能跑起来

目标：不等界面，先能在真实环境里用，攒真实请求数据给后面的用量面板。

- [x] 新 crate `crates/tern-cli`，二进制名 `tern`：`serve` / `init` / `check`
- [x] 配置文件默认路径 `%APPDATA%\tern\tern.json`，`TERN_CONFIG` / `--config` 覆盖；`init` 生成样例和随机 accessToken
- [x] 日志：`env_logger`，默认 info，`RUST_LOG` 覆盖；key 已在 `ProviderSpec` 的 Debug 里遮蔽
- [x] 启动时提醒：占位符 key、未设 accessToken、订阅类认证（命令行版无 TokenProvider）
- [ ] 验收：本机用 Claude Code + Codex 各跑一个真实会话

参数解析是手写的，没用 clap（只有三个子命令；env_logger 也关了 humantime / regex 默认特性）。

## 阶段 5：用量采集与存储（面板的数据底座）

先把数据做对，再谈好看。数据错了图再漂亮也没用。

**采集点**：网关已经把响应转换成客户端协议，所以只需在出口处解析两种格式——
Claude 侧用 `TokenUsage::from_claude_stream_events / from_claude_response`，
Codex 侧用 `from_codex_stream_events_auto / from_codex_response_auto`。
cc-switch 为每种上游各写一套收集器，这里不需要。

- `gateway` 加 `UsageSink` trait，网关只产出事件，不碰数据库
- 流式：包一层旁路解析，不阻塞转发；流被客户端中断时也要落一条（标记 `aborted`）
- 失败请求也记录（状态码、错误摘要、耗时），面板要能看错误率
- 新 crate `crates/tern-store`：SQLite（rusqlite），一张 `requests` 表

**`requests` 表字段**：时间、客户端（claude/codex）、供应商 id、会话 id、新鲜输入 / 输出 /
缓存读 / 缓存写 token、成本、首 token 耗时、总耗时、状态码、是否流式、错误摘要，以及三个模型字段：

- `client_model`：客户端原样发来的（如 `claude-sonnet-4-6`）
- `upstream_model`：路由后实际发出的（如 `claude-opus-5.5`）
- `route_kind`：`explicit`（写了 `provider/`）还是 `fallback`（落到 defaultProvider）

再加一个 `role` 字段，从 Claude Code 的请求推断：主对话 / 子代理（Task）/ 后台小模型（haiku）。

**来自 cc-switch 实际数据的教训**（2026-10-01 当天 562 条记录）：
- 模型统计按"响应里的模型"分组，失败请求没有响应，就退回到请求模型。结果 8 条 429
  显示成"claude-sonnet-4-6，8 次请求，0 token"，看起来像偷偷换了模型，其实是子代理并发
  撞了上游的 `gateway_concurrency_limit`。**失败必须单独呈现，不能混进模型分布**
- 212 条 `claude-sonnet-4-6 → claude-opus-5.5` 是子代理请求，被供应商映射到了 Opus。
  用户看不出"谁在花钱"，所以要有 `role` 维度
- 模型没定价时整张成本图是一条 0 线。未定价要醒目提示，并一键跳到定价

**口径坑**（cc-switch 踩过的）：
- Anthropic 的 `input_tokens` 不含缓存读，OpenAI Responses 的含。入库前统一成"新鲜输入"，
  否则缓存命中率和成本都会算错
- 全 0 usage 不入库（上游省略 usage 时转换器会合成 0）
- 同一条消息可能被记两次（重试、SSE 聚合兜底），用 `message_id` 去重

**定价**：
- 移植 cc-switch `usage/calculator.rs`（`rust_decimal`，避免浮点误差）
- 价格表来源：内置常用模型 + 从 models.dev 同步 + 用户手填覆盖
- 供应商级倍率（中转站常见"0.3 倍价"）
- 没有定价的模型照样记 token，成本显示"未定价"而不是 0

## 阶段 6：桌面壳 + 用量面板

Tauri 2 + React + Tailwind，图表用 recharts（cc-switch 现成经验）。
**先做视觉稿定稿，再写代码**：用静态 HTML + 假数据出 2~3 版对比，选定后再接真实数据。

面板首屏（草案，待视觉稿确认）：
1. **今日卡片**：花费、请求数、token 数、缓存命中率，各带与昨日对比
2. **花费趋势**：按天堆叠柱状图，按供应商着色；可切 7 天 / 30 天 / 自定义
3. **花在哪**：供应商 → 模型的占比（环形图或 treemap），点击下钻
4. **缓存省了多少**：按缓存读价格 vs 新鲜输入价格算出节省金额。这是 Claude Code 用户最关心
   但 cc-switch 没有直接给的数字
5. **会话视图**：按 `session_id` 聚合，看"这次重构花了多少"
6. **请求流**：最近请求实时滚动，失败标红，点开看详情（不存请求 / 响应正文）
7. **活跃度热力图**：GitHub 风格的日历格子

8. **模型流向**：`客户端模型 → 实际模型`的桑基图或流向条，一眼看出"子代理请求的
   sonnet 全被送去了 Opus"
9. **失败面板**：按原因聚类（限流 / 连接失败 / 鉴权），不混进模型统计

视觉方向：参照 OpenRouter Activity 的信息量，但不照搬它的样子。要有自己的配色和卡片
层叠方式，让人一眼记住。不用 cc-switch 现在这种"灰底表格 + 默认蓝按钮"的设置页风格；
供应商名不能在窄列里折成三行。

性能与体验：
- 聚合查询走 SQL（按天预聚合表），不把全量明细拉到前端
- 数字用 tabular-nums 等宽，金额统一保留到分，token 用 K/M 缩写
- 深浅色都要好看；空数据状态要有引导而不是空白图表

## 阶段 7：供应商管理

- 供应商增删改、连通性测试、从 cc-switch 数据库一键导入
- 文件夹分组 + 智能分组（迁移 cc-switch fork 的 TypeSafe 方案，见 cc-switch 的
  `services/folder_suggest.rs`）
- 一键写入 Claude Code / Codex 配置，指向 tern 并设好模型名
- 订阅登录（Copilot / ChatGPT / xAI），实现 `TokenProvider`

## 阶段 8：健壮性

按真实使用中遇到的问题排序，不预先全做：
- 故障转移与熔断（`proxy/circuit_breaker.rs` 已搬运，接上即可）
- Copilot 动态端点、按模型厂商选 Responses / Chat
- 原生 Anthropic 上游的请求头大小写保持（部分中转站按指纹校验）
- Gemini OAuth refresh token 换取

## 待定问题

- 是否要导入 cc-switch 的历史用量数据（`proxy_request_logs` 表），口径需要逐列核对
- 桌面应用是否需要托盘常驻
