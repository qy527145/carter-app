import { existsSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/**
 * 看应用日志（`$CARTER_APP_HOME/app.log`，默认 `~/.carter-app/app.log`）。
 *
 *   bun run logs            # 最后 200 行
 *   bun run logs -f         # 持续跟踪
 *   bun run logs -n 50      # 最后 50 行
 *   bun run logs --sandbox  # 看 dev:sandbox 的日志
 *   bun run logs --path     # 只打印路径
 */

const args = process.argv.slice(2);
const appHome = args.includes("--sandbox")
  ? join(import.meta.dir, "..", ".sandbox", "carter-app")
  : (process.env.CARTER_APP_HOME ?? join(homedir(), ".carter-app"));
const file = join(appHome, "app.log");

if (args.includes("--path")) {
  console.log(file);
  process.exit(0);
}
if (!existsSync(file)) {
  console.error(`还没有日志：${file}\n（应用启动过一次才会生成）`);
  process.exit(1);
}

const follow = args.includes("-f") || args.includes("--follow");
const nIdx = args.indexOf("-n");
const lines = nIdx >= 0 ? Number(args[nIdx + 1] ?? 200) : 200;

console.error(`# ${file}（${(statSync(file).size / 1024).toFixed(1)} KB）`);

if (process.platform === "win32") {
  // Windows 没有 tail，用 PowerShell 的 Get-Content 代替。
  const cmd = `Get-Content -LiteralPath '${file}' -Tail ${lines}${follow ? " -Wait" : ""} -Encoding UTF8`;
  const proc = Bun.spawn({
    cmd: ["powershell", "-NoProfile", "-Command", cmd],
    stdio: ["inherit", "inherit", "inherit"],
  });
  process.exit(await proc.exited);
}

const proc = Bun.spawn({
  cmd: ["tail", "-n", String(lines), ...(follow ? ["-F"] : []), file],
  stdio: ["inherit", "inherit", "inherit"],
});
process.exit(await proc.exited);
