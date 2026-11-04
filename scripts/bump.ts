import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

/**
 * 统一改版本号。版本散在三处，手改总会漏一处：
 *   - package.json                       `version`
 *   - Cargo.toml                         `[workspace.package] version`（两个 crate 都继承它）
 *   - src-tauri/tauri.conf.json          `version`（安装包与「关于」页显示的就是它）
 *
 *   bun run bump            # 打印当前版本
 *   bun run bump patch      # 0.1.0 → 0.1.1（也可以 minor / major）
 *   bun run bump 0.2.0      # 指定版本
 *
 * 只改文件，不提交、不打 tag。改完会提示接下来的 git 命令。
 */

const root = join(import.meta.dir, "..");
const files = {
  pkg: join(root, "package.json"),
  cargo: join(root, "Cargo.toml"),
  tauri: join(root, "src-tauri", "tauri.conf.json"),
};

const pkg = JSON.parse(await readFile(files.pkg, "utf8")) as { version: string };
const current = pkg.version;
const arg = process.argv[2];

if (!arg) {
  console.log(current);
  process.exit(0);
}

function next(v: string, kind: string): string {
  const m = /^(\d+)\.(\d+)\.(\d+)$/.exec(v);
  if (!m) throw new Error(`当前版本 ${v} 不是 x.y.z 形式`);
  const [maj, min, pat] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (kind === "major") return `${maj + 1}.0.0`;
  if (kind === "minor") return `${maj}.${min + 1}.0`;
  if (kind === "patch") return `${maj}.${min}.${pat + 1}`;
  if (/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(kind)) return kind;
  throw new Error(`看不懂的版本参数：${kind}（用 major / minor / patch 或 x.y.z）`);
}

const target = next(current, arg);

/** 只替换第一处匹配，替换不到就报错 —— 静默跳过正是「漏改一处」的来源。 */
async function replaceIn(file: string, pattern: RegExp, label: string) {
  const text = await readFile(file, "utf8");
  if (!pattern.test(text)) throw new Error(`${label} 里没找到版本号`);
  await writeFile(
    file,
    text.replace(pattern, (_m, pre: string) => `${pre}${target}"`),
  );
}

await replaceIn(files.pkg, /("version":\s*")[^"]+"/, "package.json");
await replaceIn(
  files.cargo,
  /(\[workspace\.package\][^[]*?version\s*=\s*")[^"]+"/,
  "Cargo.toml [workspace.package]",
);
await replaceIn(files.tauri, /("version":\s*")[^"]+"/, "tauri.conf.json");

// 让 Cargo.lock 跟上新版本号，免得下一次构建再改出一份 diff。
Bun.spawnSync({ cmd: ["cargo", "update", "--workspace", "--offline"], cwd: root });

console.log(`${current} → ${target}

接下来：
  git commit -am "chore: 发布 v${target}"
  git tag v${target}`);
