// 预制上下文：「交给卡特」这类操作带的一大段说明，人只需要看到一枚小卡片。
//
// 它存在消息文本里，而不是另开字段：会话文件只有纯文本，重开、重试、分叉拿到的都是
// 这段文本，只有写进文本才能在这些路径上都认得回来。模型看到的就是标签里的正文。
//
// ## 为什么是**一组**而不是一张
//
// 「我的任务」里挑几件事一起交给卡特，是最自然的用法：同一个目标下拆出来的几件事，
// 本来就该一起说。早期实现只认第一块标签（`/…/` 不带 `g`），于是界面上只能挂一张卡，
// 多选被悄悄折成单选。现在解析、格式化、摘要都按数组走，界面上的卡片也排成一列。

export type PromptContextKind = "task" | "goal" | "board";

export type PromptContext = {
  kind: PromptContextKind;
  title: string;
  body: string;
  /**
   * 稳定标识（如 `task:t-abc`）。**只在本机内存里用**，不写进标签、不发给模型。
   *
   * 有了它，「这张卡片在不在输入框里」就能按 id 判断，而不是按正文比较 ——
   * 待办的正文里带着「当前状态」这类随时会变的东西，任务一被标成进行中，
   * 按正文算的勾选就悄悄对不上了：卡片还在输入框里，勾却掉了。
   *
   * 从文本解析回来的卡片没有这个字段（那时也不需要它：解析出来的是已经发出去的
   * 消息，没有「勾选状态」这回事）。
   */
  ref?: string;
};

/** 标签体。属性顺序不固定（模型或旧版本可能写反），逐个取而不是一条正则吃掉。 */
const BLOCK_SRC = "<carter-context\\b([^>]*)>([\\s\\S]*?)<\\/carter-context>";
const BLOCKS = new RegExp(BLOCK_SRC, "g");
const TAGS = /<\/?carter-context\b[^>]*>\n?/g;
const ATTR = (attrs: string, name: string): string | null => {
  const m = new RegExp(`\\b${name}="([^"]*)"`).exec(attrs);
  return m ? unescapeAttr(m[1]) : null;
};

export const CONTEXT_LABEL: Record<PromptContextKind, string> = {
  task: "任务",
  goal: "目标",
  board: "看板",
};

/**
 * 同一张卡片的身份。
 *
 * 有 `ref` 就用它 —— 任务正文里的状态、受阻原因都会变，按正文算身份的话，
 * 任务一推进，输入框里那张卡就和列表里的这条对不上了。没有 `ref` 的
 * （从历史消息解析回来的）才退回按内容比。
 */
export function contextKey(c: PromptContext): string {
  if (c.ref) return `${c.kind}\u0000${c.ref}`;
  return `${c.kind}\u0000${c.title}\u0000${c.body}`;
}

function escapeAttr(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\n/g, " ");
}

function unescapeAttr(s: string): string {
  return s
    .replace(/&gt;/g, ">")
    .replace(/&lt;/g, "<")
    .replace(/&quot;/g, '"')
    .replace(/&amp;/g, "&");
}

export function formatContext(c: PromptContext): string {
  return `<carter-context kind="${c.kind}" title="${escapeAttr(c.title)}">\n${c.body}\n</carter-context>`;
}

/**
 * 把文本拆成上下文卡片和其余的话。
 *
 * 卡片可以有好几张，顺序就是摆进输入框的顺序 —— 一起交给卡特时，先后本身就是
 * 用户表达的意思。没有上下文时 `rest` 原样返回。
 */
export function parseContexts(text: string): { contexts: PromptContext[]; rest: string } {
  const contexts: PromptContext[] = [];
  let rest = "";
  let cursor = 0;
  for (const m of text.matchAll(BLOCKS)) {
    const kind = ATTR(m[1], "kind");
    if (kind !== "task" && kind !== "goal" && kind !== "board") continue;
    contexts.push({ kind, title: ATTR(m[1], "title") ?? "", body: m[2].trim() });
    // 按**位置**切掉，而不是 `replace(原文, "")`：后者遇到两张一模一样的卡片时
    // 只会删掉第一处，剩下那张留在正文里，屏幕上就多了一段谁也读不懂的标签。
    rest += text.slice(cursor, m.index);
    cursor = m.index + m[0].length;
  }
  if (contexts.length === 0) return { contexts, rest: text };
  rest += text.slice(cursor);
  return { contexts, rest: rest.trim() };
}

/** 去掉标签、留下正文：复制出去给人读，或搜索片段里用。 */
export function stripContextTags(text: string): string {
  return text.replace(TAGS, "");
}

/** 一行摘要：排队条、飞行中的卡片这类只放得下一行的地方。 */
export function contextSummary(text: string): string {
  const { contexts, rest } = parseContexts(text);
  const line = rest.split("\n")[0];
  if (contexts.length === 0) return line;
  const head =
    contexts.length === 1
      ? `${CONTEXT_LABEL[contexts[0].kind]} · ${contexts[0].title}`
      : `${contexts.length} 项 · ${contexts[0].title}`;
  return `${head}${line ? `  ${line}` : ""}`;
}
