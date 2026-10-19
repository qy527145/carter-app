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
//!
//! ## 为什么主对话与侧边聊天共用一份 reducer
//!
//! 侧边聊天（`/btw`）是同时跑着的**另一条**对话，它的事件与主对话走同一条广播，
//! 只是 `session_id` 不同（见 `lib/events.ts`）。两条对话的事件词汇完全一样，
//! 所以状态形状与处理逻辑也应该是同一份：`Conv` + `reduceConv`。差别只在
//! 「放在哪儿」——主对话摊在 store 顶层（`turns` / `streaming` / …），侧边聊天挂在
//! `side` 上。
//!
//! 顶层那一份**刻意不顺带改成 `conv` 子对象**：那样要动五个组件的选择器，
//! 而收益只是对称好看一点。

import { create } from "zustand";

import type {
  AppInfo,
  AppSettings,
  AskQuestion,
  BlockView,
  CreatedSession,
  CreatedSide,
  GoalView,
  MessageView,
  SessionEvent,
  SessionRow,
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
      /** **完整的**参数对象。差异化渲染靠它（见 components/toolcards）。 */
      args: unknown;
      result?: { ok: boolean; content: string; elapsedMs: number };
    }
  | { kind: "server"; rawType: string; summary: string }
  | { kind: "image"; mediaType: string; hash: string | null }
  | { kind: "note"; text: string };

export type Turn =
  | { role: "user"; text: string; auto?: boolean }
  // `turn` 来自 `turn_started`，是「这条回答对应哪一轮请求」的钥匙 —— 报文面板
  // 靠它把助手消息筛到那次 API 调用。历史消息没有这个数（事件不回放），为 undefined。
  // `auto` = 这一轮是长任务目标自动推起来的（用户没说话），界面上画一条分隔而不是
  // 假装用户又发了一条。
  | { role: "assistant"; turn?: number; items: Item[]; auto?: boolean };

/** 模型提问，等用户填完一次性提交。 */
export type PendingForm = { id: number; questions: AskQuestion[] };

/**
 * 一条对话的全部状态。
 *
 * 主对话摊在 store 顶层，侧边聊天整包挂在 `side` 上 —— 两边用同一个 `reduceConv`。
 */
export type Conv = {
  turns: Turn[];
  streaming: boolean;
  usage: UsageView | null;
  cost: number;
  todos: TodoView[];
  pendingForm: PendingForm | null;
  notice: string | null;
  /** 长任务目标（`/goal`）。`null` = 这条对话没有目标。 */
  goal: GoalView | null;
};

/** 侧边聊天：一份 `Conv` + 它是谁（事件按 session_id 路由，见 `lib/events.ts`）。 */
export type SideConv = Conv & {
  sessionId: string;
  model: string;
  cwd: string;
  /** 从主对话继承下来的上下文（折叠着显示，默认不展开）。 */
  inheritedTurns: Turn[];
};

type Store = Conv & {
  // 会话
  sessionId: string | null;
  model: string;
  cwd: string;
  /** 侧边聊天（`/btw`）。同时只开一个，`null` = 没开。 */
  side: SideConv | null;
  // 报文面板：open 控制显示，turn 是「只看这一轮」的筛选（null = 全部）。
  inspectorOpen: boolean;
  inspectorTurn: number | null;
  // 侧栏 / 底栏用
  sessions: SessionRow[];
  appInfo: AppInfo | null;
  /** 界面设置（主题、工具开关、侧栏宽度）。carter CLI 不读它。 */
  settings: AppSettings | null;
  /**
   * 配置被改过几次。设置页把它放进 useEffect 的依赖里，就能在
   * 「改完渠道 → 模型页跟着刷新」这类联动上自动重拉数据。
   */
  configNonce: number;
  /** 主区显示哪个视图。默认对话。 */
  view: "chat" | "settings";

  setAppInfo: (i: AppInfo) => void;
  setSettings: (s: AppSettings) => void;
  bumpConfig: () => void;
  setSessions: (s: SessionRow[]) => void;
  beginSession: (c: CreatedSession) => void;
  resetSession: () => void;
  addUserTurn: (text: string) => void;
  applyEvent: (e: SessionEvent) => void;
  setNotice: (m: string | null) => void;
  clearPendingForm: () => void;
  openInspector: (turn?: number | null) => void;
  closeInspector: () => void;
  setInspectorTurn: (turn: number | null) => void;
  setView: (v: "chat" | "settings") => void;
  // 侧边聊天
  beginSide: (c: CreatedSide) => void;
  closeSide: () => void;
  addSideUserTurn: (text: string) => void;
  applySideEvent: (e: SessionEvent) => void;
  setSideNotice: (m: string | null) => void;
};

// ---------------------------------------------------------------------------
// 纯函数：turns 的增量更新。全部返回新数组/新对象，保证 zustand 能看出变化。
// ---------------------------------------------------------------------------

/** 取末尾的助手轮；末尾不是助手轮就新开一个空的。 */
function withAssistant(turns: Turn[], fn: (items: Item[]) => Item[]): Turn[] {
  const last = turns[turns.length - 1];
  if (last && last.role === "assistant") {
    // 带上 turn 与 auto，否则每次增量都会把「这条回答属于哪一轮」丢掉。
    return [
      ...turns.slice(0, -1),
      { role: "assistant", turn: last.turn, auto: last.auto, items: fn(last.items) },
    ];
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
  result: { ok: boolean; content: string; elapsedMs: number },
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
      next[i] = { role: "assistant", turn: t.turn, auto: t.auto, items };
      return next;
    }
  }
  return turns;
}

/** 事件 → 一条对话状态的增量。主对话与侧边聊天共用这一份。 */
export function reduceConv(c: Conv, e: SessionEvent): Partial<Conv> {
  switch (e.type) {
    case "turn_started": {
      const last = c.turns[c.turns.length - 1];
      // 末尾已经是**同号**的助手轮 → 重复事件，复用。
      if (last && last.role === "assistant" && last.turn === e.turn) {
        return { streaming: true, turns: c.turns };
      }
      // 末尾是**另一个**助手轮 → 用户没说这句话，是长任务目标在推着模型走
      // （自动继续）。另起一条并标明，别让它跟上一轮粘成一大坨 ——
      // 粘在一起就分不清哪一段是模型自己停下来的。
      if (last && last.role === "assistant") {
        return {
          streaming: true,
          turns: [...c.turns, { role: "assistant", turn: e.turn, items: [], auto: true }],
        };
      }
      return {
        streaming: true,
        turns: [...c.turns, { role: "assistant", turn: e.turn, items: [] }],
      };
    }
    case "text_delta":
      return { turns: withAssistant(c.turns, (items) => appendText(items, e.text)) };
    case "thinking_delta":
      return { turns: withAssistant(c.turns, (items) => appendThinking(items, e.text)) };
    case "text_end":
      return { turns: withAssistant(c.turns, closeText) };
    case "tool_started":
      return {
        turns: withAssistant(c.turns, (items) => [
          ...closeText(items),
          {
            kind: "tool",
            callId: e.call_id,
            name: e.name,
            args: e.args,
          },
        ]),
      };
    case "tool_finished":
      return {
        turns: fillToolResult(c.turns, e.call_id, {
          ok: e.ok,
          content: e.content,
          elapsedMs: e.elapsed_ms,
        }),
      };
    case "todo_updated":
      return { todos: e.todos };
    case "ask_form":
      return { pendingForm: { id: e.id, questions: e.questions } };
    case "turn_usage":
      return { usage: e.usage, cost: e.cost };
    case "goal":
      return { goal: e.goal };
    case "idle":
      // 一轮收尾：解除生成态，并清掉可能还悬着的提问（模型已不再等）。
      return { streaming: false, pendingForm: null };
    case "notice":
      return { notice: e.message };
    case "error":
      return { notice: e.message };
    case "ready":
      // 模型与工作目录在 `ready` 里，但它们属于「会话是谁」而不是「对话内容」，
      // 由调用方处理（见 `applyEvent` / `beginSide`）。
      return {};
  }
}

/**
 * 历史里的工具参数是**紧凑 JSON 字符串**（保留模型原样的分片拼接结果）。
 * 卡片需要对象才能按字段取值，所以这里解析一次；解析不了就原样留着字符串，
 * 让兜底卡片把它显示出来 —— 坏数据不该让整条消息渲染不出来。
 */
function parseArgs(raw: string): unknown {
  if (!raw.trim()) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
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
      return { kind: "tool", callId: b.id, name: b.name, args: parseArgs(b.args) };
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
 * 长任务目标自动继续时，后端往历史里追加的那条用户消息的开头。
 *
 * 与 `crates/carter-app-core/src/goal.rs::CONTINUE_PROMPT` 逐字对应 —— 那边改了
 * 这里必须跟着改（否则回看历史时会冒出一个像是用户自己发的、莫名其妙的方块）。
 */
const AUTO_CONTINUE_PREFIX = "（自动继续）";

/**
 * 历史消息 → UI 的轮次。
 *
 * 底座把「多条 tool_result」合成**一条 user 消息**（Anthropic 的规范形态），
 * 所以 user 消息里可能夹着工具结果块 —— 那些要回填到前面助手轮的工具卡片上，
 * 而不是渲染成一个右对齐的用户气泡。
 */
export function historyToTurns(history: MessageView[]): Turn[] {
  let turns: Turn[] = [];
  // 上一条用户消息是「自动继续」的注入 → 接下来的助手轮是目标模式推起来的。
  let pendingAuto = false;
  for (const msg of history) {
    if (msg.role === "user") {
      for (const b of msg.blocks) {
        if (b.kind === "tool_result") {
          // 历史里没有耗时（事件不回放），给 0 —— 卡片会跳过 0 的展示。
          turns = fillToolResult(turns, b.tool_use_id, {
            ok: !b.is_error,
            content: b.text,
            elapsedMs: 0,
          });
        }
      }
      const text = blockText(msg.blocks);
      if (!text) continue;
      // 自动继续那条是应用注入的，不是用户打的字 —— 不画成用户气泡，
      // 只在下一轮上打一个记号（见 `AssistantTurn` 里那条分隔线）。
      if (text.startsWith(AUTO_CONTINUE_PREFIX)) {
        pendingAuto = true;
        continue;
      }
      turns.push({ role: "user", text });
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
    if (items.length) {
      // `turn` 由后端算好（见 `view.rs`）—— 界面上「这条回答对应哪次请求」全靠它，
      // 少了它历史里的每条回答都没有「查看报文」入口。
      turns.push({ role: "assistant", items, turn: msg.turn, auto: pendingAuto || undefined });
      pendingAuto = false;
    }
  }
  return turns;
}

// ---------------------------------------------------------------------------
// store
// ---------------------------------------------------------------------------

const emptyConversation: Conv = {
  turns: [],
  streaming: false,
  usage: null,
  cost: 0,
  todos: [],
  pendingForm: null,
  notice: null,
  goal: null,
};

/** 主对话那部分状态（`Conv` 的字段藏在 store 里，还原出来给 reducer 用）。 */
function convOf(s: Conv): Conv {
  return {
    turns: s.turns,
    streaming: s.streaming,
    usage: s.usage,
    cost: s.cost,
    todos: s.todos,
    pendingForm: s.pendingForm,
    notice: s.notice,
    goal: s.goal,
  };
}

export const useStore = create<Store>((set) => ({
  sessionId: null,
  model: "",
  cwd: "",
  ...emptyConversation,
  side: null,
  // 报文面板：换会话时一起收起来（`emptyConversation` 里没有它们 —— 它们不是
  // 对话内容，是「界面上开着哪块面板」）。
  inspectorOpen: false,
  inspectorTurn: null,
  sessions: [],
  appInfo: null,
  settings: null,
  configNonce: 0,
  view: "chat",

  // 应用信息里带着设置，一起落地 —— 分开拉会让主题在首帧闪一下。
  setAppInfo: (i) => set({ appInfo: i, settings: i.settings }),

  setSettings: (settings) => set({ settings }),

  bumpConfig: () => set((s) => ({ configNonce: s.configNonce + 1 })),

  setSessions: (sessions) => set({ sessions }),

  beginSession: (c) =>
    set({
      sessionId: c.session_id,
      model: c.model,
      cwd: c.cwd,
      ...emptyConversation,
      goal: c.goal,
      turns: historyToTurns(c.history),
    }),

  // 换会话时侧边聊天一起收掉：它继承的是**上一条**主对话的上下文，
  // 留着它只会让人以为它还在跟着当前这条对话。
  resetSession: () => set({ sessionId: null, ...emptyConversation, side: null }),

  // 用户消息在本地先落一条：事件流里没有「用户发言」事件，唯一的来源就是提交动作。
  addUserTurn: (text) => set((s) => ({ turns: [...s.turns, { role: "user", text }] })),

  setNotice: (notice) => set({ notice }),

  clearPendingForm: () => set({ pendingForm: null }),

  openInspector: (turn = null) => set({ inspectorOpen: true, inspectorTurn: turn }),
  closeInspector: () => set({ inspectorOpen: false }),
  setInspectorTurn: (turn) => set({ inspectorTurn: turn }),
  setView: (view) => set({ view }),

  applyEvent: (e) =>
    set((s) => {
      // `ready` 带着「会话是谁」，由这里接住；其余交给共用的 reducer。
      if (e.type === "ready") return { model: e.model, cwd: e.cwd };
      if (e.type === "turn_usage") {
        return { ...reduceConv(convOf(s), e), model: e.model };
      }
      return reduceConv(convOf(s), e);
    }),

  // --- 侧边聊天 -------------------------------------------------------------

  beginSide: (c) =>
    set({
      side: {
        sessionId: c.session_id,
        model: c.model,
        cwd: c.cwd,
        ...emptyConversation,
        inheritedTurns: historyToTurns(c.inherited),
      },
    }),

  closeSide: () => set({ side: null }),

  addSideUserTurn: (text) =>
    set((s) =>
      s.side
        ? { side: { ...s.side, turns: [...s.side.turns, { role: "user", text }] } }
        : {},
    ),

  applySideEvent: (e) =>
    set((s) => {
      if (!s.side) return {};
      if (e.type === "ready") {
        return { side: { ...s.side, model: e.model, cwd: e.cwd } };
      }
      const next: SideConv = { ...s.side, ...reduceConv(s.side, e) };
      if (e.type === "turn_usage") next.model = e.model;
      return { side: next };
    }),

  setSideNotice: (notice) =>
    set((s) => (s.side ? { side: { ...s.side, notice } } : {})),
}));
