import { invoke } from "@tauri-apps/api/core";
import type { AskQuestion, Command, GoalView, MessageView, TodoView } from "./types";

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

/** 历史里某一轮实际改过的文件。 */
export type TurnFiles = {
  turn: number;
  /** 相对会话 cwd，`/` 分隔。 */
  files: string[];
};

/** 历史里某一轮用户**打的原文**（`/命令`、`@文件` 展开之前）。 */
export type TurnPrompt = {
  turn: number;
  text: string;
};

/**
 * 某一轮的起止时刻（epoch 毫秒）。
 *
 * 与会话文件里的 `ts` 不是一回事：消息本身不带时刻，那层信息折叠成消息时就丢了。
 * 所以后端按轮另存一份（见 core 的 `timeline.rs`）。老会话没有这份记录 ——
 * 界面上那几轮就不显示时间，别的什么都不影响。
 */
export type TurnTime = {
  turn: number;
  /** 用户发问的时刻。 */
  started_at: number;
  /** 这一轮跑完的时刻。`null` = 还在跑（或没跑完就被中断了）。 */
  ended_at: number | null;
  /** 这一轮是助理记进来的工单消息时，它属于哪张工单。与 core 的 `timeline::TurnTime` 对应。 */
  report?: ReportTag | null;
};

/**
 * 一条汇报属于哪张工单。后端记录时按工单本身填好（core 的 `timeline::ReportTag`），
 * 界面**只认它**，不从正文里解析任何东西。
 */
export type ReportTag = {
  ticket_id: string;
  /** 记这条时工单的执行会话。还没开工的工单没有。 */
  session_id?: string | null;
  /** 这条在等用户回答（只有它给回答框）；事后记的一笔（已答复、已结束……）为 false。 */
  ask: boolean;
  /**
   * 这条是**派单**（「派了一张工单去做这件事」），不是汇报。
   *
   * 界面据此换个图标：助理对话是用户唯一的入口，派出去干活的每一张工单都得在这里
   * 留一条点得过去的记录，不能只有出事时才浮上来。
   */
  dispatch?: boolean;
};

export type CreatedSession = {
  session_id: string;
  /** 真正发给上游的模型名。 */
  model: string;
  /** 模型引用 `渠道/显示名`。界面上的模型标签一律显示这一种写法。 */
  model_ref?: string;
  cwd: string;
  history: MessageView[];
  /** 这条会话现有的长任务目标。 */
  goal: GoalView | null;
  /** 历史里每一轮实际改过的文件。事件不回放，所以随历史一起带回。 */
  changes: TurnFiles[];
  /** 变更条的统计起点（点过「清空」之后只汇总这一轮及之后的）。 */
  changes_since?: number;
  /**
   * 历史里每一轮用户打的原文。界面拿它回填「编辑」框 ——
   * 会话文件里存的是展开后的正文，回填进去等于把整份文件内容摊在输入框里。
   */
  prompts?: TurnPrompt[];
  /** 历史里每一轮的起止时刻。界面在消息旁边显示时间。 */
  times?: TurnTime[];
  /** 接上的是一条正在跑的会话：显示成「生成中」，后续增量照常从事件来。 */
  running?: boolean;
  /** 正在跑的那一轮已经产出的正文与工具调用。 */
  live?: LiveView[];
  /** 这一轮正在等用户回答的问题。`ask_form` 事件不回放，错过了靠它把表单补出来。 */
  pending_ask?: { id: number; questions: AskQuestion[] } | null;
  /** 这条会话现在的 todo 列表。事件不回放，接上后台那条时靠它把底栏的进度补回来。 */
  todos?: TodoView[];
  /**
   * 这一轮里已经跑完的工具（结果已定）。工具事件不回放，接回时按 `call_id` 把结果
   * 补进卡片 —— 否则最后那张卡会一直停在「运行中」。
   */
  finished_tools?: ToolUsageView[];
  /**
   * 这条会话此刻还在跑的后台命令。事件不回放，切走再接上时靠它把「还有后台任务
   * 在跑」那条提醒补回来。
   */
  background_shells?: BackgroundShellView[];
};

/** 一条还在跑的后台命令（`bash` 的 `run_in_background`）。 */
export type BackgroundShellView = {
  /** `shell_id`（`bash` 返回的那个）。 */
  id: string;
  /** 起它时的命令行原文。界面截断显示，完整内容放在 `title` 里。 */
  command: string;
  /** 开始的时刻（epoch 毫秒）。界面据此显示「已运行多久」。 */
  started_at: number;
};

/** 一条已经跑完的工具调用（见 `CreatedSession.finished_tools`）。 */
export type ToolUsageView = {
  call_id: string;
  name: string;
  ok: boolean;
  /** 截断过的展示文本（与 `tool_finished` 的 `content` 同一口径）。 */
  result: string;
  elapsed_ms: number;
};

export type LiveView =
  | { kind: "text"; text: string }
  | { kind: "user"; text: string }
  | { kind: "tool"; call_id: string; name: string; args: string; result: string | null };

/** 侧边聊天（`/btw`）：继承主会话上下文、不落盘、关掉即丢。 */
export type CreatedSide = {
  session_id: string;
  model: string;
  /** 见 `CreatedSession.model_ref`。 */
  model_ref?: string;
  cwd: string;
  /** 继承下来的历史（打开那一刻的快照；模型那边每轮现取最新的）。 */
  inherited: MessageView[];
};

export type SessionRow = {
  id: string;
  title: string | null;
  model: string;
  cwd: string;
  /** epoch 毫秒。 */
  created_at: number;
  forked_from: string | null;
  /** 最后一次写入的时间（文件 mtime，epoch 毫秒）。项目按它排「最近」。 */
  updated_at: number;
  bytes: number;
  /** 会话记的工作目录现在还在不在（项目目录被删掉后会话文件仍然列得出来）。 */
  cwd_exists: boolean;
};

export type SearchHit = {
  id: string;
  title: string | null;
  model: string;
  cwd: string;
  created_at: number;
  /** 最后写入时间。与列表同一口径 —— 显示的「多久以前」按它算。 */
  updated_at: number;
  snippet: string;
  matches: number;
};

/**
 * 新建或续接一个会话，历史一并带回。
 *
 * `resumeId` 优先于 `resume`：给了 id 就按 id 打开那个历史会话。**若该 id 在磁盘上
 * 已不存在，后端不会报错，而是退回新建**（返回的 `session_id` 会是个新 id）——
 * 调用方要比较返回值与请求的 id，自己判断是不是「目标会话没了」。
 */
export function createSession(args: {
  cwd?: string | null;
  model?: string | null;
  resume: boolean;
  resumeId?: string | null;
}): Promise<CreatedSession> {
  return invoke<CreatedSession>("create_session", {
    cwd: args.cwd ?? null,
    model: args.model ?? null,
    resume: args.resume,
    resumeId: args.resumeId ?? null,
  });
}

/**
 * 打开（或续接）常驻的助理对话：固定 id，盘上有历史就接着聊，没有就新建。
 * 返回值与 `createSession` 一样，直接交给 `beginSession`。
 */
/** 界面换走了这条会话：空闲就关，正在跑就让它跑完再关。 */
export function detachSession(sessionId: string): Promise<void> {
  return invoke<void>("detach_session", { sessionId });
}

export function openAssistantChat(model?: string | null): Promise<CreatedSession> {
  return invoke<CreatedSession>("open_assistant_chat", { model: model ?? null });
}

/**
 * 往会话投递一条命令。
 *
 * 会话已死（窗口重开、任务退出）时 invoke 会 reject 一个字符串 —— 调用方要
 * 提示用户重建会话，而不是把消息发进黑洞。
 */
export function sendCommand(sessionId: string, command: Command): Promise<void> {
  return invoke<void>("send_command", { sessionId, command });
}

/** 一条候选记忆。字段与 `save_memory` 的参数同名同义。 */
export type MemoryCandidate = {
  content: string;
  kind: "facts" | "profile";
  /** `profile` 时恒为 `user`。 */
  scope: "project" | "user";
};

/** 把用户确认过的一条候选写进记忆。项目级的写进 `cwd/CARTER.md`。返回给用户看的一句话。 */
export function acceptMemory(cwd: string, candidate: MemoryCandidate): Promise<string> {
  return invoke<string>("accept_memory", { cwd, candidate });
}

export function closeSession(sessionId: string): Promise<void> {
  return invoke<void>("close_session", { sessionId });
}

/**
 * 开一个侧边聊天（`/btw`）。
 *
 * 后端起一条**不落盘**的会话：继承主会话的历史作为参考，关掉就没了。
 * 一个主会话同时只允许一个，重复调用会 reject 一句说明。
 */
export function createSideSession(parentSessionId: string): Promise<CreatedSide> {
  return invoke<CreatedSide>("create_side_session", { parentSessionId });
}

/**
 * 列举会话。`all = false` 只看 `cwd`（不给就是默认目录 —— 家目录），`true` 跨所有项目。
 *
 * 侧栏要传当前会话的 cwd：桌面端没有「进程工作目录」这回事，不传就永远只列家目录
 * 那一堆，切到别的项目之后侧栏里看到的还是原来那些会话。
 */
export function listSessions(all = false, cwd?: string): Promise<SessionRow[]> {
  return invoke<SessionRow[]>("list_sessions", { all, cwd: cwd ?? null });
}

/** 此刻正在跑一轮的会话 id（侧栏的「生成中」点，不只是当前打开的那条）。 */
export function busySessions(): Promise<string[]> {
  return invoke<string[]>("busy_sessions");
}

export function renameSession(sessionId: string, title: string): Promise<void> {
  return invoke<void>("rename_session", { sessionId, title });
}

/** fork 一条会话，返回新会话的 id。 */
/**
 * fork 一条会话，返回新会话的 id。
 *
 * `keepBefore` = 只保留第几轮**之前**的历史（不含该轮）。从某条回答之后分叉传
 * `turn + 1`；改写某句提问再问传 `turn`。不给 = 整条复制。
 */
export function forkSession(sessionId: string, keepBefore?: number): Promise<string> {
  return invoke<string>("fork_session", {
    sessionId,
    keepBefore: keepBefore ?? null,
  });
}

/** 删除一个会话文件。不可恢复 —— 调用方必须先确认。 */
export function deleteSession(sessionId: string): Promise<void> {
  return invoke<void>("delete_session", { sessionId });
}

/** 删除某个目录下的全部会话，返回删掉的条数。不碰目录本身。 */
export function deleteProject(cwd: string): Promise<number> {
  return invoke<number>("delete_project", { cwd });
}

export function searchSessions(query: string): Promise<SearchHit[]> {
  return invoke<SearchHit[]>("search_sessions", { query });
}

/** 导出到下载目录，返回写出的绝对路径。 */
export function exportSession(sessionId: string, format: "markdown" | "json"): Promise<string> {
  return invoke<string>("export_session", { sessionId, format });
}
