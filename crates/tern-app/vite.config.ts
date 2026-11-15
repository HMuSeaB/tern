import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Tauri 用固定端口，devtools / 热更新都靠它
  server: {
    port: 5183,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 5184 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  // Tauri 需要相对路径，否则打包后资源 404
  base: "./",
  build: {
    target: "es2021",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
});
