# tern-plugin-workbuddy — Python 参考插件

设计见 `docs/plugins-external-process-design-zh.md` §4、§6。
这是 Step 2 的参考骨架，不是能直接连 WorkBuddy 真实服务的完整插件。
真实端点和签名逻辑是缺的那一块，故意留空，不猜协议。

## 语言路线（已定，2026-11-14）

- 先 Python：改得快，签到 / 签名逻辑调起来不用重新编译。
- 逐步到 Rust：更好用，分发是单文件，不用装 Python 运行时，
  离线构建和 `cargo --offline` 的约束也和 tern 主仓库一致。
- 迁移时只换语言，不换契约：三个端点、JSON 形状、
  `TERN_PLUGIN_PORT` / `--port` 握手、`/health` 5 秒就绪，
  全部原样搬过去。Python 版的测试用例就是 Rust 版的验收用例。

## 文件

- `tern-plugin-workbuddy.py` — stdlib only，无 pip 依赖。
  `/health`、`/v1/models`、`/v1/chat/completions`（流式 / 非流式）、
  `/v1/messages`（mock 兜底）。
- `plugin.toml` — manifest，见设计稿 §4.1。默认 `--mock`。
- `session.json` — 不提交。真实模式才需要，格式 `{"token": "..."}`，
  权限 600，token 只记后 4 位。

## 本地跑通（mock 模式，不用真实账号）

```bash
python3 tern-plugin-workbuddy.py serve --port 18080 --mock
curl http://127.0.0.1:18080/health
curl http://127.0.0.1:18080/v1/models
curl -X POST http://127.0.0.1:18080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"workbuddy/hunyuan-a52b","messages":[{"role":"user","content":"hi"}],"stream":false}'
# 流式：把 stream 改成 true，看 SSE 里最后一块的 usage
```

## 接 tern（零代码验证，不用等插件系统）

mock 插件就是一个本地 HTTP 服务，今天就能当 ProviderSpec 接进去：

```json
{
  "id": "workbuddy",
  "name": "WorkBuddy",
  "baseUrl": "http://127.0.0.1:18080/v1",
  "apiFormat": "openai_chat",
  "auth": { "type": "api_key", "key": "plugin" }
}
```

模型写 `workbuddy/hunyuan-a52b`，跑一次真实会话，看用量面板里
token 和花费能不能对上。能对上，说明契约没问题，再补真实转发。

## 真实模式（待补）

1. 去掉 `plugin.toml` 里的 `--mock`，加 `--upstream <真实 base URL>`，
   或设环境变量 `TERN_PLUGIN_UPSTREAM_URL`。
2. 把登录后的 session 写进 `session.json`，不要写进 `tern.json`。
3. 在 `forward_chat()` 里补真实端点和签名逻辑，401 时最多刷新一次，
   刷新失败就返回 401，不要静默重试。
4. 提醒：反代是灰色地带，有封号风险，别拿主力号，别放关键业务。

## 迁到 Rust 时的对照表

| Python | Rust |
|---|---|
| `http.server.ThreadingHTTPServer` | `axum`（已在 tern workspace 依赖里）+ `tokio` |
| `urllib.request.urlopen` | `reqwest::Client`（`gateway/mod.rs` 的 `build_client` 同款） |
| `json` | `serde_json` |
| `argparse` / `os.environ` | 手写参数解析（和 `tern-cli` 一样，不引 clap） |
| 进程管理、超时、日志遮蔽 | 复用 tern 的 `PluginSupervisor` 计划（设计稿 §5.2） |

验收和 Python 版一样：`cargo test`、`/health` 5 秒就绪、
插件被杀后网关还在、`usage_e2e` 里 `session_id` 不为 None。
