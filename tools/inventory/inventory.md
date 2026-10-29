# 本机 agent / skills / MCP 索引

> 由 `tools/inventory/scan.py` 生成，**只读扫描**。重跑即可更新，不要手改本文件。

## 概览

| 宿主 | skills | MCP |
|---|---:|---:|
| Claude Code | 19 | 1 |
| Codex | 28 | 2 |
| Cursor | 23 | 0 |
| WorkBuddy | 19 | 0 |
| Windsurf | 0（无目录） | 0 |
| Gemini | 1 | 0 |
| **合计** | **90** | **3** |

目录总数 90，去重后 65 个名字。

## 重复与分叉

### 源码 → 镜像

**cc-switch → claude**：13 个同名，全部一致

### 跨宿主同名

| skill | 出现在 | SKILL.md |
|---|---|---|
| `acid-typography` | Claude Code + Codex | 相同 |
| `algorithmic-art` | Claude Code + Codex | 相同 |
| `claudeforge-skill` | Claude Code + Codex | 相同 |
| `clean` | Claude Code + Codex | 相同 |
| `dashi-ppt` | Claude Code + Codex | 相同 |
| `gh-knowledge-base` | Claude Code + Codex | 相同 |
| `github-to-skills` | Claude Code + Codex | 相同 |
| `graphify` | Claude Code + Codex | 相同 |
| `kill-ai-slop` | Claude Code + Codex | 相同 |
| `memory-manager` | Claude Code + Codex | 相同 |
| `skill-evolution-manager` | Claude Code + Codex | 相同 |
| `skill-manager` | Claude Code + Codex | 相同 |
| `tabbit` | Claude Code + Cursor + WorkBuddy + Gemini | 相同 |
| `yt-dlp` | Claude Code + Codex | 相同 |

## MCP

### Claude Code

| 名字 | 传输 | 配置位置 |
|---|---|---|
| `jshook` | stdio | `C:\Users\36230\.claude\mcp.json` |

### Codex

| 名字 | 传输 | 配置位置 |
|---|---|---|
| `node_repl` | ? | `C:\Users\36230\.codex\config.toml` |
| `jshook` | ? | `C:\Users\36230\.codex\config.toml` |


## 全部 skills

### Claude Code（19 个）

| 目录名 | 标题 | 说明 | 备注 |
|---|---|---|---|
| `acid-typography` | acid-typography | Acid Graphics 风格的生成式排版设计指南。用于 Processing/p5.js 中创建高密度、空间填充的字体视觉效果。 触发场景：(1) 用户要求创建生成式排版/字体艺术 (2) 用户提到 Acid Gra… |  |
| `algorithmic-art` | algorithmic-art | Creating algorithmic art using p5.js with seeded randomness and interactive parameter exploration. Use this wh… |  |
| `claudeforge-skill` | claude-md-enhancer | Analyzes, generates, and enhances CLAUDE.md files for any project type using best practices, modular architect… |  |
| `clean` | clean | 清理 Claude Code 缓存和临时文件，释放磁盘空间 |  |
| `coldbrew-builder` | coldbrew-builder |  | 没有 SKILL.md |
| `coldbrew-core` | coldbrew-core |  | 没有 SKILL.md |
| `coldbrew-creative` | coldbrew-creative |  | 没有 SKILL.md |
| `coldbrew-ishii` | coldbrew-ishii |  | 没有 SKILL.md |
| `coldbrew-research` | coldbrew-research |  | 没有 SKILL.md |
| `dashi-ppt` | dashi-ppt | 制作 PPT、演示文稿、幻灯片、汇报材料时使用。Dashi PPT 基于预置视觉主题组合页面,生成可离线打开、可在浏览器编辑的 HTML 演示,支持导出 PPTX / PDF 文件。 |  |
| `gh-knowledge-base` | gh-knowledge-base |  | frontmatter 缺 description |
| `github-to-skills` | github-to-skills | Automated factory for converting GitHub repositories into specialized AI skills. Use this skill when the user… |  |
| `graphify` | graphify | "Use for any question about a codebase, its architecture, file relationships, or project content — especially… |  |
| `kill-ai-slop` | kill-ai-slop | Find and remove AI slop — the generic, machine-default visual and copy tics of vibe-coded products — from a we… |  |
| `memory-manager` | memory-manager | 管理 Claude Code 的持久化语义记忆，支持保存、加载、搜索记忆 |  |
| `skill-evolution-manager` | Skill Evolution Manager | 专门用于在对话结束时，根据用户反馈和对话内容总结优化并迭代现有 Skills 的核心工具。它通过吸取对话中的"精华"（如成功的解决方案、失败的教训、特定的代码规范）来持续演进 Skills 库。 |  |
| `skill-manager` | skill-manager | Lifecycle manager for GitHub-based skills. Use this to batch scan your skills directory, check for updates on… |  |
| `tabbit` | tabbit | Use for browser navigation, inspection, interaction, and visual verification through Tabbit's installed CLI. |  |
| `yt-dlp` | yt-dlp | 使用 yt-dlp 下载视频。当用户提供视频链接并要求下载时使用此 skill。 |  |

### Codex（28 个）

| 目录名 | 标题 | 说明 | 备注 |
|---|---|---|---|
| `.system` | .system |  | 没有 SKILL.md |
| `acid-typography` | acid-typography | Acid Graphics 风格的生成式排版设计指南。用于 Processing/p5.js 中创建高密度、空间填充的字体视觉效果。 触发场景：(1) 用户要求创建生成式排版/字体艺术 (2) 用户提到 Acid Gra… |  |
| `algorithmic-art` | algorithmic-art | Creating algorithmic art using p5.js with seeded randomness and interactive parameter exploration. Use this wh… |  |
| `codex-primary-runtime` | codex-primary-runtime |  | 没有 SKILL.md |
| `dashi-ppt` | dashi-ppt | 制作 PPT、演示文稿、幻灯片、汇报材料时使用。Dashi PPT 基于预置视觉主题组合页面,生成可离线打开、可在浏览器编辑的 HTML 演示,支持导出 PPTX / PDF 文件。 |  |
| `kill-ai-slop` | kill-ai-slop | Find and remove AI slop — the generic, machine-default visual and copy tics of vibe-coded products — from a we… | → 链接 `C:\Users\36230\.cc-switch\skills\kill-ai-slop` |
| `mindroom-maintainer` | mindroom-maintainer | Maintain, review, build, diff, release, and publish the MindRoom repository safely. Use when the user wants Co… |  |
| `private-release-manager` | private-release-manager | "Review, diff, package, tag, and publish private app releases safely. Use when the user wants Codex to handle… |  |
| `project-publisher` | project-publisher | "Maintain, review, build, package, tag, release, and publish software projects safely. Use when the user wants… |  |
| `skill-evolution-manager` | Skill Evolution Manager | 专门用于在对话结束时，根据用户反馈和对话内容总结优化并迭代现有 Skills 的核心工具。它通过吸取对话中的"精华"（如成功的解决方案、失败的教训、特定的代码规范）来持续演进 Skills 库。 |  |
| `slides` | slides | Create and edit presentation slide decks (`.pptx`) with PptxGenJS, bundled layout helpers, and render/validati… | → 链接 `C:\Users\36230\.cc-switch\skills\slides` |
| `acid-typography` | acid-typography | Acid Graphics 风格的生成式排版设计指南。用于 Processing/p5.js 中创建高密度、空间填充的字体视觉效果。 触发场景：(1) 用户要求创建生成式排版/字体艺术 (2) 用户提到 Acid Gra… |  |
| `algorithmic-art` | algorithmic-art | Creating algorithmic art using p5.js with seeded randomness and interactive parameter exploration. Use this wh… |  |
| `claudeforge-skill` | claude-md-enhancer | Analyzes, generates, and enhances CLAUDE.md files for any project type using best practices, modular architect… |  |
| `clean` | clean | 清理 Claude Code 缓存和临时文件，释放磁盘空间 |  |
| `dashi-ppt` | dashi-ppt | 制作 PPT、演示文稿、幻灯片、汇报材料时使用。Dashi PPT 基于预置视觉主题组合页面,生成可离线打开、可在浏览器编辑的 HTML 演示,支持导出 PPTX / PDF 文件。 |  |
| `gh-knowledge-base` | gh-knowledge-base |  | frontmatter 缺 description |
| `github-to-skills` | github-to-skills | Automated factory for converting GitHub repositories into specialized AI skills. Use this skill when the user… |  |
| `graphify` | graphify | "Use for any question about a codebase, its architecture, file relationships, or project content — especially… |  |
| `kill-ai-slop` | kill-ai-slop | Find and remove AI slop — the generic, machine-default visual and copy tics of vibe-coded products — from a we… |  |
| `memory-manager` | memory-manager | 管理 Claude Code 的持久化语义记忆，支持保存、加载、搜索记忆 |  |
| `mindroom-maintainer` | mindroom-maintainer | Maintain, review, build, diff, release, and publish the MindRoom repository safely. Use when the user wants Co… |  |
| `private-release-manager` | private-release-manager | "Review, diff, package, tag, and publish private app releases safely. Use when the user wants Codex to handle… |  |
| `project-publisher` | project-publisher | "Maintain, review, build, package, tag, release, and publish software projects safely. Use when the user wants… |  |
| `skill-evolution-manager` | Skill Evolution Manager | 专门用于在对话结束时，根据用户反馈和对话内容总结优化并迭代现有 Skills 的核心工具。它通过吸取对话中的"精华"（如成功的解决方案、失败的教训、特定的代码规范）来持续演进 Skills 库。 |  |
| `skill-manager` | skill-manager | Lifecycle manager for GitHub-based skills. Use this to batch scan your skills directory, check for updates on… |  |
| `slides` | slides | Create and edit presentation slide decks (`.pptx`) with PptxGenJS, bundled layout helpers, and render/validati… |  |
| `yt-dlp` | yt-dlp | 使用 yt-dlp 下载视频。当用户提供视频链接并要求下载时使用此 skill。 |  |

### Cursor（23 个）

| 目录名 | 标题 | 说明 | 备注 |
|---|---|---|---|
| `tabbit` | tabbit | Use for browser navigation, inspection, interaction, and visual verification through Tabbit's installed CLI. |  |
| `autopilot` | autopilot | Keep a PR merge-ready by triaging comments, resolving clear conflicts, and fixing CI in a loop. |  |
| `canvas` | canvas | '' |  |
| `create-hook` | create-hook | Create Cursor hooks. Use when you want to create a hook, write hooks.json, add hook scripts, or automate behav… |  |
| `create-rule` | create-rule | Create Cursor rules for persistent AI guidance. Use when you want to create a rule, add coding standards, set… |  |
| `create-skill` | create-skill | Create Cursor Agent Skills. Use when authoring a new skill or asking about SKILL.md structure. |  |
| `create-subagent` | create-subagent | Create custom subagents for specialized AI tasks. Use when you want to create a new type of subagent, set up t… |  |
| `goal` | goal | Set a goal that Cursor will pursue to completion. |  |
| `loop` | loop | Run a prompt or skill in this session on a recurring or variable interval (e.g. /loop 5m /foo). |  |
| `migrate-to-skills` | migrate-to-skills | Convert 'Applied intelligently' Cursor rules (.cursor/rules/*.mdc) and slash commands (.cursor/commands/*.md)… |  |
| `new-repo` | new-repo | Create a Cursor-hosted repo for the current project and push it. Use when a project has no git remote yet and… |  |
| `origin` | origin | Install, sign in, update, or repair the origin CLI for repos hosted on Cursor (origin.cursor.com). Use when th… |  |
| `rename-chat` | rename-chat | Rename the current chat to match its focus. Use only when the user invokes /rename-chat. Optional text after t… |  |
| `review` | review | Review code changes with the Bugbot or Security Review subagent. |  |
| `review-bugbot` | review-bugbot | Review code changes with Bugbot subagent. |  |
| `review-security` | review-security | Review code changes with Security Review subagent. |  |
| `sdk` | sdk | Guide users building apps, scripts, CI pipelines, or automations on top of the Cursor SDK - TypeScript (`@curs… |  |
| `share` | share | Save, back up, or share the current project on Cursor — creates a repo (a saved, versioned copy that isn't pub… |  |
| `shell` | shell | Runs the rest of a /shell request as a literal shell command. Use only when the user explicitly invokes /shell… |  |
| `split-to-prs` | split-to-prs | Split current work into small reviewable PRs. Use when the user asks to split a chat, set of changes, branch,… |  |
| `statusline` | statusline | Configure a custom status line in the CLI. Use when the user mentions status line, statusline, statusLine, CLI… |  |
| `update-cli-config` | update-cli-config | View and modify Cursor CLI configuration settings in ~/.cursor/cli-config.json. Use when the user wants to cha… |  |
| `update-cursor-settings` | update-cursor-settings | Modify Cursor/VSCode user settings in settings.json. Use when you want to change editor settings, preferences,… |  |

### WorkBuddy（19 个）

| 目录名 | 标题 | 说明 | 备注 |
|---|---|---|---|
| `agently-mail` | agently-mail | 通过 agently-cli 命令行工具操作邮件：发送、回复、转发、搜索、读取、下载附件、管理收件箱。当用户需要进行任何邮件相关操作时使用此 skill。 |  |
| `douzhanzhe-fix-verify` | douzhanzhe-fix-verify | Verify whether a fix to the Douzhanzhe Console (斗战者) hardware-control app actually took effect on a running in… |  |
| `editor-sdk-bulk-sheet` | editor-sdk-bulk-sheet | 通过本地 editor_sdk MCP 对 Excel/WPS 表格做「批量重排 + 回填 + 合并单元格」这类数据工程型改动。适用于把一张汇总表改版式、跨表按姓名回填字段、按项目分块合并单元格、保持学号/身份证等长数字… |  |
| `electron-asar-inspect` | electron-asar-inspect | Inspect an Electron app's internal mechanisms, feature flags, and hidden endpoints by scanning app.asar, witho… |  |
| `foreign-app-sqlite-read` | foreign-app-sqlite-read | Read another desktop app's SQLite database (its sessions, messages, history) without writing to it, then show… |  |
| `github-wheel-research` | github-wheel-research | Research whether an off-the-shelf open-source tool already exists on GitHub before building one. Use when the… |  |
| `go-crossplatform-ci` | go-crossplatform-ci | Set up and debug cross-platform CI/release for a Go project on GitHub, and fix the class of bugs where path ha… |  |
| `go-embedded-webui-desktop` | go-embedded-webui-desktop | Turn a Go CLI into a desktop app by embedding a single-file Web UI with //go:embed and serving it on loopback,… |  |
| `inno-setup-unpack` | inno-setup-unpack | Extract and inspect Inno Setup installers, including newer versions (6.1-6.7) that innoextract 1.9 and innounp… |  |
| `markitdown-skill` | markitdown-skill | "Convert documents to Markdown using Microsoft's MarkItDown CLI (`markitdown`). Supports PDF, Word, PowerPoint… |  |
| `tabbit` | tabbit | Use for browser navigation, inspection, interaction, and visual verification through Tabbit's installed CLI. |  |
| `tauri-dep-generation-upgrade` | tauri-dep-generation-upgrade | 升级 Tauri 桌面应用的 Rust / NPM 依赖并安全验证代际一致性。处理 Dependabot 或 advisory 告警、升级 tauri 或 @tauri-apps/*、Cargo.lock 变动后必须重建… |  |
| `tool-env-external-exe` | tool-env-external-exe | Diagnose and fix failures when running external executables (dotnet, npm, git, ISCC, msbuild) from Bash/PowerS… |  |
| `webview-bundle-crash-triage` | webview-bundle-crash-triage | Diagnose a packaged WebView (Tauri/Electron) app that blanked out with a stack trace pointing at minified bund… |  |
| `win-desktop-install-locate` | win-desktop-install-locate | Locate an installed desktop application on Windows (and macOS/Linux) from Go, reliably and without third-party… |  |
| `win-explorer-lag-triage` | win-explorer-lag-triage | Diagnose why Windows File Explorer is slow, laggy on scroll, or unresponsive to mouse movement. Use when Explo… |  |
| `workbuddy-credential-decrypt` | workbuddy-credential-decrypt | Decrypt WorkBuddy / CodeBuddy desktop credentials when auth.accessToken is an encrypted envelope ($wbEncrypted… |  |
| `workbuddy-session-db` | workbuddy-session-db | 读写 WorkBuddy 桌面端的会话索引数据库（workbuddy.db / sessions 表），包括跨版本搬运历史、修正"列表空但文件在"、统计会话。用客户端自带的 Electron 以 zero-depende… |  |
| `wps-debloat` | wps-debloat | De-bloat and de-ad a local WPS Office install (especially third-party repacks like 雨糖科技特别版) safely, and diagno… |  |

### Gemini（1 个）

| 目录名 | 标题 | 说明 | 备注 |
|---|---|---|---|
| `tabbit` | tabbit | Use for browser navigation, inspection, interaction, and visual verification through Tabbit's installed CLI. |  |

