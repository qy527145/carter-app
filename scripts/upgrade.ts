import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";

/**
 * 一次做完发版：改版本号 → 提交 → 打 tag。
 *
 *   bun run upgrade            # 打印当前版本
 *   bun run upgrade patch      # 0.1.0 → 0.1.1（也可以 minor / major）
 *   bun run upgrade 0.2.0      # 指定版本
 *
 * 版本散在三处，手改总会漏一处：
 *   - package.json                       `version`
 *   - Cargo.toml                         `[workspace.package] version`（两个 crate 都继承它）
 *   - src-tauri/tauri.conf.json          `version`（安装包与「关于」页显示的就是它）
 *
 * 提交与 tag 一起做掉，省得再记两条 git 命令。刻意不 push —— 推不推由你决定。
 * tag 形如 v0.1.1 会触发 .github/workflows/release.yml（cargo-dist 生成）。
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
const tag = `v${target}`;

if (target === current) {
  console.error(`已经是 ${current}，没有要改的版本号`);
  process.exit(1);
}

/** 同步执行 git，输出直通终端；返回退出码。 */
function git(args: string[], opts: { quiet?: boolean } = {}): number {
  const proc = Bun.spawnSync({
    cmd: ["git", ...args],
    cwd: root,
    stdio: opts.quiet ? ["ignore", "ignore", "ignore"] : ["inherit", "inherit", "inherit"],
  });
  return proc.exitCode;
}

// 依赖指到仓库外面的本地目录（开发期临时写的 `path = "../carter"`）时先停：
// 本机编得过，构建机上没有那个目录，三个平台会一起挂在 `cargo metadata` 上
// （v0.3.7 第一次发版就是这么失败的）。
{
  const meta = Bun.spawnSync({
    cmd: ["cargo", "metadata", "--format-version", "1", "--offline"],
    cwd: root,
    stdout: "pipe",
    stderr: "pipe",
  });
  if (meta.exitCode !== 0) {
    console.error(`cargo metadata 失败，没法确认依赖能不能在构建机上解析：\n${meta.stderr}`);
    process.exit(1);
  }
  const { packages } = JSON.parse(meta.stdout.toString()) as {
    packages: { name: string; source: string | null; manifest_path: string }[];
  };
  // `source` 为空 = 本地路径上的包；仓库里的两个 crate 也是，所以再看它在不在仓库内。
  const outside = packages.filter(
    (p) => p.source === null && !p.manifest_path.startsWith(`${root}/`),
  );
  if (outside.length > 0) {
    console.error("这些依赖指向仓库外的本地目录，构建机上不存在，先发版并改回版本号：");
    for (const p of outside) console.error(`  ${p.name}  ${p.manifest_path}`);
    process.exit(1);
  }
}

// tag 已存在时先停：等文件改完、提交完再发现，回滚更麻烦。
if (git(["rev-parse", "--verify", "--quiet", `refs/tags/${tag}`], { quiet: true }) === 0) {
  console.error(`tag ${tag} 已存在；换个版本号，或先 git tag -d ${tag}`);
  process.exit(1);
}

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

if (git(["commit", "-am", `chore: 发布 ${tag}`]) !== 0) {
  console.error(`\n提交失败：版本号已改成 ${target}，但没有提交，也没有打 tag。`);
  process.exit(1);
}

if (git(["tag", tag]) !== 0) {
  console.error(`\n打 tag 失败：版本号已提交，但 ${tag} 没打上。`);
  process.exit(1);
}

console.log(`
${current} → ${target}
已提交：chore: 发布 ${tag}
已打 tag：${tag}

推送后触发发布流水线：
  git push origin main --tags`);
