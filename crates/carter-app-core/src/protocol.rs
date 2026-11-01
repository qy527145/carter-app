//! App ↔ 前端 的 IPC 契约。
//!
//! 只有这一个文件定义跨进程边界的形状。Tauri 命令与事件都从这里取类型，
//! 前端 `src/lib/api.ts` 是它的 TypeScript 镜像 —— 两边必须同步改。
//!
//! ## 为什么事件按会话分包
//!
//! 多个会话可以同时跑（一个在等模型、另一个在等工具）。事件里带 `session_id`，
//! 前端按会话分桶渲染，不会把两个对话的增量混到一个气泡里。
//!
//! ## 工具事件为什么带完整参数与结果
//!
//! 一开始这里只有 `args_preview`（截断字符串）与 `summary`（结果第一行截断到
//! 120 字符）。那样的数据只够画一种卡片：名字 + 一行参数 + 一行结果 —— 于是界面上
//! 每个工具长得都一样。要做差异化呈现（bash 显示命令与输出、todo_write 显示清单、
//! edit_file 显示改动），就必须把**完整的参数 JSON 与结果正文**带过来。
//! 来源是 `tools/record.rs` 那层包装，不是 carter 的 `UiEvent`（它给的就是截断的）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// epoch 毫秒。不引入日期库 —— 这里只需要一个单调可比的数。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 提问
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskOption {
    pub label: String,
    /// 这个选项意味着什么。界面上作为次要说明显示。
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskQuestion {
    pub question: String,
    /// 极短标签，界面上当标题用。
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default)]
    pub options: Vec<AskOption>,
}

// ---------------------------------------------------------------------------
// 前端 → 后端
// ---------------------------------------------------------------------------

/// 前端 → 后端。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// 提交一条用户消息。
    ///
    /// 会话正在跑时**不拒绝，而是排队**：模型写完这一轮就接着处理它。这是
    /// 「边看它干活边补充要求」必须的能力 —— 输入框在生成期间锁死，用户就只能
    /// 等它跑完（可能好几分钟）才能说下一句。
    ///
    /// `text` 里可以带 `/命令`、`@文件`、`[img:...]` —— 展开在会话侧做，
    /// 见 `session.rs::expand_input`。
    SendPrompt {
        text: String,
        /// 前端给这条消息编的号。`turn_started` 会原样带回，界面据此把
        /// 「排队中」的那条挪进对话里 —— 没有它，界面只能猜哪条被接受了。
        #[serde(default)]
        id: Option<u64>,
        /// 插队：中断正在跑的这一轮，让这条消息**下一个**执行。
        #[serde(default)]
        interject: bool,
    },
    /// 把已经排队的某条消息提到最前面，并中断当前轮（界面上那个「插队」按钮）。
    PromotePrompt { id: u64 },
    /// 撤回一条还没开始跑的消息（界面上排队条的「×」）。
    DropPrompt { id: u64 },
    /// 对**一条正在跑的**工具调用做操作（界面上 bash 卡片里那两颗按钮）。
    ///
    /// `call_id` 就是 `tool_started` 事件里那个 id。与 `Cancel` 的区别见
    /// `tools/control.rs` 的模块头：这里只动这一条命令，不动整轮。
    ToolAction {
        call_id: String,
        action: ToolActionKind,
    },
    /// 中断当前轮。已经生成的内容保留。
    Cancel,
    /// 重跑某一轮用户输入（界面上回答下面的「重试」）。
    ///
    /// `turn` 是那一轮的序号，与 `turn_started` / 历史消息上那个数**同一个口径**。
    /// 会话先把历史回退到这一轮之前，再把它原样重跑一遍 —— 所以这一轮之后
    /// 的对话会一并丢掉。注意这只回滚**对话**，不动磁盘：已经改过的文件保持现状。
    Regenerate { turn: u32 },
    /// 切换本会话使用的模型（`provider/model` 形式）。
    SetModel { model: String },
    /// 回答 `ask_user_question` 提的问题。
    ///
    /// `answers` 与问题一一对应，每项是该问题选中的标签（或自由输入）。
    /// 空数组 = 这一问被跳过。
    AnswerForm { id: u64, answers: Vec<Vec<String>> },
    /// 长任务目标（`/goal`）的操作。状态转移规则见 `goal.rs::apply_action`。
    Goal { action: GoalCommand },
    /// 手动压缩上下文（`/compact`）。
    ///
    /// 与自动压缩走同一个函数，区别只在目标值：手动压缩把目标定成 0，于是一定会
    /// 走到摘要那一档（历史太短切不出中段时除外）。生成中收到会排队，这一轮跑完再压。
    Compact,
}

/// 单条工具调用的操作。见 `Command::ToolAction`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolActionKind {
    /// 杀掉这条命令，把「被中断」的结果回灌给模型，这一轮继续。
    Interrupt,
    /// 命令继续跑，工具立刻返回 `shell_id`，之后用 `bash_output` 读。
    Background,
}

/// `/goal` 的具体动作。
///
/// 名字与 CLI 风格一致（`/goal pause` 之类），但这里刻意做成结构化枚举而不是
/// 「把用户打的字符串传下去自己解析」—— 解析在输入框那一侧做一次就够，
/// 后端不该认识「pause」这种拼写。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GoalCommand {
    /// 设定 / 改写目标正文。
    Set { objective: String },
    Clear,
    Pause,
    Resume,
    /// 标记完成（用户在目标条上点「完成」）。
    Complete,
}

// ---------------------------------------------------------------------------
// 后端 → 前端
// ---------------------------------------------------------------------------

/// 后端 → 前端。统一走 `carter://event`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    /// 某个会话里发生的事。
    Session {
        session_id: String,
        event: SessionEvent,
    },
    /// `~/.carter/config.toml` 被界面改过并已热重载。
    ///
    /// 各设置页据此重拉数据 —— 改完渠道后「模型」页上的列表必须跟着变，
    /// 否则用户会看到一份自相矛盾的界面。
    ConfigChanged,
    /// 与具体会话无关的失败（命令层出错等）。
    Error { message: String },
}

/// 一个会话内的事件流。
///
/// 顺序即语义：前端按到达顺序把 `thinking_delta` / `text_delta` 攒进同一个
/// 助手气泡，`tool_started` 与 `tool_finished` 按 `call_id` 配对。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// 会话就绪（刚创建或刚续接）。历史不由事件回放，走 `create_session` 的返回值。
    Ready {
        session_id: String,
        /// 真正发给上游的模型名（`ModelInfo::api_name`）。
        model: String,
        /// 模型引用 `渠道/显示名`。界面上一律显示这一种写法 —— `api_name` 只是
        /// 请求里那串名字，光看它分不出用户配的哪个模型。
        #[serde(default)]
        model_ref: String,
        cwd: String,
        resumed: bool,
    },
    /// 当前模型变了（`/model` 切换）。与 `ready` 分开，是因为它不动会话的其余
    /// 身份（cwd、历史）；界面只需要换掉「当前模型」那一处显示。
    ModelChanged {
        /// 真正发给上游的模型名。
        model: String,
        /// `渠道/显示名`，见 [`SessionEvent::Ready`] 的说明。
        #[serde(default)]
        model_ref: String,
    },
    /// 一轮开始。
    ///
    /// `prompt_id` 只在这条用户消息**刚被接受**时出现（排队队列里拿出来的那一条）；
    /// 自动继续（`/goal`）推起来的一轮没有它——界面靠这个区分「用户说的」和
    /// 「系统推的」，不会把排队消息算到自动继续头上。
    TurnStarted {
        turn: u32,
        #[serde(default)]
        prompt_id: Option<u64>,
        /// 这条用户消息的**原文**（展开 `/命令`、`@文件` 之前）。
        ///
        /// 界面靠它画出用户气泡 —— 不能只靠 `prompt_id` 去自己的排队表里查：
        /// 界面可能刚重连、或者用户点「撤回」的动作晚了一拍，查不到就成了一条
        /// 没有用户发言的回答。有原文就总能画对。
        #[serde(default)]
        prompt: Option<String>,
    },
    /// 历史被回退到第 `turn` 轮之前（用户点了「重试」）。
    ///
    /// 界面收到之后要把这一轮及之后的回答从列表里拿掉 —— 事件不回放，界面自己不删
    /// 就会与后台的历史对不上（刷新一次才「突然少了一截」）。这一轮的**用户气泡**
    /// 要留着：紧接着来的一条 `turn_started` 不带 `prompt`，不会再画一遍。
    ///
    /// 改写提问不走这里：它开一条新分支（见 `sessions::fork`），原会话一个字不动。
    TurnRetracted { turn: u32 },
    /// 思考增量。签名分片不会传到这里（对模型有用，对人没意义）。
    ThinkingDelta { text: String },
    /// 正文增量。
    TextDelta { text: String },
    /// 一段正文结束（换行/收尾），前端据此收束当前气泡。
    TextEnd,
    /// 工具调用即将执行。`args` 是**完整**的参数对象。
    ToolStarted {
        call_id: String,
        name: String,
        args: Value,
    },
    /// 工具执行结束。与 `ToolStarted` 按 `call_id` 配对。
    ///
    /// `content` 是工具回灌给模型的那段正文（可能被截断，截断处有明确标注）。
    ToolFinished {
        call_id: String,
        ok: bool,
        content: String,
        elapsed_ms: u64,
    },
    /// `agent` 工具里各个子 agent 的进度（整表快照）。
    AgentProgress {
        call_id: String,
        agents: Vec<AgentRun>,
    },
    /// todo 列表整表更新。用于底栏那条进度提示。
    TodoUpdated { todos: Vec<TodoView> },
    /// 长任务目标变了（设定 / 暂停 / 恢复 / 完成 / 清除）。`None` = 已经没有目标。
    ///
    /// 会话刚建好时也会发一次：界面据此画出目标条，否则重启后目标要等下一次变化
    /// 才冒出来。
    Goal { goal: Option<GoalView> },
    /// 模型在等用户回答问题。前端渲染成表单，用 `answer_form` 回。
    AskForm { id: u64, questions: Vec<AskQuestion> },
    /// 这一轮**实际**改了哪些文件（相对会话 cwd，`/` 分隔）。
    ///
    /// 来自轮首挂上、轮末收起的文件系统监听（见 `changes.rs`）—— 不是解析工具
    /// 参数，所以 `bash`、子进程、MCP 的写入都在里面。轮末发一次，界面上挂在
    /// 这条回答的末尾。
    FilesChanged { turn: u32, files: Vec<String> },
    /// 上下文占用（输入框旁那个百分比环）。会话就绪、每轮结束、换模型、压缩之后各发一次。
    Context(ContextView),
    /// 开始压缩上下文（界面上把百分比环换成转圈）。
    CompactStarted {
        /// `true` = 占用超过触发线自动压的；`false` = 用户点的 / 打的 `/compact`。
        auto: bool,
    },
    /// 上下文压缩完了，历史被整体替换。
    ///
    /// 界面要拿 `history` **重建整个对话列表**（与打开一条历史会话同一条路）：压缩把
    /// 中段换成了一条摘要，轮次号也跟着重排了 —— 只追加一个提示的话，界面上的「重试 /
    /// 编辑」会指向已经不存在的轮次。`prompts` / `changes` 是按新轮次号重排过的那两份。
    Compacted {
        history: Vec<crate::view::MessageView>,
        prompts: Vec<crate::prompts::TurnPrompt>,
        changes: Vec<crate::changes::TurnFiles>,
        /// 压缩前后的估算占用（只算历史消息）。
        before: u64,
        after: u64,
        /// 用上了哪一档摘要：`L3`（结构化）/ `L2`（要点）。
        tier: String,
        /// `true` = 占用超过触发线自动压的（含 carter 在一轮中途自己压的）。
        auto: bool,
    },
    /// 一轮的用量与成本。
    TurnUsage {
        usage: UsageView,
        cost: f64,
        model: String,
    },
    /// 提示（压缩日志、上游流内错误、`@文件` 展开情况等）。
    Notice { message: String },
    /// 会话标题自动生成好了（首轮之后）。前端据此刷新会话列表 ——
    /// 侧栏那一行与顶部标题都取自同一份列表，刷新一处两处都对。
    Title { title: String },
    /// 一轮结束，前端可以解除「正在生成」状态。
    Idle,
    /// 本轮失败。不致命：会话还在，可以继续发消息。
    Error { message: String },
}

/// 上下文占用。
///
/// `used` 是**下一次请求**大概会带上的 token 数：历史消息 + 系统提示 + 工具声明，
/// 按模型的 tokenizer 估算。不用上游回报的 usage —— 一轮里有好几次模型调用，
/// carter 回报的是整轮累加值；而开了缓存之后 `input_tokens` 只是未命中的那一截。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextView {
    pub used: u64,
    /// 模型的上下文窗口（`ModelInfo::context_window`）。0 = 不知道。
    pub window: u64,
    /// 自动压缩的触发线（`window × agent.compact_threshold_ratio`）。
    pub threshold: u64,
    /// 自动压缩开着没有（`agent.compact_enabled`）。
    pub auto: bool,
}

/// `agent` 工具里一个子 agent 的当前状态。整表随 [`SessionEvent::AgentProgress`] 下发。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRun {
    /// 主对话给这一行的短标题。
    pub description: String,
    /// `running` | `done` | `failed` | `cancelled`。
    pub status: String,
    /// 已经发起了几次工具调用 —— 「一条命令跑了很久」和「根本没动」能看出区别。
    pub steps: u32,
    /// 此刻正在跑的工具名。`None` = 在思考或已经结束。
    pub tool: Option<String>,
    /// 最近一段输出（尾部截断）。
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoView {
    pub content: String,
    pub active_form: String,
    /// `pending` | `in_progress` | `completed`
    pub status: String,
}

/// 长任务目标的界面视图。
///
/// `status` 是机器用的（`active` / `paused` / `completed` / `blocked`），
/// `status_label` 是给人看的 —— 中文标签由后端给，免得前端再维护一份映射。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalView {
    pub objective: String,
    pub status: String,
    pub status_label: String,
    /// 已经自动继续了多少轮。
    pub auto_continues: u32,
    /// 模型在 `goal_update` 里写的说明（被卡住 / 需要拍板时界面要显出来）。
    pub note: Option<String>,
}

/// 用量视图。
///
/// 字段名与底座保持一致（`input_tokens` 是**未命中缓存**的输入），
/// 因为界面要显示 cache 命中情况，改名会让人对不上账。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UsageView {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub thinking_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

impl From<&llm_adapt_core::Usage> for UsageView {
    fn from(u: &llm_adapt_core::Usage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            thinking_tokens: u.thinking_tokens,
            cache_read_tokens: u.cache.read_tokens,
            cache_write_tokens: u.cache.write.total,
        }
    }
}
