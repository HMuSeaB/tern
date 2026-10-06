# 阶段 6 · 用量面板视觉稿

> ROADMAP 阶段 6 规定：**先出静态 HTML 视觉稿定稿，再接真实数据、再上 Tauri + React**。
> 这里放 3 版可对比的静态稿。三版用的是**同一套假数据**（见下），只比视觉和编排，
> 不比数字。选定一版后，再把它翻译成 `crates/tern-app`（Tauri 2 + React + Tailwind + recharts），
> 数据换成语义化后的真实查询。

## 怎么打开

用浏览器直接开 `index.html`（本目录是个启动页，能一次跳三版）。
或者分别开 `a-terminal.html` / `b-workbench.html` / `c-canvas.html`。
纯静态、自包含、无外部请求，离线可看。

## 三版方向

| 版本 | 文件 | 气质 | 适合 |
|---|---|---|---|
| A | `a-terminal.html` | 工程终端：纯深色、密集栅格、等宽数字、单一荧光青强调、状态用色点 | 信息密度优先、一眼扫很多数 |
| B | `b-workbench.html` | 工作台：浅色为默认、可切深色、teal 强调、留白、清晰层级、表格化请求流 | 长时间盯、要清爽不累 |
| C | `c-canvas.html` | 画布：暖色深底、超大 hero、treemap 当主视觉、模型流向图、粗边框卡片叠层 | 记忆点、把"花在哪"讲成一个视觉 |

三版都实现了 ROADMAP 草稿里的首屏件，只是摆法不同：
今日卡（今日花费/请求/token/缓存命中，各带昨日对比）、花费趋势（按供应商堆叠柱，7/30/自定义）、
花在哪（供应商→模型占比，可下钻）、缓存省了多少、会话视图入口、请求流（失败标红）、
活跃度热力图、模型流向（客户端模型→实际模型，一眼看出子代理 sonnet 被送去 Opus）、
失败面板（按原因聚类，不混进模型分布）、未定价提示。

## 共享假数据（三版一致）

一个假想的 14 天真实使用切片。关键剧情：

- **模型流向**里有大量请求：客户端想用 `claude-sonnet-4-6`，供应商把它映射到 `claude-opus-5.5`
  —— 这正是 cc-switch 真实数据里暴露的问题（212 条），面板必须让用户看见"谁在花钱"。
- **失败 12 次限流**全部来自自有中转 `litellm-relay`，是被上游 `gateway_concurrency_limit` 拒的
  子代理并发。cc-switch 会把它们混进模型统计显示成"0 token"，这里按原因单独聚类呈现。
- **`step-5-preview` 未定价**：有 token、成本为 NULL，单列"未定价"卡片并给跳转入口，
  不能把整张成本图压成 0 线。

```
今日（2026-10-06）汇总        昨日（2026-10-05）汇总
  花费    $18.472              花费    $12.318
  请求    214                  请求    163
  token   8.42M                token   5.91M
  缓存命中 91.3%               缓存命中 89.7%
  缓存省下 $5.31               （昨日省 $3.66）
  未定价   step-5-preview 8 次 · 1.2M token

供应商（今日花费倒序）
  litellm-relay      $9.840  98 次   ← 自有中转，主力
  openrouter         $5.612  70 次
  official-anthropic $2.340  30 次
  copilot           $0.680  16 次

模型（今日，按计价模型，仅拿到响应的）
  claude-opus-5.5      $10.61   主要花钱对象（大部分是 sonnet-4-6 被映射来的）
  claude-sonnet-5       $4.33
  gpt-5-codex           $2.78
  claude-opus-4-1       $0.75
  step-5-preview        未定价  8 次 · 1.2M token

失败聚类（今日，按 原因/供应商/状态码）
  限流 rate_limit          litellm-relay  429   12 次  gateway_concurrency_limit
  上游过载 upstream_overloaded official    529   4 次   overloaded_error
  鉴权 auth_failed          openrouter    401   2 次   invalid api key
  连接 connection           copilot       0     3 次   connection refused

热力图：14 周 × 7 天，强度随每日花费。
请求流：最近 ~12 条，混合 success / aborted(子代理断开) / failed。
```

> 数据为纯构造，不落在任何测试里。接真实数据时以 `tern-store::query` 的
> `summary / breakdown / failures / unpriced_models / recent` 为字段口径。
