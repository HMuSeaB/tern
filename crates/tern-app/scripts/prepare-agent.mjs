//! 打包前把 `tern-agent` 放到 resources/ 下，并构建前端。
//!
//! # 为什么要有这一步
//!
//! 1. `tern-agent` 是另一个 crate。Tauri 的 `beforeBuildCommand` 只保证
//!    前端产物就位，Rust 侧它只编译面板自己那个 crate——agent 不会自动
//!    跟着构建，也不会被打进安装包
//! 2. tauri.conf.json 的 `bundle.resources` 声明要带 `tern-agent.exe`。
//!    Tauri 只负责"把声明了的文件打进去"，文件不存在就报错
//!
//! 少了这一步的后果很隐蔽：本地 `cargo run -p tern-app` 一切正常
//! （agent.exe 就在 target/debug 旁边），装好的包一打开就报
//! "找不到常驻进程"。所以宁可每次构建多花几秒。
//!
//! 由 `beforeBuildCommand` 调用，见 tauri.conf.json。

import { execSync } from "node:child_process";
import { copyFileSync, mkdirSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
// crates/tern-app/scripts → crates/tern-app → crates → 仓库根
const repoRoot = join(here, "..", "..", "..");

const exe = process.platform === "win32" ? "tern-agent.exe" : "tern-agent";
const built = join(repoRoot, "target", "release", exe);
const dest = join(here, "..", "src-tauri", "resources", exe);

function run(command, cwd = here) {
  console.log(`[prepare-agent] $ ${command}`);
  execSync(command, { cwd, stdio: "inherit" });
}

// 1. 前端产物。tauri.conf 里原先就有的 beforeBuildCommand，搬进来一起做
run("pnpm build");

// 2. agent。不判断新旧直接重建：cargo 的增量编译让"什么都不改"也只要一秒左右，
//    而判断新旧需要列源文件比时间戳，容易在边界情况下拿到旧的二进制——
//    那种包打出来要等到用户双击才发现
console.log("[prepare-agent] 构建 tern-agent（release）…");
run("cargo build --release -p tern-agent", repoRoot);

if (!existsSync(built)) {
  console.error(`[prepare-agent] 构建完了但 ${built} 不存在，放弃`);
  process.exit(1);
}

mkdirSync(dirname(dest), { recursive: true });
copyFileSync(built, dest);
console.log(`[prepare-agent] 已复制到 ${dest}`);
