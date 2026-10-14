//! 会话运行器：每个会话一个 tokio 任务，独占自己的 `Thread`。
//!
//! ## 为什么一个会话一个任务，而不是共享一个循环
//!
//! `Thread` 是可变的、且一轮 turn 会连着跑很多次模型调用。放进共享循环就得给每个
//! 会话加锁，锁还会跨 `await` 持有 —— 一个会话卡在上游网络上，别的会话全被堵住。
//! 一任务一会话则天然隔离：并发量就是会话数，且没有跨 await 的锁。
//!
//! ## 事件怎么走到界面
//!
//! ```text
//! run_turn ──UiSink──▶ ui_tx ──▶ 转发任务 ──▶ broadcast ──▶ Tauri emit ──▶ 前端
//! ```
//!
//! 中间那层转发任务做三件界面关心的事：给工具调用补 id（carter 的 `UiEvent` 不带 id，
//! 但并行工具调用必须能配对）、把 `ask_user` 的 oneshot 发送端取出来存住、
//! 把 carter 的事件词汇翻译成 IPC 词汇。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use carter::agent::{
    bootstrap::build_tools, run_turn, CancelToken, CompactModel, RunOptions, Thread, UiEvent, UiSink,
};
use carter::config::Config;
use carter::hooks::HookRegistry;
use carter::registry::{fetch, ModelInfo};
use carter::session::{self, SessionOpts};

use crate::protocol::{AppEvent, Command, SessionEvent, TodoView, UsageView};
use crate::shell::{AppShell, AskRegistry, SessionHandle};

pub struct SessionOptions {
    pub cwd: PathBuf,
    /// `provider/model`；`None` = 用配置里的 `agent.model`。
    pub model: Option<String>,
    /// 续接当前目录最近的会话。
    pub resume_latest: bool,
    /// 打开指定的历史会话（按 id）。优先于 `resume_latest`。
    pub resume_id: Option<String>,
}

/// 新建/续接的结果：句柄 + 已有历史。
///
/// 历史在这里一次性返回，而不是开个「取历史」命令 —— `Thread` 由会话任务独占，
/// 让外壳去读它就得为每个会话加锁，而锁会跨 await 持有。
pub struct StartedSession {
    pub handle: Arc<SessionHandle>,
    pub history: Vec<crate::view::MessageView>,
}

/// 新建 / 续接一个会话。返回的句柄立即可以投递命令。
///
/// **必须在 tokio runtime 上下文里调用** —— 它要 spawn 会话任务。同步的 Tauri 命令
/// 跑在主线程上、没有 runtime，会在这里拿到一个明确的错误而不是一个看不懂的 panic。
pub fn start_session(
    shell: Arc<AppShell>,
    opts: SessionOptions,
) -> carter::Result<StartedSession> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err(carter::CarterError::Config(
            "start_session 必须在 tokio runtime 内调用（Tauri 命令要写成 async fn）".into(),
        ));
    }
    let config = shell.config.load_full();
    let cache_json = fetch::read_cache()?;
    let model_ref = opts
        .model
        .clone()
        .unwrap_or_else(|| config.agent.model.clone());
    // 抓包钩子挂在 provider 上，写进 shell 里那个按会话分文件的仓库。
    let capture_store = shell.capture.clone();
    let capture_hook = Arc::new(crate::capture::CaptureHook::new(capture_store.clone()));
    let resolved = carter::provider::resolve_provider(
        &config,
        &cache_json,
        &model_ref,
        vec![capture_hook],
    )?;
    let (model, provider) = (resolved.model, resolved.provider);
    let unsupported_server_tools = resolved.unsupported_server_tools;

    // 会话文件：按 id 打开 → 续接最近的 → 起一个新的。
    let existing = match &opts.resume_id {
        Some(id) => find_by_id(&opts.cwd, id),
        None if opts.resume_latest => session::latest(&opts.cwd),
        None => None,
    };
    let (thread, meta, resumed) = match existing {
        Some(entry) => {
            let (t, m) = session::load(&entry)?;
            (t, m, true)
        }
        None => {
            let sess_opts = SessionOpts { session_id: None };
            let (t, m) = session::start_new(&opts.cwd, &model_ref, &sess_opts)?;
            (t, m, false)
        }
    };

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = Arc::new(SessionHandle {
        id: meta.id.clone(),
        cwd: opts.cwd.clone(),
        model: ArcSwap::from_pointee(model.clone()),
        asks: Arc::new(AskRegistry::default()),
        cmd_tx,
    });
    shell.sessions.insert(meta.id.clone(), handle.clone());
    let history = crate::view::to_views(&thread.messages);

    let ctx = SessionCtx {
        shell: shell.clone(),
        handle: handle.clone(),
        model,
        provider,
        config,
        resumed,
        cwd: opts.cwd,
        unsupported_server_tools,
        capture: capture_store,
    };
    tokio::spawn(runner(ctx, thread, meta, cmd_rx));
    Ok(StartedSession { handle, history })
}

/// 会话任务需要的一整套东西。打包成一个结构体是为了 `runner` 的签名不至于十来个参数。
struct SessionCtx {
    shell: Arc<AppShell>,
    handle: Arc<SessionHandle>,
    model: ModelInfo,
    provider: Arc<dyn carter::provider::LlmProvider>,
    config: Arc<Config>,
    resumed: bool,
    cwd: PathBuf,
    /// 渠道声明不出去的服务端工具（该协议没有对应物、或名字不认识）。
    unsupported_server_tools: Vec<String>,
    /// 抓包存储。换模型时要重挂钩子，所以得留着。
    capture: Arc<crate::capture::CaptureStore>,
}

async fn runner(
    mut ctx: SessionCtx,
    mut thread: Thread,
    meta: session::SessionMeta,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
) {
    let session_id = ctx.handle.id.clone();
    let cancel = CancelToken::new();

    // UiEvent → AppEvent 的转发任务。它活着期间一直是同一条通道。
    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiEvent>();
    let forwarder = tokio::spawn(forward_events(
        ctx.shell.clone(),
        session_id.clone(),
        ctx.handle.asks.clone(),
        ui_rx,
    ));

    // MCP server 启动是异步的（要连子进程 / 网络）。失败不致命：
    // carter 的 McpManager 会跳过连不上的 server。
    let (mcp_mgr, mcp_tools) = carter::mcp::McpManager::start(&ctx.config.mcp).await;

    let skills_dir = carter::config::paths::skills_dir();
    let skill_metas = if ctx.config.skills.enabled {
        carter::skills::discover(&skills_dir)
    } else {
        Vec::new()
    };
    let system = carter::prompt::assemble_system(
        &ctx.config,
        &ctx.cwd,
        &ctx.model.key,
        meta.git.as_ref(),
        &skill_metas,
        true,
    );
    let hooks = Arc::new(HookRegistry::from_configs(ctx.config.hooks.clone()));

    emit(
        &ctx.shell,
        &session_id,
        SessionEvent::Ready {
            session_id: session_id.clone(),
            model: ctx.model.api_name.clone(),
            cwd: ctx.cwd.to_string_lossy().to_string(),
            resumed: ctx.resumed,
        },
    );

    // 声明不出去的服务端工具**必须说出来**。静默丢弃是最坏的选项：
    // 用户以为开了联网搜索，模型其实一个结果都拿不到，而模型自己也不知道，
    // 只会一本正经地编。
    if !ctx.unsupported_server_tools.is_empty() {
        emit(
            &ctx.shell,
            &session_id,
            SessionEvent::Notice {
                message: format!(
                    "当前渠道不支持这些服务端工具，已忽略：{}",
                    ctx.unsupported_server_tools.join("、")
                ),
            },
        );
    }

    // 生成中只接取消与作答；其余命令排队，等这一轮跑完再处理。
    let mut queued: VecDeque<Command> = VecDeque::new();

    loop {
        let cmd = match queued.pop_front() {
            Some(c) => c,
            None => match cmd_rx.recv().await {
                Some(c) => c,
                None => break, // 句柄全 drop 了 → 会话结束
            },
        };

        match cmd {
            Command::Cancel => {
                // 没有正在跑的轮次时取消是空操作。
            }
            Command::AnswerAsk { id, answer } => {
                ctx.handle.asks.answer(id, answer);
            }
            Command::SetModel { model } => match switch_model(&ctx, &model) {
                Ok(resolved) => {
                    ctx.model = resolved.model;
                    ctx.provider = resolved.provider;
                    ctx.unsupported_server_tools = resolved.unsupported_server_tools;
                    ctx.handle.model.store(Arc::new(ctx.model.clone()));
                    emit(
                        &ctx.shell,
                        &session_id,
                        SessionEvent::Notice {
                            message: format!("模型已切换为 {}", ctx.model.api_name),
                        },
                    );
                }
                Err(e) => emit(
                    &ctx.shell,
                    &session_id,
                    SessionEvent::Error {
                        message: format!("切换模型失败：{e}"),
                    },
                ),
            },
            Command::SendPrompt { text } => {
                let tools = build_tools(
                    ctx.config.skills.enabled,
                    ctx.provider.clone(),
                    ctx.model.clone(),
                    ctx.config.agent.clone(),
                    system.join("\n"),
                    cancel.clone(),
                    mcp_tools.clone(),
                    Some(ui_tx.clone()),
                );
                let run_opts = RunOptions {
                    show_thinking: true,
                    system: system.clone(),
                    // 压缩复用主模型：GUI 里没有「fast 模型」这个概念的配置入口，
                    // 多一条链路就多一处会出错的地方。
                    compact_model: Some(CompactModel {
                        provider: ctx.provider.clone(),
                        model: ctx.model.clone(),
                    }),
                    hooks: hooks.clone(),
                    // 单会话里只有主对话这一种请求（压缩与子 agent 也会发，
                    // 但它们的 purpose 由各自那条路径设置）。
                    purpose: carter::provider::RequestPurpose::MainTurn,
                };

                thread.append_user(text);
                emit(
                    &ctx.shell,
                    &session_id,
                    SessionEvent::TurnStarted {
                    turn: thread.user_turns,
                },
                );

                let mut sink = TxSink(ui_tx.clone());
                let turn = run_turn(
                    &mut thread,
                    &*ctx.provider,
                    &ctx.model,
                    &ctx.config.agent,
                    &run_opts,
                    &tools,
                    &mut sink,
                    &cancel,
                );
                tokio::pin!(turn);

                let outcome = loop {
                    tokio::select! {
                        res = &mut turn => break res,
                        Some(inner) = cmd_rx.recv() => match inner {
                            // 生成中只有这两条命令能即时生效 —— 其它排队等下一轮。
                            Command::Cancel => cancel.set(),
                            Command::AnswerAsk { id, answer } => {
                                ctx.handle.asks.answer(id, answer);
                            }
                            other => queued.push_back(other),
                        },
                    }
                };

                match outcome {
                    Ok(_) => {}
                    Err(e) => emit(
                        &ctx.shell,
                        &session_id,
                        SessionEvent::Error {
                            message: format!("{e}"),
                        },
                    ),
                }
                emit(&ctx.shell, &session_id, SessionEvent::Idle);
            }
        }
    }

    mcp_mgr.shutdown().await;
    ctx.handle.asks.clear();
    drop(ui_tx);
    let _ = forwarder.await;
    ctx.shell.sessions.remove(&session_id);
}

/// 在某个目录的会话列表里按 id 找。
///
/// 只查这个目录：界面列出来的就是这里的会话，能在界面上点到的就必须能打开 ——
/// 否则会出现「列表里有、点开却是另一个会话」这种最令人困惑的失败。
fn find_by_id(cwd: &std::path::Path, id: &str) -> Option<session::SessionEntry> {
    session::list(cwd, false)
        .into_iter()
        .find(|e| e.meta.id == id)
}

fn switch_model(
    ctx: &SessionCtx,
    model_ref: &str,
) -> carter::Result<carter::provider::resolve::ResolvedProvider> {
    let cache_json = fetch::read_cache()?;
    // 换模型时也保留抓包 —— 否则一切换就静默停止记录了。
    let capture_hook = Arc::new(crate::capture::CaptureHook::new(ctx.capture.clone()));
    carter::provider::resolve_provider(&ctx.config, &cache_json, model_ref, vec![capture_hook])
}

fn emit(shell: &AppShell, session_id: &str, event: SessionEvent) {
    shell.emit(AppEvent::Session {
        session_id: session_id.to_string(),
        event,
    });
}

/// 把 carter 的事件词汇翻译成 IPC 词汇。
async fn forward_events(
    shell: Arc<AppShell>,
    session_id: String,
    asks: Arc<AskRegistry>,
    mut rx: mpsc::UnboundedReceiver<UiEvent>,
) {
    // 工具调用的配对：carter 的 `UiEvent` 不带 id，但 started/result 都按调用顺序发。
    // 这里补一个稳定 id，让并行工具调用的卡片能正确配对。
    let mut open_calls: VecDeque<String> = VecDeque::new();
    let mut next_call: u64 = 0;

    while let Some(ev) = rx.recv().await {
        let out = match ev {
            UiEvent::AssistantTextDelta(text) => Some(SessionEvent::TextDelta { text }),
            UiEvent::ThinkingDelta(text) => Some(SessionEvent::ThinkingDelta { text }),
            UiEvent::AssistantTextEnd => Some(SessionEvent::TextEnd),
            UiEvent::ToolCallStarted { name, args_preview } => {
                let call_id = format!("c{next_call}");
                next_call += 1;
                open_calls.push_back(call_id.clone());
                Some(SessionEvent::ToolCallStarted {
                    call_id,
                    name,
                    args_preview,
                })
            }
            UiEvent::ToolResult { ok, summary } => Some(SessionEvent::ToolResult {
                call_id: open_calls.pop_front().unwrap_or_default(),
                ok,
                summary,
            }),
            UiEvent::TodoUpdated(todos) => Some(SessionEvent::TodoUpdated {
                todos: todos.iter().map(todo_view).collect(),
            }),
            UiEvent::AskUser {
                id,
                question,
                options,
                response_tx,
            } => {
                // 把 oneshot 发送端取出来存住 —— 事件本身要被广播给多个订阅者，
                // 而 oneshot 只能发一次。取不出来说明已经有人处理过了。
                if let Some(tx) = response_tx.lock().unwrap().take() {
                    asks.insert(id, tx);
                }
                Some(SessionEvent::AskUser {
                    id,
                    question,
                    options,
                })
            }
            UiEvent::TurnUsage { usage, cost, model } => Some(SessionEvent::TurnUsage {
                usage: UsageView::from(&usage),
                cost,
                model,
            }),
            UiEvent::Notice(message) => Some(SessionEvent::Notice { message }),
            UiEvent::Title(_) | UiEvent::ReplayHistory(_) => None,
            UiEvent::ModelChanged(model) => Some(SessionEvent::Notice {
                message: format!("模型：{model}"),
            }),
            UiEvent::Divider(label) => Some(SessionEvent::Notice { message: label }),
            UiEvent::Idle => Some(SessionEvent::Idle),
        };
        if let Some(event) = out {
            emit(&shell, &session_id, event);
        }
    }
}

fn todo_view(t: &carter::tools::TodoItem) -> TodoView {
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

/// 把 `run_turn` 的渲染出口接到事件通道上。
struct TxSink(mpsc::UnboundedSender<UiEvent>);

impl UiSink for TxSink {
    fn emit(&mut self, ev: UiEvent) {
        // 接收端没了说明会话正在退出，丢弃即可。
        let _ = self.0.send(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同步上下文里调用 `start_session` 必须拿到一句能读懂的错，而不是 panic。
    ///
    /// 这条守的是一个真实踩过的坑：Tauri 的**同步**命令跑在主线程上、没有 tokio
    /// runtime，而 `start_session` 要 spawn 会话任务。当时的表现是窗口一亮就闪退，
    /// panic 信息是 "there is no reactor running" —— 从现象完全看不出是命令该写成
    /// async。加这道检查后，同样的错误会自己说清楚。
    ///
    /// 用普通 `#[test]` 而不是 `#[tokio::test]`：前者没有 runtime 上下文，正是要测的场景。
    #[test]
    fn start_session_without_a_runtime_says_so_instead_of_panicking() {
        let shell = AppShell::new(carter::config::Config::default());
        let opts = SessionOptions {
            cwd: std::path::PathBuf::from("."),
            model: None,
            resume_latest: false,
            resume_id: None,
        };
        // 不用 `expect_err`：`StartedSession` 没实现 Debug，而且这里关心的是「返回 Err」。
        let msg = match start_session(shell, opts) {
            Ok(_) => panic!("没有 runtime 时应当报错而不是成功"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("tokio runtime"),
            "错误信息要指明根因，实际是：{msg}"
        );
    }
}
