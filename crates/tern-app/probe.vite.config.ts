import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

// 布局探针专用：把 Tauri 的 IPC 换成本地假数据，好在普通浏览器里
// 截图看真实组件在 760px 下的样子。不进正式构建。
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@tauri-apps/api/core": fileURLToPath(new URL("./probe/probe-mock-tauri.ts", import.meta.url)),
      "@tauri-apps/api/event": fileURLToPath(new URL("./probe/probe-mock-event.ts", import.meta.url)),
    },
  },
  server: { port: 5199, strictPort: true, host: false },
  base: "./",
});
