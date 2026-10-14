//! 应用状态：会话列表 + 当前对话 + 一轮的实时增量。
//!
//! 用 zustand 而不是 Context：流式期间 `text_delta` 每几十毫秒来一次，每次都要改
//! 状态。Context 会把整棵树重渲染；zustand 的选择器订阅能把重渲染压到真正用到
//! 那段状态的那几个组件上。
//!
//! ## 为什么 text 与 thinking 是**并列的 item**，不是一个字段
//!
//! 模型会交错输出思考与正文（想一段、写一段、再想一段）。如果把它们塞进同一个
//! 字符串，界面上就只能二选一显示，或者把思考混进正文里。拆成按到达顺序排列的
//! item 列表，渲染层可以「思考折叠成 details、正文渲染成 markdown」，两者都保序。
//! `text_end` 到来时把当前 text item 标记 `closed`，下一个 `text_delta` 才会另起一条 ——
//! 这是让「正文 → 思考 → 正文」不会粘成一段的关键。

import { create } from "zustand";

import type {
  AppInfo,
  BlockView,
  CreatedSession,
  MessageView,
  SessionEvent,
  SessionSummary,
  TodoView,
  UsageView,
} from "./lib/api";

/** 助手轮里的一个片段。 */
export type Item =
  | { kind: "text"; text: string; closed: boolean }
  | { kind: "thinking"; text: string; hasSignature?: boolean }
  | {
      kind: "tool";
      callId: string;
      name: string;
      argsPreview: string;
      result?: { ok: boolean; summary: string };
    }
  | { kind: "server"; rawType: string; summary: string }
  | { kind: "image"; mediaType: string; hash: string | null }
  | { kind: "note"; text: string };

export type Turn =
  | { role: "user"; text: string }
  | { role: "assistant"; items: Item[] };

export type PendingAsk = { id: number; question: string; options: string[] };

type Store = {
  // 会话
  sessionId: string | null;
  model: string;
  cwd: string;
  // 对话
  turns: Turn[];
  streaming: boolean;
  usage: UsageView | null;
  cost: number;
  todos: TodoView[];
  pendingAsk: PendingAsk | null;
  notice: string | null;
  // 侧栏 / 底栏用
  sessions: SessionSummary[];
  appVersion: string;

  setAppInfo: (i: AppInfo) => void;
  setSessions: (s: SessionSummary[]) => void;
  beginSession: (c: CreatedSession) => void;
  resetSession: () => void;
  addUserTurn: (text: string) => void;
  applyEvent: (e: SessionEvent) => void;
  setNotice: (m: string | null) => void;
  clearPendingAsk: () => void;
};

// ---------------------------------------------------------------------------
// 纯函数：turns 的增量更新。全部返回新数组/新对象，保证 zustand 能看出变化。
// ---------------------------------------------------------------------------

/** 取末尾的助手轮；末尾不是助手轮就新开一个空的。 */
function withAssistant(turns: Turn[], fn: (items: Item[]) => Item[]): Turn[] {
  const last = turns[turns.length - 1];
  if (last && last.role === "assistant") {
    return [...turns.slice(0, -1), { role: "assistant", items: fn(last.items) }];
  }
  return [...turns, { role: "assistant", items: fn([]) }];
}

/** 往末尾的 text item 追加；它已 closed 或不存在就新起一条。 */
function appendText(items: Item[], text: string): Item[] {
  const last = items[items.length - 1];
  if (last && last.kind === "text" && !last.closed) {
    return [...items.slice(0, -1), { ...last, text: last.text + text }];
  }
  return [...items, { kind: "text", text, closed: false }];
}

/** 思考增量：与 text 分开攒，互不吞并。 */
function appendThinking(items: Item[], text: string): Item[] {
  const last = items[items.length - 1];
  if (last && last.kind === "thinking") {
    return [...items.slice(0, -1), { ...last, text: last.text + text }];
  }
  return [...items, { kind: "thinking", text }];
}

/** 收束末尾的 text item，让后续增量另起一条。 */
function closeText(items: Item[]): Item[] {
  const last = items[items.length - 1];
  if (last && last.kind === "text" && !last.closed) {
    return [...items.slice(0, -1), { ...last, closed: true }];
  }
  return items;
}

/**
 * 按 `call_id` 回填工具结果。
 *
 * 不从末尾盲取「最后一个没结果的工具卡片」：并发工具调用时结果可能乱序到达
 * （尤其服务端工具），只有 id 配对才可信。
 */
function fillToolResult(
  turns: Turn[],
  callId: string,
  result: { ok: boolean; summary: string },
): Turn[] {
  for (let i = turns.length - 1; i >= 0; i--) {
    const t = turns[i];
    if (t.role !== "assistant") continue;
    const j = t.items.findIndex((it) => it.kind === "tool" && it.callId === callId);
    if (j >= 0) {
      const items = t.items.slice();
      const tool = items[j] as Extract<Item, { kind: "tool" }>;
      items[j] = { ...tool, result };
      const next = turns.slice();
      next[i] = { role: "assistant", items };
      return next;
    }
  }
  return turns;
}

/** 把多行工具结果压成一行摘要，供卡片展示。 */
function firstLine(s: string, max = 160): string {
  const line = (s.split("\n", 1)[0] ?? "").trim();
  return line.length > max ? line.slice(0, max) + "…" : line;
}

// ---------------------------------------------------------------------------
// 历史 → turns（供 create_session 返回的 history 使用）
// ---------------------------------------------------------------------------

function blockToItem(b: BlockView): Item | null {
  switch (b.kind) {
    case "text":
      return { kind: "text", text: b.text, closed: true };
    case "thinking":
      return { kind: "thinking", text: b.text, hasSignature: b.has_signature };
    case "tool_call":
      return { kind: "tool", callId: b.id, name: b.name, argsPreview: b.args };
    case "server_activity":
      return { kind: "server", rawType: b.raw_type, summary: b.summary };
    case "image":
      return { kind: "image", mediaType: b.media_type, hash: b.hash };
    // 工具结果的归属由所在 user 消息处理，这里不单独成项。
    case "tool_result":
      return null;
  }
}

function blockText(blocks: BlockView[]): string {
  return blocks
    .filter((b): b is Extract<BlockView, { kind: "text" }> => b.kind === "text")
    .map((b) => b.text)
    .join("\n")
    .trim();
}

/**
 * 历史消息 → UI 的轮次。
 *
 * 底座把「多条 tool_result」合成**一条 user 消息**（Anthropic 的规范形态），
 * 所以 user 消息里可能夹着工具结果块 —— 那些要回填到前面助手轮的工具卡片上，
 * 而不是渲染成一个右对齐的用户气泡。
 */
export function historyToTurns(history: MessageView[]): Turn[] {
  let turns: Turn[] = [];
  for (const msg of history) {
    if (msg.role === "user") {
      for (const b of msg.blocks) {
        if (b.kind === "tool_result") {
          turns = fillToolResult(turns, b.tool_use_id, {
            ok: !b.is_error,
            summary: firstLine(b.text),
          });
        }
      }
      const text = blockText(msg.blocks);
      if (text) turns.push({ role: "user", text });
      continue;
    }
    if (msg.role === "system") {
      const text = blockText(msg.blocks);
      if (text) turns.push({ role: "assistant", items: [{ kind: "note", text }] });
      continue;
    }
    const items = msg.blocks
      .map(blockToItem)
      .filter((x): x is Item => x !== null);
    if (items.length) turns.push({ role: "assistant", items });
  }
  return turns;
}

// ---------------------------------------------------------------------------
// store
// ---------------------------------------------------------------------------

const emptyConversation = {
  turns: [] as Turn[],
  streaming: false,
  usage: null as UsageView | null,
  cost: 0,
  todos: [] as TodoView[],
  pendingAsk: null as PendingAsk | null,
  notice: null as string | null,
};

export const useStore = create<Store>((set) => ({
  sessionId: null,
  model: "",
  cwd: "",
  ...emptyConversation,
  sessions: [],
  appVersion: "",

  setAppInfo: (i) => set({ appVersion: i.version }),

  setSessions: (sessions) => set({ sessions }),

  beginSession: (c) =>
    set({
      sessionId: c.session_id,
      model: c.model,
      cwd: c.cwd,
      ...emptyConversation,
      turns: historyToTurns(c.history),
    }),

  resetSession: () => set({ sessionId: null, ...emptyConversation }),

  // 用户消息在本地先落一条：事件流里没有「用户发言」事件，唯一的来源就是提交动作。
  addUserTurn: (text) => set((s) => ({ turns: [...s.turns, { role: "user", text }] })),

  setNotice: (notice) => set({ notice }),

  clearPendingAsk: () => set({ pendingAsk: null }),

  applyEvent: (e) =>
    set((s) => {
      switch (e.type) {
        case "ready":
          return { model: e.model, cwd: e.cwd };
        case "turn_started":
          return {
            streaming: true,
            // 末尾已经是空的助手轮就复用，否则新开一个 —— 不能复用带内容的助手轮，
            // 那会把上一轮的回答抹掉。
            turns:
              s.turns[s.turns.length - 1]?.role === "user"
                ? [...s.turns, { role: "assistant", items: [] }]
                : s.turns,
          };
        case "text_delta":
          return { turns: withAssistant(s.turns, (items) => appendText(items, e.text)) };
        case "thinking_delta":
          return { turns: withAssistant(s.turns, (items) => appendThinking(items, e.text)) };
        case "text_end":
          return { turns: withAssistant(s.turns, closeText) };
        case "tool_call_started":
          return {
            turns: withAssistant(s.turns, (items) => [
              ...closeText(items),
              {
                kind: "tool",
                callId: e.call_id,
                name: e.name,
                argsPreview: e.args_preview,
              },
            ]),
          };
        case "tool_result":
          return {
            turns: fillToolResult(s.turns, e.call_id, { ok: e.ok, summary: e.summary }),
          };
        case "todo_updated":
          return { todos: e.todos };
        case "ask_user":
          return { pendingAsk: { id: e.id, question: e.question, options: e.options } };
        case "turn_usage":
          return { usage: e.usage, cost: e.cost, model: e.model };
        case "idle":
          // 一轮收尾：解除生成态，并清掉可能还悬着的提问（模型已不再等）。
          return { streaming: false, pendingAsk: null };
        case "notice":
          return { notice: e.message };
        case "error":
          return { notice: e.message };
      }
    }),
}));
