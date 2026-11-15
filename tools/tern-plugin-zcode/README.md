# tern-plugin-zcode —— ZCode 赠送档 GLM 的 tern 外挂插件

让 Claude Code / Codex 通过 tern 用上 ZCode 账号的**赠送档** GLM 模型
（如 `ZCODE WEEKEND BUILD` 的 GLM-5.3-Flash，5 亿 token 那种）。

不走 API key（赠送档只认 ZCode 登录态，key 会被上游 1113 拒掉），
直接读本机 ZCode 应用的登录态，按 ZCode 客户端的形状转发。

> **自用、小范围。** 赠送档额度有限、风控会盯非官方客户端
> （405/3012 "unusual activity"）、单并发（3008）、账号可能被限制。
> 别拿主力号，别放关键业务。参考 magpie 社区「反代被封号」的帖子。

## 原理（本机还原，无猜测）

```
Claude Code / Codex
  ── provider/model（zcode/glm-5.3-flash-Trial）──▶
tern :15800（OpenAI Chat 翻译）
  ── POST http://127.0.0.1:<port>/v1/chat/completions ──▶
本插件（Python，仅标准库）
  ├─ 读 %USERPROFILE%\.zcode\v2\credentials.json
  │    值是 enc:v2:os 加密：AES-256-GCM（iv.tag.data，base64url）
  │    AES 密钥（32B）在 Windows 凭据管理器：
  │      TargetName = f"{sha256(credentials.json 反斜杠绝对路径).hex}.ZCode Credential Encryption"
  │      blob = UTF-16LE 的 base64url 密钥串
  │    （enc:v1 旧格式走 sha256 种子回退，也支持）
  ├─ 取会话 JWT（zcodejwttoken）+ 余额（billing/balance）拿赠送模型表
  ├─ OpenAI Chat → Anthropic Messages，按 ZCode 客户端伪装：
  │    三段系统块（逐字节取自 ZCode 3.17.2，sha256 校验过）、
  │    <system-reminder> 日期前缀、metadata.user_id = {"device_id": …}、
  │    x-session-id / X-Device-Mid / User-Agent ZCode/3.17.2
  ── POST https://zcode.z.ai/api/v1/zcode-plan/anthropic/v1/messages ──▶
zcode.z.ai（Anthropic SSE 流回来）
  ◀─ 转成 OpenAI Chat chunks（usage 对齐 message_start/delta，tern 记账可用）
```

伪装与 magpie 社区 `@magpie-community/opencode-zcode-auth` 对齐（0.2 起）：

- **头部**：startHeaders 全套（HTTP-Referer / ai-sdk UA / X-Release-Channel /
  X-Client-Language / X-Client-Timezone / X-ZCode-Agent / X-Platform /
  X-Os-Category / X-Os-Version / x-request-id / x-zcode-session-type /
  x-zcode-trace-id / x-zcode-query-source），messages 路径不带 X-Device-Mid
- **x-query-id**：queryOf 同款——第 n 个用户文本轮派生，同轮稳定、下轮换新
- **x-session-id**：sessionOf 同款——按设备派生，利于服务端 prompt cache 亲和
- **身体**：三段系统块逐字节（sha256 校验）+ 日期前缀 + metadata.user_id
- **ToolSearch 别名**：Claude Code 延迟加载兼容（ToolSearch→ToolSearchTool，返回改回）
- **JWT**：exp 过期检查，过期自动重读凭据一次（应用刷新后无缝续）
- **429 冷却**：赠送档对某模型 429 后该模型休息 60 秒，避免连续撞墙

社区插件优势要承认：它是有人维护的上游（prompt 改版、风控新坑都有人跟），
还带 BigModel（智谱）家族、free-first、多账号故障转移。本插件是自用快照，
ZCode 改版后要手动跟。该插件 0.1.x 只认 ZCode 3.14–3.17 的 `enc:v1` 凭据，
读不了 3.18 的 `enc:v2:os` —— 这是它「ZCode app 登录」`auth: null` 的原因，
也是本插件存在的理由。

## 用法

### 方式一：手动 sidecar（今天就能用，不需要 tern 合并任何代码）

```powershell
# 1. 启动插件（占 18085 端口；ZCode 应用保持登录状态）
python tern_plugin_zcode.py serve --port 18085

# 2. tern.json 加供应商（auth 用 none，本地 sidecar 不需要供应商 key）
{
  "id": "zcode",
  "name": "ZCode 赠送档",
  "baseUrl": "http://127.0.0.1:18085",
  "apiFormat": "openai_chat",
  "auth": { "type": "none" }
}

# 3. 客户端模型写 zcode/glm-5.3-flash-Trial
```

### 方式二：tern Step 2 supervisor（`plugin.rs` 落地后自动拉起）

把本目录放进 `%APPDATA%\tern\plugins\zcode\`，tern.json 加
`"plugins": [{"id": "zcode", "enabled": true}]`，供应商配置用
`"plugin": {"pluginId": "zcode"}`（baseUrl 占位 `127.0.0.1:0`）。
tern 分临时端口 + `TERN_PLUGIN_TOKEN` / `--port` 双通道握手，
`/health` 5 秒内 200 即就绪。

## 端点（契约 v1）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/health` | `{"ok": true, "models": ["glm-5.3-flash-Trial", …]}`（读余额动态给出） |
| GET | `/v1/models` | OpenAI 兼容列表 |
| POST | `/v1/chat/completions` | OpenAI Chat，流式 SSE / 非流式，带 usage |

所有请求校验 `X-Tern-Plugin-Token`（恒定时间比较）；手动模式
（未设 `TERN_PLUGIN_TOKEN`）放行本机调用。转发真实上游前剥掉本地认证头。

诊断走 stderr；密钥永不入日志。

## 测试

```bash
# 离线契约测试（mock，不需要 ZCode 登录态）
python -m pytest tests/test_plugin.py -q
# 或
python tests/test_plugin.py

# 真实冒烟（需要 ZCode 已登录）
python tern_plugin_zcode.py --models     # 打印赠送档模型列表
python tern_plugin_zcode.py serve --port 18085
curl http://127.0.0.1:18085/v1/models
curl http://127.0.0.1:18085/v1/chat/completions -d "{\"model\":\"glm-5.3-flash-Trial\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
```

## 已知边界

- **只支持赠送档**（gift plan）。GLM Coding Plan 的 key 路径走
  `api.z.ai/api/anthropic`，直接配 ProviderSpec 即可，不需要本插件；
  本账号无 Coding Plan 权益时 key 路径 1113。
- 单并发（套餐 3008）：插件内部串行 + 退避重试 5 次。
- 风控：换网络/高频可能 405 code 3012（账号被限制，只能 ZCode 客服解）；
  断网边缘也可能给阻断页并对本机地址保持 3 分钟。
- 工具调用：OpenAI function ↔ Anthropic tools 已映射；视觉（图片）v1 不做。
- 模型表动态来自余额接口；`-Trial` 后缀只是消费端命名（如
  `glm-5.3-flash-Trial`），线上名会被剥掉后缀还原为 `glm-5.3-flash`。
- 赠送档有期限（如 WEEKEND BUILD 到周末），过期后 `/v1/models` 为空，
  `/health` 仍 ok 但模型列表空。
