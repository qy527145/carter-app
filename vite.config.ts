import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(() => ({
  // tailwindcss() 负责 v4 的新引擎（不再需要 tailwind.config.js / postcss.config）。
  plugins: [react(), tailwindcss()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      //
      // `target` 是工作区级的 cargo 产物目录，不在 `src-tauri` 下面。漏掉它，
      // Vite 启动时要多监听几万个条目，首屏被拖慢一个数量级。
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
}));
