import { join } from "node:path";

/**
 * 按平台打安装包，默认根据当前操作系统自动选择。
 *
 * 用法：
 *   bun run build                  # 自动：当前系统对应的安装包
 *   bun run build:win              # Windows：NSIS + MSI
 *   bun run build:mac              # macOS：.app + .dmg
 *   bun run build:linux            # Linux：deb + rpm + AppImage
 *   bun run build -- --debug       # 其余参数原样转给 `tauri build`
 *
 * 交叉编译：
 *   - 非 Windows 上打 win 包走 Tauri 官方的 cargo-xwin 方案，只能出 NSIS，
 *     需要先 `cargo install cargo-xwin`、`rustup target add x86_64-pc-windows-msvc`，
 *     并装好 nsis 与 llvm（macOS：`brew install nsis llvm`）。产物在
 *     `target/x86_64-pc-windows-msvc/release/bundle/`。
 *   - mac 包只能在 macOS 上打，linux 包只能在 Linux 上打（Tauri 不支持交叉），
 *     其它平台请交给 CI。
 */

type Platform = "win" | "mac" | "linux";

const root = join(import.meta.dir, "..");

const hostPlatform = (): Platform => {
  switch (process.platform) {
    case "win32":
      return "win";
    case "darwin":
      return "mac";
    case "linux":
      return "linux";
    default:
      console.error(`不认识的操作系统：${process.platform}，请显式指定 win / mac / linux`);
      process.exit(1);
  }
};

const ALIASES: Record<string, Platform> = {
  win: "win",
  windows: "win",
  mac: "mac",
  macos: "mac",
  darwin: "mac",
  linux: "linux",
};

const args = process.argv.slice(2);
const first = args[0]?.toLowerCase();
const requested = first && first in ALIASES ? ALIASES[first] : undefined;
const rest = requested ? args.slice(1) : args;
const host = hostPlatform();
const platform = requested ?? host;

const tauriArgs: string[] = [];
switch (platform) {
  case "win":
    if (host === "win") {
      tauriArgs.push("--bundles", "nsis,msi");
    } else {
      // MSI 依赖 WiX，只能在 Windows 上跑；交叉编译只出 NSIS。
      tauriArgs.push(
        "--runner",
        "cargo-xwin",
        "--target",
        "x86_64-pc-windows-msvc",
        "--bundles",
        "nsis",
      );
    }
    break;
  case "mac":
    if (host !== "mac") {
      console.error("macOS 安装包只能在 macOS 上打（需要 Xcode 工具链），请在 Mac 或 CI 上执行。");
      process.exit(1);
    }
    tauriArgs.push("--bundles", "app,dmg");
    break;
  case "linux":
    if (host !== "linux") {
      console.error(
        "Linux 安装包只能在 Linux 上打（Tauri 不支持交叉编译），请在 Linux 或 CI 上执行。",
      );
      process.exit(1);
    }
    tauriArgs.push("--bundles", "deb,rpm,appimage");
    break;
}

const cmd = ["bun", "x", "tauri", "build", ...tauriArgs, ...rest];
console.log(`目标平台：${platform}${requested ? "" : "（自动）"}\n> ${cmd.join(" ")}\n`);

const proc = Bun.spawn({ cmd, cwd: root, stdio: ["inherit", "inherit", "inherit"] });
process.exit(await proc.exited);
