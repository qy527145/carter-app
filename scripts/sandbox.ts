import { existsSync } from "node:fs";
import { copyFile, mkdir, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

/**
 * 用隔离的数据目录启动 `tauri dev`：`CARTER_HOME` 与 `CARTER_APP_HOME` 指向
 * `.sandbox/` 下的两个目录，真实的 `~/.carter`（会话、技能、渠道）和
 * `~/.carter-app`（助理状态、工单）都不会被碰到。
 *
 * 用法：
 *   bun run dev:sandbox            # 沿用上次的沙箱（首次会拷一份真实 config.toml 进去）
 *   bun run dev:sandbox --fresh    # 清空沙箱重来
 *   bun run dev:sandbox --empty    # 不拷 config.toml，模拟全新用户
 *   其余参数原样转给 `tauri dev`
 */

const root = join(import.meta.dir, "..");
const box = join(root, ".sandbox");
const carterHome = join(box, "carter");
const appHome = join(box, "carter-app");

const args = process.argv.slice(2);
const fresh = args.includes("--fresh");
const empty = args.includes("--empty");
const rest = args.filter((a) => a !== "--fresh" && a !== "--empty");

if (fresh && existsSync(box)) {
  await rm(box, { recursive: true, force: true });
  console.log("已清空 .sandbox/");
}
await mkdir(carterHome, { recursive: true });
await mkdir(appHome, { recursive: true });

// 渠道配置里一般是 `${ENV}` 引用而不是明文密钥，拷过去就能直接聊；
// models.json 是 models.dev 的公共元数据缓存，没有它助理和模型标签都起不来。
const realHome = process.env.CARTER_HOME ?? join(homedir(), ".carter");
if (!empty) {
  for (const name of ["config.toml", "models.json"]) {
    const from = join(realHome, name);
    const to = join(carterHome, name);
    if (!existsSync(to) && existsSync(from)) {
      await copyFile(from, to);
      console.log(`已拷贝 ${from} → .sandbox/carter/${name}`);
    }
  }
}

console.log(`沙箱：CARTER_HOME=${carterHome}\n      CARTER_APP_HOME=${appHome}\n`);

const proc = Bun.spawn({
  cmd: ["bun", "x", "tauri", "dev", ...rest],
  cwd: root,
  env: { ...process.env, CARTER_HOME: carterHome, CARTER_APP_HOME: appHome },
  stdio: ["inherit", "inherit", "inherit"],
});
process.exit(await proc.exited);
