import { invoke } from "@tauri-apps/api/core";
import type { MessageView } from "./types";

// ---------------------------------------------------------------------------
// 常驻助理（镜像 `assistant/mod.rs`、`assistant/runtime.rs`）
// ---------------------------------------------------------------------------

export type AssistantMode = "observe" | "auto";

/** 值守层的工作方式：关 / 只写建议 / 授权内自决。 */
export type SuperviseMode = "off" | "observe" | "assist";
export type VerifyLevel = "recorded" | "rerun" | "off";

export type AssistantBudget = {
  max_turns: number;
  max_minutes: number;
  max_judgements: number;
};

export type SupervisorSettings = {
  mode: SuperviseMode;
  batch_ms: number;
  max_batch: number;
  daily_escalations: number;
  quiet_hours: [number, number] | null;
  verify: VerifyLevel;
  max_judgements: number;
  budget: AssistantBudget;
};

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
  supervisor: SupervisorSettings;
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
  /**
   * 触发命令：一条长驻命令，它每吐一行就让这个来源立刻查一次。空 = 只按定时检查。
   * 它不产生事件 —— 查的仍是 `command`，格式与去重都照旧。
   */
  trigger: string;
  enabled: boolean;
  state: AssistantSourceState;
};

/** 触发命令的运行状态。面板上用它说明监听器还活着，还是已经退了几次。 */
export type AssistantTriggerStatus = {
  /** 重启过几次（不含第一次拉起）。 */
  restarts: number;
  /** 最近吐出来的一行 —— 有更新就说明监听器活着。 */
  last_line: string | null;
  last_line_at: number | null;
  /** 最近一次退出 / 拉不起来的原因。正常时是 null。 */
  last_error: string | null;
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
  | "verifying"
  | "rework"
  | "ready"
  | "blocked"
  | "failed"
  | "needs_decision"
  | "needs_user"
  | "unreported"
  | "waiting"
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
  /** `waiting` 时在等什么。唤醒后就没有了。 */
  wait?: AssistantWait;
  /** 用哪份授权书。 */
  charter?: string | null;
  /** 验收标准。 */
  acceptance?: string[];
  budget?: AssistantBudget;
  used?: { turns: number; minutes: number; judgements: number };
  judgement?: {
    rounds: number;
    last_at: number;
    last_topic: string | null;
    escalation: string | null;
  };
  evidence?: AssistantEvidence | null;
  /** 已发出、尚未被汇报回应的值守指令。 */
  open_directive?: string | null;
};

export type AssistantEvidence = {
  checks: {
    cmd: string;
    ok: boolean;
    exit: number | null;
    tail: string;
    at: number;
  }[];
  changes: string[];
  diff_hash?: string | null;
  note: string;
  verified: boolean;
};

/** 可授予 / 可请示的动作类别。与 `assistant/charter.rs` 一一对应。 */
export type ActionClass =
  | "edit_code"
  | "run_checks"
  | "retry"
  | "adjust_plan"
  | "inspect"
  | "add_dependency"
  | "change_public_api"
  | "config_write"
  | "push"
  | "open_pr"
  | "comment_external"
  | "send_message"
  | "delete_files"
  | "spend_money";

export const ACTION_CLASS_LABEL: Record<ActionClass, string> = {
  edit_code: "改代码",
  run_checks: "跑检查",
  retry: "重试",
  adjust_plan: "调整方案",
  inspect: "查看",
  add_dependency: "加依赖",
  change_public_api: "改公共接口",
  config_write: "改配置",
  push: "推送",
  open_pr: "开 PR",
  comment_external: "对外评论",
  send_message: "对外发消息",
  delete_files: "删除文件",
  spend_money: "花钱",
};

/** 不可逆动作：永远不能默认授予。 */
export const IRREVERSIBLE_ACTIONS: ActionClass[] = [
  "push",
  "open_pr",
  "comment_external",
  "send_message",
  "delete_files",
  "spend_money",
];

export type CharterScope =
  | { kind: "global" }
  | { kind: "project"; value: string }
  | { kind: "source"; value: string };

export type Charter = {
  id: string;
  name: string;
  scope: CharterScope;
  may: ActionClass[];
  must_ask: ActionClass[];
  checks: string[];
  notify: "immediate" | "digest" | "silent";
  granted: "default" | "user";
  expires_at: number | null;
  note: string;
};

export type EscalationChoice = {
  id: string;
  label: string;
  consequence: string;
  risky: boolean;
};

export type Escalation = {
  id: string;
  ticket: string | null;
  ticket_label: string;
  created_at: number;
  expires_at: number;
  on_expiry: "hold" | "execute_default";
  brief: {
    situation: string;
    proposal: string;
    options: EscalationChoice[];
    if_silent: string;
    refs: string[];
  };
  status: "open" | "answered" | "expired" | "withdrawn" | "superseded";
  answer?: {
    choice?: string;
    text?: string;
    at: number;
  } | null;
  topic: string;
};

export type LedgerEntry = {
  id: string;
  at: number;
  kind:
    | "report"
    | "verify"
    | "directive"
    | "escalation"
    | "answer"
    | "action"
    | "outcome"
    | "interrupt";
  actor: "worker" | "supervisor" | "user" | "system";
  ticket?: string;
  session?: string;
  in_reply_to?: string;
  auth?: unknown;
  data: unknown;
};

export type AssistantMetrics = {
  day: string;
  escalations_today: number;
  answered_today: number;
  expired_today: number;
  directives_today: number;
  proposed_today: number;
  verify_fail_today: number;
  open_escalations: number;
  open_tickets: number;
  self_resolved: number | null;
};

/**
 * 一张「等回复」的工单在等什么：盯哪个观察源、新行里含哪些关键词算等到、最晚等到几点。
 * 等到了运行时直接把那几行送回工单，不经评估。
 */
export type AssistantWait = {
  source_id: string;
  keywords: string[];
  until: number;
  note: string;
  /** 探测命令：观察源看不到对方后续时，工单自己给的那条命令，运行时每 30 秒替它跑。 */
  probe?: string;
  /** 探测命令的运行状态；只关心上次有没有失败。 */
  probe_state?: { last_error: string | null; failures: number; last_polled_at: number | null };
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
  /** 新的在前。 */
  escalations?: Escalation[];
  /** 授权书。至少有一份全局默认。 */
  charters?: Charter[];
  next_number: number;
  /** 正在检查的观察源 id。 */
  busy: string[];
  /** 要人处理的件数。 */
  attention: number;
  /** 各来源触发命令的运行状态（来源 id → 状态）。没配触发命令的不在这里。 */
  triggers?: Record<string, AssistantTriggerStatus>;
  /** 有工单在等回复、正在加密轮询的来源 id。 */
  urgent_sources?: string[];
  /** 每张工单最近一条汇报在助理对话里的轮次号（工单 id → 轮）。卡片上的「去对话」用它。 */
  report_turns?: Record<string, number>;
  /** 值守指标。 */
  metrics?: AssistantMetrics;
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

/** 拍板一条请示。`choice` 是选项 id；也可以只给一句自由回答。 */
export function answerAssistantEscalation(
  id: string,
  choice: string | null,
  text: string | null,
): Promise<void> {
  return invoke<void>("answer_assistant_escalation", { id, choice, text });
}

/** 新增或修改一份授权书。 */
export function saveAssistantCharter(charter: Charter): Promise<Charter> {
  return invoke<Charter>("save_assistant_charter", { charter });
}

export function deleteAssistantCharter(id: string): Promise<void> {
  return invoke<void>("delete_assistant_charter", { id });
}

/** 决策台账。`ticket` 给了就只看那一张。 */
export function getAssistantLedger(ticket?: string | null, limit?: number): Promise<LedgerEntry[]> {
  return invoke<LedgerEntry[]>("get_assistant_ledger", {
    ticket: ticket ?? null,
    limit: limit ?? null,
  });
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
