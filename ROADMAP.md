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
| 4 | `2f93ed7` | `tern` 命令行：`serve` / `init` / `check` |

## 阶段 4：能跑起来

目标：不等界面，先能在真实环境里用，攒真实请求数据给后面的用量面板。

- [x] 新 crate `crates/tern-cli`，二进制名 `tern`：`serve` / `init` / `check`
- [x] 配置文件默认路径 `%APPDATA%\tern\tern.json`，`TERN_CONFIG` / `--config` 覆盖；`init` 生成样例和随机 accessToken
- [x] 日志：`env_logger`，默认 info，`RUST_LOG` 覆盖；key 已在 `ProviderSpec` 的 Debug 里遮蔽
- [x] 启动时提醒：占位符 key、未设 accessToken、订阅类认证（命令行版无 TokenProvider）
- [ ] 验收：本机用 Claude Code + Codex 各跑一个真实会话

参数解析是手写的，没用 clap：这台机器访问 crates.io 经常超时，依赖只用本地缓存里已有的版本，
`--offline` 能构建。env_logger 也关了 humantime / regex 默认特性。

## 阶段 5：用量采集与存储（面板的数据底座）

先把数据做对，再谈好看。数据错了图再漂亮也没用。

- [x] `gateway/usage.rs`：`UsageSink` trait，网关只产出 `UsageEvent`，不碰数据库
- [x] 出口处只解析两种客户端协议；流式包一层旁路解析（只留 usage / error 事件，不缓冲正文）
- [x] 客户端中途断开在 `Drop` 里补记 `aborted`，已产生的输入 / 缓存读照样计费
- [x] 失败请求入库：状态码、`error_kind`（限流 / 过载 / 鉴权 / 超时 / 连接 / 上游 4xx·5xx / 请求无效…）、错误摘要
- [x] `role`：Claude 看 `__SUBAGENT_MARKER__`、compact 提示词、有无工具；Codex 看 `x-openai-subagent` 和 `/compact`
- [x] 新 crate `crates/tern-store`：SQLite（WAL），`requests` 明细 + `daily` 按天预聚合（同事务维护）+ `prices` 覆盖
- [x] Responses 的 input 入库前扣掉缓存读写；全 0 usage 记为无 token；`(provider_id, message_id)` 唯一去重
- [x] 定价：`rust_decimal`，成本以纳美元整数入库；内置 201 条（cc-switch 种子 + 本机库）< models.dev < 手填
- [x] 计价模型取上游回显 → 路由后的模型，**不退回客户端别名**；供应商 `costMultiplier`
- [x] 未定价记 token、成本为 NULL；补价时只补未定价行，已定价的历史账单不重算
- [x] 缓存省下的钱：缓存读 ×（输入价 − 缓存读价）× 倍率
- [x] 模型分布不含失败行；失败按（原因, 供应商, 状态码）单独聚类
- [x] CLI：`serve` 自动记录到配置同目录的 `usage.db` 并每请求打一行摘要；`tern usage`；`tern price list/set/rm/sync`
- [ ] 验收：真实会话跑一天，对照上游账单核对成本

单测 + e2e：网关 960 + 16，store 19，CLI 17。

**留到后面**：models.dev 定时自动同步（目前手动 `price sync`，网络不通时可 `--file` 导入）；
导入 cc-switch 历史用量（见待定问题）。

### 设计记录

- 采集点在出口：响应已经是客户端协议，Claude 用 `from_claude_*`、Codex 用 `from_codex_*_auto`，
  不用像 cc-switch 那样每种上游一套收集器
- `count_tokens` 不记录
- `session_id` 只记客户端自带的（网关兜底生成的聚合没意义）
- 四个 token 桶互斥：`fresh_input + cache_read + cache_write` 就是全部输入

**来自 cc-switch 实际数据的教训**（2026-10-01 当天 562 条记录）：
- 模型统计按"响应里的模型"分组，失败请求没有响应，就退回到请求模型。结果 8 条 429
  显示成"claude-sonnet-4-6，8 次请求，0 token"，看起来像偷偷换了模型，其实是子代理并发
  撞了上游的 `gateway_concurrency_limit`。**失败必须单独呈现，不能混进模型分布**
- 212 条 `claude-sonnet-4-6 → claude-opus-5.5` 是子代理请求，被供应商映射到了 Opus。
  用户看不出"谁在花钱"，所以要有 `role` 维度
- 模型没定价时整张成本图是一条 0 线。未定价要醒目提示，并一键跳到定价

## 阶段 6：桌面壳 + 用量面板

Tauri 2 + React + Tailwind，图表用 recharts（cc-switch 现成经验）。
**先做视觉稿定稿，再写代码**：用静态 HTML + 假数据出 2~3 版对比，选定后再接真实数据。

- [x] 应用**内嵌网关**：双击 exe 就能用，不再需要先开一个终端跑 `tern serve`。
  `server.rs` 自建 tokio 运行时跑 serve，停机用 `Notify` 唤醒 select! 里的中止分支，
  关窗即停（不留在后台占端口）。好处是用户少一步、网关死了面板不会显示旧数据。
- [x] 首次运行向导：探测 cc-switch → 展示将导入什么 → 用户勾选确认后才落盘。
  凭据确认不能默认勾选、不能折叠——那是把 key 从 A 工具搬到 B 工具，得用户自己决定。
- [x] 面板首屏：cc-switch 使用统计页的密度 + B 版视觉（hero 大数字 + 4 小卡 + 命中率条）
- [x] `web_tools.rs`：识别会让 WebSearch / WebFetch 失效的第三方网关，`serve`/`check` 均提示
- [x] 视觉稿三版（`design/`），已提交待选版
- [ ] **待用户选版**后再打磨视觉；当前首屏按"cc-switch 密度"实现，够用但不出彩
- [ ] Rust 侧补 `breakdown` / `sessions` 查询，前端做趋势与占比图（二级视图）
- [ ] 验收：装好后本机跑几天，对照上游账单核对成本口径

**做过又推翻的决定**：面板最初刻意不在进程内跑网关、只当 `tern serve` 的观察窗口。
那个判断对"只做面板"成立，对"点 exe 就用"不成立——用户还得先开终端，且网关死了
面板仍在显示旧数据。所以改成应用自己持有网关。

- [x] `design/` 三版静态视觉稿 + `index.html` 启动页 + `README.md` 数据口径
  - `a-terminal.html` 工程终端：深色密集、等宽数字、单一青强调
  - `b-workbench.html` 工作台：浅色默认可切深、teal 强调、表格化请求流
  - `c-canvas.html` 画布：暖色深底、超大 hero、treemap 主视觉 + 桑基模型流向
  - 三版共用同一套假数据（一张供应商×模型成本矩阵，行和=供应商总额，列和=模型总额，
    两端都是 $18.47；`step-5-preview` 未定价 cost=0 只计 token），只比视觉不比重算
  - 剧情照 cc-switch 真实教训埋：sonnet→opus 映射、12 次限流单独聚类、未定价醒目
- [ ] **待用户选版**：在这三版里定一版（或提改法）
- [ ] 新建 `crates/tern-app`（Tauri 2 + React + Tailwind + recharts）
- [ ] Rust 侧加查询命令：`summary` / `breakdown` / `failures` / `unpriced` / `recent` /
      `sessions`，前端只拿聚合结果，不拉全量明细
- [ ] 按选定版式实现首屏件：今日卡、趋势堆叠柱、花在哪（环形/treemap）、
      缓存省了多少、会话视图、请求流、活跃度热力图、模型流向、失败面板
- [ ] 深浅双主题、空状态引导、tabular-nums
- [ ] 验收：本机攒的真实数据能对上 `tern usage` CLI 的输出

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

## 联网工具识别（横切，与阶段 6 并行）

`WebSearch` / `WebFetch` 不走网关消息通道，网关转发得再完美也覆盖不到。tern 只做
**识别和告知**，不做改写（也做不到）。详见 `docs/guides/web-tools-on-third-party-gateways-zh.md`。

- [x] `tern-gateway/src/web_tools.rs`：按上游 host 判官方 / 第三方，六个单测
- [x] `tern serve` 启动时对每个第三方供应商打 warn
- [x] `tern check` 末尾「联网工具」段落列出需注意的供应商
- [x] 三语文档（缘由 + 三种处理 + 判据与边界情况）
- [ ] （可选）桌面面板的供应商列表页加标记

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

## 托盘图标及其之后（2026-10-08 立）

按你确认要做托盘常驻来排。顺序的理据：托盘是**外壳收口**，它之后界面才能从一个
"用完即关的窗口"变成"常驻 + 按需开"的东西，所以 tab 化、面板补全都挂在它后面。

### T. 托盘常驻（已完成，待装机验收）

目标：关窗不杀进程，网关照转，托盘一个图标管开合/启停。

- [x] `tauri` 加 `tray-icon` feature（workspace 的 `Cargo.toml`）。tray-icon 本来就在
      tauri 的依赖树里，开特性不引入新 crate，离线构建认得
- [x] 新模块 `src-tauri/src/tray.rs`：`TrayIconBuilder` + 菜单
      **打开面板 / 网关启停 / 退出**
- [x] 菜单文本随状态刷：`server_start` / `server_stop` 之后立刻刷，鼠标进入图标时
      也刷（网关可能从面板之外启停，静态菜单会是个点了没反应的按钮）
- [x] 拦 `close_requested`：`api.prevent_close()` + 记住外坐标 + `hide()`；
      托盘"退出"走 `QUITTING` 标志让拦截放行，再 `app.exit(0)`
- [x] 窗口坐标只在进程内记忆，唤回时用 `available_monitors()` 验它还在不在屏上，
      不在就 `center()`——不裸还原坐标。cc-switch 在这栽过
      （`.window-state.json` 残留的 `prev_x/prev_y` 把窗口甩出屏）
- [x] 托盘建失败不阻断启动：`on_window_event` 先问 `is_installed`，没装就放行关闭。
      拦了又没有托盘 = 亲手把界面弄丢
- [x] 图标用 `app.default_window_icon()`（即打包图标），不运行时生成
- [ ] 验收：起网关 → 关窗 → 托盘显示运行中 → 重开面板不重连数据库 / 不重记账

**没做**：`tauri.conf.json` 的 `visible: false` + `setup` 里 `show()` 那套留着。
它和托盘不冲突（启动时正常显示，关窗才隐藏），而且去掉会让 Tauri 在窗口就绪前
先闪一下白底。真要动它属于"启动体验"，归到 tab 化那一轮更合适。

风险点两个的处置：`close_requested` 拦失败那条用 `is_installed` 兜住了；托盘菜单
事件不碰 `AppState`——启停走 `server::start_gateway_now()` /
`stop_gateway_now()`，和 `server_status` 同一条读法，没新建并发路径。

### T+1. 界面 tab 化（已提交 `1cebd25`）

现在 `App.tsx` 没有 tab，四块上下堆一个滚动页：Providers(261) / PanelView(311) /
Permissions(127) / Wire(178)。改成横滑 tab 容器，四块各占一页。

- [x] 新 `src/Tabs.tsx`：tab 栏 + `translateX` 视口，`role=tablist` + 方向键
- [x] 容器管 `translateX` 过渡 + tab 指示条；四块只搬内容，逻辑一行不改
- [x] **供应商是默认页**（ROADMAP 阶段 6 已说"最常用的操作"），权限/接线收进次级
- [x] 页序按频率：供应商 → 用量 → 接入 → 权限
- [x] 离屏页用 `inert` 禁焦点**但不从 DOM 摘掉**：`usePanel` 的轮询不随页卸载，
      翻回「用量」拿的是刚拉的数据；分组展开态、搜索词也不丢
- [x] 横滑要求固定高度：`.app` 改 `height:100%`，每页自己是滚动容器
- [x] 错误条提到 tab 栏上方而非某一页：它多半来自 `server_start` 这种全局动作
- [x] 「用量」页补 reading error 分支：跑着却读不到库 ≠ 网关停了

**刻意没做**：`document.hidden` 时停轮询、切 tab 时 lazy mount。前者是优化不是
需求，后者会把刚说的"翻回就有数据"的优势让掉。

> 这一条你说"跟想象差距大"的就是它，不是拖拽。

### T+2. 供应商管理补齐（ROADMAP 阶段 7 剩余）

- [x] 增删改 + 连通性测试：新模块 `src-tauri/src/providers.rs`，
      命令 `provider_save` / `provider_remove` / `provider_detail` / `provider_probe`
- [x] **key 不回传**：`provider_detail` 里没有这个字段，编辑框永远空着、
      留空 = 不修改。把凭据搬进渲染进程等于交给 webview，拿不回来
- [x] 连通性测试复用 `adapter::prepare_request`，和真实流量同一条转换路径——
      直接打 `/v1/models` 会漏掉"协议不对"的那种（问得到列表、发不了消息）
- [x] 4xx / 5xx 算"通到了"：限流、过载、模型名不对都是上游在回话，
      和"地址填错"的修法完全不同，只有真连不上才判不通
- [x] 删 default 时清空 `default_provider`（不自动猜下一个）：悬空引用会让
      `ModelRouter::new` 校验失败、网关起不来，而空 default 网关照样起
- [x] 写前备份沿用 `server::write_config`，不复制一套
- [x] 订阅登录的地址 / key 两栏在编辑时隐藏：`effective_base_url()` 托管，填了不生效
- [x] 文件夹分组（早前已完成）：`folders.rs` 注册表落 `%APPDATA%	ernolders.json`，
      平铺 / 按地址 / 按文件夹三视图，批量移动、按域名一键归组
- [ ] 智能分组：cc-switch 那边是 LLM（TypeSafe choice）+ 本地启发式两级。
      tern 先落了离线那级（按域名归组），LLM 那级要看用户想不想为一个分组功能
      多带一个上游依赖
- [ ] 订阅登录（Copilot / ChatGPT / xAI），实现 `TokenProvider`，这是 `ProviderAuth`
      里三个订阅变体现在只有占位 `AuthInfo` 的根因
- [x] 验收：能纯靠面板从零加一个中转站并切过去，不碰 `tern.json`

### T+3. 用量面板补全（你最看重的）

- Rust 侧补查询命令：`breakdown` / `sessions` / `trend`，前端只拿聚合不拉明细
- 按 `design/` 三版里选定的版式做：趋势堆叠柱、花在哪（环形/treemap）、模型流向、
  缓存省了多少、会话视图、请求流、失败面板
- 深浅双主题、空状态引导、tabular-nums
- 验收：本机真实攒的数据能对上 `tern usage` CLI 输出

### T+4. 健壮性（ROADMAP 阶段 8，按真实问题排序）

- 熔断与故障转移接上（`proxy/circuit_breaker.rs` 已搬）
- Copilot 动态端点、按模型厂商选 Responses / Chat
- Gemini OAuth refresh token 换取
- 原生 Anthropic 上游的请求头大小写保持（部分中转站按指纹校验）

## 设计记录：分组数据放哪

`folders.json` 而不是 `tern.json` 的一个字段，也不进 `usage.db`：

- `tern.json` 是网关和 agent 都要读的东西。分组是纯 UI 概念，塞进去之后
  "改分组"也要 rewrite 网关配置，而 `write_config` 每次都备份一份 `.bak`
- 面板的 `usage.db` 是**只读**打开的（`db.rs` 没写方行为），记账的库不承担配置职责
- 归属表按 provider id 存而不是按文件夹名：改名只动注册表一处。cc-switch 那边名字
  才是主键（重命名要连着改所有供应商的 `folder`），tern 的 id 导入后不再变，用 id 更稳

## 已决定

- 桌面应用托盘常驻：**做**。关窗隐藏、托盘退出（原待定问题已消）
