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
| `provider.rs` | 只含 `CodexChatReasoningConfig` |

## 跟进上游

```powershell
git -C D:\4rchive\Code\cc-switch diff 68ef3b99 HEAD --stat -- src-tauri/src/proxy
```
