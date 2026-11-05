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
      /** 这一轮被问的时刻（epoch 毫秒）。用户气泡旁边显示它。 */
      started_at?: number;
    }
  /**
   * 一轮跑完了（回答已经有了，或这一轮失败/被中断）。
   *
   * 与 `idle` 分开：长任务目标会在一轮结束之后立刻推起下一轮，整条自动继续链
   * 走完之前 `idle` 根本不会来 —— 而每条回答完成后就要显示完成时刻。
   */
  | {
      type: "turn_ended";
      turn: number;
      /** 跑完的时刻（epoch 毫秒）。 */
      at: number;
      /** 这一轮失败了。 */
      failed?: boolean;
      /** 用户中途按了停止：回答不完整，时间照样显示。 */
      cancelled?: boolean;
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
      changes_since: number;
      /** 每一轮的起止时刻，按重建后的轮次号。 */
      times?: TurnTime[];
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
   * 与前端的 `write_file` / `edit_file` 参数合并后汇总在底栏的变更条里（后者能补上
   * 写在 cwd 之外的文件，并提供「写入 / 修改」的来源标签）。
   */
  | { type: "files_changed"; turn: number; files: string[] }
  /** 变更条被清空：从第 `since` 轮起重新统计。 */
  | { type: "changes_cleared"; since: number }
  | { type: "turn_usage"; usage: UsageView; cost: number; model: string }
  /** 上下文占用（输入框旁的百分比环）。就绪、每次主对话请求、每轮结束、换模型、压缩之后各来一次。 */
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
      changes_since: number;
      /** 每一轮的起止时刻，按新轮次号重排过。 */
      times?: TurnTime[];
      /** 压缩前后的估算占用（只算历史消息）。 */
      before: number;
      after: number;
      /** `L3`（结构化摘要）/ `L2`（要点摘要）。 */
      tier: string;
      auto: boolean;
    }
  | { type: "notice"; message: string }
  | { type: "assistant_episode"; number: number }
  /**
   * 这一轮里有值得长期记住的东西，请用户确认。轮末异步到达（可能晚于 `idle`）。
   * 确认走 `acceptMemory`，不经过会话。
   */
  | { type: "memory_suggestion"; candidates: MemoryCandidate[] }
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
  /** 常驻助理的状态变了。面板与侧栏角标据此重拉 `getAssistant()`。 */
  | { type: "assistant_changed" }
  /** 工单进入要人处理的状态。系统通知由外壳发，界面只需要重拉。 */
  | { type: "assistant_alert"; ticket_id: string; title: string; body: string }
  /** 托盘菜单点了「助理面板」：切过去。 */
  | { type: "assistant_open" }
  | { type: "assistant_turn_ended"; session_id: string }
  /** 一条会话给另一条发了信（跨会话消息）。收信方可能刚在后台被续接起来。 */
  | { type: "peer_message"; from: string; to: string; preview: string }
  /**
   * IM 连接器底层命令的一行输出（安装脚本 / 登录流程）。弹窗里的日志框逐行追加。
   */
  | { type: "connector_output"; vendor: string; line: string }
  /** 扫码登录的授权页地址。换成一个「打开授权页」按钮 —— 浏览器没起来时靠它。 */
  | { type: "connector_auth_url"; vendor: string; url: string }
  /** 一个连接器动作（装 CLI / 装技能 / 登录 / 退出）结束了。界面据此重拉整张卡片。 */
  | { type: "connector_done"; vendor: string; ok: boolean; message: string | null }
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
 * 上下文占用。`used` 以最近一次主对话请求上游回报的 usage 为准（完整输入 + 输出）；
 * 还没有回报时（刚打开、刚压缩完）退回本地估算（历史 + 系统提示 + 工具声明）。
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
  | { type: "compact" }
  /** 清空底栏的变更条，从下一轮起重新统计。生成中发的话排队，这一轮跑完再清。 */
  | { type: "clear_changes" }
  /** 助理对话立刻开新一期（之前的对话只留纪要与记忆）。别的会话收到时什么都不做。 */
  | { type: "new_episode" };

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
   * 变更条额外忽略的规则，每条一个 glob，相对会话 cwd。
   *
   * 含 `/` 按相对路径匹配（`dist/**`），不含 `/` 按任意层级的名字匹配（`*.log`）。
   * 内置目录规则与 `.gitignore` 不在这里，它们始终生效。
   */
  changes_ignore: string[];
  /** 全局昵称：加入新团队时预填的称呼。空 = 用 git 的 `user.name`。 */
  nickname: string;
  /** 从 GitHub 下载（连接器安装、更新）时允许走加速站。 */
  github_accel: boolean;
  /** 显式代理，如 `http://127.0.0.1:7897`。空 = 直连。 */
  http_proxy: string;
  /** models.dev 的额外源，一行一个完整 URL，排在内置源之后试。 */
  model_sources: string;
  /** 启动后自动检查新版本。关掉只剩「关于」页的手动入口。 */
  auto_check_update: boolean;
};

/** models.dev 缓存是哪一次、从哪个源拉的。镜像 `catalog::CacheMeta`。 */
export type ModelsMeta = {
  source: string;
  fetched_at: number;
  count: number;
  snapshot_date: string;
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
  /** models.dev 缓存的来源记录；从没刷新过是 `null`。 */
  models_meta: ModelsMeta | null;
  /** 编进安装包的目录快照是哪天的（`YYYY-MM-DD`）。 */
  models_snapshot_date: string;
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
// 自更新
//
// 查、下、装全在后端（`crates/carter-app-core/src/update.rs`）。Release 里放的是
// 三平台的原生安装包，不是 Tauri 更新器那一套 `latest.json` + 归档 —— 这里没有
// 「签名」这一步，装之前由界面把文件名与大小报给用户确认。
// ---------------------------------------------------------------------------

/** 下载进度事件。与 `src-tauri/src/lib.rs` 的 `UPDATE_PROGRESS` 必须一致。 */
export const UPDATE_PROGRESS = "carter://update-progress";

/** 「发现新版本」事件（启动后自动检查时发出）。与 `UPDATE_AVAILABLE` 一致。 */
export const UPDATE_AVAILABLE = "carter://update-available";

/** GitHub Release 里挑出来的那个安装包。 */
export type ReleaseInfo = {
  /** `v0.1.2`。 */
  tag: string;
  /** 去掉 `v` 的版本号。 */
  version: string;
  /** Release 页面，界面上的「更新说明」跳这里。 */
  html_url: string;
  /** 安装包文件名，如 `Carter_0.1.2_aarch64.dmg`。 */
  asset_name: string;
  /** 下载地址（后端按仓库名拼出来的，前端只回显，不能改）。 */
  asset_url: string;
  asset_size: number;
};

export type UpdateCheck = {
  has_update: boolean;
  /** 当前运行的版本。 */
  current: string;
  /** 有新版时才有；没有新版时是 `null`。 */
  release: ReleaseInfo | null;
};

export function checkUpdate(): Promise<UpdateCheck> {
  return invoke<UpdateCheck>("check_update");
}

/** 下载安装包，返回落盘路径。进度走 [`UPDATE_PROGRESS`] 事件。 */
export function downloadUpdate(): Promise<string> {
  return invoke<string>("download_update");
}

export function cancelUpdateDownload(): Promise<void> {
  return invoke<void>("cancel_update_download");
}

/** 把下载好的安装包交给系统装。返回给用户看的一句话。 */
export function installUpdate(path: string): Promise<string> {
  return invoke<string>("install_update", { path });
}

/** 重启应用（macOS 用 `ditto` 覆盖后在跑的这一份已经是新的了）。 */
export function restartApp(): Promise<void> {
  return invoke<void>("restart_app");
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
  /** 编辑时的原渠道名；与 `name` 不同即改名。 */
  rename_from?: string | null;
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
export function checkServerTools(kind: string, tools: string[]): Promise<[string[], string[]]> {
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

export type AssistantMemory = {
  id: string;
  text: string;
  source: string;
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
export function saveAssistantMemory(id: string | null, text: string): Promise<void> {
  return invoke<void>("save_assistant_memory", { id, text });
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
export function saveSkill(draft: SkillDraft, cwd?: string | null): Promise<string> {
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
export function listFiles(cwd: string | null, query: string, limit = 50): Promise<string[]> {
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
 * 文件预览：点击变更条里的一行时打开。
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
 * 某个文件从第 `since` 轮起累计的 diff（点开预览时按需算）。
 *
 * 只有 cwd 在 git 仓库里、且那几轮记下了轮首基线时才会有 `diff`；其余情况是
 * `unavailable` + 一句原因。
 */
export type FileDiff =
  | { kind: "diff"; path: string; text: string; truncated: boolean }
  | { kind: "empty"; path: string }
  | { kind: "unavailable"; path: string; reason: string };

export function fileDiff(
  sessionId: string,
  since: number,
  cwd: string,
  path: string,
): Promise<FileDiff> {
  return invoke<FileDiff>("file_diff", { sessionId, since, cwd, path });
}

/** 变更条的固定忽略名单。设置页展示用，真实来源在 core 里。 */
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

export function getExchange(sessionId: string, exchangeId: string): Promise<ExchangeRecord | null> {
  return invoke<ExchangeRecord | null>("get_exchange", { sessionId, exchangeId });
}

// ---------------------------------------------------------------------------
// IM 连接器
// ---------------------------------------------------------------------------
//
// 连接器底下是「官方 CLI + 配套 skill」两层。三层状态分开给：CLI 装了没有、
// skill 装了没有、登录态还有效没有 —— 只有一个「连不上」的话，用户猜错的每一次
// 都是一轮无效操作。镜像 `connectors.rs` 的同名结构体。

export type ConnectorCli = {
  name: string;
  /** 找到的可执行文件绝对路径。`null` = 没装（或装了但不在找得到的地方）。 */
  path: string | null;
  version: string | null;
};

export type ConnectorSkill = {
  name: string;
  /** 技能目录（`~/.carter/skills/<name>`）。 */
  dir: string;
  installed: boolean;
  /** 在 `skills.disabled/` 里：装了但被停用，技能页能把它打开。 */
  disabled: boolean;
  /** `SKILL.md` 里写的 CLI 版本要求。只用来提示，不做版本比较。 */
  requires_cli: string | null;
};

export type ConnectorAuth = {
  logged_in: boolean;
  user_name: string | null;
  corp_name: string | null;
  corp_id: string | null;
  /** access token 过期时间（RFC3339）。刷新由 CLI 自己做。 */
  expires_at: string | null;
  /** 没登录 / 有问题时的一句话。 */
  message: string | null;
  hint: string | null;
};

export type ConnectorRow = {
  id: string;
  name: string;
  blurb: string;
  docs: string;
  /** 「装不上」时给人看的那条手动命令。 */
  install_hint: string;
  cli: ConnectorCli;
  skill: ConnectorSkill;
  auth: ConnectorAuth;
  /** 三层都齐了。 */
  ready: boolean;
  /** 登录方式：扫码 / 先配应用再扫码 / 填邮箱账号。 */
  auth_kind: "oauth" | "app_then_oauth" | "account";
};

export function listConnectors(): Promise<ConnectorRow[]> {
  return invoke<ConnectorRow[]>("list_connectors");
}

/** 跑官方安装脚本（下载几十 MB）。输出走 `Connector*` 事件。 */
export function installConnectorCli(vendor: string): Promise<void> {
  return invoke<void>("install_connector_cli", { vendor });
}

/** 把官方 skill 装进 `~/.carter/skills`，会话里立刻可用。 */
export function installConnectorSkill(vendor: string): Promise<void> {
  return invoke<void>("install_connector_skill", { vendor });
}

/**
 * 弹出浏览器扫码登录。`recommend` = 顺带授予服务端推荐权限（官方给 Agent 用的一条路）。
 *
 * 最长可能等 15 分钟（组织没开 CLI 权限时 `dws` 还会等管理员审批），期间输出与
 * 授权页地址都走事件。**取消**用 `cancelConnectorLogin` —— 它是一条独立的同步命令，
 * 因为这条 invoke 还没返回时也得能叫停。
 */
export function loginConnector(vendor: string, recommend: boolean): Promise<void> {
  return invoke<void>("login_connector", { vendor, recommend });
}

export function cancelConnectorLogin(vendor: string): Promise<void> {
  return invoke<void>("cancel_connector_login", { vendor });
}

export function logoutConnector(vendor: string): Promise<void> {
  return invoke<void>("logout_connector", { vendor });
}

/**
 * 配置应用（飞书）：`config init --new`，一次性在浏览器里创建应用，之后才能登录。
 * 和登录一样会阻塞到用户完成，输出与授权页地址走事件，取消用 `cancelConnectorLogin`。
 */
export function configureConnectorApp(vendor: string): Promise<void> {
  return invoke<void>("configure_connector_app", { vendor });
}

export type MailAccountDraft = {
  name: string;
  email: string;
  imapHost: string;
  smtpHost: string;
  /** 密码 / 授权码。只经环境变量交给 CLI，由它存进系统钥匙串。 */
  password: string;
};

/** 添加邮箱账号（邮件）。 */
export function addMailAccount(vendor: string, d: MailAccountDraft): Promise<void> {
  return invoke<void>("add_mail_account", {
    vendor,
    name: d.name,
    email: d.email,
    imapHost: d.imapHost,
    smtpHost: d.smtpHost,
    password: d.password,
  });
}

/** 移除邮箱账号（连同钥匙串里的密码）。 */
export function removeMailAccount(vendor: string, name: string): Promise<void> {
  return invoke<void>("remove_mail_account", { vendor, name });
}

// ---------------------------------------------------------------------------
// 团队目录（目标 / 任务 / 看板）
// ---------------------------------------------------------------------------

/**
 * 这个工作目录（或它上面某一层）的团队目录状态。
 *
 * `null` = 还没初始化。界面上「初始化」与「展示」是两种完全不同的样子，
 * 所以这一条要能明确回答「有没有」，而不是返回一个全空的假对象。
 */
export type TeamStatus = {
  root: string;
  project_root: string;
  name: string;
  remote: string | null;
  initialized: boolean;
  goals: number;
  issues: number;
};

export type TeamInitOutcome = {
  root: string;
  project_root: string;
  steps: string[];
  commit: string | null;
  identity: { name: string; email: string };
  remote: string | null;
  sync_source: boolean;
  warnings: string[];
};

export type TeamCounts = {
  total: number;
  todo: number;
  doing: number;
  blocked: number;
  done: number;
  dropped: number;
};

export type TeamTask = {
  id: string;
  title: string;
  /** `todo` | `doing` | `blocked` | `done` | `dropped` */
  status: string;
  status_label: string;
  priority: string;
  assignee: string | null;
  parent_id: string | null;
  depends_on: string[];
  children: string[];
  /** 还没完成的前置任务。 */
  waiting_on: string[];
  /** 0~100。有子任务时按子任务算。 */
  progress: number;
  /** 前置全完成、状态是待办 —— 可以直接开工。 */
  ready: boolean;
  blocked_reason: string;
  estimate_hours: number | null;
  due_at: number | null;
};

export type TeamGoal = {
  id: string;
  title: string;
  description: string;
  /** `active` | `paused` | `done` | `archived` */
  status: string;
  status_label: string;
  priority: string;
  owner: string | null;
  due_at: number | null;
  percent: number;
  counts: TeamCounts;
  /** 根任务 id，按顺序。 */
  roots: string[];
  tasks: TeamTask[];
};

/** 一块看板的元信息。正文是一整页 HTML，用 [`teamBoardHtml`] 按需取。 */
export type TeamBoard = {
  /** `boards/` 下的文件名，不带 `.html`。 */
  id: string;
  title: string;
  goal_id: string | null;
};

/**
 * 一条团队事件（协议见 `docs/team-protocol.md`）。
 *
 * `kind` 是开放的字符串：新版客户端写的、本版不认识的种类也要能显示出来，
 * 不能吞掉 —— 发的人以为对方看到了。
 */
export type TeamEvent = {
  /** 提交的短 SHA。只用来显示；引用请用 `eid`。 */
  sha: string;
  /** 协议版本。0 = 旧版打在代码分支上的事件。 */
  v: number;
  /** `comment` | `mention` | `broadcast` | `request` | `assign` | `handoff` | `claim` | `release` | `join` | `note` | `sync` | … */
  kind: string;
  /** 稳定的事件 id（`e-…`）。旧事件用 SHA 顶上。 */
  eid: string;
  target: string | null;
  goal_id: string | null;
  /** 发出者的成员 id。旧事件没有，按作者邮箱去名单里认。 */
  from: string | null;
  /** 收件人成员 id，`*` = 全员。 */
  to: string[];
  /** `"carter"` = 由 agent 代发。 */
  via: string | null;
  reply_to: string | null;
  due: number | null;
  body: string;
  subject: string;
  author: { name: string; email: string } | null;
  at: number;
  legacy: boolean;
};

/** 收件箱里的一条：事件 + 为什么在这里。 */
export type TeamInboxItem = TeamEvent & {
  reason: "mention" | "broadcast" | "assigned" | "my_task";
};

export type TeamMember = {
  id: string;
  name: string;
  emails: string[];
  role: string;
  joined_at: number;
};

export type TeamPeople = {
  members: TeamMember[];
  /** 本机的成员 id。`null` = 还没加入这个团队。 */
  me: string | null;
  /** 本机的 git 身份（`名字 <邮箱>`）。没配 git 时为空，那时没法加入。 */
  identity: string | null;
  /** 提交过代码、还没加入的人（`名字 <邮箱>`）。 */
  pending: string[];
};

export type TeamOverview = {
  root: string;
  name: string;
  remote: string | null;
  identity: string | null;
  goals: TeamGoal[];
  boards: TeamBoard[];
  events: TeamEvent[];
  /** 读不出来的文件。给人看的诊断信息。 */
  warnings: string[];
};

export type TeamTodo = {
  id: string;
  title: string;
  status: string;
  status_label: string;
  priority: string;
  goal_id: string;
  goal_title: string;
  blocked_reason: string;
  /** 还差几个前置。 */
  waiting_on: number;
  ready: boolean;
};

/** 这个工作目录的团队目录状态。`null` = 没初始化过。 */
export function teamStatus(cwd: string): Promise<TeamStatus | null> {
  return invoke<TeamStatus | null>("team_status", { cwd });
}

/** 初始化团队目录。会 `git init`（必要时）并提交一次。 */
export function teamInitialize(
  projectRoot: string,
  name?: string | null,
): Promise<TeamInitOutcome> {
  return invoke<TeamInitOutcome>("team_initialize", { projectRoot, name: name ?? null });
}

/** 全景：目标 + 任务树 + 看板 + 最近事件。界面一次拉完。 */
export function teamOverview(cwd: string): Promise<TeamOverview> {
  return invoke<TeamOverview>("team_overview", { cwd });
}

/** 一块看板的整页 HTML。 */
export function teamBoardHtml(cwd: string, boardId: string): Promise<string> {
  return invoke<string>("team_board_html", { cwd, boardId });
}

/** 我名下未完成的任务。消息框下方的待办条用它。 */
export function teamMyTasks(cwd: string): Promise<TeamTodo[]> {
  return invoke<TeamTodo[]>("team_my_tasks", { cwd });
}

/**
 * 把团队数据的改动提交成一条 git 提交。返回提交短 SHA；没有改动时是 `null`。
 *
 * 由界面在**轮到结束**时调用 —— 一轮里模型可能改了好多处，那是同一件事。
 */
export function teamCommit(cwd: string, message: string): Promise<string | null> {
  return invoke<string | null>("team_commit", { cwd, message });
}

/** 立刻同步一次（拉远端、只做快进）。观察源也会定时跑。 */
export function teamSync(cwd: string): Promise<string> {
  return invoke<string>("team_sync", { cwd });
}

/**
 * 配（或改）远端，并顺手把当前分支推上去。
 *
 * 远端不必叫 `origin` —— 后端会自动找已有的那个。界面上给一个输入框就够了，
 * 不该让人为了同步去开终端。
 */
export function teamSetRemote(cwd: string, url: string): Promise<string> {
  return invoke<string>("team_set_remote", { cwd, url });
}

/** 团队里有谁、本机是谁。 */
export function teamPeople(cwd: string): Promise<TeamPeople> {
  return invoke<TeamPeople>("team_people", { cwd });
}

/**
 * 加入团队。给 `memberId` = 「我是这个人，这是我的另一台机器」（把本机邮箱挂到他名下）；
 * 否则用 `name` 新建一个成员。已经在名单里时什么都不做。
 */
export function teamJoin(
  cwd: string,
  join: { name: string } | { memberId: string },
): Promise<{ member: TeamMember; inbox_source: string | null }> {
  return invoke<{ member: TeamMember; inbox_source: string | null }>("team_join", {
    cwd,
    name: "name" in join ? join.name : null,
    memberId: "memberId" in join ? join.memberId : null,
  });
}

export function teamRenameMe(cwd: string, name: string): Promise<TeamMember> {
  return invoke<TeamMember>("team_rename_me", { cwd, name });
}

/**
 * 团队操作：指派、认领、评论、@、广播……参数与 `team` 工具一致（`action` + 各自的字段），
 * 后端走同一份实现，只是不带「经卡特」的标记。返回给人看的一句结果。
 */
export type TeamAction =
  | { action: "assign"; task_id: string; assignee: string; text?: string }
  | { action: "task_claim" | "task_release"; task_id: string }
  | { action: "task_update"; task_id: string; assignee: string }
  /**
   * 推进任务状态。看板上拖一下就走这条 —— 原来只有模型能改状态，人要在界面上
   * 把一件事标成完成，只能开一条对话让卡特去做。
   *
   * `blocked` 必须带 `blocked_reason`（后端会拒），其余状态带了也会写进去。
   */
  | {
      action: "task_status";
      task_id: string;
      status: "todo" | "doing" | "blocked" | "done" | "dropped";
      blocked_reason?: string;
    }
  | { action: "comment"; task_id?: string; goal_id?: string; text: string; to?: string[] }
  | { action: "mention"; to: string[]; text: string; task_id?: string; goal_id?: string }
  | { action: "request"; to: string[]; text: string; task_id?: string; due_at?: number }
  | { action: "broadcast"; text: string; goal_id?: string };

export function teamAct(cwd: string, args: TeamAction): Promise<string> {
  return invoke<string>("team_act", { cwd, args });
}

/** 跟本机有关的消息，新的在前。`fetch` = 先拉一次远端。 */
export function teamInbox(cwd: string, fetch = false): Promise<TeamInboxItem[]> {
  return invoke<TeamInboxItem[]>("team_inbox", { cwd, fetch });
}
