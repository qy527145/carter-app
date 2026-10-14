//! App ↔ 前端 的 IPC 契约。
//!
//! 只有这一个文件定义跨进程边界的形状。Tauri 命令与事件都从这里取类型，
//! 前端 `src/lib/api.ts` 是它的 TypeScript 镜像 —— 两边必须同步改。
//!
//! ## 为什么事件按会话分包
//!
//! 多个会话可以同时跑（一个在等模型、另一个在等工具）。事件里带 `session_id`，
//! 前端按会话分桶渲染，不会把两个对话的增量混到一个气泡里。

use serde::{Deserialize, Serialize};

/// epoch 毫秒。不引入日期库 —— 这里只需要一个单调可比的数。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 前端 → 后端。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// 提交一条用户消息。会话正在跑时会被拒绝（先 Cancel 或等 Idle）。
    SendPrompt { text: String },
    /// 中断当前轮。已经生成的内容保留。
    Cancel,
    /// 切换本会话使用的模型（`provider/model` 形式）。
    SetModel { model: String },
    /// 回答模型通过 `ask_user_question` 工具提的问题。
    AnswerAsk { id: u64, answer: String },
}

/// 后端 → 前端。统一走 `carter://event`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    /// 某个会话里发生的事。
    Session {
        session_id: String,
        event: SessionEvent,
    },
    /// 与具体会话无关的失败（命令层出错等）。
    Error { message: String },
}

/// 一个会话内的事件流。
///
/// 顺序即语义：前端按到达顺序把 `thinking_delta` / `text_delta` 攒进同一个
/// 助手气泡，`tool_call_started` 与 `tool_result` 按 `call_id` 配对。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// 会话就绪（刚创建或刚续接）。历史不由事件回放，走 `session_history` 命令取。
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
    /// 工具调用即将执行。
    ToolCallStarted {
        call_id: String,
        name: String,
        args_preview: String,
    },
    /// 工具执行结束。与 `ToolCallStarted` 按 `call_id` 配对。
    ToolResult {
        call_id: String,
        ok: bool,
        summary: String,
    },
    /// todo 列表整表更新。
    TodoUpdated { todos: Vec<TodoView> },
    /// 模型主动提问，等用户作答（`answer_ask` 命令）。
    AskUser {
        id: u64,
        question: String,
        options: Vec<String>,
    },
    /// 一轮的用量与成本。
    TurnUsage {
        usage: UsageView,
        cost: f64,
        model: String,
    },
    /// 提示（压缩日志、上游流内错误等）。
    Notice { message: String },
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
