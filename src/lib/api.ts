//! App ↔ 前端的 IPC 契约（TypeScript 侧）。
//!
//! 这是 `crates/carter-app-core/src/{protocol,config_edit,app_settings,sessions,
//! skills,catalog}.rs` 与 `src-tauri/src/lib.rs` 的逐字段镜像 —— Rust 侧改了这里
//! 就必须同步改，反之亦然。
//!
//! ## 两种命名约定，别混
//!
//! 1. **命令的参数名**走 camelCase：Tauri v2 会把 `sessionId` 自动转成 Rust 的
//!    `session_id`。这是 Tauri 的行为。
//! 2. **参数里的结构体字段**走 snake_case：那部分是 serde 直接反序列化的，
//!    Tauri 不参与。所以 `ProviderDraft` 里是 `api_key` 而不是 `apiKey`。
//!
//! 界面上这两种写法挨在一起（`save_provider({ draft: { api_key, … } })`），
//! 记混了只会得到一个「字段缺失」的运行时错误，所以特意写在这里。

import { invoke } from "@tauri-apps/api/core";

/// 后端广播事件用的通道名。
///
/// 这是字符串字面量，改名会**静默**破坏订阅（前端再也收不到任何事件，也不报错）。
/// 与 `src-tauri/src/lib.rs` 的 `CARTER_EVENT` 必须一致。
export const EVENT_NAME = "carter://event";

// ---------------------------------------------------------------------------
// 会话内事件流
// ---------------------------------------------------------------------------

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
  /** `~/.carter/config.toml` 被改过并已热重载，各设置页据此重拉数据。 */
  | { type: "config_changed" }
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
  | { kind: "tool_call"; id: string; name: string; args: string }
  | { kind: "tool_result"; tool_use_id: string; text: string; is_error: boolean }
  | { kind: "server_activity"; raw_type: string; summary: string }
  | { kind: "image"; media_type: string; hash: string | null };

export type MessageView = { role: "user" | "assistant" | "system"; blocks: BlockView[] };

/** 前端 → 后端。 */
export type Command =
  | { type: "send_prompt"; text: string }
  | { type: "cancel" }
  | { type: "set_model"; model: string }
  | { type: "answer_ask"; id: number; answer: string };

// ---------------------------------------------------------------------------
// 应用信息与设置
// ---------------------------------------------------------------------------

export type ThemePref = "system" | "light" | "dark";

/** `~/.carter-app/settings.json`。**carter CLI 不读它。** */
export type AppSettings = {
  theme: ThemePref;
  /** 不注册给模型的本地工具名。 */
  disabled_tools: string[];
  sidebar_width: number;
};

export type AppInfo = {
  name: string;
  version: string;
  default_cwd: string;
  default_model: string;
  /** 配置文件位置 —— 有些事只能去文件里做，界面要把路径写出来。 */
  config_path: string;
  config_exists: boolean;
  models_cache_exists: boolean;
  settings: AppSettings;
};

export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

export function setAppSettings(settings: AppSettings): Promise<void> {
  return invoke<void>("set_app_settings", { settings });
}

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

export type CreatedSession = {
  session_id: string;
  model: string;
  cwd: string;
  history: MessageView[];
};

export type SessionRow = {
  id: string;
  title: string | null;
  model: string;
  cwd: string;
  /** epoch 毫秒。 */
  created_at: number;
  forked_from: string | null;
  bytes: number;
};

export type SearchHit = {
  id: string;
  title: string | null;
  model: string;
  created_at: number;
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

export function listSessions(all = false): Promise<SessionRow[]> {
  return invoke<SessionRow[]>("list_sessions", { all });
}

export function renameSession(sessionId: string, title: string): Promise<void> {
  return invoke<void>("rename_session", { sessionId, title });
}

/** fork 一条会话，返回新会话的 id。 */
export function forkSession(sessionId: string): Promise<string> {
  return invoke<string>("fork_session", { sessionId });
}

/** 删除一个会话文件。不可恢复 —— 调用方必须先确认。 */
export function deleteSession(sessionId: string): Promise<void> {
  return invoke<void>("delete_session", { sessionId });
}

export function searchSessions(query: string): Promise<SearchHit[]> {
  return invoke<SearchHit[]>("search_sessions", { query });
}

/** 导出到下载目录，返回写出的绝对路径。 */
export function exportSession(sessionId: string, format: "markdown" | "json"): Promise<string> {
  return invoke<string>("export_session", { sessionId, format });
}

// ---------------------------------------------------------------------------
// 渠道 / 模型 / 协议
// ---------------------------------------------------------------------------

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
  /** 该协议确实支持的服务端工具。 */
  supported_server_tools: string[];
  models: string[];
  is_default: boolean;
};

export type ModelDetail = {
  name: string;
  meta: string;
  api_name: string | null;
  /**
   * **原样的 TOML 值**：字符串（`"medium"`）或表（`{ budget = 1024 }`）。
   * 表形态界面表达不了，编辑时要原样放过 —— 见 `lib/protocols.ts`。
   */
  default_reasoning: unknown;
};

export type ProviderDetail = {
  name: string;
  kind: string;
  /** **原始文本**，可能还是 `${ANTHROPIC_API_KEY}` 这种引用。 */
  api_key: string;
  has_api_key: boolean;
  base_url: string;
  server_tools: string[];
  server_tool_max_uses: number | null;
  models: ModelDetail[];
};

export type ModelDraft = {
  name: string;
  meta: string;
  api_name?: string | null;
  default_reasoning?: string | null;
};

export type ProviderDraft = {
  name: string;
  kind: string;
  /**
   * **`null` = 保持原样。** 界面不回传已存的密钥，于是「不碰它」这个意图
   * 在协议层就是明确的。传空串则是「清掉」。
   */
  api_key: string | null;
  base_url: string | null;
  server_tools: string[];
  server_tool_max_uses: number | null;
  /** 整表对齐：不在这个列表里的模型会被删掉。 */
  models: ModelDraft[];
};

/** 全部模型引用 + 从 models.dev 解出来的元数据。 */
export type ModelRow = {
  /** `provider/model`。 */
  reference: string;
  provider: string;
  name: string;
  api_name: string;
  meta: string;
  is_default: boolean;
  protocol: string;
  /** 配置里给这个模型指定的默认推理档位，原样（字符串或表）。 */
  default_reasoning: unknown;
  context_window: number | null;
  max_output_tokens: number | null;
  capabilities: string[];
  pricing: {
    input: number;
    output: number;
    cache_read: number | null;
    cache_write: number | null;
  } | null;
  /** 解析元数据失败的原因。有值时上面几栏是空的，但这一行仍然有效。 */
  error: string | null;
};

export function listChannels(): Promise<ChannelRow[]> {
  return invoke<ChannelRow[]>("list_channels");
}

export function getProviderDetail(name: string): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("get_provider_detail", { name });
}

/**
 * 某个协议能吃下哪些服务端工具，返回 `[生效的, 不生效的]`。
 *
 * 直接问底座而不是在前端维护一张协议 → 工具名的表：那张表会和实现悄悄漂移，
 * 而这里给出的答案永远是真的。
 */
export function checkServerTools(
  kind: string,
  tools: string[],
): Promise<[string[], string[]]> {
  return invoke<[string[], string[]]>("check_server_tools", { kind, tools });
}

export function saveProvider(draft: ProviderDraft): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("save_provider", { draft });
}

export function deleteProvider(name: string): Promise<void> {
  return invoke<void>("delete_provider", { name });
}

/** 新增或覆盖一个模型（按块名 upsert），返回写回后的渠道详情。 */
export function saveModel(provider: string, model: ModelDraft): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("save_model", { provider, model });
}

export function deleteModel(provider: string, model: string): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("delete_model", { provider, model });
}

export function listModels(): Promise<ModelRow[]> {
  return invoke<ModelRow[]>("list_models");
}

export function setDefaultModel(modelRef: string): Promise<void> {
  return invoke<void>("set_default_model", { modelRef });
}

/** 拉一次 models.dev 目录，返回拉到的模型条数。 */
export function refreshModelCatalog(): Promise<number> {
  return invoke<number>("refresh_model_catalog");
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

export type ToolRow = {
  name: string;
  description: string;
  /** 参数 JSON Schema。`task` 那一行是 null —— 它构造需要 provider，列不出来。 */
  parameters: unknown;
  enabled: boolean;
  /** `false` = 界面不提供开关。 */
  toggleable: boolean;
  note: string | null;
};

/**
 * 本地会执行的工具。
 *
 * 厂商在服务端执行的（web_search 等）不在这里 —— 它们是渠道能力，见
 * `listChannels`。MCP 工具也不在这里：名字要连上 server 才知道，见
 * `listMcpServers`。
 */
export function listTools(): Promise<ToolRow[]> {
  return invoke<ToolRow[]>("list_tools");
}

// ---------------------------------------------------------------------------
// 技能
// ---------------------------------------------------------------------------

export type SkillRow = {
  name: string;
  description: string;
  allowed_tools: string[];
  enabled: boolean;
  path: string;
};

export type SkillListing = {
  skills: SkillRow[];
  /** 全局开关（`[skills].enabled`）。关掉时整个技能目录都不加载。 */
  globally_enabled: boolean;
};

export type SkillDraft = {
  name: string;
  description: string;
  allowed_tools: string[];
  body: string;
};

export function listSkills(): Promise<SkillListing> {
  return invoke<SkillListing>("list_skills");
}

export function readSkill(name: string): Promise<string> {
  return invoke<string>("read_skill", { name });
}

export function saveSkill(draft: SkillDraft): Promise<void> {
  return invoke<void>("save_skill", { draft });
}

/** 启停单个技能。实现上是把目录在 `skills/` 与 `skills.disabled/` 之间搬。 */
export function toggleSkill(name: string, enabled: boolean): Promise<void> {
  return invoke<void>("toggle_skill", { name, enabled });
}

export function deleteSkill(name: string): Promise<void> {
  return invoke<void>("delete_skill", { name });
}

export function setSkillsEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_skills_enabled", { enabled });
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

export type McpServerRow = {
  name: string;
  transport: string;
  /** stdio 给 command，http 给 url。 */
  target: string;
  enabled: boolean;
  args: string[];
  url: string;
  command: string;
  env: [string, string][];
  headers: [string, string][];
};

export type McpDraft = {
  name: string;
  enabled: boolean;
  /** `stdio` | `http`。carter 那边是必填，界面要挡住空值。 */
  transport: string;
  command: string | null;
  args: string[];
  env: [string, string][];
  url: string | null;
  headers: [string, string][];
};

export function listMcpServers(): Promise<McpServerRow[]> {
  return invoke<McpServerRow[]>("list_mcp_servers");
}

export function saveMcpServer(draft: McpDraft): Promise<void> {
  return invoke<void>("save_mcp_server", { draft });
}

export function deleteMcpServer(name: string): Promise<void> {
  return invoke<void>("delete_mcp_server", { name });
}

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

export function getExchange(
  sessionId: string,
  exchangeId: string,
): Promise<ExchangeRecord | null> {
  return invoke<ExchangeRecord | null>("get_exchange", { sessionId, exchangeId });
}
