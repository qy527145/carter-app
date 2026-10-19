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
    /// 提交一条用户消息。会话正在跑时会被拒绝（先 Cancel 或等 Idle）。
    ///
    /// `text` 里可以带 `/命令`、`@文件`、`[img:...]` —— 展开在会话侧做，
    /// 见 `session.rs::expand_input`。
    SendPrompt { text: String },
    /// 中断当前轮。已经生成的内容保留。
    Cancel,
    /// 切换本会话使用的模型（`provider/model` 形式）。
    SetModel { model: String },
    /// 回答 `ask_user_question` 提的问题。
    ///
    /// `answers` 与问题一一对应，每项是该问题选中的标签（或自由输入）。
    /// 空数组 = 这一问被跳过。
    AnswerForm { id: u64, answers: Vec<Vec<String>> },
    /// 长任务目标（`/goal`）的操作。状态转移规则见 `goal.rs::apply_action`。
    Goal { action: GoalCommand },
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
        model: String,
        cwd: String,
        resumed: bool,
    },
    /// 一轮开始。
    TurnStarted { turn: u32 },
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
    /// todo 列表整表更新。用于底栏那条进度提示。
    TodoUpdated { todos: Vec<TodoView> },
    /// 长任务目标变了（设定 / 暂停 / 恢复 / 完成 / 清除）。`None` = 已经没有目标。
    ///
    /// 会话刚建好时也会发一次：界面据此画出目标条，否则重启后目标要等下一次变化
    /// 才冒出来。
    Goal { goal: Option<GoalView> },
    /// 模型在等用户回答问题。前端渲染成表单，用 `answer_form` 回。
    AskForm { id: u64, questions: Vec<AskQuestion> },
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
