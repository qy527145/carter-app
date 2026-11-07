use super::helpers::*;
use super::*;

/// carter `run_turn` 的渲染出口 → IPC 事件。
///
/// 工具卡片不从这里走：那一路是 [`crate::tools::record::Recorded`] 发的（带完整的
/// 参数与结果），carter 那两条只有截断的预览。
pub(super) struct TxSink {
    pub(super) shell: Arc<AppShell>,
    pub(super) session_id: String,
    /// 进行中那一轮的记录，见 `shell.rs::LiveLog`。
    pub(super) live: Option<Arc<crate::shell::LiveLog>>,
    /// 待办落在这里（`thread.todos` 由 carter 自己维护并复诵给模型）。
    pub(super) todos: Arc<arc_swap::ArcSwap<Vec<carter::tools::TodoItem>>>,
    /// carter 在一轮**中途**自己压缩过时的摘要档位（见 [`Runner::after_turn`]）。
    pub(super) mid: Arc<std::sync::Mutex<Option<String>>>,
}

impl UiSink for TxSink {
    fn emit(&mut self, ev: UiEvent) {
        let event = match ev {
            UiEvent::AssistantTextDelta(text) => {
                if let Some(live) = &self.live {
                    live.text(&text);
                }
                SessionEvent::TextDelta { text }
            }
            UiEvent::ThinkingDelta(text) => SessionEvent::ThinkingDelta { text },
            UiEvent::AssistantTextEnd => {
                // 一轮里的每个文本 / 思考块结束时都会来一次。`live` 里几段之间补个
                // 空行，切走再回来时读起来才不像连成一句。
                if let Some(live) = &self.live {
                    live.text("\n");
                }
                SessionEvent::TextEnd
            }
            UiEvent::TodoUpdated(todos) => {
                self.todos.store(Arc::new(todos.clone()));
                SessionEvent::TodoUpdated {
                    todos: todos.iter().map(todo_view).collect(),
                }
            }
            UiEvent::TurnUsage { usage, cost, model } => SessionEvent::TurnUsage {
                usage: UsageView::from(&usage),
                cost,
                model,
            },
            UiEvent::Notice(msg) => {
                // carter 的压缩过程日志（英文、按档逐条）不往界面送：摘要成了的话
                // 轮后会有一条 `compacted` 事件把对话整个重建。失败的那几句照常送。
                if let Some(tier) = summary_tier(&msg) {
                    if let Ok(mut slot) = self.mid.lock() {
                        *slot = Some(tier);
                    }
                    return;
                }
                if msg.starts_with("[compact] L0") || msg.starts_with("[compact] L1") {
                    return;
                }
                SessionEvent::Notice { message: msg }
            }
            UiEvent::Divider(label) => SessionEvent::Notice { message: label },
            UiEvent::ModelChanged(model) => SessionEvent::Notice {
                message: format!("模型：{model}"),
            },
            // 下面这些由我们自己发（标题、历史回放），或已被顶掉的实现接管。
            UiEvent::Title(_)
            | UiEvent::ReplayHistory(_)
            | UiEvent::Idle
            | UiEvent::ToolCallStarted { .. }
            | UiEvent::ToolResult { .. }
            | UiEvent::AskUser { .. } => return,
        };
        self.shell.emit(AppEvent::Session {
            session_id: self.session_id.clone(),
            event,
        });
    }
}

/// 套在主模型 provider 外面：**每次主对话请求**收到上游的 usage 就刷新一次百分比环。
///
/// 一轮里工具回灌会让模型被调好几次，只在轮末刷新的话，一条长任务跑上十几分钟环
/// 都纹丝不动；carter 的 `TurnUsage` 又是整轮累加的，拿它算占用会越算越大 ——
/// 所以在流上直接看每一次请求自己的 usage。
///
/// 只套主对话：压缩走 `compact_model`、子 agent 走它们自己的线程，它们的 usage
/// 不会把环带歪。
pub(super) struct ContextTap {
    pub(super) inner: Arc<dyn carter::provider::LlmProvider>,
    pub(super) shell: Arc<AppShell>,
    pub(super) session_id: String,
    pub(super) window: u32,
    pub(super) reported: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl carter::provider::LlmProvider for ContextTap {
    async fn stream(
        &self,
        req: carter::provider::ChatRequest,
    ) -> carter::Result<carter::provider::EventStream> {
        use carter::provider::{RequestPurpose, StreamChunk, Usage};
        use futures::StreamExt;

        let stream = self.inner.stream(req.clone()).await?;
        if req.wire.meta.purpose != RequestPurpose::MainTurn {
            return Ok(stream);
        }
        let shell = self.shell.clone();
        let session_id = self.session_id.clone();
        let window = self.window;
        let reported = self.reported.clone();
        // 用量常被拆成几帧下发（Claude 的 `message_start` 给输入、`message_delta`
        // 给输出），按非零覆盖合并，每来一帧就按合并后的数刷新。
        let mut usage = Usage::default();
        Ok(Box::pin(stream.map(move |chunk| {
            if let Ok(StreamChunk::Usage { usage: u }) = &chunk {
                usage.merge_non_zero(u);
                let used = u64::from(usage.total_input_billed()) + u64::from(usage.output_tokens);
                if used > 0 {
                    reported.store(used, Ordering::Relaxed);
                    let config = shell.config.load();
                    emit(
                        &shell,
                        &session_id,
                        SessionEvent::Context(context_view(used, window, &config.agent)),
                    );
                }
            }
            chunk
        })))
    }
}

/// 占用 + 窗口 + 配置 → 界面上那个环要的数。
pub(super) fn context_view(
    used: u64,
    window: u32,
    agent: &carter::config::AgentConfig,
) -> ContextView {
    let window = u64::from(window);
    let ratio = agent.compact_threshold_ratio.clamp(0.0, 1.0);
    ContextView {
        used,
        window,
        threshold: (window as f64 * ratio) as u64,
        auto: agent.compact_enabled && window > 0,
    }
}

/// carter 压缩日志里「摘要成功」那一句（`[compact] L3 rolling summary applied: …`）
/// 用的是哪一档。不是这句就返回 `None`。
pub(super) fn summary_tier(notice: &str) -> Option<String> {
    let rest = notice.strip_prefix("[compact] ")?;
    if !rest.contains("rolling summary applied") {
        return None;
    }
    rest.split_whitespace().next().map(str::to_string)
}

/// 手动 / 自动压缩时接 carter 压缩日志的出口。
///
/// 那几句英文日志（`[compact] L1 elided …`）不往界面送：压完之后由
/// [`Runner::compact`] 给一句说人话的结果。这里只从里面捞两件事 —— 摘要成没成、
/// 没成的话为什么。
#[derive(Default)]
pub(super) struct CompactSink {
    pub(super) tier: Option<String>,
    pub(super) failures: Vec<String>,
}

impl UiSink for CompactSink {
    fn emit(&mut self, ev: UiEvent) {
        let UiEvent::Notice(msg) = ev else {
            return;
        };
        if let Some(tier) = summary_tier(&msg) {
            self.tier = Some(tier);
        } else if msg.contains("failed") || msg.contains("error") {
            self.failures
                .push(msg.trim_start_matches("[compact] ").to_string());
        }
    }
}

pub(super) fn todo_view(t: &carter::tools::TodoItem) -> TodoView {
    TodoView {
        content: t.content.clone(),
        active_form: t.active_form.clone(),
        status: match t.status {
            carter::tools::TodoStatus::Pending => "pending",
            carter::tools::TodoStatus::InProgress => "in_progress",
            carter::tools::TodoStatus::Completed => "completed",
        }
        .to_string(),
    }
}
