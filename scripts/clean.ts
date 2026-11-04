import { existsSync } from "node:fs";
import { rm } from "node:fs/promises";
import { join } from "node:path";

/**
 * 清理构建产物。
 *
 *   bun run clean          # dist/ + cargo clean（target/ 动辄几十 GB）
 *   bun run clean --web    # 只清 dist/
 *   bun run clean --all    # 再加 node_modules/ 与 .sandbox/
 */

const root = join(import.meta.dir, "..");
const args = process.argv.slice(2);
const webOnly = args.includes("--web");
const all = args.includes("--all");

const dirs = ["dist", ...(all ? ["node_modules", ".sandbox"] : [])];
for (const d of dirs) {
  const p = join(root, d);
  if (existsSync(p)) {
    await rm(p, { recursive: true, force: true });
    console.log(`已删除 ${d}/`);
  }
}

if (!webOnly) {
  const proc = Bun.spawn({
    cmd: ["cargo", "clean"],
    cwd: root,
    stdio: ["inherit", "inherit", "inherit"],
  });
  process.exit(await proc.exited);
}
