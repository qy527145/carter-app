// 预制上下文：「交给卡特」这类操作带的一大段说明，人只需要看到一枚小卡片。
//
// 它存在消息文本里，而不是另开字段：会话文件只有纯文本，重开、重试、分叉拿到的都是
// 这段文本，只有写进文本才能在这些路径上都认得回来。模型看到的就是标签里的正文。

export type PromptContextKind = "task" | "goal" | "board";

export type PromptContext = {
  kind: PromptContextKind;
  title: string;
  body: string;
};

const BLOCK = /<carter-context kind="(task|goal|board)" title="([^"]*)">\n?([\s\S]*?)\n?<\/carter-context>/;
const TAGS = /<\/?carter-context\b[^>]*>\n?/g;

export const CONTEXT_LABEL: Record<PromptContextKind, string> = {
  task: "任务",
  goal: "目标",
  board: "看板",
};

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

/** 把文本拆成上下文卡片和其余的话。没有上下文时 `rest` 原样返回。 */
export function parseContext(text: string): { context: PromptContext | null; rest: string } {
  const m = BLOCK.exec(text);
  if (!m) return { context: null, rest: text };
  return {
    context: { kind: m[1] as PromptContextKind, title: unescapeAttr(m[2]), body: m[3] },
    rest: (text.slice(0, m.index) + text.slice(m.index + m[0].length)).trim(),
  };
}

/** 去掉标签、留下正文：复制出去给人读，或搜索片段里用。 */
export function stripContextTags(text: string): string {
  return text.replace(TAGS, "");
}

/** 一行摘要：排队条、飞行中的卡片这类只放得下一行的地方。 */
export function contextSummary(text: string): string {
  const { context, rest } = parseContext(text);
  const line = rest.split("\n")[0];
  if (!context) return line;
  return `${CONTEXT_LABEL[context.kind]} · ${context.title}${line ? `  ${line}` : ""}`;
}
