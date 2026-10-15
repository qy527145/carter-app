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
    run_turn, CancelToken, CompactModel, RunOptions, Thread, UiEvent, UiSink,
};
use carter::config::Config;
use carter::hooks::HookRegistry;
use carter::registry::{fetch, ModelInfo};
use carter::session::{self, SessionOpts};

use crate::app_settings::AppSettings;
use crate::protocol::{AppEvent, Command, SessionEvent, TodoView, UsageView};
use crate::shell::{AppShell, AskRegistry, SessionHandle};
use crate::tools::ask_user::AskRequest;
use crate::tools::build_tools;
use crate::tools::record::ToolEvent;
use crate::tools::ToolBuildArgs;

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
        settings: AppSettings::load(),
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
    /// 启动时的配置快照。**每一轮开始前会重新读一遍**，见 [`SessionCtx::refresh`]。
    config: Arc<Config>,
    /// 应用私有设置（工具开关）。同样每轮重读 —— 用户在界面上关掉一个工具，
    /// 下一条消息就该生效。
    settings: AppSettings,
    resumed: bool,
    cwd: PathBuf,
    /// 渠道声明不出去的服务端工具（该协议没有对应物、或名字不认识）。
    unsupported_server_tools: Vec<String>,
    /// 抓包存储。换模型时要重挂钩子，所以得留着。
    capture: Arc<crate::capture::CaptureStore>,
}

impl SessionCtx {
    /// 重新读一遍配置与应用设置。
    ///
    /// 这是**热重载的落点**：界面写完 `config.toml` 会 `store` 进 `AppShell`，
    /// 但已经跑着的会话手里还攥着启动时那份 `Arc<Config>`。每轮开头刷新一次，
    /// 于是「改完配置 → 下一条消息生效」，不必重开会话。
    ///
    /// 刻意**不动** `model` / `provider`：那是这条对话的身份。用户改了默认模型，
    /// 新会话用新的，这条接着用它自己选的那个。
    fn refresh(&mut self) {
        self.config = self.shell.config.load_full();
        self.settings = AppSettings::load();
    }

    /// 当前的系统提示（含技能目录）。技能开关会变，所以每轮重算。
    fn system(&self, git: Option<&carter::session::GitInfo>) -> Vec<String> {
        let skill_metas = if self.config.skills.enabled {
            carter::skills::discover(&carter::config::paths::skills_dir())
        } else {
            Vec::new()
        };
        carter::prompt::assemble_system(
            &self.config,
            &self.cwd,
            &self.model.key,
            git,
            &skill_metas,
            true,
        )
    }
}

async fn runner(
    mut ctx: SessionCtx,
    mut thread: Thread,
    meta: session::SessionMeta,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
) {
    let session_id = ctx.handle.id.clone();
    let cancel = CancelToken::new();

    // 事件有三条来源，最终都汇到 AppEvent 上：
    // * `ui_tx`   —— carter 的 UiEvent（文本增量、用量、通知…）
    // * `tool_tx` —— 我们自己那层记录包装（**完整**的参数与结果）
    // * `ask_tx`  —— 工具发起的提问，需要等前端作答
    //
    // 分成三条而不是硬塞进一条：`UiEvent` 是 carter 的封闭枚举，加不了变体；
    // 而工具记录与提问本来就不是 carter 的概念。
    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiEvent>();
    let (tool_tx, tool_rx) = mpsc::unbounded_channel::<ToolEvent>();
    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
    let forwarder = tokio::spawn(forward_events(
        ctx.shell.clone(),
        session_id.clone(),
        ctx.handle.asks.clone(),
        ui_rx,
        tool_rx,
        ask_rx,
    ));

    // MCP server 启动是异步的（要连子进程 / 网络）。失败不致命：
    // carter 的 McpManager 会跳过连不上的 server。
    //
    // 它在会话开头连一次就不再重连 —— 所以界面上改了 MCP 配置要**新开会话**
    // 才生效，这与「工具开关下一条消息就生效」不同，界面上分开提示。
    let (mcp_mgr, mcp_tools) = carter::mcp::McpManager::start(&ctx.config.mcp).await;

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
            Command::AnswerForm { id, answers } => {
                ctx.handle.asks.answer(id, answers);
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
                // 热重载落点：每轮开始前重读配置与应用设置。
                // 界面上改完工具开关、换完渠道、动过 system.md，下一条消息就按新的来。
                ctx.refresh();
                let system = ctx.system(meta.git.as_ref());
                let hooks = Arc::new(HookRegistry::from_configs(ctx.config.hooks.clone()));

                // 输入框里的 `/命令`、`@文件`、`@图片` 在这里展开。
                //
                // 放在后端而不是前端：展开后的文本（几百行文件内容）不该出现在输入框
                // 里让用户看着它发出去。放在这里还有一个好处 —— 与 CLI 走的是同一套
                // 规则（自定义命令用 carter 的 `commands::expand`，图片用 carter 的
                // `media::inline_user_attachments`），两边行为对得上。
                let text = match expand_input(&ctx.cwd, &text) {
                    Ok((expanded, note)) => {
                        if let Some(note) = note {
                            emit(&ctx.shell, &session_id, SessionEvent::Notice { message: note });
                        }
                        expanded
                    }
                    Err(e) => {
                        // 展开失败不吞掉这条消息：把原文发出去，但要说清发生了什么。
                        emit(
                            &ctx.shell,
                            &session_id,
                            SessionEvent::Notice {
                                message: format!("{e}（已按原文发送）"),
                            },
                        );
                        text
                    }
                };

                let tools = build_tools(
                    ToolBuildArgs {
                        provider: ctx.provider.clone(),
                        model: ctx.model.clone(),
                        agent_cfg: ctx.config.agent.clone(),
                        base_system: system.join("\n"),
                        cancel: cancel.clone(),
                        mcp_tools: mcp_tools.clone(),
                        cwd: ctx.cwd.clone(),
                        skills_enabled: ctx.config.skills.enabled,
                        tool_tx: tool_tx.clone(),
                        ask_tx: Some(ask_tx.clone()),
                    },
                    &ctx.settings,
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
                            Command::AnswerForm { id, answers } => {
                                ctx.handle.asks.answer(id, answers);
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

/// 展开输入框里的 `/命令` 与 `@文件`。
///
/// 返回 `(展开后的文本, 给用户看的说明)`。说明只在真的发生了展开时才给 ——
/// 普通消息不该多出一条提示。
///
/// 三步，顺序不能换：
/// 1. **自定义命令**：`/name args` 且 `name` 是 `~/.carter/commands/` 或
///    `.carter/commands/` 里的命令时，用 carter 的 `commands::expand` 展开成 prompt
///    （它自己会注入正文里的 `@文件` 与 `!shell`）。
/// 2. **`@文件`**：展开成 `--- path ---` 包裹的上下文块。
/// 3. **`@图片`**：转成 `[img:...]` 引用，发请求时由 provider 层取字节。
///
/// 第 2 步对图片路径返回 None（不注入），正好把图片留给第 3 步。
fn expand_input(cwd: &std::path::Path, raw: &str) -> Result<(String, Option<String>), String> {
    let mut text = raw.to_string();
    let mut note = None;

    // 先把（可能存在的）命令名与参数取成自有字符串，再动 `text` ——
    // 否则 `strip_prefix` 借出去的那一段会一直活到赋值处。
    let command = text.strip_prefix('/').map(|rest| {
        let (name, args) = match rest.split_once(char::is_whitespace) {
            Some((n, a)) => (n.to_string(), a.trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
        (name, args)
    });

    if let Some((name, args)) = command {
        // 只认真的存在的自定义命令。`/usr/bin/env` 这种路径不能当命令处理。
        if !name.is_empty() && carter::commands::discover(cwd).iter().any(|c| c.name == name) {
            text = crate::input::expand_custom(cwd, &name, &args)?;
            note = Some(format!("已展开自定义命令 /{name}"));
        }
    }

    let before = text.clone();
    text = crate::input::expand_file_refs(&text, cwd);
    text = carter::media::inline_user_attachments(&text, cwd);

    if note.is_none() && text != before {
        let files = crate::input::count_file_refs(&before, cwd);
        if files > 0 {
            note = Some(format!("已把 {files} 个文件的内容附进这条消息"));
        }
    }
    Ok((text, note))
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

/// 把三条来源翻译成统一的 IPC 事件。
///
/// carter 的 `ToolCallStarted` / `ToolResult` / `AskUser` **刻意丢弃**：
/// 前两者只带截断的预览（结果只有第一行 120 字符），界面上画不出有意义的卡片；
/// 后者是 carter 那个一问一答的 ask_user 工具，已经被我们自己的实现顶掉了。
/// 留着它们只会让每个工具出现两张卡片。
async fn forward_events(
    shell: Arc<AppShell>,
    session_id: String,
    asks: Arc<AskRegistry>,
    mut ui_rx: mpsc::UnboundedReceiver<UiEvent>,
    mut tool_rx: mpsc::UnboundedReceiver<ToolEvent>,
    mut ask_rx: mpsc::UnboundedReceiver<AskRequest>,
) {
    let mut ui_open = true;
    let mut tool_open = true;
    let mut ask_open = true;

    let emit = |shell: &AppShell, event: SessionEvent| {
        shell.emit(AppEvent::Session {
            session_id: session_id.clone(),
            event,
        });
    };

    while ui_open || tool_open || ask_open {
        tokio::select! {
            ev = ui_rx.recv(), if ui_open => match ev {
                Some(ev) => {
                    if let Some(out) = map_ui(ev) {
                        emit(&shell, out);
                    }
                }
                None => ui_open = false,
            },
            ev = tool_rx.recv(), if tool_open => match ev {
                Some(ToolEvent::Started { call_id, name, args }) => {
                    emit(&shell, SessionEvent::ToolStarted { call_id, name, args });
                }
                Some(ToolEvent::Finished { call_id, ok, content, elapsed_ms }) => {
                    emit(&shell, SessionEvent::ToolFinished { call_id, ok, content, elapsed_ms });
                }
                None => tool_open = false,
            },
            req = ask_rx.recv(), if ask_open => match req {
                Some(AskRequest::Questions { id, questions, reply }) => {
                    // 发送端存住：前端要跨一次进程边界才回得来，而 oneshot 只能发一次。
                    asks.insert(id, reply);
                    emit(&shell, SessionEvent::AskForm { id, questions });
                }
                None => ask_open = false,
            },
        }
    }
}

/// carter 的事件 → IPC 事件。返回 `None` = 这条不往前端送（见上面的说明）。
fn map_ui(ev: UiEvent) -> Option<SessionEvent> {
    Some(match ev {
        UiEvent::AssistantTextDelta(text) => SessionEvent::TextDelta { text },
        UiEvent::ThinkingDelta(text) => SessionEvent::ThinkingDelta { text },
        UiEvent::AssistantTextEnd => SessionEvent::TextEnd,
        UiEvent::TodoUpdated(todos) => SessionEvent::TodoUpdated {
            todos: todos.iter().map(todo_view).collect(),
        },
        UiEvent::TurnUsage { usage, cost, model } => SessionEvent::TurnUsage {
            usage: UsageView::from(&usage),
            cost,
            model,
        },
        UiEvent::Notice(message) => SessionEvent::Notice { message },
        UiEvent::Idle => SessionEvent::Idle,
        UiEvent::ModelChanged(model) => SessionEvent::Notice {
            message: format!("模型：{model}"),
        },
        UiEvent::Divider(label) => SessionEvent::Notice { message: label },
        UiEvent::Title(_) | UiEvent::ReplayHistory(_) => return None,
        // 见函数文档：这三条由记录包装层或我们自己的实现接管。
        UiEvent::ToolCallStarted { .. }
        | UiEvent::ToolResult { .. }
        | UiEvent::AskUser { .. } => return None,
    })
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
