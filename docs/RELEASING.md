# 发布

发布一套 Windows 安装包只需两步，构建在 GitHub 的机器上跑：

```powershell
git tag v0.1.0
git push origin v0.1.0
```

推送 `v*` tag 会触发 [`.github/workflows/release.yml`](.github/workflows/release.yml)：
在 `windows-2022` 上构建，产物自动挂到同名 release 上。

## 为什么不在本地打包

Tauri 带着 webview，本地 release 构建实测吃 **1~2 GB 内存**。这条正好和用户的
诉求相反（"本地占用内存太大了"）——所以发布不该占他的机器。CI 上有 7 GB 内存和
16 GB 磁盘，构建全程约 8~15 分钟，期间本地可以照常用。

## 发布前检查

1. **CI 是绿的**。推送 tag 会同时触发两次构建，红的那次会挡住 release。
   先看 <https://github.com/HMuSeaB/tern/actions>。

2. **`cargo test --workspace` 在本地过**。CI 也跑，但本地先过能省一轮。

3. **版本号要不要动**。三个地方现在是同一个 `0.1.0`：
   - `Cargo.toml` 的 `workspace.package.version`
   - `crates/tern-app/package.json`
   - `crates/tern-app/src-tauri/tauri.conf.json`

   tag 名（`v0.1.0`）和这三个里的版本号**不联动**，tag 是给 release 页面看的，
   安装包内的版本号由 tauri.conf.json 决定。保持一致靠人工——要不要在 CI 里加一
   条"tag 名与 tauri.conf.json 不一致就失败"的检查，等第一次真发布时再定。

## 试用一次不正式发版本

Actions 页面手动跑 `Release`（`workflow_dispatch`）：不建 tag，版本号沿用仓库里
最新的 tag，release 标记为 prerelease。适合在正式发版前验证一版打包是否正常。

## 产物

只有一样：`tern-<版本>-setup-x64.exe`（NSIS 安装包）。

装上之后双击即用——网关由内嵌的 `tern-agent.exe` 常驻，不需要先开终端。
`prepare-agent.mjs` 负责把 agent 复制进安装目录，少了这一步的包**本地测试看不出来**
（`cargo run` 时 agent.exe 就在旁边），装好才报"找不到常驻进程"。
