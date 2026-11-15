# 工具盘点与整合计划

> 2026-10-29 立。回答一个问题：**本机到底装了多少 agent / skills / MCP，
> 哪些是重复的、哪些能收。** 附带一个可重复跑的扫描器做法。
>
> 这不是一次性清单，是**扫描 + 判据 + 处置**的流程。以后每加一个工具，
> 跑一次 `tools/inventory/` 下的脚本即可更新，不再手写。

## 一、实测现状（2026-10-29 扫描）

### 实测现状

跑 `tools/inventory/scan.py` 得到（数字随时会变，以重跑结果为准）：

| 宿主 | skills | MCP |
|---|---:|---:|
| Claude Code | 19 | 1 |
| Codex | 28 | 2 |
| Cursor | 23 | 0 |
| WorkBuddy | 19 | 0 |
| Gemini | 1 | 0 |
| **合计** | **90 个目录** | **3** |

**90 个目录去重后只有 65 个名字**，即约 25 个是副本或链接。

### 真正的结构：三份 skill 仓库 + 一堆链接

这是扫描才挖出来的，光看 `ls ~/.claude/skills` 看不出来：

- `~/.claude/skills/` —— 19 个，**实际维护的那份**
- `~/.cc-switch/skills/` —— 17 个，cc-switch 自己的技能库
- `~/.codex/skills/` —— 11 个，其中 **`kill-ai-slop` 和 `slides` 是符号链接**，
  指向 `~/.cc-switch/skills/`，不是真副本

所以 Codex 那 28 个里有 17 个是 `.cc-switch` 的镜像。

**13 个名字在 claude 与 cc-switch 两处都有，其中 12 个内容一致，只有
`kill-ai-slop` 分叉**：claude 那份 6287 字节、带"去除AI味"等中文触发词；
cc-switch 那份 5795 字节、没有中文触发词、`"not just X — it's Y"` 讲得更细。
**是 claude 那份被更新过，cc-switch 的原版没跟上。**

两边各有的独有项（不冲突，说明是不同来源装的）：

- 只在 `.cc-switch`：`mindroom-maintainer`、`private-release-manager`、
  `project-publisher`、`slides`
- 只在 `.claude`：`coldbrew-*` 五个、`tabbit`

### MCP：只有 2 个真服务

| 位置 | 内容 |
|---|---|
| `~/.claude/mcp.json` | `jshook`（`npx @jshookmcp/jshook@0.3.4`） |
| `~/.codex/config.toml` | `jshook` + `node_repl`（Codex 自带运行时） |

**MCP 完全不是问题**。数量少，且 jshook 两边配置一致。

### 关键结论：宿主不共享 skills 目录

实测各宿主只读自己的目录，没有共享机制。这意味着：

- **不能**把这些目录合并成一份（宿主找不到）
- 能做的只有两件事：**建立统一索引**（人看得见）、**消除分叉**（防改漏）

## 二、判据：什么该收，什么不该动

判断顺序，先风险后数量：

1. **两个宿主都读、内容不同的** → 立刻处理。这不是整洁问题，是"改了 A 忘了 B"的 bug 源头。
2. **明文凭据** → 收。`~/.claude/settings.json` 的 StepFun token、
   `~/.codex/config.toml` 的 jiurelay token，都是可直接使用的明文。
3. **宿主已经不用了的** → 收。Windsurf / Gemini 只有空目录。
4. **装了但和现有项目无关的** → 收。如 `gopls-lsp`（没有 Go 项目）。
5. **纯整洁**（空目录、历史残留） → 最后做，价值最低。

**不该动的**：正在用的（`rust-analyzer-lsp`、`github`、`tabbit`、`yt-dlp`、
以及 workbuddy 下那些 Windows 诊断类 skill）。

## 三、整合方案

### 3.1 统一索引（先做，纯只读）

写一个扫描器 `tools/inventory/scan.py`，产出一份 `inventory.md`：

- 遍历五个 skills 目录 + 三个 mcp 配置
- 从 `SKILL.md` 的 frontmatter 提 `name` / `description`
- 用内容 sha256 判定跨宿主是否相同
- 输出三张表：**全部清单**（按宿主） / **重复与分叉** / **MCP**

为什么用 Python 而不是 Rust：这是运维脚本不是产品功能，不该进 tern 的代码质量约束
（`--offline` 构建、依赖白名单）。放 `tools/` 下，跟 `tools/*.html` 同一层级。

### 3.2 消除分叉（已完成 2026-10-29）

**只有一例：`kill-ai-slop`。**

查明是 `.claude` 那份新、`.cc-switch` 那份旧：

- claude 版 6287 字节，带「去除AI味」等中文触发词，多一个
  `scripts/rules.zh.mjs`（中文 AI 文案规则），多了「以减法为先但不要剥夺美感」
  「避免过度修正到乏味」等三条原则，以及 `--rules` 用法
- cc-switch 版 5795 字节，是旧的

**已处理**：备份旧的到 `SystemTools\_Archive\skills-before-sync-2026-10-29\`，
然后以 claude 版覆盖 cc-switch 版。核对结果：**13 个同名 skill 整目录指纹全部一致。**

**不做符号链接**。虽然 Codex 已经在用链接（`kill-ai-slop` / `slides`），但那是
既有结构，不主动推广——Windows 下建 symlink 需要开发者模式或管理员，
且链断了报错很隐蔽。内容同步到一致比链接可靠。

### 3.2b 扫描器已能认出镜像对（已完成）

`scan.py` 里的 `MIRROR_SETS` 写死了「源 → 镜像」关系，用**整目录指纹**
（所有文件路径+内容的哈希）而不是只看 `SKILL.md`——因为 `kill-ai-slop` 那次
分叉里 `rules.zh.mjs` 是新增文件，只比 `SKILL.md` 会漏判。

现在跑一次就能知道有没有新分叉，不用手工比对。

### 3.3 收掉明确无用的

| 对象 | 理由 | 状态 |
|---|---|---|
| `gopls-lsp` 插件 | 没有 Go 项目 | **已卸载**（注册表从 5 个降到 3 个） |
| `obsidian` 插件 | 不在用 Obsidian | **已卸载** |
| `~/.windsurf`、`~/.gemini` | 只有空目录/单个残留 | 待确认 |
| `clean` skill | 与 `kill-ai-slop` 定位交叉，名字无辨识度 | 待看内容再定 |

卸载后留下的缓存残骸（gopls 14 KB + obsidian 63 KB）不值得单独清。

### 3.4 一个反直觉的结论

**问题不是"装太多"，是"同一套东西装了三份"。** 90 个目录里真正的独立 skill
只有 65 个，而其中 `.cc-switch` 那 17 个基本是 `.claude` 的旧镜像。
要瘦身，清镜像比卸载插件有效得多。

### 3.4 凭据收拢（独立议题，不在本次范围）

两处明文 key 的迁出涉及"tern 和 cc-switch 抢 `settings.json`"这个结构性问题，
**不属于工具盘点**。记在 `docs/DECISIONS.md` 第 7.1 节，要动时单独排期。

## 四、不做的事

- **不合并 skills 目录**：宿主不认。
- **不做自动清理**：删东西一律列清单等确认。
- **不追"装得少"**：72 个 skill 里真重复的只有 6 个，问题不在数量。

## 五、执行顺序

1. [x] 写 `tools/inventory/scan.py` + 产出 `tools/inventory/inventory.md`（只读，
       验证过：识别 90 目录 / 65 名字 / 13 处跨宿主 / 链接标注 / 分叉检测）
2. [x] 复核 `kill-ai-slop` 的 diff，以 `.claude` 版覆盖 `.cc-switch`（旧版已备份），
       重跑确认 13 个整目录指纹一致
3. [x] 卸载 `gopls-lsp`、`obsidian` 两个插件
4. [x] 把扫描器路径登记到 `C:\Users\36230\SystemTools\索引.md` 第二节
5. [ ] 扫描器加镜像对识别（已完成，见 3.2b）
6. [ ] 剩下的 `.windsurf` / `.gemini` 空目录、`clean` skill——待定

## 六、以后的计划：让 tern 接管 skill 管理

这是"集成到项目里"的真正含义，属于**产品功能**，不该和这轮清理混做。

现状的痛点是：cc-switch 同步一份镜像到 `.claude/skills`，codex 又链回 cc-switch，
**没有任何标记告诉你哪个是源头**。改了镜像真身不变，改了真身要等同步，
同步卡住就分叉——用户只能靠比时间戳猜。

要做的是 tern 的 skill 库：

- **一个源**：`%APPDATA%\tern\skills\`，这是唯一的真身
- **多宿主扇出**：Claude Code / Codex / Cursor 各自要什么格式写什么，带来源标记
- **改动一次全生效**：不再有"改哪份"的问题
- **不抢 cc-switch 的文件**：和 `settings.json` 那个问题一样，得先定归属

前置条件：先确认 cc-switch 还在不在用。如果它只负责同步 skills 而不再切供应商，
tern 可以直接接管，比维护两套简单。

## 七、扫描器怎么改

- 加新宿主：往 `HOSTS` 里加一条 `{skills: [...], mcp: [...]}`，路径不存在会自动跳过。
- 加新的 MCP 格式（比如某宿主用 TOML 但表名不同）：在 `scan_mcp` 里加分支。
- **不要让它具备写能力**。这个脚本的价值在于随便跑、跑错也不会有后果。
