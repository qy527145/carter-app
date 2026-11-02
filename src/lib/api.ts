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
  | {
      type: "ready";
      session_id: string;
      /** 真正发给上游的模型名。 */
      model: string;
      /** `渠道/显示名`。界面上的模型标签一律显示这一种写法。 */
      model_ref?: string;
      cwd: string;
      resumed: boolean;
    }
  /**
   * 一轮开始。`prompt_id` 是前端在 `send_prompt` 里带的编号 —— 后端把排队里的
   * 那条消息**真正开始跑**时原样带回，界面据此把它从「排队中」挪进对话。
   * 自动继续（`/goal`）推起来的一轮没有它。
   */
  | {
      type: "turn_started";
      turn: number;
      prompt_id?: number | null;
      /** 这条用户消息的原文（`/命令`、`@文件` 展开之前）。界面用它画气泡。 */
      prompt?: string | null;
    }
  /** 当前模型变了（`/model`）。只换「当前模型」那一处显示。 */
  | { type: "model_changed"; model: string; model_ref?: string }
  /**
   * 历史被回退到第 `turn` 轮之前（点了「重试」）。
   *
   * 界面要把这一轮的**回答**及之后的轮次从列表里拿掉，那条用户气泡留着 ——
   * 紧接着会来一条不带 `prompt` 的 `turn_started`，不会再画一遍。
   *
   * 改写提问不走这里：它开一条新分支（`forkSession`），原会话一个字不动。
   */
  | { type: "turn_retracted"; turn: number }
  /**
   * 历史被就地截短（回答上的「重试」）。界面按这份历史整个重建，紧接着来一条
   * 不带 `prompt` 的 `turn_started`，新的回答接在重建出来的那一轮末尾。
   */
  | {
      type: "history_replaced";
      history: MessageView[];
      prompts: TurnPrompt[];
      changes: TurnFiles[];
    }
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
  /** `agent` 工具里几个子 agent 的进度（整表快照，按 `call_id` 配对那张卡片）。 */
  | { type: "agent_progress"; call_id: string; agents: AgentRun[] }
  | { type: "todo_updated"; todos: TodoView[] }
  /** 长任务目标变了（设定 / 暂停 / 恢复 / 完成 / 清除）。`null` = 已经没有目标。 */
  | { type: "goal"; goal: GoalView | null }
  | { type: "ask_form"; id: number; questions: AskQuestion[] }
  /**
   * 这一轮**实际**改了哪些文件（相对会话 cwd，`/` 分隔）。
   *
   * 后端在轮首挂文件系统监听、轮末收回，所以 `bash`、子进程、MCP 的写入都在里面。
   * 与前端的 `write_file` / `edit_file` 参数合并后显示在回答末尾（后者能补上
   * 写在 cwd 之外的文件，并提供「写入 / 修改」的来源标签）。
   */
  | { type: "files_changed"; turn: number; files: string[] }
  | { type: "turn_usage"; usage: UsageView; cost: number; model: string }
  /** 上下文占用（输入框旁的百分比环）。就绪、每轮结束、换模型、压缩之后各来一次。 */
  | ({ type: "context" } & ContextView)
  /** 开始压缩上下文。`auto` = 占用超过触发线自动压的。 */
  | { type: "compact_started"; auto: boolean }
  /**
   * 上下文压缩完了，历史被整体替换。
   *
   * 界面拿 `history` **重建整个对话**（与打开历史会话同一条路）：中段换成了摘要，
   * 轮次号也重排了。`prompts` / `changes` 是按新轮次号重排过的那两份。
   */
  | {
      type: "compacted";
      history: MessageView[];
      prompts: TurnPrompt[];
      changes: TurnFiles[];
      /** 压缩前后的估算占用（只算历史消息）。 */
      before: number;
      after: number;
      /** `L3`（结构化摘要）/ `L2`（要点摘要）。 */
      tier: string;
      auto: boolean;
    }
  | { type: "notice"; message: string }
  /** 会话标题自动生成好了。前端据此刷新会话列表（侧栏那一行 + 顶部标题）。 */
  | { type: "title"; title: string }
  | { type: "idle" }
  | { type: "error"; message: string };

/** 后端 → 前端的总线载荷。 */
export type AppEvent =
  | { type: "session"; session_id: string; event: SessionEvent }
  /** `~/.carter/config.toml` 被改过并已热重载，各设置页据此重拉数据。 */
  | { type: "config_changed" }
  | {
      /** 定时任务触发了：新开了一条会话。界面只刷新列表，不切过去。 */
      type: "schedule_fired";
      schedule_id: string;
      name: string;
      session_id: string | null;
      error: string | null;
    }
  | { type: "error"; message: string };

/** `agent` 工具里一个子 agent 的当前状态。 */
export type AgentRun = {
  description: string;
  status: "running" | "done" | "failed" | "cancelled";
  steps: number;
  /** 此刻正在跑的工具名；`null` = 在思考或已结束。 */
  tool: string | null;
  /** 最近一段输出（尾部截断）。 */
  text: string;
};

export type TodoView = {
  content: string;
  active_form: string;
  status: "pending" | "in_progress" | "completed";
};

/**
 * 上下文占用。`used` 是下一次请求大概会带上的 token 数（历史 + 系统提示 + 工具声明，
 * 按模型的 tokenizer 估算），不是上游回报的用量 —— 后者是整轮累加、且不含缓存命中。
 */
export type ContextView = {
  used: number;
  /** 模型的上下文窗口。0 = 不知道。 */
  window: number;
  /** 自动压缩的触发线（`window × agent.compact_threshold_ratio`）。 */
  threshold: number;
  /** 自动压缩开着没有。 */
  auto: boolean;
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

export type MessageView = {
  role: "user" | "assistant" | "system";
  blocks: BlockView[];
  /**
   * 这条消息属于哪一轮用户输入（0 起）。
   *
   * 历史里的回答靠它接上「查看报文」——实时那一轮的轮次号走
   * `turn_started` 事件，历史靠后端按同样的口径算出来（`view.rs`）。
   * 两者必须是同一个数，否则点历史消息会筛到别人的报文。
   */
  turn?: number;
};

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
  /**
   * 提交一条用户消息。会话正在跑时**排队**，等这一轮结束再跑。
   *
   * `id` 由前端发号，`turn_started` 原样带回；`interject` = 立刻中断当前轮,
   * 插到队首（输入框里的 Ctrl/⌘+Enter）。
   */
  | { type: "send_prompt"; text: string; id?: number | null; interject?: boolean }
  /** 把一条**已经排队**的消息提到队首，并中断当前轮（排队条上的「插队」）。 */
  | { type: "promote_prompt"; id: number }
  /** 撤回一条还没开始跑的消息（排队条上的「×」）。 */
  | { type: "drop_prompt"; id: number }
  /**
   * 对**一条正在跑的**工具调用做操作（bash 卡片上那两颗按钮）。
   *
   * `call_id` 来自 `tool_started`。`interrupt` 只结束这条命令、这一轮继续；
   * `background` 让命令继续跑、工具立刻返回 `shell_id`。
   */
  | { type: "tool_action"; call_id: string; action: "interrupt" | "background" }
  | { type: "cancel" }
  /**
   * 重跑某一轮用户输入（回答下面的「重试」）。
   *
   * `turn` 与 `turn_started` / 历史消息上的轮次号同一口径。会话把历史回退到这一轮
   * 之前再原样重跑 —— 之后那几轮会一并丢掉。**只回滚对话，不动磁盘。**
   */
  | { type: "regenerate"; turn: number }
  /**
   * 就地重写第 `turn` 轮的最后一条回答：提问与前面的工具往返都留着，拿同一份
   * 上下文再请求一次。之后那几轮同样会丢掉，磁盘同样不动。
   */
  | { type: "retry_reply"; turn: number }
  | { type: "set_model"; model: string }
  /**
   * 回答 `ask_user_question` 提的问题。
   *
   * `answers` 与问题一一对应，每项是该问题选中的标签（含自由输入）。
   * 空数组 = 这一问被跳过。
   */
  | { type: "answer_form"; id: number; answers: string[][] }
  /** 长任务目标（`/goal`）的操作。 */
  | { type: "goal"; action: GoalCommand }
  /** 手动压缩上下文（`/compact`）。生成中发的话排队，这一轮跑完再压。 */
  | { type: "compact" };

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

/**
 * 对话主区的呈现密度。
 *
 * `detailed` = 现在的样子：思考、每一次工具调用都各占一张卡片；
 * `compact` = 只留正文，中间过程折成一行可展开的摘要。
 *
 * **只影响界面**，不改变发给模型的东西 —— 两种模式下工具照常执行、结果照常回灌。
 */
export type ConversationMode = "detailed" | "compact";

/** `~/.carter-app/settings.json`。**carter CLI 不读它。** */
export type AppSettings = {
  theme: ThemePref;
  /** 不注册给模型的本地工具名。 */
  disabled_tools: string[];
  sidebar_width: number;
  /** 对话主区的呈现密度。随时可切，来回切不碰会话历史。 */
  conversation_mode: ConversationMode;
  /**
   * 额外的技能扫描父目录（规范化后的绝对路径）。技能从当前项目的 `.carter/skills`、
   * `~/.carter/skills` 与这些目录**取并集**，项目优先、然后用户、再往后是这里。
   * 改动请走 `addSkillRoot` / `removeSkillRoot` ——
   * `setAppSettings` 是整体覆盖写入，拿旧值写回会把这些路径抹掉。
   */
  skill_roots: string[];
  /** 按名字忽略的技能。**只作用于外部扫描路径**（项目 / 用户技能靠搬目录启停）。 */
  ignored_skills: string[];
  /**
   * 「本轮变更的文件」额外忽略的规则，每条一个 glob，相对会话 cwd。
   *
   * 含 `/` 按相对路径匹配（`dist/**`），不含 `/` 按任意层级的名字匹配（`*.log`）。
   * 内置目录规则与 `.gitignore` 不在这里，它们始终生效。
   */
  changes_ignore: string[];
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

/**
 * 开发用：打开 WebView 的开发者工具。
 *
 * 应用接管右键菜单之后，默认菜单里那个「检查元素」也没了 —— 这个命令把同一个入口
 * 留给 dev 构建（见 components/ui/ContextMenu.tsx），正式包里不会挂出来。
 */
export function openDevtools(): Promise<void> {
  return invoke<void>("open_devtools");
}

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
  /**
   * 历史里每一轮用户打的原文。界面拿它回填「编辑」框 ——
   * 会话文件里存的是展开后的正文，回填进去等于把整份文件内容摊在输入框里。
   */
  prompts?: TurnPrompt[];
};

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

/**
 * 列举会话。`all = false` 只看 `cwd`（不给就是默认目录 —— 家目录），`true` 跨所有项目。
 *
 * 侧栏要传当前会话的 cwd：桌面端没有「进程工作目录」这回事，不传就永远只列家目录
 * 那一堆，切到别的项目之后侧栏里看到的还是原来那些会话。
 */
export function listSessions(all = false, cwd?: string): Promise<SessionRow[]> {
  return invoke<SessionRow[]>("list_sessions", { all, cwd: cwd ?? null });
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
  /**
   * models.dev 目录里的展示名（「Claude Opus 5.5」）。界面上的模型标签用它 ——
   * 用户配的是 `opus`、请求里发的是 `claude-opus-5-5`，都不如这个名字好认。
   * 没绑 meta 或目录里查不到时是 `null`，那时退回显示引用本身。
   */
  label: string | null;
  is_default: boolean;
  /** 是不是副模型（`agent.fast_model`）：压缩上下文、生成标题用的那个。 */
  is_fast: boolean;
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

/**
 * 设副模型（压缩上下文、生成标题用）。传 `null` 清除、回落主模型。
 */
export function setFastModel(modelRef: string | null): Promise<void> {
  return invoke<void>("set_fast_model", { modelRef });
}

/** 自动压缩：开关 + 触发比例（占上下文窗口的比例，0.3–0.95）。 */
export interface CompactionSettings {
  enabled: boolean;
  ratio: number;
}

export function getCompaction(): Promise<CompactionSettings> {
  return invoke<CompactionSettings>("get_compaction");
}

export function setCompaction(enabled: boolean, ratio: number): Promise<void> {
  return invoke<void>("set_compaction", { enabled, ratio });
}

// ---------------------------------------------------------------------------
// 定时任务
// ---------------------------------------------------------------------------

/** 触发方式。镜像 `schedules.rs::ScheduleKind`（`tag = "kind"`）。 */
export type ScheduleKind =
  /** 一次性，到点跑一次就自己停用。 */
  | { kind: "once"; at_ms: number }
  /** 每隔 N 分钟。 */
  | { kind: "interval"; every_minutes: number }
  /** 每天的 `hour:minute`（本机时间）。 */
  | { kind: "daily"; hour: number; minute: number };

/** 镜像 `schedules.rs::Schedule`。 */
export type Schedule = {
  id: string;
  name: string;
  /** 到点原样发出去的正文。 */
  prompt: string;
  kind: ScheduleKind;
  cwd: string;
  model: string | null;
  enabled: boolean;
  created_at: number;
  next_at: number;
  last_fired_at: number | null;
  last_session_id: string | null;
  runs: number;
  last_error: string | null;
};

export function listSchedules(): Promise<Schedule[]> {
  return invoke<Schedule[]>("list_schedules");
}

/** 新建 / 编辑一条。节奏变了后端会重算下一次触发。 */
export function saveSchedule(schedule: Schedule): Promise<void> {
  return invoke<void>("save_schedule", { schedule });
}

export function deleteSchedule(id: string): Promise<void> {
  return invoke<void>("delete_schedule", { id });
}

/** 暂停 / 恢复。返回更新后的那条（重新打开时下一次可能被重算）。 */
export function toggleSchedule(id: string, enabled: boolean): Promise<Schedule | null> {
  return invoke<Schedule | null>("toggle_schedule", { id, enabled });
}

/** 立即跑一次，返回新开的会话 id。不动它原来的节奏。 */
export function runSchedule(id: string): Promise<string> {
  return invoke<string>("run_schedule", { id });
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

/** 拉一次 models.dev 目录，返回拉到的模型条数。 */
export function refreshModelCatalog(): Promise<number> {
  return invoke<number>("refresh_model_catalog");
}

// ---------------------------------------------------------------------------
// 上游模型 / 元数据匹配 / 渠道模板
// ---------------------------------------------------------------------------

/** 上游一个模型名对到目录之后的结论。镜像 `catalog.rs` 的 `MetaMatch`。 */
export type MetaMatch = {
  /** 上游给的模型名，原样。 */
  name: string;
  /** `服务商/模型id`。推断不出来是 `null` —— 界面要让人手动绑。 */
  meta: string | null;
  /**
   * 推断的可靠度。`exact`（名字与目录 id 一致）/ `alias`（归一化或 canonical 对上）/
   * `reseller`（只有转售商条目）/ `none`（没对上）。
   */
  match_kind: "exact" | "alias" | "reseller" | "none";
  label: string | null;
  context_window: number | null;
  max_output_tokens: number | null;
  /** 能力标记，与 `ModelRow.capabilities` 同一套字符串。 */
  capabilities: string[];
  deprecated: boolean;
  /** 看起来像不像对话模型（embedding / 语音 / 生图是 `false`）。 */
  chat_capable: boolean;
};

/** 「从上游拉取模型」的结果。镜像 `upstream_models.rs` 的 `UpstreamModels`。 */
export type UpstreamModels = {
  /** 实际请求的 URL —— 出错时第一件要看的东西。 */
  url: string;
  /** 从 base_url 认出来的目录服务商（匹配元数据时的提示）。 */
  provider_hint: string | null;
  models: MetaMatch[];
};

/**
 * 向渠道的上游要模型清单（`GET {base_url}/v1/models`）并对好元数据。
 *
 * `baseUrl` 留空时用该 `kind` 的协议默认地址。`apiKey` 留空 = 沿用已存的密钥
 * （编辑已有渠道时就是这样：界面手里没有明文密钥）。
 */
export function fetchUpstreamModels(
  kind: string,
  baseUrl: string,
  apiKey: string,
  provider: string | null,
): Promise<UpstreamModels> {
  return invoke<UpstreamModels>("fetch_upstream_models", { kind, baseUrl, apiKey, provider });
}

/** 跨服务商搜目录条目（手动绑元数据用）。空查询返回空表。 */
export function searchCatalogModels(query: string): Promise<CatalogHit[]> {
  return invoke<CatalogHit[]>("search_catalog_models", { query });
}

/** 目录条目的可读摘要（镜像 `catalog.rs` 的 `MetaInfo`）。 */
export type MetaInfo = {
  /** 目录里的规范写法 —— 大小写写错时会纠正过来。 */
  meta: string;
  label: string;
  context_window: number | null;
  max_output_tokens: number | null;
  /** 能力标记，与 `ModelRow.capabilities` 同一套字符串。 */
  capabilities: string[];
  /** 每百万 token 的美元价。目录条目没录 `cost` 时是 `null` —— 那不是免费。 */
  pricing: {
    input: number;
    output: number;
    cache_read: number | null;
    cache_write: number | null;
  } | null;
  deprecated: boolean;
};

/**
 * 一批 meta 在目录里查得到什么。查不到的位置是 `null`（不是错误）——
 * 编辑框据此把那一行标成「未绑定」，而不是等保存时才炸。
 */
export function lookupMetas(metas: string[]): Promise<(MetaInfo | null)[]> {
  return invoke<(MetaInfo | null)[]>("lookup_metas", { metas });
}

export type CatalogHit = {
  /** `服务商/模型id`，可以直接写进 meta。 */
  meta: string;
  provider: string;
  id: string;
  name: string;
  context_window: number | null;
  max_output_tokens: number | null;
  /** 能力标记，与 `ModelRow.capabilities` 同一套字符串。 */
  capabilities: string[];
  deprecated: boolean;
};

/** 一个渠道模板（官方服务商的 kind / base_url / 目录 id）。 */
export type ProviderTemplate = {
  id: string;
  label: string;
  kind: string;
  protocol: string;
  base_url: string;
  name: string;
  catalog: string | null;
  note: string;
};

export function listProviderTemplates(): Promise<ProviderTemplate[]> {
  return invoke<ProviderTemplate[]>("list_provider_templates");
}

/**
 * base_url 对应 models.dev 的哪家（认不出来是 `null`）。
 *
 * 决定「模型能不能从目录直接预填」。与请求发去哪无关：请求永远走配置里的 base_url。
 */
export function detectCatalogProvider(baseUrl: string): Promise<string | null> {
  return invoke<string | null>("detect_catalog_provider", { baseUrl });
}

/**
 * 某个目录服务商下「值得预先填进渠道」的几个模型（同 family 只要最新的，弃用的
 * 与 embedding / 生图都不要）。还没有缓存时 reject。
 */
export function recommendedCatalogModels(providerId: string): Promise<CatalogEntry[]> {
  return invoke<CatalogEntry[]>("recommended_catalog_models", { providerId });
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
export function listTools(cwd?: string | null): Promise<ToolRow[]> {
  return invoke<ToolRow[]>("list_tools", { cwd: cwd ?? null });
}

// ---------------------------------------------------------------------------
// 技能
// ---------------------------------------------------------------------------

/**
 * 技能根的三类归属。镜像 `skills.rs::RootKind`。
 *
 * - `project`：`<会话 cwd>/.carter/skills`，跟着项目走，可写；
 * - `local`：`~/.carter/skills`，用户自己的，可写；
 * - `external`：用户加进来的其它工具目录（Claude Code / Codex），只读。
 */
export type SkillRootKind = "project" | "local" | "external";

export type SkillRow = {
  name: string;
  description: string;
  allowed_tools: string[];
  /** 界面上那个开关的状态：本应用的技能看目录在哪一侧，外部技能看忽略名单。 */
  enabled: boolean;
  /** 这一份 `SKILL.md` 的绝对路径。 */
  path: string;
  /** 这一行来自哪个扫描根（绝对路径）。写操作照它定位。 */
  source: string;
  /** 这一行属于哪一类根。列表按它分组与标注。 */
  scope: SkillRootKind;
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
  /** `project` / `local` 是本应用的目录（不可移除）；`external` = 用户加进来的。 */
  source: SkillRootKind;
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
  /** 扫描根一览：项目 → 用户 → 附加路径（顺序 = 优先级）。 */
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
  /**
   * 写到哪个根（列表里那一行的 `source`）。不给 = 按名字找它现在所在的目录，
   * 新技能落用户目录。「新建」时由编辑框上的选择决定。
   */
  source?: string | null;
};

/**
 * 技能列表。`cwd` 是**当前会话**的工作目录 —— 项目级技能靠它定位
 * （`<cwd>/.carter/skills`，找不到就沿父目录往上找）。没有会话时传 `null`。
 */
export function listSkills(cwd?: string | null): Promise<SkillListing> {
  return invoke<SkillListing>("list_skills", { cwd: cwd ?? null });
}

/** 读技能正文。`source` 给了就只读那一行对应的根（同名技能可能有好几行）。 */
export function readSkill(
  name: string,
  source?: string | null,
  cwd?: string | null,
): Promise<string> {
  return invoke<string>("read_skill", { name, source: source ?? null, cwd: cwd ?? null });
}

/** 保存技能，返回实际写入的根目录。 */
export function saveSkill(
  draft: SkillDraft,
  cwd?: string | null,
): Promise<string> {
  return invoke<string>("save_skill", { draft, cwd: cwd ?? null });
}

/**
 * 启停单个技能：项目级 / 用户级的把目录在 `skills/` 与 `skills.disabled/` 之间搬，
 * 外部路径的（Claude Code / Codex 那边）只记进忽略名单 —— 本应用不碰别人的目录。
 *
 * `source` 必须传**点的是哪一行**（行上的 `source`）。同一个名字可能同时在项目、用户、
 * 外部三个根里出现，能区分它们的只有来源。从「已忽略」名单恢复一个名字时传 `null`。
 *
 * 回传新的 `AppSettings`，调用方要 `setSettings` 对齐，否则下一次「改主题」会用
 * 旧值整体覆盖，把忽略名单清掉。
 */
export function toggleSkill(
  name: string,
  enabled: boolean,
  source: string | null,
  cwd?: string | null,
): Promise<AppSettings> {
  return invoke<AppSettings>("toggle_skill", {
    name,
    enabled,
    source,
    cwd: cwd ?? null,
  });
}

/** 删技能。`source` 指到哪个根就删哪个根 —— 同名的可能在项目与用户两处都有。 */
export function deleteSkill(
  name: string,
  source?: string | null,
  cwd?: string | null,
): Promise<void> {
  return invoke<void>("delete_skill", { name, source: source ?? null, cwd: cwd ?? null });
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

/**
 * 文件预览：点击「本轮变更的文件」里的一行时打开。
 *
 * `path` 可以是相对会话 cwd 的路径，也可以是绝对路径（工具可能写到 cwd 之外）。
 * 后端把「文件不在了 / 是二进制 / 是图片 / 读失败」都收成枚举变体，所以这里
 * 不会 reject —— 调用方按 `kind` 分别渲染即可。
 */
export function previewFile(cwd: string | null, path: string): Promise<FilePreview> {
  return invoke<FilePreview>("preview_file", { cwd, path });
}

/** 一个文件的预览结果。`kind` 决定弹窗里画什么。 */
/**
 * 某一轮里某个文件的 diff（点开预览时按需算）。
 *
 * 只有 cwd 在 git 仓库里、且那一轮记下了轮首基线时才会有 `diff`；其余情况是
 * `unavailable` + 一句原因。
 */
export type FileDiff =
  | { kind: "diff"; path: string; text: string; truncated: boolean }
  | { kind: "empty"; path: string }
  | { kind: "unavailable"; path: string; reason: string };

export function fileDiff(
  sessionId: string,
  turn: number,
  cwd: string,
  path: string,
): Promise<FileDiff> {
  return invoke<FileDiff>("file_diff", { sessionId, turn, cwd, path });
}

/** 「本轮变更的文件」的固定忽略名单。设置页展示用，真实来源在 core 里。 */
export type ChangeRules = { dirs: string[]; files: string[] };

export function changeRules(): Promise<ChangeRules> {
  return invoke<ChangeRules>("change_rules");
}

export type FilePreview =
  | {
      kind: "text";
      path: string;
      bytes: number;
      mtime: number | null;
      content: string;
      /** 文件过大，`content` 只是开头一段（后端已截断）。 */
      truncated: boolean;
    }
  | { kind: "image"; path: string; bytes: number; mtime: number | null; data_url: string }
  | { kind: "binary"; path: string; bytes: number; mtime: number | null }
  | { kind: "dir"; path: string }
  | { kind: "missing"; path: string }
  | { kind: "error"; path: string; message: string };

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

export function getExchange(
  sessionId: string,
  exchangeId: string,
): Promise<ExchangeRecord | null> {
  return invoke<ExchangeRecord | null>("get_exchange", { sessionId, exchangeId });
}
