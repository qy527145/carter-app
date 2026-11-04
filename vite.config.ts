import { fileURLToPath, URL } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// 移动端真机调试时由 `tauri android/ios dev` 注入，桌面端为空。
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  // tailwindcss() 负责 v4 的新引擎（不再需要 tailwind.config.js / postcss.config）。
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },

  // 不清屏，否则 Rust 编译错误会被 Vite 的输出冲掉。
  clearScreen: false,
  // 让 TAURI_ENV_ 开头的变量（平台、调试标志等）也能在前端通过 import.meta.env 读到。
  envPrefix: ["VITE_", "TAURI_ENV_"],

  server: {
    // 与 tauri.conf.json 的 devUrl 对应，端口被占时直接失败而不是悄悄换端口。
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: {
      // Rust 改动由 tauri dev 自己处理，Vite 不必监听。
      //
      // `target` 是工作区级的 cargo 产物目录，不在 `src-tauri` 下面。漏掉它，
      // Vite 启动时要多监听几万个条目，首屏被拖慢一个数量级。`.sandbox` 是
      // `dev:sandbox` 的数据目录，会话写盘不该触发前端重载。
      ignored: ["**/src-tauri/**", "**/target/**", "**/crates/**", "**/.sandbox/**"],
    },
  },

  build: {
    // 各平台 WebView 的最低能力：Windows 用 Chromium 内核，macOS/Linux 用 WebKit。
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari15",
    // 只在 debug 构建（`tauri build --debug`）里保留 sourcemap，并跳过压缩。
    minify: process.env.TAURI_ENV_DEBUG ? false : "oxc",
    sourcemap: Boolean(process.env.TAURI_ENV_DEBUG),
  },
});
