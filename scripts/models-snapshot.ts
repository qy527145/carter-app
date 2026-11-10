import { mkdir, mkdtemp, readdir, readFile, readlink, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

/**
 * 生成打进安装包的 models.dev 快照。
 *
 *   bun run models:snapshot
 *
 * ## 为什么不用 `https://models.dev/api.json`
 *
 * 那个接口是 v1 导出、云端生成，国内访问挂在 Cloudflare 美国节点：本机实测
 * 6 MB 的包要 30 秒以上、还经常中途断掉。而源仓库里的 `models/*\/*.toml` 与
 * `providers/*\/provider.toml` 经 GitHub 加速站取，一两秒就到，而且**形状与
 * models.dev 自己生成的一致**（字段名、`limit`、`cost`、`modalities` 都照 v1 的
 * 写法落进同一个 JSON），carter 的 `resolve_model` 不需要任何改动。
 *
 * 取的是 v2 源文件、自己翻译回 v1 的形态：
 *
 *   - 模型 = 各家原厂（`models/<lab>/*.toml`）自己的模型，外加（顺便放进**原厂**名下）
 *     （`providers/*\/models/…`）里 `base_model` 能指回原厂的那些 —— 后者只是
 *     名字的另一种写法（转售商叫 `gpt-5.2`，原厂叫 `gpt-5.2`，价格一样），
 *     保留它们让用户从别处抄来的 meta 也能对上；字段全部沿用原厂那一条。
 *   - 转售商**自己**的条目也收（`base_model` 指回原厂那种，名字、字段沿用原厂）：
 *     从别处分享来的 meta 常常就是云平台那边的名字。代价是文件从 0.25 MB 涨到
 *     3.5 MB、生成从 1 分钟涨到 4 分钟（源仓库要读近万个文件）—— 换算成安装包只
 *     多 1 MB 出头。
 *
 *     生成脚本第一次跑时会先在临时目录里解开源仓库；之后如果 `MODELS_DEV_SRC`
 *     指向那边，就只读下载、不再解包（见 `listFromTarball`）。
 *     原厂名下的条目**也收**，是因为官方那几家（anthropic / openai）在
 *     `api.json` 里正是把它们列在同一家下面（`meta = "anthropic/<转售商那边的名字>"`）——
 *     别人的 meta 抄的就是这种写法，不收的话「按官方模板建的渠道」会有一半模型
 *     查不到。同一家目录里如果已经有 `base_model = "<本家>/<这个名字>"` 的条目，
 *     先合并它自带的覆盖（价格、缓存），再用原厂字段补齐。
 *   - 只留对话模型（输出里有 text）。生图 / 语音 / embedding 这个应用用不上，
 *     去掉能省一半体积。
 *   - 服务商端点 `api` 只有 210/235 家写了，其余是官方那几家（anthropic / openai
 *     / xai / google …）—— 用 `carter-app-core/src/catalog.rs` 里同一张
 *     `KNOWN_ENDPOINTS` 补上，免得快照装完「按 base_url 认服务商」少一大截。
 *
 * ## 国内源
 *
 * 官方 GitHub 优先，失败逐个试加速站。
 *
 * ## 打包时自动跑
 *
 * `tauri.conf.json` 的 `beforeBuildCommand` 会在每次 `tauri build` 前跑一次
 * （带 `--best-effort`）：能拉就换成当天的目录，拉不到就打一行警告、**原样保留**
 * 仓库里那份 —— 发版不该因为网络抖动失败。文件只在全部读完后才写，中途失败不会
 * 留下半截。
 */

const ROOT = join(import.meta.dir, "..");
const OUT = join(ROOT, "crates", "carter-app-core", "data", "models-snapshot.json");
const REPO = "anomalyco/models.dev";
const REF = "dev";

/**
 * 与 `crates/carter-app-core/src/net.rs` 里的 `GITHUB_ACCELS` 保持一致。
 *
 * 每个源的超时按「直连 / 加速站」分开：直连不通时 TCP 握手能挂掉好几分钟，
 * 不设超时的话一个文件就能把整次生成卡死。加速站三五秒没回就别等了。
 */
const ACCELS = ["", "https://gh-proxy.com/", "https://ghproxy.net/", "https://ghfast.top/"];
const TIMEOUT_MS = 8_000;

/**
 * 目录里没写 `api` 的官方服务商。与 `catalog.rs` 的 `KNOWN_ENDPOINTS` 同表，
 * 改一处就要改另一处（快照里补的是同一个值，只是提前拼进了文件）。
 */
const KNOWN_ENDPOINTS: [string, string][] = [
  ["anthropic", "api.anthropic.com"],
  ["openai", "api.openai.com"],
  ["xai", "api.x.ai"],
  ["google", "generativelanguage.googleapis.com"],
  ["groq", "api.groq.com"],
  ["deepseek", "api.deepseek.com"],
  ["moonshotai", "api.moonshot.ai"],
  ["zai", "api.z.ai"],
];

const API = `https://api.github.com/repos/${REPO}/git/trees/${REF}?recursive=1`;
const RAW = `https://raw.githubusercontent.com/${REPO}/${REF}`;
const TARBALL_URL = `https://github.com/${REPO}.git`;

function accelerate(url: string, accel: string): string {
  return accel ? `${accel}${url}` : url;
}

/** 同 [`fetchFirst`]，但要原始字节（tarball）。 */
async function netFirstRaw(url: string): Promise<Uint8Array> {
  const errors: string[] = [];
  for (const accel of ACCELS) {
    try {
      // 直连不通就赶紧换；换过去之后 tarball 本身要下几十秒，给长一点。
      const timeout = accel ? 120_000 : 20_000;
      const res = await fetch(accelerate(url, accel), { signal: AbortSignal.timeout(timeout) });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      return new Uint8Array(await res.arrayBuffer());
    } catch (e) {
      errors.push(`${accel || "github.com"}：${(e as Error).message}`);
    }
  }
  throw new Error(`全部源都失败 —— ${errors.join("；")}`);
}

/**
 * 逐个源试，谁先成功用谁。非 GitHub 地址不套加速站。
 *
 * 判断「成功」的标准是**形状对**，不是 HTTP 200：加速站出错时可能回一个 200 的
 * 错误页，raw.githubusercontent 对路径也有重定向。所以拿到内容之后交给 `valid`
 * 再验一遍，不对就换下一个源。
 */
async function fetchFirst(
  url: string,
  valid: (text: string) => boolean = () => true,
): Promise<string> {
  const errors: string[] = [];
  for (const accel of ACCELS) {
    const target = accelerate(url, accel);
    try {
      const res = await fetch(target, { signal: AbortSignal.timeout(TIMEOUT_MS) });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const text = await res.text();
      if (!valid(text)) throw new Error(`内容不对：${JSON.stringify(text.slice(0, 60))}`);
      return text;
    } catch (e) {
      errors.push(`${accel || "github.com"}：${(e as Error).message}`);
    }
  }
  throw new Error(`全部源都失败 —— ${errors.join("；")}`);
}

type Entry = { path: string; mode: string; target?: string };

/**
 * 仓库文件清单（一次请求，别按目录一棵棵爬）。
 *
 * 优先 `git ls-remote` + tarball 走加速站（不需要 token、也没有 API 限流）；
 * 都失败才退回 GitHub API（不带 token 时每小时只有 60 次）。
 */
async function listTree(): Promise<{ entries: Entry[]; commit: string; root?: string }> {
  try {
    return await listFromTarball();
  } catch (e) {
    console.warn(`从 tarball 取清单失败（${(e as Error).message}），改试 GitHub API`);
    return await listFromApi();
  }
}

/** 先问分支指向哪个提交，再把那个提交的 tarball 放进临时目录看一遍。 */
async function listFromTarball(): Promise<{ entries: Entry[]; commit: string; root?: string }> {
  const branch = /(?:refs\/heads\/)?([^/\s]+)$/.exec(REF)?.[1] ?? REF;
  const ls = Bun.spawnSync({ cmd: ["git", "ls-remote", TARBALL_URL, `refs/heads/${branch}`] });
  if (ls.exitCode !== 0) throw new Error(`git ls-remote 失败：${ls.stderr.toString().trim()}`);
  const sha = ls.stdout.toString().split(/\s/)[0];
  if (!/^[0-9a-f]{40}$/.test(sha ?? "")) throw new Error(`ls-remote 没找到分支 ${branch}`);
  const commit = sha.slice(0, 12);

  // 本地已经有这份源码（同一个提交）就直接用：解包近万个文件要小半分钟，
  // 反复调脚本时没必要每次都重来。`MODELS_DEV_SRC` 显式给目录时也按它来。
  const local = await localSource(commit);
  if (local) {
    console.log(`用本地源码 ${local.root}（提交 ${local.commit ?? "未知"}）`);
    return { entries: await scanDir(local.root), commit: local.commit ?? commit, root: local.root };
  }

  const tarball = await netFirstRaw(`https://codeload.github.com/${REPO}/tar.gz/${sha}`);
  const dir = await mkdtemp(join(tmpdir(), "models-snapshot-"));
  try {
    await writeFile(join(dir, "repo.tar.gz"), tarball);
    // 必须 cwd + 相对文件名：Windows 上 PATH 里常是 GNU tar（Git Bash 带的），
    // 它把 `C:\Users\…` 当成 `主机:路径` 的远程写法，报「Cannot connect to C:」。
    if (Bun.spawnSync({ cmd: ["tar", "-xzf", "repo.tar.gz"], cwd: dir }).exitCode !== 0) {
      throw new Error("解包失败");
    }
    const root = (await readdir(dir)).find((n) => n.startsWith("models.dev-")) ?? dir;
    await writeFile(join(dir, "COMMIT"), commit);
    return { entries: await scanDir(join(dir, root)), commit, root: join(dir, root) };
  } finally {
    // 源码留在临时目录里不删：下一次生成（同一次会话里往往要跑好几次）直接复用。
    // 系统清理临时目录时会连带清掉。
    void rm;
  }
}

/** 本地源码候选：`MODELS_DEV_SRC`，或上一次解包留下的那份（提交对得上才用）。 */
async function localSource(
  commit: string,
): Promise<{ root: string; commit: string | null } | null> {
  const candidates = process.env.MODELS_DEV_SRC
    ? [process.env.MODELS_DEV_SRC]
    : (await readdir(tmpdir()))
        .filter((n) => n.startsWith("models-snapshot-"))
        .map((n) => join(tmpdir(), n));
  for (const dir of candidates.reverse()) {
    const root = (await readdir(dir).catch(() => [])).find((n) => n.startsWith("models.dev-"));
    const stamped = await readFile(join(dir, "COMMIT"), "utf8").catch(() => null);
    if (root && process.env.MODELS_DEV_SRC) return { root: join(dir, root), commit: stamped };
    if (root && stamped === commit) return { root: join(dir, root), commit: stamped };
  }
  return null;
}

/** 扫一遍源码目录：普通文件与符号链接（带链接目标）。 */
async function scanDir(root: string): Promise<Entry[]> {
  const entries: Entry[] = [];
  const walk = async (rel: string) => {
    for (const e of await readdir(join(root, rel), { withFileTypes: true })) {
      const path = rel ? `${rel}/${e.name}` : e.name;
      if (e.isDirectory()) await walk(path);
      else if (e.isSymbolicLink()) {
        entries.push({ path, mode: "120000", target: await readlink(join(root, path)) });
      } else entries.push({ path, mode: "100644" });
    }
  };
  await walk("");
  return entries;
}

async function listFromApi(): Promise<{ entries: Entry[]; commit: string }> {
  // 树接口没有加速站版本，走系统代理（有就设 HTTPS_PROXY）。
  const res = await fetch(API, { signal: AbortSignal.timeout(60_000) });
  if (!res.ok) throw new Error(`HTTP ${res.status}（GitHub API 限流，需要代理或稍后再试）`);
  const tree = (await res.json()) as {
    tree: { path: string; type: string; sha: string; mode: string }[];
    sha: string;
  };
  return {
    entries: tree.tree.filter((t) => t.type === "blob").map(({ path, mode }) => ({ path, mode })),
    commit: tree.sha.slice(0, 12),
  };
}

/** 并发取一批文件；任何一个失败就整体失败（快照不能缺一块还照样发）。 */
async function fetchAll<T extends { path: string }>(
  entries: T[],
  fetchOne: (e: T) => Promise<string>,
  concurrency = 24,
  localRoot?: string,
): Promise<Map<string, string>> {
  const out = new Map<string, string>();
  let next = 0;
  let failed: Error | null = null;
  const workers = Array.from({ length: concurrency }, async () => {
    while (next < entries.length && !failed) {
      const entry = entries[next++];
      try {
        // 有本地源码就先读它：一次生成要读好几千个文件，走网络要几分钟。
        const local = localRoot
          ? await readFile(join(localRoot, entry.path), "utf8").catch(() => null)
          : null;
        out.set(entry.path, local ?? (await fetchOne(entry)));
      } catch (e) {
        failed = new Error(`${entry.path}：${(e as Error).message}`);
      }
    }
  });
  await Promise.all(workers);
  if (failed) throw failed;
  return out;
}

function normalizePath(p: string): string {
  const out: string[] = [];
  for (const seg of p.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") out.pop();
    else out.push(seg);
  }
  return out.join("/");
}

// biome-ignore lint/suspicious/noExplicitAny: 目录里的 TOML 形状各异，逐字段按名字取再按需校验。
type Toml = Record<string, any>;

function toml(text: string, path = "<unknown>"): Toml {
  try {
    return Bun.TOML.parse(text) as Toml;
  } catch (e) {
    if (process.env.SNAPSHOT_DEBUG) {
      const file = join("/tmp", `snapshot-bad-${path.replaceAll("/", "_")}`);
      writeFile(file, text).catch(() => {});
      console.error(
        `解析失败 ${path}，原文已存到 ${file}（${text.length} 字节，开头 ${JSON.stringify(text.slice(0, 60))}）`,
      );
    }
    throw new Error(`${path}：${(e as Error).message}`);
  }
}

/**
 * 普通文件：raw 按路径取，固定在这个提交上。
 *
 * raw 不跟随符号链接（它对链接返回的是目标路径那一行文本），而走 GitHub API 列
 * 目录时清单里只有 `mode = 120000`、没有链接目标（tarball 那条路才有）。所以拿到
 * 一行 `.toml` 路径就地跟着取 —— 不加这一步，一个符号链接的转售条目就会让整次
 * 生成失败。
 */
async function byRaw(e: { path: string }): Promise<string> {
  let path = e.path;
  for (let i = 0; i < 8; i++) {
    const text = await fetchFirst(`${RAW}/${path}`, (t) => !/^\s*[{<]/.test(t));
    // 目标路径里可能有冒号（`amazon.nova-lite-v1:0.toml`、`:free`）。
    const link = /^\s*(?:\.{0,2}\/)?[\w.:/-]+\.toml\s*$/.exec(text);
    if (!link) return text;
    path = normalizePath(`${dirname(path)}/${link[0].trim()}`);
  }
  throw new Error(`${e.path}：符号链接层数太多`);
}

/**
 * 符号链接：解析到目标文件再取一次。链接指向的也可以是另一个链接（多层），
 * 目标本身已经在清单里所以链接目标也都是普通文件。
 */
async function bySymlink(e: Entry, all: Entry[]): Promise<string> {
  let path = e.path;
  let target = e.target;
  for (let i = 0; i < 8 && target; i++) {
    path = normalizePath(`${dirname(path)}/${target}`);
    target = all.find((x) => x.path === path)?.target;
  }
  return fetchFirst(`${RAW}/${path}`, (t) => !/^\s*[{<]/.test(t));
}

/** 文件路径的最后一段（去掉 `.toml`）—— 模型 id 就是它。 */
function pathName(rel: string): string {
  const last = rel.split("/").slice(-1)[0];
  return last.endsWith(".toml") ? last.slice(0, -".toml".length) : last;
}

/** v1 的模态写法：具体 MIME 归回大类。 */
function v1Modality(m: string): string {
  if (m === "application/pdf") return "pdf";
  return m.split("/")[0];
}

function v1Modalities(list: string[]): string[] {
  const out: string[] = [];
  for (const m of list.map(v1Modality)) if (!out.includes(m)) out.push(m);
  return out;
}

/**
 * 原厂模型文件 → v1 模型对象。字段名与 models.dev 的 `api.json` 一一对应
 * （见它的 README「API Reference」），多一个少一个都会让 carter 那边解析错。
 */
function labModelToV1(id: string, t: Toml): Toml | null {
  const output: string[] = t.modalities?.output ?? [];
  // 只留对话模型：没有 text 输出的一律不要（生图 / 语音 / embedding）。
  if (!output.includes("text") || t.type !== undefined) return null;

  const caps: Toml = t.capabilities ?? {};
  const out: Toml = { id, name: t.name };
  if (t.description) out.description = t.description;
  if (t.family) out.family = t.family;
  out.attachment = (t.modalities?.input ?? []).some((m: string) => m !== "text");
  out.reasoning = caps.reasoning?.supported === true;
  out.tool_call = caps.tools?.supported === true;
  if (caps.structured_output !== undefined) out.structured_output = caps.structured_output;
  if (caps.temperature !== undefined) out.temperature = caps.temperature;
  if (t.open_weights !== undefined) out.open_weights = t.open_weights;
  if (t.knowledge) out.knowledge = t.knowledge;
  if (t.release_date) out.release_date = t.release_date;
  if (t.last_updated) out.last_updated = t.last_updated;
  out.modalities = {
    input: v1Modalities(t.modalities?.input ?? []),
    output: v1Modalities(output),
  };
  if (t.limit) {
    const limit: Toml = {};
    if (t.limit.context !== undefined) limit.context = t.limit.context;
    if (t.limit.output !== undefined) limit.output = t.limit.output;
    if (Object.keys(limit).length) out.limit = limit;
  }
  return out;
}

async function main() {
  const { entries, commit, root } = await listTree();
  // 普通文件（`100644`）与符号链接（`120000`）。后者是「同一个模型挂两家卖」的
  // 写法。raw 不跟随链接（全球一致，各加速站也一样），所以链接按清单里的目标
  // 自己解析，由 `bySymlink` 处理。
  const files = entries.filter((e) => e.mode === "100644" || e.mode === "120000");
  const providerTomls = files.filter((e) => /^providers\/[^/]+\/provider\.toml$/.test(e.path));
  const labModels = files.filter((e) => /^models\/[^/]+\/[^/]+\.toml$/.test(e.path));
  const resellerModels = files.filter((e) => /^providers\/[^/]+\/models\/.+\.toml$/.test(e.path));
  console.log(
    `仓库 ${commit}：${providerTomls.length} 家服务商、${labModels.length} 条原厂模型、${resellerModels.length} 条转售商条目`,
  );

  const [providers, labs, resellers] = await Promise.all([
    fetchAll(providerTomls, byRaw, 24, root),
    fetchAll(labModels, byRaw, 24, root),
    fetchAll(resellerModels, (e) => (e.target ? bySymlink(e, resellerModels) : byRaw(e)), 24, root),
  ]);

  // 1) 原厂模型：`lab/model` 是 meta 的写法，也是 v1 的 canonical id。
  const labById = new Map<string, Toml>();
  for (const [path, text] of labs) {
    const m = /^models\/([^/]+)\/([^/]+)\.toml$/.exec(path);
    if (!m) continue;
    const [, lab, id] = m;
    if (toml(text, path).v2_only) continue;
    const v1 = labModelToV1(id, toml(text, path));
    if (v1) labById.set(`${lab}/${id}`, v1);
  }

  // 2) 服务商：name / env / doc / api。官方那几家没写 api 的用已知端点补。
  type Provider = {
    id: string;
    name: string;
    env?: string[];
    doc?: string;
    api?: string;
    models: Toml;
  };
  const out: Record<string, Provider> = {};
  for (const [path, text] of providers) {
    const m = /^providers\/([^/]+)\/provider\.toml$/.exec(path);
    if (!m) continue;
    const [, id] = m;
    const t = toml(text, path);
    if (t.v1_alias) continue;
    const api = t.api ?? KNOWN_ENDPOINTS.find(([pid]) => pid === id)?.at(1);
    out[id] = {
      id,
      name: t.name ?? id,
      ...(t.env ? { env: t.env } : {}),
      ...(t.doc ? { doc: t.doc } : {}),
      ...(api ? { api: t.api ?? `https://${api}` } : {}),
      models: {},
    };
  }

  // 原厂自己的服务商也带上：服务商目录里没列出来的原厂（只出现在 models/ 下的）
  // 也要能查。
  for (const [key, model] of labById) {
    const [lab, id] = key.split("/");
    out[lab] ??= { id: lab, name: lab, models: {} };
    out[lab].models[id] = { ...model, id };
  }

  // 3) 转售条目：指向原厂那条，键是转售商那边的模型名。原厂名下的同名条目
  //    先用它自带的覆盖打底，再被生成的文件（若有）盖一层。
  let resold = 0;
  for (const [path, text] of resellers) {
    const m = /^providers\/([^/]+)\/models\/(.+)\.toml$/.exec(path);
    if (!m) continue;
    const [, pid, rel] = m;
    const p = out[pid];
    if (!p) continue;
    const t = toml(text, path);
    const id = pathName(rel);
    const baseId = typeof t.base_model === "string" ? t.base_model : "";
    const base = labById.get(baseId);
    if (!base) continue;
    p.models[id] = { ...base, id, canonical_model_id: baseId };
    resold++;
  }

  // 空的服务商（只卖生图之类）没有保留价值。
  for (const [pid, p] of Object.entries(out))
    if (Object.keys(p.models).length === 0) delete out[pid];

  // 键排序：同一个输入每次生成出字节一样的文件，diff 才看得懂。
  const sorted: Record<string, Provider> = {};
  for (const pid of Object.keys(out).sort()) {
    const models = Object.fromEntries(
      Object.entries(out[pid].models).sort(([a], [b]) => a.localeCompare(b)),
    );
    sorted[pid] = { ...out[pid], models };
  }

  // 头一项是生成信息，不是服务商。`catalog.rs` 按 key 取服务商，它会自然被忽略
  // （没有 `models` 字段）；界面从 `builtin_snapshot_date()` 读它。
  const generatedAt = new Date().toISOString().slice(0, 10);
  const json = JSON.stringify({ $generated_at: generatedAt, ...sorted });
  // 旧内容要在覆盖**之前**读，否则永远等于新内容、永远报「无变化」。
  const old = await readFile(OUT, "utf8").catch(() => "");
  await mkdir(dirname(OUT), { recursive: true });
  await writeFile(OUT, `${json}\n`);

  const modelCount = Object.values(sorted).reduce((n, p) => n + Object.keys(p.models).length, 0);
  console.log(
    `已写入 ${OUT}\n  ${Object.keys(sorted).length} 家服务商、${modelCount} 条模型（其中转售商条目 ${resold} 条）\n` +
      `  ${(json.length / 1024 / 1024).toFixed(2)} MB` +
      (old === `${json}\n` ? "（无变化）" : "（有变化）"),
  );
}

// `--best-effort`：打包时用。拉不到就保留仓库里那份并正常退出，别为网络抖动
// 挡住发版；手动跑（`bun run models:snapshot`）仍然失败即报错。
if (process.argv.includes("--best-effort")) {
  try {
    await main();
  } catch (e) {
    console.warn(`快照没更新（沿用仓库里那份）：${(e as Error).message}`);
  }
} else {
  await main();
}
