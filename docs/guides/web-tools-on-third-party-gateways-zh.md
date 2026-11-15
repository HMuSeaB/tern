# 第三方网关下 WebSearch / WebFetch 失效的原因与处理

> 本文说明：为什么把 Claude Code 指到第三方网关（含通过 tern 这类本地网关转发）后，`WebSearch` 与 `WebFetch` 两个联网工具会报错，以及怎么处理。示例中的地址、端口、模型名均已去敏，以你实际使用的值为准。
>
> tern 在 `serve` 启动和 `check` 时会自动标出属于第三方网关的供应商，本文就是那条提示的依据。

## 结论先说

`WebSearch` 和 `WebFetch` 走的**不是** `ANTHROPIC_BASE_URL` 这条消息通道，它们是 Claude Code 客户端自己发起的独立能力，部分还需要 Anthropic 侧的服务配合。所以 tern 把 `/v1/messages` 转发得再完美，这两个工具也可能完全不工作——这不是网关 bug，也不需要网关修。

两个工具的故障性质还不一样，报错文本能直接区分：

| 工具 | 典型报错 | 性质 | 谁的问题 |
|---|---|---|---|
| `WebSearch` | `API Error: 400 The input you provided is invalid` | 搜索请求被上游拒绝或端点未实现 | 网关不支持该端点 |
| `WebFetch` | `Unable to verify if domain <域名> is safe to fetch` | **前置域名安全校验失败**，还没开始抓网页 | 校验服务经网关出不去 |

`WebFetch` 那条尤其容易误判：它发生在真正抓取之前，是 Claude Code 自带的一道域名安全检查。只要那道检查不通，无论目标站点是否可达，都会报同一句话。

## 一个容易混淆的现场

同样的报错下，本机网络往往是**通**的。例如：

```powershell
curl -s -o /dev/null -w "%{http_code}" --max-time 8 https://example.com
```

返回 `200` 并不矛盾——curl 用的是系统代理或直连，而 Claude Code 的这两个工具走的是自己的出口。所以「浏览器能打开」推不出「工具能用」。

## tern 侧做了什么

只做**识别和告知**，不改写请求、也不代发这两个工具的请求（做不到）：

- `tern check` 末尾的「联网工具」段落列出属于第三方网关的供应商
- `tern serve` 启动时对每个第三方供应商打一条 warn

判据是上游地址是否为 `api.anthropic.com`（比较 host，忽略端口、路径、userinfo）。
因此两种边界情况要知道：

- **误报**：上游是兼容 Anthropic 协议、且确实支持搜索的官方代理时，也会被标成第三方。
  宁可多说一句，也好过用户以为 tern 坏了。
- **漏报的伪装**：`https://api.anthropic.com@evil.com/` 这类把真 host 藏进 userinfo 的地址，
  会被正确判为第三方（只认 `@` 之后那段）。

## 处理方式

### 方式一：在 settings.json 里禁用（彻底，推荐给不需要联网的用户）

如果日常用不到联网，可以直接从权限层面关掉，Claude Code 便不再尝试调用：

```json
{
  "permissions": {
    "deny": ["WebSearch", "WebFetch"]
  }
}
```

几点说明：

- 这是**用户全局配置**（`~/.claude/settings.json`），对所有项目生效。
- 需要与现有配置**合并**，不要覆盖整个文件。至少保留已有的 `env`、`includeCoAuthoredBy` 等键。
- deny 规则在会话启动时读取。写入后开新会话最稳；若当前会话内工具立刻被挡住，说明配置已热加载。
- 不影响模型本身的对话与代码能力，只关掉这两个联网工具。
- tern 不管理这个文件。若同时用 CC Switch 等工具管家在管它，改动的持久化以那个工具为准。

### 方式二：跳过 WebFetch 的前置校验（只想救回 WebFetch 时）

如果网关本身能出去，只是被域名安全校验挡住，可跳过该检查：

```json
{
  "skipWebFetchPreflight": true
}
```

注意这**只可能**让 `WebFetch` 恢复，对 `WebSearch` 无效——后者的问题是上游不认搜索端点，跳过校验无济于事。

tern 的供应商配置里没有"预设"概念，这个键需要你自己写进 `~/.claude/settings.json`。
带上它的供应商，典型表现就是「能抓网页、不能搜索」。

### 方式三：切回官方端点

`WebSearch` / `WebFetch` 在 Anthropic 官方端点下具备完整支持。需要恢复时，把
`ANTHROPIC_BASE_URL`、`ANTHROPIC_AUTH_TOKEN` 等参数改回官方 defaults（或把模型名前的
`供应商/` 前缀去掉、改由官方端点直接服务）。本文新增的 `permissions.deny` 与
`skipWebFetchPreflight` 也是可逆的：删掉对应键，或按需只保留其中一个。

## 排障速查

| 现象 | 判断 |
|---|---|
| `400 The input you provided is invalid` + `WebSearch` | 网关未实现搜索端点。禁用搜索，或换支持搜索的端点 |
| `Unable to verify if domain ... is safe to fetch` + `WebFetch` | 域名校验服务出不去。试 `skipWebFetchPreflight: true` |
| 两者同时报错 | 网关整体不配合这两个能力。建议按方式一直接禁用 |
| 浏览器能打开但工具仍失败 | 预期行为，两条出口不同，不构成反例 |
| 加完 deny 仍看到报错 | 配置未生效，确认写入的是 `~/.claude/settings.json` 并重开会话 |
| `tern check` 没标出我的第三方网关 | 地址是 `api.anthropic.com` 或其伪装形式；见上一节的判据说明 |

## 备注

- `WebSearch` 的可用性还取决于账号侧是否开通，即使端点官方也不代表一定返回结果。
- 本文的告警文案与 `tern check` / `tern serve` 输出的那段文字同源，改一处就够。
