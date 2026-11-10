import type {
  BackgroundShellView,
  MemoryCandidate,
  ReportTag,
  TurnFiles,
  TurnPrompt,
  TurnTime,
} from "./session";

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
      /** 助理记进来的工单消息：属于哪张工单。 */
      report?: ReportTag | null;
    }
  /**
   * 一轮进行中用户补的话，这一次请求就带给模型了。不开新的一轮：气泡画在这一轮
   * 已经产出的内容后面。`prompt_id` 用来把它从待发条摘掉。
   */
  | { type: "prompt_injected"; prompt_id: number; prompt: string }
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
  /**
   * 后台命令表变了（启动 / 从前台转后台 / 自己结束 / 被结束）。
   *
   * 整表快照，只含**还在跑的**。前台那一轮可能已经结束而命令还在跑（起服务、
   * 长构建、`tail -f`），界面要在输入框上方留一条提醒，让用户知道「还没完，
   * 跑完会自动唤醒接着做」。空数组 = 全跑完了。
   */
  | { type: "background_shells"; shells: BackgroundShellView[] }
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
  /**
   * 助理刚给一张工单开出了执行会话。会话文件这一下已经带着标题落盘了，
   * 界面收到要**立刻**重拉会话列表 —— 左边那条对话不该等它跑出第一句话才出现。
   */
  | { type: "ticket_session_opened"; session_id: string }
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
