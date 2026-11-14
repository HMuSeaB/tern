# tern-plugin-workbuddy — Python 参考插件

设计见 `docs/plugins-external-process-design-zh.md` §4、§6。这是一个 mock 合约样例，不是完整的 WorkBuddy 插件：真实服务端点、登录流程和签名均未实现，刻意不猜协议。

## 当前范围

- Python 标准库实现 `GET /health`、`GET /v1/models` 和 `POST /v1/chat/completions`。
- Mock 模式返回固定 OpenAI Chat 响应（含用量），不访问真实上游。
- 非 mock 的上游转发代码仅是通用 OpenAI 兼容转发示例，不代表 WorkBuddy 协议已打通。
- 客户端写 `workbuddy/hunyuan-a52b`；Tern 路由时剥掉供应商前缀，插件收到的模型 ID 是 `hunyuan-a52b`。manifest 和 `/v1/models` 也用裸上游 ID。

## 信任与安全

插件以当前用户权限运行，不是 OS 沙箱。启用插件就意味着信任其代码能读取该用户可访问的数据、访问网络或启动子进程。不要启用来源不明的插件。

参考服务要求所有本地 HTTP 请求携带 `X-Tern-Plugin-Token`。正式运行时 token 应由 tern 启动器按进程随机生成，通过 `TERN_PLUGIN_TOKEN` 提供；不要复用 README 示例中的测试值。该 token 只防同机其它进程随意调用 sidecar，不会限制插件自身。插件真实上游请求不得转发此头。

`session.json` 不提交仓库。POSIX 系统应限制为 600；Windows 使用当前用户 ACL。该权限不能阻止同一用户权限下运行的插件读取它。

## 本地 mock

以下用 Bash 举例；token 必须至少 32 字符。另开终端启动服务：

```bash
TERN_PLUGIN_TOKEN=0123456789abcdef0123456789abcdef python3 tern-plugin-workbuddy.py serve --port 18080 --mock
```

用另一个终端检查接口：

```bash
TOKEN=0123456789abcdef0123456789abcdef
curl -H "X-Tern-Plugin-Token: $TOKEN" http://127.0.0.1:18080/health
curl -H "X-Tern-Plugin-Token: $TOKEN" http://127.0.0.1:18080/v1/models
curl -X POST http://127.0.0.1:18080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -H "X-Tern-Plugin-Token: $TOKEN" \
  -d '{"model":"hunyuan-a52b","messages":[{"role":"user","content":"hi"}],"stream":false}'
```

把 `stream` 设为 `true` 可看带用量块的 SSE。缺少或错误的本地 token 应得到 HTTP 401。

## 与 Tern 插件集成的状态

当前 Tern 还没有 sidecar 插件管理器，也不会注入 `X-Tern-Plugin-Token`。因此该受保护的参考服务**不能直接作为普通 ProviderSpec 接入 Tern**。上面的 curl 只验证插件 HTTP 契约；待插件 supervisor 实现后，再验证 Tern 注入本地 token、剥离本地认证头，以及流式记账。

## 真实 WorkBuddy 支持

当前**尚不支持**。需要先根据合法、可验证的官方协议实现登录、真实端点与签名，并评估服务条款和账户风险。不要用主力账号或关键业务测试未验证的反代。
