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
  /** `args` 是**完整的**参数对象 —— 差异化渲染靠它。 */
  | { type: "tool_started"; call_id: string; name: string; args: unknown }
  | {
      type: "tool_finished";
      call_id: string;
      ok: boolean;
      /** 工具回灌给模型的那段正文（过长时后端已截断并标注）。 */
      content: string;
      elapsed_ms: number;
    }
  | { type: "todo_updated"; todos: TodoView[] }
  /** 长任务目标变了（设定 / 暂停 / 恢复 / 完成 / 清除）。`null` = 已经没有目标。 */
  | { type: "goal"; goal: GoalView | null }
  | { type: "ask_form"; id: number; questions: AskQuestion[] }
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

/** 一个问题。支持多选与带说明的选项。 */
export type AskOption = {
  label: string;
  /** 这个选项意味着什么。界面上作为次要说明。 */
  description: string | null;
};

export type AskQuestion = {
  question: string;
  /** 极短标签，界面上当标题用。 */
  header: string | null;
  multi_select: boolean;
  options: AskOption[];
};

/** 前端 → 后端。 */
export type Command =
  | { type: "send_prompt"; text: string }
  | { type: "cancel" }
  | { type: "set_model"; model: string }
  /**
   * 回答 `ask_user_question` 提的问题。
   *
   * `answers` 与问题一一对应，每项是该问题选中的标签（含自由输入）。
   * 空数组 = 这一问被跳过。
   */
  | { type: "answer_form"; id: number; answers: string[][] }
  /** 长任务目标（`/goal`）的操作。 */
  | { type: "goal"; action: GoalCommand };

/** `/goal` 的动作。拼写由输入框那一侧解析一次，后端只认结构化枚举。 */
export type GoalCommand =
  | { type: "set"; objective: string }
  | { type: "clear" }
  | { type: "pause" }
  | { type: "resume" }
  | { type: "complete" };

/** 长任务目标的界面视图。`status_label` 是后端给的中文标签，前端不再维护一份映射。 */
export type GoalView = {
  objective: string;
  status: "active" | "paused" | "completed" | "blocked";
  status_label: string;
  /** 已经自动继续了多少轮。 */
  auto_continues: number;
  /** 模型在 `goal_update` 里写的说明（被卡住 / 需要拍板时界面要显出来）。 */
  note: string | null;
};

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
  /**
   * 额外的技能扫描父目录（规范化后的绝对路径）。技能从 `~/.carter/skills` 与这些目录
   * **取并集**，本地优先。改动请走 `addSkillRoot` / `removeSkillRoot` ——
   * `setAppSettings` 是整体覆盖写入，拿旧值写回会把这些路径抹掉。
   */
  skill_roots: string[];
  /** 按名字忽略的技能。**只作用于外部扫描路径**（本地技能靠搬目录启停）。 */
  ignored_skills: string[];
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
  /** 这条会话现有的长任务目标。 */
  goal: GoalView | null;
};

/** 侧边聊天（`/btw`）：继承主会话上下文、不落盘、关掉即丢。 */
export type CreatedSide = {
  session_id: string;
  model: string;
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

/**
 * 开一个侧边聊天（`/btw`）。
 *
 * 后端起一条**不落盘**的会话：继承主会话的历史作为参考，关掉就没了。
 * 一个主会话同时只允许一个，重复调用会 reject 一句说明。
 */
export function createSideSession(parentSessionId: string): Promise<CreatedSide> {
  return invoke<CreatedSide>("create_side_session", { parentSessionId });
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

/** models.dev 目录里的一个模型。镜像 `catalog.rs` 的 `CatalogEntry`。 */
export type CatalogEntry = {
  /** models.dev 的模型 id —— `meta` 的后半截，也是官方端点认的模型名。 */
  id: string;
  name: string;
  context_window: number | null;
  max_output_tokens: number | null;
  reasoning: boolean;
  deprecated: boolean;
  release_date: string | null;
};

/** 某个 models.dev 服务商下的模型，能用的、新的在前。还没有缓存时会 reject。 */
export function listCatalogModels(providerId: string): Promise<CatalogEntry[]> {
  return invoke<CatalogEntry[]>("list_catalog_models", { providerId });
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
  /** 界面上那个开关的状态：本地技能看目录在哪一侧，外部技能看忽略名单。 */
  enabled: boolean;
  /** 这一份 `SKILL.md` 的绝对路径。 */
  path: string;
  /** 这一行来自哪个扫描根（绝对路径）。列表按它分组。 */
  source: string;
  /** 外部路径 → `true`：不能编辑、不能删除，开关也只能改忽略名单。 */
  readonly: boolean;
  /** 有没有进**并集** —— 也就是模型到底用不用得到它。 */
  effective: boolean;
  /** 被同名技能挡住时，指向挡住它的那一份 `SKILL.md`。 */
  shadowed_by: string | null;
};

/** 一条扫描路径，附带「为什么它扫到的技能没全生效」的几个计数。 */
export type SkillRoot = {
  path: string;
  /** `local` = 本应用自己的技能目录（不可移除）；`external` = 用户加进来的。 */
  source: string;
  exists: boolean;
  /** 直接子目录数。 */
  subdirs: number;
  /** 解析出 `SKILL.md` 的技能数。 */
  found: number;
  /** 进了并集的数量。 */
  effective: number;
  /** 因为同名被前面的根挡住的数量。 */
  shadowed: number;
  /** 因为忽略名单被排掉的数量。 */
  ignored: number;
};

/** 快捷选项：别的工具放技能的默认位置。 */
export type SkillPreset = {
  id: string;
  label: string;
  /** 这个预设包含的全部路径（Codex 是两条，见 `hint`）。 */
  paths: string[];
  /** 其中**还没加**的那些 —— 点一下只加这些，于是重复点也不会撞上「已经在列表里」。 */
  missing: string[];
  exists: boolean;
  hint: string;
};

export type SkillListing = {
  skills: SkillRow[];
  /** 全局开关（`[skills].enabled`）。关掉时整个技能目录都不加载。 */
  globally_enabled: boolean;
  /** 扫描根一览：本地目录排第一，然后是附加路径（顺序 = 优先级）。 */
  roots: SkillRoot[];
  presets: SkillPreset[];
  /** 被按名字忽略的外部技能。有些名字可能已经没有对应的行了，靠它撤销。 */
  ignored_skills: string[];
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

/**
 * 启停单个技能：本地的把目录在 `skills/` 与 `skills.disabled/` 之间搬，
 * 外部路径的（Claude Code / Codex 那边）只记进忽略名单 —— 本应用不碰别人的目录。
 *
 * `external` 必须传**点的是哪一行**（行上的 `readonly`）。本地技能停用后外部的同名技能会
 * 顶上来，那时两行同名，能区分它们的只有来源。
 *
 * 回传新的 `AppSettings`，调用方要 `setSettings` 对齐，否则下一次「改主题」会用
 * 旧值整体覆盖，把忽略名单清掉。
 */
export function toggleSkill(name: string, enabled: boolean, external: boolean): Promise<AppSettings> {
  return invoke<AppSettings>("toggle_skill", { name, enabled, external });
}

export function deleteSkill(name: string): Promise<void> {
  return invoke<void>("delete_skill", { name });
}

export function setSkillsEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_skills_enabled", { enabled });
}

/** 加一条技能扫描路径。回传新的 `AppSettings`（理由同 `toggleSkill`）。 */
export function addSkillRoot(path: string): Promise<AppSettings> {
  return invoke<AppSettings>("add_skill_root", { path });
}

export function removeSkillRoot(path: string): Promise<AppSettings> {
  return invoke<AppSettings>("remove_skill_root", { path });
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
// 输入框：斜杠命令、@ 文件、图片
// ---------------------------------------------------------------------------

/**
 * 一条能出现在 `/` 菜单里的命令。
 *
 * `builtin` 由本应用执行；`custom` 是 `~/.carter/commands/*.md` 里的命令，
 * 按回车后**不立即发送**，而是把它的展开交给后端（见下）——
 * 展开后的正文（含注入的文件内容）不该出现在输入框里。
 */
export type CommandRow = {
  name: string;
  description: string;
  argument_hint: string | null;
  kind: "builtin" | "custom";
};

export function listCommands(cwd?: string | null): Promise<CommandRow[]> {
  return invoke<CommandRow[]>("list_commands", { cwd: cwd ?? null });
}

/** `@` 菜单的文件列表：相对工作目录的路径。 */
export function listFiles(
  cwd: string | null,
  query: string,
  limit = 50,
): Promise<string[]> {
  return invoke<string[]>("list_files", { cwd, query, limit });
}

/**
 * 把粘贴进来的图片存进 carter 的图片库，返回可插进消息文本的 `[img:hash.ext]` 引用。
 *
 * `data` 是裸 base64 或 data URL 都行。服务端按 sha256 内容寻址去重，
 * 并在入库前把长边压到 1568px。
 */
export function saveImage(data: string, ext?: string | null): Promise<string> {
  return invoke<string>("save_image", { data, ext: ext ?? null });
}

/** 把一条图片引用（`[img:hash.ext]` 或裸 `hash.ext`）读成 data URL；读不到返回 null。 */
export function readImage(token: string): Promise<string | null> {
  return invoke<string | null>("read_image", { token });
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
