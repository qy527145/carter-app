import { invoke } from "@tauri-apps/api/core";
import type { BlockView, MessageView, UsageView } from "./types";

// ---------------------------------------------------------------------------
// 报文检视（需求 4）：抓到的原始 HTTP 交换
// ---------------------------------------------------------------------------

/** 这次模型调用是干什么用的。 */
export type RequestPurpose = "main_turn" | "compaction" | "title" | "subagent";

export type ExchangeSummary = {
  id: string;
  ts: number;
  turn: number | null;
  round: number | null;
  purpose: RequestPurpose | null;
  method: string;
  url: string;
  /** 上游渠道名；老记录没有。 */
  channel: string | null;
  /** 本地模型引用（`provider/model`）；老记录没有。 */
  model: string | null;
  status: number | null;
  ttfb_ms: number | null;
  total_ms: number | null;
  frame_count: number;
  /** 还在进行中 —— 请求已发出，可以从此刻起就显示。 */
  in_flight: boolean;
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
  /** 这次交换说的线协议（`anthropic` / `openai_chat` / `openai_responses`）；老记录为 null。 */
  protocol: string | null;
  /** 上游渠道名（`[providers.X]` 的 X）；老记录为 null。 */
  channel: string | null;
  /** 本地模型引用（`provider/model`）；老记录为 null。 */
  model: string | null;
  /** 敏感头已在后端脱敏（authorization / x-api-key → ***），前端不要再动。 */
  request_headers: [string, string][];
  request_body: string;
  status: number | null;
  response_headers: [string, string][];
  response_body: string;
  stream_frames: string[];
  stream_error: string | null;
  ttfb_ms: number | null;
  total_ms: number | null;
};

/** 用途的中文标签；主对话不显示标签（默认情形，标出来只是噪声）。 */
export function purposeLabel(raw: string | null | undefined): string | null {
  switch (raw) {
    case "compaction":
      return "压缩";
    case "title":
      return "标题";
    case "subagent":
      return "子 agent";
    default:
      return null;
  }
}

export function listExchanges(sessionId: string): Promise<ExchangeSummary[]> {
  return invoke<ExchangeSummary[]>("list_exchanges", { sessionId });
}

// --- 报文的语义视图（「可视化」那一页） ----------------------------------

/**
 * 一条交换解回 IR 之后的样子。
 *
 * 后端用**与真实链路同一套 codec** 解（`crates/carter-app-core/src/inspect.rs`），
 * 所以这里显示的就是模型实际收到/说出的内容 —— 而不是对原始字节的另一套猜测。
 * 逐帧的碎片只留在「原始」页。
 */
export type InspectedExchange = {
  protocol: string;
  /** 协议是按 URL 猜的（老记录里没存协议）。界面上要说明，别当成权威。 */
  protocol_guessed: boolean;
  request: InspectedRequest | null;
  response: InspectedResponse | null;
  stream: StreamStats;
  /** 解不开的原因。有值时界面降级到「原始」页，并说明为什么。 */
  decode_error: string | null;
};

export type InspectedRequest = {
  model: string;
  stream: boolean;
  params: { key: string; value: string }[];
  system: string;
  /** 与历史消息同一个形状，所以两边可以共用一套渲染。 */
  messages: MessageView[];
  tools: InspectedTool[];
  /** 没建模的顶层字段名（值在「格式化」页里看）。 */
  extra_keys: string[];
};

export type InspectedTool = {
  name: string;
  description: string | null;
  /** JSON Schema 的美化文本。 */
  schema: string;
  /** 厂商自己执行的工具（`web_search` 之类），不是我们能跑的函数。 */
  server_side: boolean;
};

export type InspectedResponse = {
  blocks: BlockView[];
  /** 收尾原因（已翻成人话）。 */
  finish: string | null;
  usage: UsageView | null;
  extra_keys: string[];
};

/** 流式响应有多碎。**只有统计，没有逐帧**。 */
export type StreamStats = {
  streaming: boolean;
  /** 网络字节块数。 */
  frames: number;
  /** 拼起来之后切出的 SSE 事件数。 */
  events: number;
  /** 事件名 → 次数，按次数降序。 */
  event_kinds: { name: string; count: number }[];
};

export function inspectExchange(
  sessionId: string,
  exchangeId: string,
): Promise<InspectedExchange | null> {
  return invoke<InspectedExchange | null>("inspect_exchange", { sessionId, exchangeId });
}

export function getExchange(sessionId: string, exchangeId: string): Promise<ExchangeRecord | null> {
  return invoke<ExchangeRecord | null>("get_exchange", { sessionId, exchangeId });
}
