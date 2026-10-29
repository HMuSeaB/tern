# 当前阶段与待办

> 2026-10-29 建立。这份文件的作用是**替代翻聊天记录**：以后开新会话，先读这里，
> 再读 `docs/DECISIONS.md`（决策来历）和 `ROADMAP.md` 对应章节，
> 不必让 agent 去 `~/.claude/projects/` 里翻三十兆的 jsonl。
>
> 交接规则：每次收尾时更新本文的「现在的真实状态」与「下一步」两节，其余章节只在
> 判断改变时改。

## 一句话

内核（协议转换 + 路由 + 用量记账）已经能用且算得对，外壳也按「保切换、推前缀、
削尖用量护城河」重新定向完了。**现在唯一欠着的是"真核对一次账单"——而这个
外部条件已经解除（StepFun 可用），只是还没跑。**

## 仓库现状

- 分支 `main`，领先 `origin/main` **5 个提交未推**
- 工作区干净。`crates/tern-gateway/src/claude_settings.rs` 的 169 行已于
  2026-10-29 提交（`af7a521`），1040 个测试全绿。

### 代码规模

| crate | 行数 | 职责 |
|---|---:|---|
| `tern-gateway` | 52,755 | 协议转换（47 个文件逐字节取自 cc-switch）、路由、转发、用量出口 |
| `tern-app` | 8,203 | Tauri 2 + React 面板 |
| `tern-store` | 2,561 | SQLite（WAL）：requests 明细 + daily 预聚合 + prices |
| `tern-cli` | 2,110 | `tern serve/init/check/usage/agent`，手写参数解析 |
| `tern-agent` | 1,122 | Claude Code / Codex 的配置接线 |
| `tern-tui` | 1,058 | TUI 外壳 |

## 现在的真实状态

### 已经坐实的

- **路由**：`provider/model` 显式路由 + `defaultProvider` 兜底，`route_kind` 已暴
  露到「最近请求」表。这是验证「推前缀」有没有生效的唯一反馈口。
- **用量口径**：出口处只解析客户端协议的 usage 事件，中断在 `Drop` 里补记 `aborted`，
  失败请求带 `error_kind` 入库、不混进模型统计。定价 `rust_decimal`、成本以纳美元
  整数存。
- **对账路径**：`tern usage --from --to` 钉死账期，与 `--days` 互斥；三语对账文档
  `docs/guides/reconciling-with-upstream-billing-{zh,en,ja}.md`。
- **面板首屏**：告警区在最上，顺序「模型被换 → 失败聚类 → 未定价 → 最近失败」；
  hero 是**今日花费**（不是 token），不到一分钱留四位小数。
- **内嵌网关**：面板进程内跑 serve，关窗即停，不需要先开终端。
- **联网工具识别**：`web_tools.rs` 标出哪些第三方网关会让 WebSearch / WebFetch 失效。

### 明确还欠着的

| # | 事项 | 状态 |
|---|---|---|
| 1 | **真核对一次账单** | **外部条件已解除**（StepFun 实测可用），只是还没执行 |
| 2 | 二级视图查询命令 | `breakdown` / `sessions` / `failures` / `unpriced` / `recent` 的 Rust 侧未补 |
| 3 | 趋势图 / 占比图 / 会话视图 / 失败面板 | 等上一条 |
| 4 | 5 个提交没推 | 直接 push 即可 |

### 2026-10-29 已完成

- `claude_settings.rs` 那 169 行**已提交**（`af7a521`）。它原本 `cargo build`
  直接失败——`tempfile` 只是 dev-dependency，测试却浮在模块顶层，lib 构建解析不到。
  搬进 `mod tests` 后 1040 个测试全绿，`cargo fmt --check` 干净。
- **StepFun 上游实测可用**：`api.stepfun.com/step_plan/v1/messages` +
  `step-5-preview` 返回 HTTP 200，`usage` 字段齐全。之前「缺一家能用的 key」
  这个卡点不存在了。
- 12 份散装记忆合并成 `docs/DECISIONS.md` 一份。

### 明确不做的

- 订阅类 OAuth（Copilot / ChatGPT / xAI）：`TokenProvider` trait 在，token 交换没实现
- Magpie 那种 50+ agent 适配 —— 不追 agent 数，护城河在用量

## 下一步（按顺序）

1. **push 那 5 个提交**，让远端和本地对齐。
2. **做真核对**。这是唯一还欠着的验证，现在没有外部阻碍了。用 StepFun 跑几天
   真实流量，然后对照它家的账单走
   `docs/guides/reconciling-with-upstream-billing-zh.md` 的八步检查表。
3. **补 `breakdown` / `sessions` 查询**。这是面板二级视图的地基。

4. **找一家能用的 key 做一次真核对**。这是唯一必须外部条件的活，优先级应高于
   所有 UI 打磨——因为「算得对」是整个产品的立身之本，而它至今没被验证过。

## 这一轮暴露的流程问题

不是代码问题，是协作方式问题，记下来免得重复：

1. **tern 的会话全开在 `cc-switch` 目录下**。9 个会话里有 8 个 `cwd` 是
   `D:\4rchive\Code\cc-switch`，讨论的却是 tern。原因是 tern 是从 cc-switch 分出来的，
   用户习惯在旧目录里开窗口。后果：`--continue` 拿到的上下文里塞满了 cc-switch 的
   代码，agent 得先分清"这个仓库是哪个"。
   **对策：tern 的活一律在 `D:\4rchive\Code\tern` 下开窗。**
2. **靠 `/compact` 续命**。`129fe585` 一个会话里 compact 了 5 次，`3d03f9cf` 4 次。
   每次 compact 都会丢细节、留一份摘要，而摘要是 agent 写的、不是用户确认过的。
   **对策：一个阶段一个会话；阶段收尾时写进 `ROADMAP.md` 的 checkbox，而不是靠会话记。**
3. **交接靠贴一大段"已经落地的"进去**（`b285b931` 的第一条消息就是这种）。
   说明上下文已经接不上了。
   **对策：交接读本文 + ROADMAP，不贴摘要。**

## 与 ROADMAP 的关系

`ROADMAP.md` 是**设计推理**档案，回答"为什么这么做"，按阶段编号，历史层积，不删。
本文是**当前状态**，只回答"现在在哪、下一步做什么"。两者冲突时以本文的状态为准，
但改判断时请回写到 ROADMAP 的对应章节（那里有当时的权衡记录，删掉就丢了）。

## 历史对话在哪

tern 的开发会话原本全开在 `cc-switch` 目录下（原因见上面第 1 条）。2026-10-29 已把
其中属于 tern 的 6 个复制到 `.claude-archive/sessions/`，原文件保留。
对照表见 [session-archive-manifest.md](session-archive-manifest.md)。

**但优先读本文，不是读对话。** 需要当时的报错原文或完整推理时才去翻归档。

