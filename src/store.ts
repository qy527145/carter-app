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
  ContextView,
  GoalView,
  MessageView,
  SessionEvent,
  SessionRow,
  AgentRun,
  TodoView,
  TurnFiles,
  TurnPrompt,
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
      /**
       * 只有 `agent` 工具有：它派出去的那几个子 agent 的实时状态。
       * 整表替换，见后端 `AgentProgress`。
       */
      agents?: AgentRun[];
      /**
       * 这条调用属于哪条会话。卡片上的「中断命令 / 转后台」要按它回传 ——
       * 侧边聊天的卡片不能用主对话的 id 去查。
       *
       * 历史消息里没有（那些早就跑完了，也没有按钮可点）。
       */
      sessionId?: string;
    }
  | { kind: "server"; rawType: string; summary: string }
  | { kind: "image"; mediaType: string; hash: string | null }
  | { kind: "note"; text: string };

export type Turn =
  // `turn` 是**这一轮提问**的序号（0 起）。用户气泡本身也要它 —— 回答下面的
  // 「重试 / 编辑」与提问下面的「编辑」都要指名改哪一轮。自动继续那几轮不是用户
  // 说的，没有气泡，也就没有这个号。
  | {
      role: "user";
      text: string;
      /**
       * 用户**打的原文**（`/命令`、`@文件` 展开之前）。「编辑」框用它回填。
       *
       * 实时那几轮来自 `turn_started.prompt`，历史那几轮来自后端按轮存的
       * `prompts`（见 `prompts.rs`）。没有它时（这份存档出现之前录下的会话）回填
       * 退回 `text` —— 历史里存的是展开后的正文，可能带着整份文件内容。
       */
      raw?: string;
      turn?: number;
      /** 自动继续注入的那句「继续」：不画成用户气泡，但历史里确实算一轮。 */
      auto?: boolean;
    }
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
  /**
   * 每一轮**实际改过**的文件（轮次号 → 相对 cwd 的路径）。
   *
   * 来源是后端的文件系统监听（见 core 的 `changes.rs`），不是解析工具参数。
   * 历史里那几轮随 `create_session` 带回，实时那几轮走 `files_changed` 事件。
   */
  changed: Record<number, string[]>;
  /** 上下文占用（输入框旁的百分比环）。`null` = 后端还没报过。 */
  context: ContextView | null;
  /** 正在压缩上下文（环换成转圈）。 */
  compacting: boolean;
};

/**
 * 一条已经发给后端、但还没轮到它跑的消息。
 *
 * 它**不是**对话内容，所以不进 `turns`：后端真正开始处理它时会发
 * `turn_started { prompt_id }`，那时才把气泡插进对话里。提前插会让正在流式输出的
 * 那一轮被这笔用户消息截断成两段（助手轮是按「末尾」累积的）。
 */
export type QueuedPrompt = { id: number; text: string };

/** 侧边聊天：一份 `Conv` + 它是谁（事件按 session_id 路由，见 `lib/events.ts`）。 */
export type SideConv = Conv & {
  sessionId: string;
  /** 真正发给上游的模型名（事件里带的就是它）。 */
  model: string;
  /** 模型引用 `渠道/块名`。界面上的模型标签用它 —— 见 `lib/modelLabel.ts`。 */
  modelRef: string;
  cwd: string;
  /** 从主对话继承下来的上下文（折叠着显示，默认不展开）。 */
  inheritedTurns: Turn[];
};

type Store = Conv & {
  // 会话
  sessionId: string | null;
  /** 真正发给上游的模型名。 */
  model: string;
  /** 模型引用 `渠道/块名`，界面上的模型标签用这一种写法（见 `lib/modelLabel.ts`）。 */
  modelRef: string;
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
  /**
   * 会话列表被改过几次（新建 / 改名 / fork / 删除）。
   *
   * 侧栏与「会话」设置页各有一份列表（都跨项目，设置页另有全文搜索），改完一边
   * 要让另一边也跟着更新。两边都把 `refreshSessions` 作为唯一的写入口，那里把它 +1。
   */
  sessionsNonce: number;
  /**
   * 已发出、还没开始跑的消息（后端排队中的那些）。
   *
   * 「生成中还能继续打字」靠它：发出去就进这里，`turn_started` 带着 `prompt_id`
   * 回来时挪进 `turns`。换会话、重建会话都会清空。
   */
  queued: QueuedPrompt[];

  setAppInfo: (i: AppInfo) => void;
  setSettings: (s: AppSettings) => void;
  bumpConfig: () => void;
  setSessions: (s: SessionRow[]) => void;
  /** 把某条会话的 `updated_at` 记成现在（它刚被写过）。不碰 `sessionsNonce`。 */
  touchSession: (id: string) => void;
  beginSession: (c: CreatedSession) => void;
  resetSession: () => void;
  addUserTurn: (text: string) => void;
  applyEvent: (e: SessionEvent) => void;
  /** 记下一条已发出、等后端开始处理的消息。 */
  enqueuePrompt: (p: QueuedPrompt) => void;
  /** 从排队里拿掉（被撤回、或已经开始跑了）。 */
  removeQueuedPrompt: (id: number) => void;
  /** 会话重建之后把排队原样放回去（顺序不变）。 */
  restoreQueued: (list: QueuedPrompt[]) => void;
  /** 后端明确失败时的兜底：别让界面永远停在「正在生成」。 */
  clearStreaming: () => void;
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

/**
 * 第 `turn` 轮回答在列表里的起点（下标）；没有就是 -1。
 *
 * 一次用户输入可能对应**多条**助手轮（工具往返各一条，见 `historyToTurns`），
 * 所以要从后往前找第一条 `turn` 相同的助手轮 —— 它们总是连在一起。
 */
function lastAssistantIndexOfTurn(turns: Turn[], turn: number): number {
  for (let i = turns.length - 1; i >= 0; i--) {
    const t = turns[i];
    if (t.role === "assistant" && t.turn === turn) {
      let j = i;
      while (j > 0) {
        const prev = turns[j - 1];
        if (prev.role !== "assistant" || prev.turn !== turn) break;
        j -= 1;
      }
      return j;
    }
  }
  return -1;
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

/**
 * 事件 → 一条对话状态的增量。主对话与侧边聊天共用这一份。
 *
 * `sessionId` 只用于给新出现的工具卡片打上归属（见 `Item` 的 tool 变体）。
 */
export function reduceConv(c: Conv, e: SessionEvent, sessionId?: string): Partial<Conv> {
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
            sessionId,
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
    case "agent_progress":
      return {
        turns: withAssistant(c.turns, (items) =>
          items.map((it) =>
            it.kind === "tool" && it.callId === e.call_id ? { ...it, agents: e.agents } : it,
          ),
        ),
      };
    case "todo_updated":
      return { todos: e.todos };
    case "ask_form":
      return { pendingForm: { id: e.id, questions: e.questions } };
    case "files_changed":
      // 轮末一次性到达。空清单后端就不发了，这里只负责合并。
      return { changed: { ...c.changed, [e.turn]: e.files } };
    case "turn_retracted": {
      // 重试：把这一轮的回答及之后的轮次从列表里拿掉，只留那条用户气泡。
      //
      // 后端回退历史时保留了这一轮的用户消息（重跑要它），而紧接着的
      // `turn_started` 不带 `prompt`，不会再画一遍 —— 所以这里**不能**连用户气泡
      // 一起删，否则重跑时界面上那句话就没了。
      const i = lastAssistantIndexOfTurn(c.turns, e.turn);
      const turns = i < 0 ? c.turns : c.turns.slice(0, i);
      // 被回退掉的那几轮的变更清单也要一起丢掉，否则它们会挂在已经不存在的回答下面。
      const changed: Record<number, string[]> = {};
      for (const [k, v] of Object.entries(c.changed)) {
        if (Number(k) < e.turn) changed[Number(k)] = v;
      }
      return { turns, changed };
    }
    case "turn_usage":
      return { usage: e.usage, cost: e.cost };
    case "goal":
      return { goal: e.goal };
    case "idle":
      // 一轮收尾：解除生成态，并清掉可能还悬着的提问（模型已不再等）。
      return { streaming: false, pendingForm: null, compacting: false };
    case "context": {
      // 压缩收尾时后端总会补发一次占用 —— 摘要没生成出来时没有 `compacted`，
      // 转圈靠这里收。
      const { type: _, ...context } = e;
      return { context, compacting: false };
    }
    case "compact_started":
      return { compacting: true };
    case "compacted": {
      // 历史被整体替换：与打开一条历史会话同一条路重建。正在跑的那一轮（一轮中途
      // 被 carter 自己压过）已经整个在新历史里了，所以不用拼接，直接换掉。
      //
      // 摘要那条消息由 `historyToTurns` 标出来（续接时同样要认得出），这里只补一句
      // 前后对比放进提示条。
      const what = e.auto ? "上下文接近上限，已自动压缩" : "上下文已压缩";
      return {
        turns: historyToTurns(e.history, e.prompts),
        changed: changedRecord(e.changes),
        compacting: false,
        notice: `${what}（${e.tier} 摘要）：历史约 ${e.before.toLocaleString()} → ${e.after.toLocaleString()} tokens`,
      };
    }
    case "notice":
      return { notice: e.message };
    case "title":
      // 标题不是对话内容，是会话列表里那一行的属性 —— 由事件层去重拉列表
      // （见 `lib/events.ts`）。这里什么都不用改。
      return {};
    case "error":
      return { notice: e.message };
    case "ready":
      // 模型与工作目录在 `ready` 里，但它们属于「会话是谁」而不是「对话内容」，
      // 由调用方处理（见 `applyEvent` / `beginSide`）。
      return {};
    case "model_changed":
      // 同上：模型属于「会话是谁」，由 `applyEvent` 落进 store 顶层。
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
 * 上下文压缩后，carter 换进历史的那条摘要消息的开头（`【历史摘要·L3】`）。
 *
 * 与 carter 的 `agent/context.rs::compact` 逐字对应。认出来是为了在它前面挂一句
 * 说明 —— 否则它看上去就像模型某一轮莫名其妙地自己写了一大段总结。
 */
const SUMMARY_PREFIX = "【历史摘要·";

/**
 * 历史消息 → UI 的轮次。
 *
 * 底座把「多条 tool_result」合成**一条 user 消息**（Anthropic 的规范形态），
 * 所以 user 消息里可能夹着工具结果块 —— 那些要回填到前面助手轮的工具卡片上，
 * 而不是渲染成一个右对齐的用户气泡。
 *
 * `prompts` 是后端按轮存下的**用户原文**（`/命令`、`@文件` 展开之前）。历史里
 * 那一段是展开后的正文，直接拿来回填「编辑」框等于把整份文件摊进去 —— 查得到
 * 原文就挂在 `raw` 上，查不到时「编辑」退回气泡里显示的那段（见 `UserTurn`）。
 */
export function historyToTurns(
  history: MessageView[],
  prompts: TurnPrompt[] = [],
): Turn[] {
  const rawOf = new Map(prompts.map((p) => [p.turn, p.text]));
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
      turns.push({
        role: "user",
        text,
        turn: msg.turn,
        raw: msg.turn !== undefined ? rawOf.get(msg.turn) : undefined,
      });
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
    const first = items[0];
    if (first?.kind === "text" && first.text.startsWith(SUMMARY_PREFIX)) {
      items.unshift({ kind: "note", text: "较早的对话已压缩成下面这段摘要，模型此后看到的就是它。" });
    }
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

/**
 * 让列表里一定有「当前这条会话」。
 *
 * carter 的会话文件是**惰性创建**的（`Recorder` 等到第一条消息才建文件），所以刚
 * 点「新对话」时它还没落盘、不在 `list_sessions` 的结果里 —— 界面上就表现为
 * 「左边什么都没多」。可它确实是当前开着的对话，只是还没说过话。
 *
 * 于是补一行临时的：标题为 null（界面上显示「新对话」），等第一条消息写下去之后，
 * 下一次刷新自然会用磁盘上那份把它换掉 —— 那时通常也已经有自动标题了。
 */
export function withCurrent(
  list: SessionRow[],
  cur: { id: string | null; model: string; cwd: string },
): SessionRow[] {
  if (!cur.id || list.some((s) => s.id === cur.id)) return list;
  return [
    {
      id: cur.id,
      // 与顶部标题一致地显示成「新对话」（真名由第一轮的自动标题补上）。
      title: "新对话",
      model: cur.model,
      cwd: cur.cwd,
      created_at: Date.now(),
      forked_from: null,
      updated_at: Date.now(),
      bytes: 0,
      // 刚开的会话，目录刚被后端校验过。
      cwd_exists: true,
    },
    ...list,
  ];
}

const emptyConversation: Conv = {
  turns: [],
  streaming: false,
  usage: null,
  cost: 0,
  todos: [],
  pendingForm: null,
  notice: null,
  goal: null,
  changed: {},
  context: null,
  compacting: false,
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
    changed: s.changed,
    context: s.context,
    compacting: s.compacting,
  };
}

/** 历史里的按轮清单 → 界面用的映射。 */
function changedRecord(list: TurnFiles[]): Record<number, string[]> {
  const out: Record<number, string[]> = {};
  for (const t of list) out[t.turn] = t.files;
  return out;
}

export const useStore = create<Store>((set) => ({
  sessionId: null,
  model: "",
  modelRef: "",
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
  queued: [],
  sessionsNonce: 0,

  // 应用信息里带着设置，一起落地 —— 分开拉会让主题在首帧闪一下。
  setAppInfo: (i) => set({ appInfo: i, settings: i.settings }),

  setSettings: (settings) => set({ settings }),

  bumpConfig: () => set((s) => ({ configNonce: s.configNonce + 1 })),

  setSessions: (sessions) => set((s) => ({ sessions, sessionsNonce: s.sessionsNonce + 1 })),

  touchSession: (id) =>
    set((s) => ({
      sessions: s.sessions.map((r) => (r.id === id ? { ...r, updated_at: Date.now() } : r)),
    })),

  beginSession: (c) =>
    set((s) => ({
      sessionId: c.session_id,
      model: c.model,
      modelRef: c.model_ref ?? c.model,
      cwd: c.cwd,
      ...emptyConversation,
      // 换/重建会话时排队清空：那些消息属于原来那条会话任务，
      // 留着它们会让界面显示一批永远不会开始跑的「排队中」。
      queued: [],
      goal: c.goal,
      turns: historyToTurns(c.history, c.prompts),
      changed: changedRecord(c.changes),
      sessions: withCurrent(s.sessions, {
        id: c.session_id,
        model: c.model,
        cwd: c.cwd,
      }),
    })),

  // 换会话时侧边聊天一起收掉：它继承的是**上一条**主对话的上下文，
  // 留着它只会让人以为它还在跟着当前这条对话。
  //
  // 报文面板同样要收起来：它开着的时候选中的是**上一条**会话的报文，留着它，
  // 面板会拿那个 id 去新会话里查 —— 查不到，于是弹一句「该报文已不存在」，
  // 而用户只是开了个新对话。
  resetSession: () =>
    set({
      sessionId: null,
      ...emptyConversation,
      queued: [],
      side: null,
      inspectorOpen: false,
      inspectorTurn: null,
    }),

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
      if (e.type === "ready") {
        return { model: e.model, modelRef: e.model_ref ?? e.model, cwd: e.cwd };
      }
      // 换模型只动顶层那个「当前模型」标签，不碰对话内容。
      if (e.type === "model_changed") {
        return { model: e.model, modelRef: e.model_ref ?? e.model };
      }
      if (e.type === "turn_started" && (e.prompt_id != null || e.prompt != null)) {
        const q = e.prompt_id != null ? s.queued.find((x) => x.id === e.prompt_id) : undefined;
        const queued =
          e.prompt_id != null ? s.queued.filter((x) => x.id !== e.prompt_id) : s.queued;
        // 轮到这条消息了：先把用户气泡插进去，再按 reducer 开助手轮。
        // 顺序很重要 —— 反过来的话回答会压在用户那句话上面。
        //
        // 文本优先取排队表里的那份（它与用户点发送时看到的一模一样），
        // 表里没有就退回后端带回来的原文：界面重连过、或「撤回」晚了一拍时，
        // 靠它仍然能画出用户那句话。
        const text = q?.text ?? e.prompt ?? null;
        // 用户气泡也带上轮次号（提问下面的「编辑」要靠它指名改哪一轮），以及
        // **用户打的原文**（编辑框回填要用它，不是展开后的正文）。
        //
        // 但**重跑/编辑**那一轮的气泡已经在界面上了（`turn_retracted` 交代过），
        // 这时不能再插一条 —— 判据是「最后一条已经是同一轮的用户轮」。
        const last = s.turns[s.turns.length - 1];
        const reuse = last?.role === "user" && last.turn === e.turn;
        const conv =
          text != null && !reuse
            ? {
                ...convOf(s),
                turns: [
                  ...s.turns,
                  {
                    role: "user" as const,
                    text,
                    raw: e.prompt ?? undefined,
                    turn: e.turn,
                  },
                ],
              }
            : convOf(s);
        return { ...reduceConv(conv, e, s.sessionId ?? undefined), queued };
      }
      // `turn_usage` 只带实际请求名（carter 的 `emit_usage` 手上只有它），
      // **不覆盖** `modelRef`：那会让「渠道/显示名」在每轮结束时退回成裸名字。
      if (e.type === "turn_usage") {
        return { ...reduceConv(convOf(s), e, s.sessionId ?? undefined), model: e.model };
      }
      return reduceConv(convOf(s), e, s.sessionId ?? undefined);
    }),

  enqueuePrompt: (p) =>
    set((s) =>
      s.queued.some((x) => x.id === p.id) ? {} : { queued: [...s.queued, p] },
    ),

  removeQueuedPrompt: (id) =>
    set((s) => (s.queued.some((x) => x.id === id)
      ? { queued: s.queued.filter((x) => x.id !== id) }
      : {})),

  restoreQueued: (list) => set({ queued: list }),

  clearStreaming: () => set({ streaming: false }),

  // --- 侧边聊天 -------------------------------------------------------------

  beginSide: (c) =>
    set({
      side: {
        sessionId: c.session_id,
        model: c.model,
        modelRef: c.model_ref ?? c.model,
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
        return {
          side: {
            ...s.side,
            model: e.model,
            modelRef: e.model_ref ?? e.model,
            cwd: e.cwd,
          },
        };
      }
      if (e.type === "model_changed") {
        return {
          side: {
            ...s.side,
            model: e.model,
            modelRef: e.model_ref ?? e.model,
          },
        };
      }
      const next: SideConv = {
        ...s.side,
        ...reduceConv(s.side, e, s.side.sessionId),
      };
      if (e.type === "turn_usage") next.model = e.model;
      return { side: next };
    }),

  setSideNotice: (notice) =>
    set((s) => (s.side ? { side: { ...s.side, notice } } : {})),
}));
