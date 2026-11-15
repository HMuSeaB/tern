# 报 bug / 提需求

tern 的核心价值是**能连上尽量多的中转站**，而那件事只有真在用的人才能发现。
所以这里最欢迎的就是"我家那个站连不上"这类报告。

## 报 bug 请带上这几样

**1. tern 的日志。** 没有日志的报告基本无法动手——"连不上"可能是上游 404、
key 无效、协议不匹配、DNS 挂了，各自的修法完全不同。

```powershell
$env:RUST_LOG="debug"
.\tern.exe serve
```

把复现那次请求附近的几行贴上来。**key 会被自动掩码**（`sk-1...cdef`），
但贴之前还是建议自己再扫一眼。

**2. 供应商的地址和协议。** 地址只需要 origin + path，不要带 key。

**3. tern 怎么说这件事的。** 面板里「编辑 → 测连通性」那条结果，或
`tern check` 的输出，比一句"连不上"信息量大得多——它会带上游的原话。

## 提新供应商支持

先跑 `tern check` 和连通性测试。如果结果是 4xx/5xx，说明地址和 key 都对上了，
是协议层面的不匹配——那种恰恰是最该报的。

## 暂不支持

- **macOS / Linux**：目前只发 Windows 包。协议层本身不挑平台
  （`tern-gateway` 在 Linux 上测试全过），只是没配打包。
- **联网工具**：WebSearch / WebFetch 不走网关，第三方网关卡它们不是 tern 能改的。
  详见 [`docs/guides/web-tools-on-third-party-gateways-zh.md`](docs/guides/web-tools-on-third-party-gateways-zh.md)
