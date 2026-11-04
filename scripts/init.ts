import { existsSync } from "node:fs";
import { copyFile } from "node:fs/promises";
import { join } from "node:path";

/**
 * 一键初始化：检查工具链 -> 安装依赖 -> 准备 .env -> 提示下一步。
 * 幂等，可重复执行。
 */

const root = join(import.meta.dir, "..");

function check(cmd: string, args: string[], hint: string): string | null {
  try {
    const result = Bun.spawnSync({ cmd: [cmd, ...args], stdout: "pipe", stderr: "pipe" });
    if (result.exitCode === 0) {
      return result.stdout.toString().trim().split("\n")[0] ?? "";
    }
  } catch {
    // 命令不存在时 spawnSync 直接抛错，与非零退出同样处理。
  }
  console.warn(`⚠ ${cmd} 不可用：${hint}`);
  return null;
}

console.log("检查工具链…");
const cargo = check("cargo", ["--version"], "安装 Rust: https://rustup.rs");
console.log(`  bun   ${Bun.version}`);
console.log(`  cargo ${cargo ?? "(缺失)"}`);
if (process.platform === "linux") {
  console.log("  Linux 还需要 WebKitGTK 等系统库：https://v2.tauri.app/start/prerequisites/#linux");
}

console.log("\n安装依赖…");
const install = Bun.spawnSync({
  cmd: ["bun", "install"],
  cwd: root,
  stdio: ["inherit", "inherit", "inherit"],
});
if (install.exitCode !== 0) {
  throw new Error("依赖安装失败");
}

const envPath = join(root, ".env");
if (!existsSync(envPath)) {
  await copyFile(join(root, ".env.example"), envPath);
  console.log("\n已从 .env.example 创建 .env");
} else {
  console.log("\n.env 已存在，保持不变");
}

console.log(`
完成。下一步：

  bun run dev           # 启动桌面应用（Vite HMR + Rust 自动重编）
  bun run dev:sandbox   # 同上，但用临时数据目录，不碰 ~/.carter 与 ~/.carter-app
  bun run check         # lint + 类型检查 + fmt + clippy
  bun run build         # 打包安装程序 → target/release/bundle/
`);
