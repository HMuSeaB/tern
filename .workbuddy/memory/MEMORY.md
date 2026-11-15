# tern 项目记忆（跨会话入口）

## 交接：读三份文件，别翻对话记录

1. `docs/STATUS.md` —— 现在在哪、下一步做什么（每次收尾更新）
2. `docs/DECISIONS.md` —— 决策与架构的完整来历，含不能违反的硬规矩
3. `ROADMAP.md` 对应章节 —— 按阶段编号的设计推理

**不要去翻 `~/.claude/projects/` 的 jsonl**。tern 的开发会话原本全开在
`cc-switch` 目录下，9 个会话 30 MB；需要原始报错时才查
`docs/session-archive-manifest.md` 的对照表，不要去翻目录。

三份文件的分工：ROADMAP = 为什么这么做（历史层积，不删）；
DECISIONS = 判断的完整推理和被推翻过的版本；STATUS = 现在。

## 骨架事实

- 仓库 `D:\4rchive\Code\tern`，main。
- 约 68k 行 Rust：gateway 52.7k / app 8.2k / store 2.6k / cli 2.1k / agent 1.1k / tui 1.1k。
- 六个 crate：`tern-gateway`（协议转换+路由+记账）、`tern-app`（Tauri 面板）、
  `tern-store`（SQLite）、`tern-cli`、`tern-agent`（headless 常驻）、`tern-tui`。
- 依赖只用 `~/.cargo/registry/cache` 里已有的版本，`--offline` 必须能构建。
  不用 clap、不用 chrono-parser 都是这个原因。

## 五条硬规矩（违反会出事）

1. **`~/.claude/settings.json` 由 cc-switch 整份托管**，tern 的
   `claude_settings.rs` 也写它。两边都写 = 最后一次写的赢。
2. **key 不进渲染进程**。`provider_detail` 没有 key 字段，留空 = 不修改。
3. **新查询一律加在 `tern-store`**，面板只调方法，不手抄 SQL。
4. **改转发路径或 `RequestMeta`/`UsageEvent` 填充后必须跑 e2e**
   （`--test usage_e2e` `--test gateway_e2e`），单测绿不够。
5. **本文档不写"临时状态"**。进度写 STATUS，一次性的东西写当日日志。

## 两处明文 key（该由 tern 收拢，是产品卖点）

- `~/.claude/settings.json` 的 `ANTHROPIC_AUTH_TOKEN`（StepFun，**实测可用**）
- `~/.codex/config.toml` 的 `experimental_bearer_token`（jiurelay）

tern 的设计是 key 只进 `%APPDATA%\tern\tern.json` 且 Debug 遮蔽。

## 会话归档

`docs/session-archive-manifest.md` 是对照表。
`.claude-archive/sessions/` 存 6 个主会话 + 7 个子代理会话（49 MB），
已在 `.gitignore` 排除本体，只有 manifest 进仓库。
