# 上游同步记录

`crates/tern-gateway/src/proxy/` 下的转换层原样取自 cc-switch，文件名与内容保持不变，
以便用 diff / cherry-pick 跟进上游的翻译层修复。

- 来源：HMuSeaB/cc-switch（fork 自 farion1231/cc-switch）
- 提取自提交：`68ef3b9985066d16eb6fc2fd02ff168e964c445c`
- 上游最后合入：`413c09e0`（2026-08-06）
- 提取日期：2026-10-01

## 原样搬运（与上游逐字节一致）

`proxy/` 下除以下文件外的全部 `.rs`。

## 本地改写（不要直接覆盖）

| 文件 | 原因 |
|---|---|
| `proxy/mod.rs` | 去掉 forwarder / handlers / server 等胶水层模块声明 |
| `proxy/providers/mod.rs` | 去掉依赖 `Provider` 的适配器与 `ProviderType` |
| `proxy/usage/mod.rs` | 只保留 parser |
| `provider.rs` | `CodexChatReasoningConfig` 原样保留，新增中立的 `ProviderSpec` |

## 改写自上游（逻辑同源，跟进修复时需手动比对）

`crates/tern-gateway/src/adapter/` 把 cc-switch 的三个适配器改写成基于 `ProviderSpec`：

| tern | 上游来源 |
|---|---|
| `adapter/claude.rs` | `proxy/providers/claude.rs`：厂商修正、`transform_claude_request_for_api_format` |
| `adapter/codex.rs` | `proxy/providers/codex.rs`：reasoning 推断、prompt_cache_key 路由；`forwarder.rs` 的 Codex 请求分支 |
| `adapter/auth.rs` | `ClaudeAdapter::get_auth_headers`、`proxy/providers/gemini.rs` 的 OAuth 解析 |
| `adapter/endpoint.rs` | `ClaudeAdapter/CodexAdapter::build_url`、`forwarder.rs` 的 `rewrite_*_endpoint` |

上游改了上述位置时，用下面的命令查看差异，再决定是否移植：

```powershell
git -C D:\4rchive\Code\cc-switch diff 68ef3b99 HEAD -- src-tauri/src/proxy/providers/claude.rs src-tauri/src/proxy/providers/codex.rs src-tauri/src/proxy/providers/gemini.rs
```

## 跟进上游

```powershell
git -C D:\4rchive\Code\cc-switch diff 68ef3b99 HEAD --stat -- src-tauri/src/proxy
```
