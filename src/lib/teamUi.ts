// 目标 / 任务 / 看板的共用工具：状态色、进度环、时间的说法。
//
// 这些是**纯函数与常量**，单独放一个文件是为了让三个地方（主页面、消息框下方的
// 待办条、任务详情抽屉）说的是同一套话。状态色散落三处时，最容易出现「同一件事
// 在列表里是橙色、在抽屉里是蓝色」—— 那不叫设计，那叫没设计。

import type { TeamEvent, TeamGoal, TeamMember, TeamPeople, TeamTask } from "./api";
import { formatContext } from "./promptContext";

/** 「把这个任务交给卡特」带上的上下文。 */
export function taskHandoff(goal: TeamGoal, task: TeamTask): string {
  return formatContext({
    kind: "task",
    title: task.title,
    body:
      `接着做这个任务：\n\n` +
      `- 任务：${task.title}\n` +
      `- 任务 id：${task.id}\n` +
      `- 所属目标：${goal.title}（${goal.id}）\n` +
      `- 当前状态：${task.status_label}\n` +
      (task.blocked_reason ? `- 受阻原因：${task.blocked_reason}\n` : "") +
      `\n先看一眼任务的具体要求（acceptance 里写了验收标准），再动手。`,
  });
}

/**
 * 「让卡特建一块看板」。
 *
 * 走上下文卡片而不是裸一句话：卡片那头带着「要建的是看板」这个类型，输入框里是
 * 一枚能一眼认出形状的标签，模型收到的也是一段结构化的说明。以前这里传的是普通
 * 字符串，于是同一个入口在空状态和侧栏里长得不一样 —— 一处是卡片，一处是裸文本。
 */
export function newBoardHandoff(boards: { title: string }[]): string {
  return formatContext({
    kind: "board",
    title: "新建看板",
    body:
      `帮我建一块看板。\n\n` +
      (boards.length > 0
        ? `目前已经有 ${boards.length} 块：${boards
            .slice(0, 5)
            .map((b) => `「${b.title}」`)
            .join("、")}${boards.length > 5 ? " 等" : ""}，新建的要和它们不重复。\n`
        : "") +
      `\n先用 board_list 看一眼现有的，再想清楚这一块要回答什么问题，然后 board_new 建出来。` +
      `\n内容我在这条消息里补：要回顾哪一段、想看清什么。`,
  });
}

/** 「让卡特建一个目标」。与看板同理：走卡片，不走裸文本。 */
export function newGoalHandoff(): string {
  return formatContext({
    kind: "goal",
    title: "新建目标",
    body:
      `帮我在团队目录里建一个目标。\n\n` +
      `先把要做成什么说清楚（我在这条消息里补），再拆成任务、理好依赖。` +
      `\n任务要具体到「谁做完算完」，验收标准写进 acceptance。`,
  });
}

/** 任务 / 目标状态 → 语义色。用 body 上的 CSS 变量，两套主题各自成立。 */
export function statusTone(status: string): {
  fg: string;
  bg: string;
  label: string;
} {
  switch (status) {
    case "doing":
    case "active":
      return { fg: "var(--tone-blue)", bg: "var(--tone-blue-soft)", label: "进行中" };
    case "blocked":
      return { fg: "var(--tone-orange)", bg: "var(--tone-orange-soft)", label: "受阻" };
    case "done":
      return { fg: "var(--tone-green)", bg: "var(--tone-green-soft)", label: "已完成" };
    case "dropped":
      return { fg: "var(--text-dim)", bg: "var(--bubble)", label: "已放弃" };
    case "archived":
      return { fg: "var(--text-dim)", bg: "var(--bubble)", label: "已归档" };
    case "paused":
      return { fg: "var(--tone-violet)", bg: "var(--tone-violet-soft)", label: "搁置" };
    default:
      return { fg: "var(--text-dim)", bg: "var(--bubble)", label: "待办" };
  }
}

/** 优先级 → 色。P0 用红（真会烧起来的那种），P1 橙，其余中性。 */
export function priorityTone(p: string): { fg: string } {
  switch (p) {
    case "P0":
      return { fg: "var(--danger)" };
    case "P1":
      return { fg: "var(--tone-orange)" };
    case "P2":
      return { fg: "var(--tone-blue)" };
    default:
      return { fg: "var(--text-dim)" };
  }
}

/**
 * 从 `名字 <邮箱>` 里取显示名。
 *
 * 团队成员的身份就是 git 提交里的那一串，界面上一律只显示名字 —— 邮箱在
 * 悬浮提示里给。邮箱全露出来会让一行字长得不像人名。
 */
export function displayName(identity: string | null | undefined): string {
  if (!identity) return "";
  const at = identity.indexOf("<");
  return (at > 0 ? identity.slice(0, at) : identity).trim() || identity.trim();
}

/** 同一个人在两台机器上配的邮箱大小写可能不一样，比较时统一小写。 */
export function samePerson(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false;
  return a.trim().toLowerCase() === b.trim().toLowerCase();
}

/**
 * 头像色：从邮箱哈希出一个稳定的色相。
 *
 * 不用随机数、不用顺序索引 —— 同一个人在任何地方、任何时候都该是同一个颜色，
 * 否则「那个蓝色的谁」这种说法在团队里就失效了。
 */
export function avatarHue(identity: string | null | undefined): number {
  const s = (identity ?? "").trim().toLowerCase();
  let h = 0;
  for (let i = 0; i < s.length; i++) {
    h = (h * 31 + s.charCodeAt(i)) % 360;
  }
  return h;
}

/** 相对时间。团队成员看的是「三小时前」，不是「2026-11-03 09:12」。 */
export function since(ms: number | null | undefined, now = Date.now()): string {
  if (!ms) return "";
  const d = now - ms;
  if (d < 0) return "刚刚";
  const min = Math.floor(d / 60_000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h} 小时前`;
  const days = Math.floor(h / 24);
  if (days < 30) return `${days} 天前`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months} 个月前`;
  return `${Math.floor(months / 12)} 年前`;
}

/**
 * 一个任务在画图上属于哪一「层」：沿着 `depends_on` 往上走，最长的链就是层号。
 *
 * 这是把依赖图排成横向泳道的关键 —— 没有它，所有任务挤在一列，箭头全是交叉线。
 * 用记忆化避免 O(n²)，并**守住环**（数据被手改坏时不能把界面卡死）。
 */
export function layerize(tasks: TeamTask[]): Map<string, number> {
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const memo = new Map<string, number>();
  const limit = tasks.length + 1;

  function depth(id: string, seen: Set<string>): number {
    const hit = memo.get(id);
    if (hit !== undefined) return hit;
    if (seen.has(id) || seen.size > limit) return 0;
    const t = byId.get(id);
    if (!t) return 0;
    seen.add(id);
    let d = 0;
    for (const dep of t.depends_on) {
      d = Math.max(d, depth(dep, seen) + 1);
    }
    seen.delete(id);
    memo.set(id, d);
    return d;
  }

  for (const t of tasks) depth(t.id, new Set());
  return memo;
}

// ---------------------------------------------------------------------------
// 成员
// ---------------------------------------------------------------------------

/**
 * 把一个「人」的引用解析成成员。任务里的 `assignee` 可能是成员 id（v1）、也可能是
 * `名字 <邮箱>`（v0 留下的），两种都认 —— 与后端 `Roster::resolve` 同一套规则。
 */
export function findMember(
  people: TeamPeople | null,
  raw: string | null | undefined,
): TeamMember | null {
  if (!people || !raw) return null;
  const r = raw.trim();
  if (r.startsWith("m-")) return people.members.find((m) => m.id === r) ?? null;
  const email = /<([^>]+)>/.exec(r)?.[1] ?? (r.includes("@") ? r : null);
  if (email) {
    const e = email.trim().toLowerCase();
    return people.members.find((m) => m.emails.some((x) => x.toLowerCase() === e)) ?? null;
  }
  return null;
}

/** 给人看的名字：成员的昵称，认不出就从 `名字 <邮箱>` 里取名字。 */
export function personName(people: TeamPeople | null, raw: string | null | undefined): string {
  if (!raw) return "";
  return findMember(people, raw)?.name ?? displayName(raw);
}

/** 头像配色用的稳定键：成员用 id，认不出的用原串。同一个人换了邮箱颜色不变。 */
export function personKey(people: TeamPeople | null, raw: string | null | undefined): string {
  return findMember(people, raw)?.id ?? raw ?? "";
}

/** 这个引用是不是本机这个人。 */
export function isMe(people: TeamPeople | null, raw: string | null | undefined): boolean {
  if (!people?.me || !raw) return false;
  return findMember(people, raw)?.id === people.me || raw === people.me;
}

/** 事件是谁发的：v1 看 `from`，v0 按作者邮箱认。返回成员 id 或 `名字 <邮箱>`。 */
export function eventSender(e: TeamEvent): string | null {
  if (e.from) return e.from;
  return e.author ? `${e.author.name} <${e.author.email}>` : null;
}

/** 事件种类 → 动词。不认识的原样给出去（协议：不认识的种类也要显示）。 */
export function eventVerb(kind: string): string {
  return (
    (
      {
        comment: "评论",
        mention: "提到",
        broadcast: "广播",
        request: "请求",
        assign: "指派",
        handoff: "转交",
        claim: "认领",
        release: "释放",
        join: "加入了团队",
        note: "更新进展",
        sync: "同步",
      } as Record<string, string>
    )[kind] ?? kind
  );
}

/**
 * 正文里的 `@昵称` → 成员 id。发消息时用：协议规定收件人只看 `to=`，不从正文里猜，
 * 所以这一步在发送前做，用户看到的是 `@小张`，发出去的是 id。
 *
 * 昵称里可能有空格以外的任何字符，按名单里的名字**从长到短**去匹配，避免「小张」
 * 把「小张三」截成两半。重名的不解析（不知道是哪个）。
 */
export function mentionsIn(text: string, people: TeamPeople | null): string[] {
  if (!people) return [];
  const counts = new Map<string, number>();
  for (const m of people.members) counts.set(m.name, (counts.get(m.name) ?? 0) + 1);
  const named = people.members
    .filter((m) => counts.get(m.name) === 1)
    .sort((a, b) => b.name.length - a.name.length);
  const out = new Set<string>();
  let rest = text;
  for (const m of named) {
    const tag = `@${m.name}`;
    if (rest.includes(tag)) {
      out.add(m.id);
      rest = rest.split(tag).join(" ");
    }
  }
  return [...out];
}
