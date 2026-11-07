import { invoke } from "@tauri-apps/api/core";
import type { MessageView } from "./types";

// ---------------------------------------------------------------------------
// 常驻助理（镜像 `assistant/mod.rs`、`assistant/runtime.rs`）
// ---------------------------------------------------------------------------

export type AssistantMode = "observe" | "auto";

export type AssistantRule = {
  id: string;
  name: string;
  instruction: string;
  enabled: boolean;
};

export type AssistantSettings = {
  mode: AssistantMode;
  rules: AssistantRule[];
  scope: string;
  max_running: number;
  eval_model: string | null;
  work_model: string | null;
  paused: boolean;
};

export type AssistantSourceState = {
  baselined: boolean;
  next_at: number;
  last_polled_at: number | null;
  last_new: number;
  last_error: string | null;
  failures: number;
};

export type AssistantSource = {
  id: string;
  name: string;
  command: string;
  cwd: string;
  every_minutes: number;
  enabled: boolean;
  state: AssistantSourceState;
};

export type Verdict = "accept" | "would_accept" | "attach" | "ignore" | "unavailable";

export type AssistantDecision = {
  id: string;
  at: number;
  source_id: string;
  source_name: string;
  events: string[];
  verdict: Verdict;
  rule: string | null;
  reason: string;
  title: string | null;
  task: string | null;
  ticket_id: string | null;
  dismissed: boolean;
};

export type TicketStatus =
  | "queued"
  | "working"
  | "ready"
  | "blocked"
  | "failed"
  | "needs_user"
  | "unreported"
  | "dropped"
  | "done"
  | "stopped";

export type AssistantTicket = {
  id: string;
  number: number;
  title: string;
  task: string;
  rule: string;
  source_id: string;
  source_name: string;
  cwd: string;
  events: string[];
  created_at: number;
  updated_at: number;
  status: TicketStatus;
  session_id: string | null;
  summary: string | null;
  question: string | null;
  /** `question` 的候选答案。没有就不给。 */
  options?: string[];
  pending: string[];
  runs: number;
  /** 助理对话里这条的汇报被划掉了（「忽略」）。面板上照常显示。 */
  ignored?: boolean;
};

/**
 * 这条记忆由谁遵守。
 *
 * `worker` 的会随工单任务书一起发进执行会话 —— 工单读不到这条对话，只有它带得过去。
 * 用户说「回复钉钉要通俗点」属于这一类。
 */
export type AssistantAudience = "assistant" | "worker";

export type AssistantMemory = {
  id: string;
  text: string;
  source: string;
  /** 缺省是 `assistant`（老数据反序列化出来的都按它）。 */
  audience?: AssistantAudience;
  created_at: number;
  updated_at: number;
};

export type AssistantEpisode = {
  number: number;
  started_at: number;
  ended_at: number;
  summary: string;
  turns: number;
};

export type AssistantView = {
  settings: AssistantSettings;
  memories: AssistantMemory[];
  episodes: AssistantEpisode[];
  sources: AssistantSource[];
  /** 新的在前。 */
  decisions: AssistantDecision[];
  tickets: AssistantTicket[];
  next_number: number;
  /** 正在检查的观察源 id。 */
  busy: string[];
  /** 要人处理的件数。 */
  attention: number;
  /** 每张工单最近一条汇报在助理对话里的轮次号（工单 id → 轮）。卡片上的「去对话」用它。 */
  report_turns?: Record<string, number>;
};

export type TicketAction =
  | { type: "reply"; text: string }
  | { type: "retry" }
  | { type: "done" }
  | { type: "stop" }
  /** 助理对话里那条汇报划掉：不回答、也不结束，只是别再排在「等你回复」里。 */
  | { type: "ignore" };

export function getAssistant(): Promise<AssistantView> {
  return invoke<AssistantView>("get_assistant");
}

/** 改长期记忆：`text` 为空 = 删掉，`id` 为空 = 新增。 */
export function saveAssistantMemory(
  id: string | null,
  text: string,
  audience?: AssistantAudience,
): Promise<void> {
  return invoke<void>("save_assistant_memory", { id, text, audience: audience ?? null });
}

export function getAssistantEpisode(number: number): Promise<MessageView[]> {
  return invoke<MessageView[]>("get_assistant_episode", { number });
}

/**
 * 助理对话立刻开新一期：之前的原话进往期存档、整理出纪要与记忆，之后不再进上下文。
 * 正在说话时排队，这一轮跑完再切；完成时会话发 `assistant_episode`。
 */
export function newAssistantEpisode(): Promise<void> {
  return invoke<void>("new_assistant_episode");
}

export function saveAssistantSettings(settings: AssistantSettings): Promise<void> {
  return invoke<void>("save_assistant_settings", { settings });
}

export function saveAssistantSource(source: AssistantSource): Promise<void> {
  return invoke<void>("save_assistant_source", { source });
}

export function deleteAssistantSource(id: string): Promise<void> {
  return invoke<void>("delete_assistant_source", { id });
}

/** 现在就检查一次。返回新行数（第一次只建基线，是 0）。 */
export function pollAssistantSource(id: string): Promise<number> {
  return invoke<number>("poll_assistant_source", { id });
}

/** 试跑命令，返回原样输出。 */
export function testAssistantCommand(command: string, cwd: string): Promise<string> {
  return invoke<string>("test_assistant_command", { command, cwd });
}

export function assistantTicketAction(ticketId: string, action: TicketAction): Promise<void> {
  return invoke<void>("assistant_ticket_action", { ticketId, action });
}

export function approveAssistantDecision(decisionId: string): Promise<string> {
  return invoke<string>("approve_assistant_decision", { decisionId });
}

export function dismissAssistantDecision(decisionId: string): Promise<void> {
  return invoke<void>("dismiss_assistant_decision", { decisionId });
}

export function reassessAssistantDecision(decisionId: string): Promise<AssistantDecision> {
  return invoke<AssistantDecision>("reassess_assistant_decision", { decisionId });
}

export function setDefaultModel(modelRef: string): Promise<void> {
  return invoke<void>("set_default_model", { modelRef });
}

/** models.dev 目录里的一个模型。镜像 `catalog.rs` 的 `CatalogEntry`。 */
export type CatalogEntry = {
  /** models.dev 的模型 id —— `meta` 的后半截，也是官方端点认的模型名。 */
  id: string;
  name: string;
  context_window: number | null;
  max_output_tokens: number | null;
  reasoning: boolean;
  /** 能力标记，与 `ModelRow.capabilities` 同一套字符串。 */
  capabilities: string[];
  /** 同一系列的名字（`claude-opus` / `deepseek-v4`）。目录对话框按它分组。 */
  family: string | null;
  deprecated: boolean;
  release_date: string | null;
};

/** 某个 models.dev 服务商下的模型，能用的、新的在前。还没有缓存时会 reject。 */
export function listCatalogModels(providerId: string): Promise<CatalogEntry[]> {
  return invoke<CatalogEntry[]>("list_catalog_models", { providerId });
}

/**
 * 一次刷新的结果。镜像 `catalog::RefreshOutcome`。
 *
 * `source === "builtin"` 表示所有源都没拉到，用的是编进安装包的快照 —— 界面要
 * 明说这一点，别让用户以为刷新成功了。
 */
export type RefreshOutcome = {
  count: number;
  source: string;
  failed: { source: string; error: string }[];
  fetched_at: number;
};

/** 拉一次 models.dev 目录（按官方 → 镜像 → 用户填的源的顺序试）。 */
export function refreshModelCatalog(): Promise<RefreshOutcome> {
  return invoke<RefreshOutcome>("refresh_model_catalog");
}
