/**
 * 由提交记录生成一版的更新说明（Markdown，打到标准输出）。
 *
 *   bun run release:notes            # 预览：上一个 tag 到 HEAD
 *   bun run release:notes v0.3.7     # v0.3.7 相对它前一个 tag
 *
 * 发布流水线（.github/workflows/release.yml）用它写 Release 正文，应用里的
 * 「更新内容」读的就是这段正文 —— 所以这里的输出是**给用户看的**：
 *
 *   - 只收 `feat` / `fix` / `perf`，`chore` / `ci` / `refactor` 这些用户感知不到的不收；
 *   - 提交标题原样当一条说明，所以标题要写成「用户能看懂的一句话」。
 *
 * 为什么不维护一份 CHANGELOG.md：那是第二份要手写、要记得更新的东西，漏一次
 * 用户看到的就是空白。提交标题本来每次都得写。
 */

/**
 * 正文里「更新说明」与「安装包文件表」的分界。应用只展示它之前的部分。
 * 与 `crates/carter-app-core/src/update.rs` 的 `INSTALLERS_MARK` 必须一致。
 */
export const INSTALLERS_MARK = "<!-- installers -->";

/** 收哪些类型、各自归到哪个标题下。顺序就是输出顺序。 */
const GROUPS: [type: string, title: string][] = [
  ["feat", "新增"],
  ["fix", "修复"],
  ["perf", "优化"],
];

/** 一条用户感知不到的改动都没有时的说明：宁可说实话，也不要留一块空白。 */
export const NOTHING_USER_FACING = "这一版是内部维护与稳定性改进，没有界面上的变化。";

/** `feat(team)!: 看板支持拖拽` → `["feat", "看板支持拖拽"]`；不是约定格式的返回 null。 */
export function parseSubject(subject: string): [type: string, text: string] | null {
  const m = /^([a-z]+)(?:\([^)]*\))?!?[:：]\s*(.+)$/.exec(subject.trim());
  return m ? [m[1], m[2].trim()] : null;
}

/** 提交标题（新的在前）→ 更新说明。 */
export function renderNotes(subjects: string[]): string {
  const sections: string[] = [];
  for (const [type, title] of GROUPS) {
    const seen = new Set<string>();
    for (const s of subjects) {
      const parsed = parseSubject(s);
      // 同一句话出现两次（cherry-pick、revert 后重提）只留一条。
      if (parsed && parsed[0] === type) seen.add(parsed[1]);
    }
    if (seen.size > 0) {
      sections.push(`### ${title}\n\n${[...seen].map((t) => `- ${t}`).join("\n")}`);
    }
  }
  return sections.length > 0 ? sections.join("\n\n") : NOTHING_USER_FACING;
}

function git(args: string[]): string | null {
  const proc = Bun.spawnSync({ cmd: ["git", ...args], stdout: "pipe", stderr: "ignore" });
  return proc.exitCode === 0 ? proc.stdout.toString().trim() : null;
}

if (import.meta.main) {
  const tag = process.argv[2];
  // tag 还没打（本地预览，或手动触发流水线时 tag 由 `gh release create` 现建）
  // 就按 HEAD 算。
  const tagExists = tag ? git(["rev-parse", "--verify", "--quiet", `refs/tags/${tag}`]) : null;
  const head = tagExists ? `refs/tags/${tag}` : "HEAD";
  // 已经打了 tag 就从它的上一个提交往回找，否则会找到它自己。
  const prev = git(["describe", "--tags", "--abbrev=0", tagExists ? `${head}^` : head]);
  // 第一版没有「上一个 tag」，整段历史都算。
  const range = prev ? `${prev}..${head}` : head;
  const log = git(["log", "--no-merges", "--format=%s", range]);
  if (log === null) {
    console.error(`读不到提交记录（${range}）；流水线里要用 fetch-depth: 0 检出完整历史`);
    process.exit(1);
  }
  console.log(renderNotes(log.split("\n").filter(Boolean)));
}
