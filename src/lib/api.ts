//! App ↔ 前端的 IPC 契约（TypeScript 侧）。
//!
//! 这是 `crates/carter-app-core/src/protocol.rs` 与 `view.rs` 的逐字段镜像 ——
//! Rust 侧改了这里就必须同步改，反之亦然。两侧**不**做 camelCase 转换：
//! serde 原样透传 `snake_case` 的枚举 tag 与字段名，所以下面的类型里全是下划线。

import { invoke } from "@tauri-apps/api/core";

/// 后端广播事件用的通道名。
///
/// 这是字符串字面量，改名会**静默**破坏订阅（前端再也收不到任何事件，也不报错）。
/// 与 `src-tauri/src/lib.rs` 的 `CARTER_EVENT` 必须一致。
export const EVENT_NAME = "carter://event";

/** 一个会话内的事件流。顺序即语义：按到达顺序把增量攒进同一个助手气泡。 */
export type SessionEvent =
  | { type: "ready"; session_id: string; model: string; cwd: string; resumed: boolean }
  | { type: "turn_started"; turn: number }
  | { type: "thinking_delta"; text: string }
  | { type: "text_delta"; text: string }
  | { type: "text_end" }
  | { type: "tool_call_started"; call_id: string; name: string; args_preview: string }
  | { type: "tool_result"; call_id: string; ok: boolean; summary: string }
  | { type: "todo_updated"; todos: TodoView[] }
  | { type: "ask_user"; id: number; question: string; options: string[] }
  | { type: "turn_usage"; usage: UsageView; cost: number; model: string }
  | { type: "notice"; message: string }
  | { type: "idle" }
  | { type: "error"; message: string };

/** 后端 → 前端的总线载荷。 */
export type AppEvent =
  | { type: "session"; session_id: string; event: SessionEvent }
  | { type: "error"; message: string };

export type TodoView = {
  content: string;
  active_form: string;
  status: "pending" | "in_progress" | "completed";
};

/** 用量视图。`input_tokens` 是**未命中缓存**的输入，缓存单列 —— 界面要显示命中情况。 */
export type UsageView = {
  input_tokens: number;
  output_tokens: number;
  thinking_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
};

/** 历史消息块。`image` 只带类型与 hash，**不背字节**。 */
export type BlockView =
  | { kind: "text"; text: string }
  | { kind: "thinking"; text: string; has_signature: boolean }
  | { kind: "tool_call"; id: string; name: string; args: string } // args = 紧凑 JSON 字符串
  | { kind: "tool_result"; tool_use_id: string; text: string; is_error: boolean }
  | { kind: "server_activity"; raw_type: string; summary: string } // 厂商在服务端执行的活动
  | { kind: "image"; media_type: string; hash: string | null };

export type MessageView = { role: "user" | "assistant" | "system"; blocks: BlockView[] };

/** 前端 → 后端。 */
export type Command =
  | { type: "send_prompt"; text: string }
  | { type: "cancel" }
  | { type: "set_model"; model: string }
  | { type: "answer_ask"; id: number; answer: string };

export type AppInfo = {
  name: string;
  version: string;
  default_cwd: string;
  default_model: string;
};

export type CreatedSession = {
  session_id: string;
  model: string;
  cwd: string;
  history: MessageView[];
};

export type SessionSummary = {
  id: string;
  title: string | null;
  model: string;
  cwd: string;
  created_at: number;
};

export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

/**
 * 新建或续接一个会话，历史一并带回。
 *
 * 注意参数键用 **camelCase**：Tauri v2 会自动把它转成 Rust 的 `snake_case`
 * （`resumeId` → `resume_id`），而 `cwd`/`model`/`resumeId` 为 `null` 时后端才走默认值。
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
 * 往会话投递一条命令。
 *
 * 会话已死（窗口重开、任务退出）时 invoke 会 reject 一个字符串 —— 调用方要
 * 提示用户重建会话，而不是把消息发进黑洞。
 */
export function sendCommand(sessionId: string, command: Command): Promise<void> {
  return invoke<void>("send_command", { sessionId, command });
}

export function closeSession(sessionId: string): Promise<void> {
  return invoke<void>("close_session", { sessionId });
}

export function listSessions(cwd?: string | null): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>("list_sessions", { cwd: cwd ?? null });
}

// ---------------------------------------------------------------------------
// 报文检视（需求 4）：抓到的原始 HTTP 交换
// ---------------------------------------------------------------------------

/** 这次模型调用是干什么用的。 */
export type RequestPurpose = "main_turn" | "compaction" | "title" | "subagent";

/**
 * 列表项 —— 只给概览。完整记录里带着几 MB 的 SSE 帧，列表里全带上会让界面一开就卡。
 *
 * ⚠️ 后端 `ExchangeSummary.purpose` 是 `format!("{:?}")` 出来的，**是 PascalCase**
 * （`MainTurn`），而 `ExchangeRecord.meta.purpose` 走 serde 是 snake_case
 * （`main_turn`）—— 两边对不上。所以这里不信任字面值，统一用 [`purposeOf`] 归一。
 */
export type ExchangeSummary = {
  id: string;
  ts: number;
  turn: number | null;
  round: number | null;
  purpose: RequestPurpose | null;
  method: string;
  url: string;
  status: number | null;
  ttfb_ms: number | null;
  total_ms: number | null;
  frame_count: number;
};

export type ExchangeRecord = {
  id: string;
  ts: number;
  meta: {
    session_id: string | null;
    turn: number;
    round: number;
    purpose: RequestPurpose;
  } | null;
  method: string;
  url: string;
  /** 敏感头已在后端脱敏（authorization / x-api-key → ***），前端不要再动。 */
  request_headers: [string, string][];
  request_body: string; // 原始请求体（最终字节）
  status: number | null;
  response_headers: [string, string][];
  response_body: string; // 非流式的响应正文
  stream_frames: string[]; // 流式响应收到的原始字节块，按到达顺序
  stream_error: string | null;
  ttfb_ms: number | null;
  total_ms: number | null;
};

/**
 * 把两种拼写（`MainTurn` / `main_turn`）都归一成小写蛇形。
 *
 * 见 `ExchangeSummary` 上的说明：列表与详情两条路的序列化方式不同，界面不能只认
 * 一种。认不出来的值原样返回，让界面至少能把原文显示出来而不是显示 `undefined`。
 */
export function purposeOf(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const key = raw.replace(/_/g, "").toLowerCase();
  switch (key) {
    case "mainturn":
      return "main_turn";
    case "compaction":
      return "compaction";
    case "title":
      return "title";
    case "subagent":
      return "subagent";
    default:
      return raw;
  }
}

/** 用途的中文标签；主对话不显示标签（默认情形，标出来只是噪声）。 */
export function purposeLabel(raw: string | null | undefined): string | null {
  switch (purposeOf(raw)) {
    case "compaction":
      return "压缩";
    case "title":
      return "标题";
    case "subagent":
      return "子 agent";
    case "main_turn":
      return null;
    default:
      return raw ?? null;
  }
}

export function listExchanges(sessionId: string): Promise<ExchangeSummary[]> {
  return invoke<ExchangeSummary[]>("list_exchanges", { sessionId });
}

export function getExchange(
  sessionId: string,
  exchangeId: string,
): Promise<ExchangeRecord | null> {
  return invoke<ExchangeRecord | null>("get_exchange", { sessionId, exchangeId });
}

// ---------------------------------------------------------------------------
// 管理 / 检视（M7，只读）
// ---------------------------------------------------------------------------

/** 一个已发现的技能（`~/.carter/skills/<name>/SKILL.md`）。 */
export type SkillRow = {
  name: string;
  description: string;
  allowed_tools: string[];
};

export type McpServerRow = {
  name: string;
  transport: string;
  /** stdio 给 command，http 给 url。 */
  target: string;
  enabled: boolean;
};

export type ChannelRow = {
  name: string;
  /** 配置里写的 kind，可能是别名或兼容写法。 */
  kind: string;
  /** 实际走的线协议 —— 与 kind 不同时说明走了别名映射。 */
  protocol: string;
  base_url: string;
  has_api_key: boolean;
  /** 该渠道声明了、但其协议不认的服务端工具（会被静默忽略）。 */
  unsupported_server_tools: string[];
  models: string[];
  is_default: boolean;
};

/** 本地会执行的工具。服务端工具（web_search 等）在渠道里，不在这儿。 */
export type ToolRow = { name: string; description: string; parameters: unknown };

export function listSkills(): Promise<SkillRow[]> {
  return invoke<SkillRow[]>("list_skills");
}

export function listMcpServers(): Promise<McpServerRow[]> {
  return invoke<McpServerRow[]>("list_mcp_servers");
}

export function listChannels(): Promise<ChannelRow[]> {
  return invoke<ChannelRow[]>("list_channels");
}

export function listTools(): Promise<ToolRow[]> {
  return invoke<ToolRow[]>("list_tools");
}

/** 删除一个会话文件。不可恢复 —— 调用方必须先确认。 */
export function deleteSession(sessionId: string, cwd?: string | null): Promise<void> {
  return invoke<void>("delete_session", { cwd: cwd ?? null, sessionId });
}
