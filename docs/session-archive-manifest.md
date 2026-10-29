# 会话归档清单

> 2026-10-29 建立并执行（方案 B：复制到本仓库，原文件保留不动）。
> 归档目录 `.claude-archive/sessions/` 已在 `.gitignore` 排除——单个会话动辄十几兆，
> 且含完整工具输出（可能有 key 片段），不能进 git。只有本清单进仓库。

## 已经归档的文件

源：`~/.claude/projects/D--4rchive-Code-cc-switch/`（原文件全部保留）
目标：`.claude-archive/sessions/`
校验：逐个 sha256 比对，全部一致。

| 归档名 | 大小 | 时间 | 对应内容 |
|---|---:|---|---|
| `tern-00-origin-5430f97f.jsonl` | 2.9 M | 10-01 | **分叉点**：看 magpie 对比，决定做新产品并取名 tern，建仓。ROADMAP「定位」一节 |
| `tern-01-phase3-close-e9862ba4.jsonl` | 2.1 M | 10-01 | 阶段三收尾。第一次提出"面板要好看""什么时候请求到别的模型都不知道""要独特配色与卡片堆叠" |
| `tern-02-phase6-panel-fd00e63d.jsonl` | 11 M | 10-01→10-07 | 阶段六主力：内嵌网关、B 版视觉定稿、Go vs Tauri 之争、权限放行、供应商选择 / 获取模型列表、文件夹分组 |
| `tern-03-T-stage-handoff-b285b931.jsonl` | 2.5 M | 10-07→10-08 | 托盘 T 阶段的三个 commit。消息不多但状态关键（"已经落地的"那条交接） |
| `tern-04-panel-detail-3d03f9cf.jsonl` | 21 M | 10-08→10-12 | 最长的一个：发版 0.1.3 / 0.1.4、导入供应商与模型、档位映射、窄屏滚动、常用 baseurl 排序 |
| `tern-05-shell-reorient-bef6f749.jsonl` | 5.2 M | 10-12→10-20 | Magpie 分析后的外壳重定向三步、对账路径、告警重排 |

附带子代理会话（`<归档名>-subagents/`）：

- `tern-02-phase6-panel-subagents/` — 1 个
- `tern-04-panel-detail-subagents/` — 1 个
- `tern-05-shell-reorient-subagents/` — 5 个

合计 6 个主会话 + 7 个子代理会话，约 49 MB。

## 留在原地的（没归档，原文件未动）

| 会话 | 时间 | 内容 | 为什么不归档 |
|---|---|---|---|
| `129fe585` | 09-21→09-22 | cc-switch 拉 JEV skills、文件夹分组、构建修复、msi/exe 安装报错、发版。compact 5 次 | **属于 cc-switch 自己**，不是 tern |
| `051e4b01` | 09-24 | cc-switch 最大化后 UI 消失、缩小工作区内存、精简依赖 | **属于 cc-switch 自己** |
| `8e8d0fd2` | 10-01 | 10 条消息全是对空响应的重试，无可见输出 | 无内容，留着也不碍事 |

`D--4rchive-Code-tern/927654de...jsonl`（48 KB，11 行）只有一次 tabbit skill 调用，
无内容价值，未归档。

## 怎么用

- **找某件事怎么决定的**：先看 `docs/STATUS.md` 和 `ROADMAP.md`，那里是提炼过的结论。
  只有需要原始对话（当时的报错原文、agent 的完整推理）时才翻这份归档。
- 翻的时候按文件名定位阶段，别按时间——这几个会话的日期是交错的
  （`tern-01` 和 `tern-02` 是同一天开的两个窗）。
- `.jsonl` 每行一个 JSON，`type: "user"` 的行是用户输入，`type: "assistant"` 是模型输出，
  `tool_use` / `tool_result` 包在 `message.content` 数组里。
- **别用编辑器直接打开大文件**：`tern-04` 有 21 MB，用 `jq` 或按行过滤。

## 还没处理的

- `~/.claude/history.jsonl`（562 KB）是全量输入历史，和 projects 下的会话是两套。
- `~/.claude/file-history/` 是 agent 改文件前的快照，体积未测。
- 14 个空的项目会话目录（`Box`、`tvbox`、`wechat` 等）只剩 `sessions-index.json`。
  删了会丢"这个项目以前干过什么"的一行摘要，暂时留着。
- 归档是**复制**，所以 `~/.claude` 的 243 MB 不会因为这次操作变小。
  要真腾空间，得确认归档可用之后再删原件——那一步单独做，不混在这里。
