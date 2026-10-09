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

3. **版本号**。三个地方要保持一致：
   - `Cargo.toml` 的 `workspace.package.version`
   - `crates/tern-app/package.json`
   - `crates/tern-app/src-tauri/tauri.conf.json`

   tag 名和这三个里的版本号**不联动**：tag 给 release 页面和文件名用，
   安装包内部版本由 tauri.conf.json 决定（`tern_0.1.2_x64-setup.exe` 里的就是它）。
   `v0.1.1` 那版忘了同步，装出来的包内部版本还是 0.1.0——从 `v0.1.2` 起三处
   每次发布前一起改。`Cargo.lock` 里六个 `tern-*` 条目也要跟着改，
   CI 不传 `--locked`，不改只是留一个脏 diff，不会红。

## 已知的两个坑（都是发布时才炸的类型）

**安装包落点在 workspace 根**。`src-tauri` 属于这个 workspace，Cargo 共享一个
target 目录，于是 bundle 被提到根的 `target/release/bundle/nsis/` 下，**不是**
`src-tauri/target/`。workflow 里两个位置都扫了。照着单 crate 的惯例推会白跑一轮
13 分钟的构建——`v0.1.0` 那一轮就是这么废的（安装包本身是好的，8.9 MB）。

**删掉的 tag 不能用**。`v0.1.0` 指向一个构建失败的提交，删远端 tag 要单独授权，
所以它还在。别复用旧 tag 号，往下发新版即可。

## 试用一次不正式发版本

Actions 页面手动跑 `Release`（`workflow_dispatch`）：不建 tag，版本号沿用仓库里
最新的 tag，release 标记为 prerelease。适合在正式发版前验证一版打包是否正常。

## 产物

只有一样：`tern-<版本>-setup-x64.exe`（NSIS 安装包）。

装上之后双击即用——网关由内嵌的 `tern-agent.exe` 常驻，不需要先开终端。
`prepare-agent.mjs` 负责把 agent 复制进安装目录，少了这一步的包**本地测试看不出来**
（`cargo run` 时 agent.exe 就在旁边），装好才报"找不到常驻进程"。
