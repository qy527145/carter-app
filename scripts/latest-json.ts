import { readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

/**
 * 生成自更新清单 `latest.json`。
 *
 *   bun run scripts/latest-json.ts <版本> <tag> <bundle 目录…>
 *
 * 例：`bun run scripts/latest-json.ts 0.2.0 v0.2.0 artifacts` —— 会在 `artifacts/`
 * 里递归找 tauri 打出来的自更新包，写一份 `artifacts/latest.json`。
 *
 * ## 找什么
 *
 * tauri bundle 的自更新产物（`--bundles updater`）按平台是：
 *
 * | 平台 | 包 | updater target |
 * | --- | --- | --- |
 * | macOS | `carter-app.app.tar.gz` | `darwin-aarch64` / `darwin-x86_64` |
 * | Windows | `carter-app_<版本>_x64-setup.exe.zip` | `windows-x86_64` |
 * | Linux | `carter-app_<版本>_amd64.AppImage.tar.gz` | `linux-x86_64` |
 *
 * 每个包旁边都有同名 `.sig`（minisign，`version:<版本>` 已绑进 trusted comment）。
 * **没有 `.sig` 的包直接报错**：宁可发版失败，也别发布一个装不上的更新。
 *
 * ## 下载地址
 *
 * 写死成 `https://github.com/<仓库>/releases/download/<文件名>`。客户端检查时
 * 会把这条**整串**接到加速站前缀后面去下载（见 `carter-app-core/src/net.rs`
 * 的 `accelerate`），所以这里必须是完整的 GitHub 地址。不带 tag：用
 * `releases/download/<文件名>` 会落到**最新**那个 release 上 —— 加速站按 URL
 * 缓存，带 tag 的地址每个版本都是冷缓存，而 `app.tar.gz` 这类文件名每个版本
 * 里只有一份，指向最新不会串版本。
 */

const REPO = "qy527145/carter-app";

const [version, _tag, ...dirs] = process.argv.slice(2);
if (!version || !_tag || dirs.length === 0) {
  console.error(
    "用法：bun run scripts/latest-json.ts <版本> <tag> <bundle 目录…>（tag 目前只用于打印）",
  );
  process.exit(1);
}
const ver = version.replace(/^v/, "");

async function walk(dir: string): Promise<string[]> {
  const out: string[] = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...(await walk(path)));
    else out.push(path);
  }
  return out;
}

const files = (await Promise.all(dirs.map((d) => walk(d).catch(() => [])))).flat();

/** 一个包属于哪个平台（认不出就是 `null`，比如 dmg / deb，它们不是自更新包）。 */
function platformOf(file: string): string | null {
  const name = file.split("/").pop() ?? "";
  const arch = name.includes("aarch64") || name.includes("arm64") ? "aarch64" : "x86_64";
  if (name.endsWith(".app.tar.gz")) {
    return `darwin-${arch}`;
  }
  if (name.endsWith("-setup.exe.zip")) {
    return "windows-x86_64";
  }
  if (name.endsWith(".AppImage.tar.gz")) {
    return "linux-x86_64";
  }
  return null;
}

const platforms: Record<string, { signature: string; url: string }> = {};
for (const file of files) {
  const target = platformOf(file);
  if (!target) continue;
  const sigPath = `${file}.sig`;
  const sig = await readFile(sigPath, "utf8").catch(() => null);
  if (sig === null) {
    console.error(`自更新包 ${file} 没有签名（${sigPath} 不存在）—— 发版中止`);
    process.exit(1);
  }
  const name = file.split("/").slice(-1)[0];
  platforms[target] = {
    signature: sig.trim(),
    url: `https://github.com/${REPO}/releases/download/${encodeURIComponent(name)}`,
  };
}

if (Object.keys(platforms).length === 0) {
  console.error("没有找到任何自更新包（.app.tar.gz / -setup.exe.zip / .AppImage.tar.gz）");
  process.exit(1);
}

const manifest = {
  version: ver,
  // 发版说明随 `latest.json` 一起下到客户端（侧栏「重启更新」旁边那行）。
  // 发版时可以用 `RELEASE_NOTES="…" bun run scripts/latest-json.ts …` 覆盖。
  notes: process.env.RELEASE_NOTES?.trim() || `Carter ${ver}`,
  pub_date: new Date().toISOString(),
  platforms,
};

const out = join(dirs[0], "latest.json");
await writeFile(out, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`已写 ${out}`);
console.log(JSON.stringify(manifest, null, 2));
