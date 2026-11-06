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
//! run_one_turn ──SessionEvent──▶ shell.emit ──▶ broadcast ──▶ Tauri emit ──▶ 前端
//! ```
//!
//! 正文增量（`TextDelta` / `ThinkingDelta` / `TextEnd`）与工具卡片一样，在产生的那
//! 一刻直接 `emit`：它们必须与工具事件走**同一条同步路径**，否则「先这段字、后这张
//! 工具卡」的顺序会在异步转发里被打乱。工具卡片同样不走队列 ——
//! [`crate::tools::record::ToolTx`] 是个同步回调。另外还有一条**并行的**记录：
//! `live`（见 `shell.rs` 的 `LiveLog`），给切走再回来的窗口补画面用 —— 它不是界面
//! 收字的主路径。
//!
//! 转发任务只剩一条来源（`forward_events`）：`ask_user` 的 oneshot 发送端。
//!
//! ## 长任务目标（`/goal`）住在哪
//!
//! 目标的**判定**在 `goal.rs`（纯函数），**执行**在这里：一轮跑完后问
//! `decide_after_turn`，要继续就再追加一条「继续」消息接着跑。所以「跑一轮」被抽成了
//! [`Runner::run_one_turn`]，用户发消息与自动继续走的是同一条路 —— 两条路各写一遍的话，
//! 自动继续迟早会漏掉某一轮里新加的东西（工具开关、热重载、抓包…）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::catalog;
use carter::agent::{CompactModel, Thread, UiEvent, UiSink};
use carter::config::Config;
use carter::hooks::{HookDecision, HookEvent, HookRegistry};
use carter::registry::ModelInfo;
use carter::session::{self, SessionOpts};
use carter::tools::Tool;

use crate::app_settings::AppSettings;
use crate::changes::ChangeWatch;
use crate::goal::{self, Goal, GoalSignal, GoalStatus, TurnOutcome};
use crate::protocol::{
    AppEvent, Command, ContextView, GoalCommand, GoalView, SessionEvent, TodoView, UsageView,
};
use crate::shell::{AppShell, AskRegistry, SessionHandle};
use crate::tools::ToolBuildArgs;
use crate::tools::ask_user::AskRequest;
use crate::tools::build_tools;
use crate::tools::record::ToolEvent;

pub struct SessionOptions {
    pub cwd: PathBuf,
    /// `provider/model`；`None` = 用配置里的 `agent.model`。
    pub model: Option<String>,
    /// 续接当前目录最近的会话。
    pub resume_latest: bool,
    /// 打开指定的历史会话（按 id）。优先于 `resume_latest`。
    pub resume_id: Option<String>,
    /// 给**新**会话指定 id。用来让助理的对话有一个固定身份 —— 界面靠它找到那条会话，
    /// 续接时也用 `resume_id` 走同一条路。续接已有会话时忽略。
    pub new_id: Option<String>,
    /// 新会话一开始就带的标题。
    ///
    /// 给助理派出去的工单用：它的标题用户已经定过了（派发时那个 `title`），
    /// 不该再拿任务书交给副模型重新起一个 —— 那样侧栏里同一张工单在对话和面板上
    /// 是两个名字，而且起标题那几秒它还叫「未命名会话」。给了就不再自动起标题。
    ///
    /// 只在会话还没有标题时生效（续接一条老会话时忽略）。
    pub title: Option<String>,
}

/// 新建/续接的结果：句柄 + 已有历史。
///
/// 历史在这里一次性返回，而不是开个「取历史」命令 —— `Thread` 由会话任务独占，
/// 让外壳去读它就得为每个会话加锁，而锁会跨 await 持有。
pub struct StartedSession {
    pub handle: Arc<SessionHandle>,
    pub history: Vec<crate::view::MessageView>,
    /// 这条会话现有的长任务目标（续接时从盘上读回来）。
    ///
    /// 目标变化本来靠事件推，但 `ready` / `goal` 事件可能在界面挂上监听之前就发出去了
    /// （`create_session` 是 await 的，事件比 `beginSession` 先到）—— 返回值是那条路的兜底。
    pub goal: Option<GoalView>,
    /// 历史里每一轮**实际改过**的文件（见 `changes.rs`）。
    ///
    /// 与 `goal` 同理：这些数据只存在应用私有目录里，不能回放成事件，所以随
    /// `create_session` 一次性带回；实时那几轮走 `files_changed` 事件。
    pub changes: Vec<crate::changes::TurnFiles>,
    /// 变更条的统计起点（轮次号）。用户点过「清空」之后，只汇总这一轮及之后的。
    pub changes_since: u32,
    /// 历史里每一轮用户**打的原文**（`/命令`、`@文件` 展开之前，见 `prompts.rs`）。
    ///
    /// 界面拿它回填「编辑」框 —— 会话文件里存的是展开后的正文，回填进去等于把
    /// 整份文件内容摊在输入框里。同上：只存在应用私有目录，随历史一起带回。
    pub prompts: Vec<crate::prompts::TurnPrompt>,
    /// 历史里每一轮的**起止时刻**（见 `timeline.rs`）。界面在消息旁边显示时间。
    ///
    /// 与会话文件里的 `ts` 不是一回事：那一层在折叠成消息时就丢了。老会话没有
    /// 这份记录，界面上那几轮就不显示时间。
    pub times: Vec<crate::timeline::TurnTime>,
    /// 接上的会话此刻正在跑一轮（见 [`attach_session`]）。新开的总是 `false`。
    pub running: bool,
    /// 正在跑的那一轮已经产出的东西（正文、工具调用）。界面接上时据此把这一轮画出来，
    /// 之后的增量照常从事件流来。不在跑时为空。
    pub live: Vec<crate::shell::LiveItem>,
    /// 这一轮正卡在等用户回答的问题。`ask_form` 事件不回放，界面不在这条会话上时
    /// 就错过了；不随接回一起带上，表单出不来，这一轮只能永远等下去。
    pub pending_ask: Option<crate::protocol::PendingAsk>,
    /// 这条会话现在的 todo 列表（`thread.todos`）。
    ///
    /// 与 `goal` 同理：事件不回放，而用户切走再回来（接上后台那条）时拿到的是这里，
    /// 不是一次 `todo_updated` —— 不带上的话底栏那条进度就是空的，明明还在干活。
    /// 每次工具调用落地时由 `TxSink` 直接更新句柄上那份（不经过历史快照）。
    pub todos: Vec<TodoView>,
    /// 这一轮里**已经跑完**的工具（结果已定）。工具事件不回放，切走页面再回来时
    /// 那些卡片只剩 `live` 里一条没有结果的记录 —— 看起来像还卡着。接回时一并带上，
    /// 界面按 `call_id` 把结果补进卡片。
    pub finished_tools: Vec<crate::protocol::ToolUsageView>,
    /// 这条会话此刻还在跑的后台命令（`bash` 的 `run_in_background`）。
    ///
    /// 与 `todos` 同理：事件不回放，用户切走再回来时靠它把「还有后台任务在跑」
    /// 那条提醒补回来 —— 不带的话，明明有任务在跑，界面上却什么都不显示。
    pub background_shells: Vec<crate::protocol::BackgroundShellView>,
}

/// 接上一条正在后台跑的会话（不关掉重开），返回界面要的那份状态。
///
/// 关掉重开会打断它正在跑的命令：`close_session` 要先让工具停下才能放掉会话文件。
/// 接上则什么都不动 —— 历史取自句柄上的快照，进行中那一轮另外带回（见 `live`）。
pub fn attach_session(shell: &AppShell, id: &str) -> Option<StartedSession> {
    let handle = shell.session(id)?;
    handle.attached.store(true, Ordering::SeqCst);
    tracing::info!(
        session = id,
        busy = handle.busy.load(Ordering::SeqCst),
        "接上正在后台的会话"
    );
    Some(StartedSession {
        history: crate::view::to_views(&handle.history.load()),
        goal: goal::load(id).as_ref().map(goal::view),
        changes: crate::changes::load(id),
        changes_since: crate::changes::since(id),
        prompts: crate::prompts::load(id),
        times: crate::timeline::load(id),
        running: handle.busy.load(Ordering::SeqCst),
        live: handle.live.snapshot(),
        pending_ask: handle.asks.current(),
        // 接上时不 await 会话自己的那把锁（它可能正跑着一轮）：读的是事件流已经
        // 发布过的那份快照，见 `Runner::emit` 里 `TodoUpdated` 那一支。
        todos: handle.todos.load().iter().map(todo_view).collect(),
        finished_tools: handle.live.finished_calls(),
        background_shells: handle.background.running(),
        handle,
    })
}

/// 把后台命令表的变化接到事件总线上：界面据此画「还有后台任务在跑」的提醒条。
///
/// 表挂在句柄上（工具在轮内直接往里塞命令），所以开头这一接、侧边聊天各接一次。
/// 事件不回放 —— 切走再回来靠 [`attach_session`] 带回的那份快照。
fn watch_background(shell: &Arc<AppShell>, handle: &Arc<SessionHandle>) {
    let shell = shell.clone();
    let session_id = handle.id.clone();
    handle.background.set_observer(Arc::new(move |shells| {
        emit(
            &shell,
            &session_id,
            SessionEvent::BackgroundShells { shells },
        );
    }));
}

/// 新建 / 续接一个会话。返回的句柄立即可以投递命令。
///
/// **必须在 tokio runtime 上下文里调用** —— 它要 spawn 会话任务。同步的 Tauri 命令
/// 跑在主线程上、没有 runtime，会在这里拿到一个明确的错误而不是一个看不懂的 panic。
pub fn start_session(shell: Arc<AppShell>, opts: SessionOptions) -> carter::Result<StartedSession> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err(carter::CarterError::Config(
            "start_session 必须在 tokio runtime 内调用（Tauri 命令要写成 async fn）".into(),
        ));
    }
    // 同一条会话不能有两个 runner：两个 `Thread` 各带一个 `Recorder` 往同一个
    // 文件追加，历史会错位到「无论如何都没法继续」。
    //
    // 正常路径上走不到这里：`close_session` 会等旧任务退出之后才返回，而前端是
    // 「先关再开」。这条是兜底 —— 某个不响应取消的工具（MCP）把旧任务多拖了几秒时，
    // 宁可让用户看到一句「还在收尾」，也不要把会话文件写坏。
    if let Some(id) = opts.resume_id.as_deref().or(opts.new_id.as_deref())
        && (shell.session(id).is_some() || shell.is_closing(id))
    {
        tracing::warn!(session = id, "拒绝重开：会话还在跑或还在收尾");
        return Err(carter::CarterError::Config(format!(
            "会话 {id} 还在收尾（上一轮的命令没有响应中断），请稍后再打开。"
        )));
    }
    // 新开会话的目录必须还在：目录被删掉之后侧栏里那一组仍然会列出来（会话文件存在
    // `~/.carter/projects/` 下，不随项目目录消失），从那里点「+」不拦的话，会得到
    // 一条每个工具调用都报「找不到目录」的会话。续接不受此限 —— 那条路由
    // `resumed_cwd` 退回请求里的目录。
    if opts.resume_id.is_none() && !opts.resume_latest && !opts.cwd.is_dir() {
        return Err(carter::CarterError::Config(format!(
            "目录 {} 不存在，无法在这里新建对话",
            opts.cwd.display()
        )));
    }
    let config = shell.config.load_full();
    let cache_json = catalog::read_cache().map_err(carter::CarterError::Config)?;
    let model_ref = opts
        .model
        .clone()
        .unwrap_or_else(|| config.agent.model.clone());
    // 抓包钩子挂在 provider 上，写进 shell 里那个按会话分文件的仓库。
    let capture_store = shell.capture.clone();
    let capture_hook = Arc::new(crate::capture::CaptureHook::new(capture_store.clone()));
    let resolved =
        catalog::resolve_provider(&config, &cache_json, &model_ref, vec![capture_hook.clone()])?;
    let (model, provider) = (resolved.model, resolved.provider);
    let unsupported_server_tools = resolved.unsupported_server_tools;
    // 协议回填给钩子：抓包存的是最终字节，解码要先知道按哪套语法解。
    label_capture(&capture_hook, &config, &model);

    // 会话文件：按 id 打开 → 续接最近的 → 起一个新的。
    let existing = match &opts.resume_id {
        Some(id) => find_by_id(id),
        // 「最近一条」不能选中助理对话：它从应用启动起就在后台跑着，再开一个 runner
        // 就是同一个会话文件两个写者。它有自己的入口，不参与这个选择。
        // 还在跑的（助理的工单、定时任务）也跳过，理由相同。
        None if opts.resume_latest => session::list(&opts.cwd, false).into_iter().find(|e| {
            e.meta.id != crate::assistant::CHAT_SESSION_ID
                && shell.session(&e.meta.id).is_none()
                && !shell.is_closing(&e.meta.id)
        }),
        None => None,
    };
    // 会话文件最后一次写入的时间（见 `Runner::last_message_at`）。
    let last_written = existing
        .as_ref()
        .and_then(|e| std::fs::metadata(&e.path).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    let (mut thread, mut meta, resumed) = match existing {
        Some(entry) => {
            let (mut t, m) = session::load(&entry)?;
            // 续接时把用户轮序号接到历史后面。
            //
            // carter 的 `Thread::from_parts` 把它从 0 重数，而抓包（`RequestMeta.turn`）、
            // 界面上的「第 N 轮」、历史消息上的轮次号全都按这一个数对齐 —— 从 0 重数
            // 会让重启后的第一轮与历史里的第一轮撞号，报文面板上一个筛选条件就套住
            // 了两条不相干的请求。压缩过的会话这里会数小（早期消息已被摘要替换），
            // 那是压缩本身的固有损失，与这里无关。
            let turns = crate::view::user_turn_count(&t.messages);
            t.user_turns = turns;
            // 老版本记的时刻可能整体错了一号（见 `timeline::realign`）。轮数当场
            // 数得出来，顺手把那条会话的键理顺；汇报卡的工单归属认的就是这些键。
            if let Err(e) = crate::timeline::realign(&entry.meta.id, turns as usize) {
                tracing::warn!(error = %e, "修正轮次号错位失败");
            }
            (t, m, true)
        }
        None => {
            let sess_opts = SessionOpts {
                session_id: opts.new_id.clone(),
            };
            let (t, m) = session::start_new(&opts.cwd, &model_ref, &sess_opts)?;
            (t, m, false)
        }
    };

    // 上次异常退出留下的断口（模型发了 tool_use，工具没跑、也没写回 tool_result）
    // 在这里补上。**必须在返回历史与接受新消息之前** —— 带着断口发请求，上游会
    // 一直 400，而用户看到的是「说什么都发不出去」。
    let repaired = crate::history::repair_dangling_tool_uses(&mut thread);
    if !repaired.is_empty() {
        tracing::warn!(
            session = %meta.id,
            calls = repaired.len(),
            "会话历史里有未完成的工具调用，已补上「被中断」的结果"
        );
    }

    // 工作目录以**会话文件里记的**为准，不用请求里带来的那个。
    //
    // 「会话」设置页会列出别的项目下的会话，点开它时工具、系统提示、git 快照都该
    // 落在那条对话自己的目录里；用请求里的 cwd（桌面端默认是家目录）会把一条在 A
    // 目录的对话接到 B 目录下跑。
    let cwd = match resumed {
        true => resumed_cwd(&meta.cwd, &opts.cwd),
        false => opts.cwd.clone(),
    };

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = SessionHandle::new(meta.id.clone(), cwd.clone(), model.clone(), None, cmd_tx);
    // 侧边聊天要靠这份快照拿父会话的上下文，所以一开始就得有值。
    handle.publish_history(&thread.messages);
    shell.sessions.insert(meta.id.clone(), handle.clone());
    watch_background(&shell, &handle);
    let history = crate::view::to_views(&thread.messages);

    // 盘上的目标是 `active` 的话，降级成 `paused`：用户关掉应用时它还在跑，
    // 原样恢复就等于人不在的时候模型自己开工。
    let (goal, downgraded) = match goal::load(&meta.id) {
        Some(g) => {
            let (g, downgraded) = goal::on_session_start(g);
            if downgraded {
                let _ = goal::save(&meta.id, &g);
            }
            (Some(g), downgraded)
        }
        None => (None, false),
    };
    let goal_view = goal.as_ref().map(goal::view);

    let ctx = SessionCtx {
        shell: shell.clone(),
        handle: handle.clone(),
        model,
        provider,
        config,
        settings: AppSettings::load(),
        resumed,
        shell_cwd: crate::tools::bash::ShellCwd::new(cwd.clone()),
        cwd,
        unsupported_server_tools,
        capture: Some(capture_store),
        include_task: true,
        fast: None,
        fast_ref: None,
        // 与句柄上那份是同一个：界面接回这条会话时要能读到「还有几条在跑」。
        shells: handle.background.clone(),
        running: crate::tools::control::RunningTools::new(),
    };
    // 会话文件里还没标题（`meta` 下面要整个移进 Runner，先问一句）。
    // 调用方给了标题（助理派工单时那个 `title`）：新会话一开始就叫这个名字，
    // 不再交给副模型起 —— 见 `SessionOptions::title`。
    let preset = opts
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty());
    if let (true, Some(t)) = (!resumed && meta.title.is_none(), preset) {
        meta.title = Some(t.to_string());
        // 用会话自己的 recorder 追加（不是新开一个句柄）：同一个文件上的两个追加
        // 句柄并发时会绞在一起，见 `Runner::spawn_title` 的同一段说明。
        //
        // 这一写还有第二个作用：会话文件是惰性建的，写下这条标题文件就存在了 ——
        // 派出去的工单对话因此**立刻**出现在侧栏里，不用等它跑出第一条消息。
        if let Some(rec) = thread.recorder() {
            rec.record(carter::session::RecordKind::Title {
                title: t.to_string(),
            });
        }
    }
    // 有名字了就不再欠一次自动起标题 —— 这里必须重算，不能沿用上面那个旧判断，
    // 否则带着标题开出来的会话还会白白问一次副模型（结果会被
    // `auto_title_allowed` 挡掉，纯浪费一次请求）。
    let unnamed = meta.title.is_none();
    // 续接一条没标题的老会话时，补标题该用**历史里的第一句**，而不是用户现在说的
    // 这句话（见 `Runner::title_seed`）。
    let first_user_text = if unnamed && resumed {
        first_user_prompt(&thread.messages)
    } else {
        None
    };
    let runner = Runner {
        ctx,
        thread,
        meta,
        cmd_rx,
        tool_tx: None,
        ask_tx: None,
        mcp_tools: Vec::new(),
        activity: Arc::new(AtomicU32::new(0)),
        goal_signal: Arc::new(GoalSignal::default()),
        goal,
        downgraded_goal: downgraded,
        queued: VecDeque::new(),
        // 主会话的系统提示每轮现算（技能会变、目标会变），见 `run_one_turn`。
        fixed_system: None,
        // 新会话：用这一轮的第一句话起名。
        // 续接一条**没有标题**的老会话：用历史里的第一句补上（`title_seed`）——
        // 否则它会永远停在「未命名会话」，而侧栏里一排没名字的会话分不清谁是谁。
        title_pending: unnamed && (!resumed || first_user_text.is_some()),
        title_seed: if resumed { first_user_text } else { None },
        last_message_at: last_written,
        changes: None,
        changes_turn: 0,
        overhead: 0,
        reported: Arc::default(),
    };
    // 待办在 `thread` 移进 runner 之前取一份：续接时底栏要直接显示已有的那份。
    let thread_todos = runner.thread.todos.clone();
    tokio::spawn(runner.run());
    let changes = crate::changes::load(&handle.id);
    let changes_since = crate::changes::since(&handle.id);
    let prompts = crate::prompts::load(&handle.id);
    let times = crate::timeline::load(&handle.id);
    Ok(StartedSession {
        handle,
        history,
        goal: goal_view,
        changes,
        changes_since,
        prompts,
        times,
        running: false,
        live: Vec::new(),
        pending_ask: None,
        // 会话文件里记着待办（carter 的 `RecordKind::Todo`），续接时直接带上 ——
        // 事件不回放，不在这里给的话底栏那条进度要等到下一次 `todo_write` 才有。
        todos: thread_todos.iter().map(todo_view).collect(),
        finished_tools: Vec::new(),
        // 新起的 runner 手里还没有后台命令（续接一条**还在跑**的会话走的是
        // `attach_session`，那条路会带回真正在跑的那些）。
        background_shells: Vec::new(),
    })
}

/// 侧边聊天（`/btw`）要一个精简版会话，见 [`crate::side`]。
///
/// 这里把 `start_session` 里那些侧边不需要的东西摊开成显式开关，而不是让 `side.rs`
/// 复制一遍装配流程 —— 会话的装配细节（provider 解析、抓包钩子、MCP）以后还会变，
/// 复制一份就意味着以后要改两处。
pub(crate) struct SideWiring {
    pub shell: Arc<AppShell>,
    pub cwd: PathBuf,
    pub model: ModelInfo,
    pub provider: Arc<dyn carter::provider::LlmProvider>,
    pub config: Arc<Config>,
    pub unsupported_server_tools: Vec<String>,
    /// 空的、**没有挂 recorder** 的 `Thread`（见 `side.rs::side_thread`）。
    pub thread: Thread,
    /// 系统提示的固定部分（父会话那一套 + 侧边那一段说明）。主对话的内容每轮现取、
    /// 接在它后面，见 [`Runner::run_one_turn`]。
    pub system: Vec<String>,
}

/// 起一个不落盘的侧边会话。返回的句柄照常可以投递命令。
pub(crate) fn start_side(w: SideWiring, id: String, parent_id: String) -> Arc<SessionHandle> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let handle = SessionHandle::new(
        id.clone(),
        w.cwd.clone(),
        w.model.clone(),
        Some(parent_id),
        cmd_tx,
    );
    handle.publish_history(&w.thread.messages);
    w.shell.sessions.insert(id.clone(), handle.clone());
    watch_background(&w.shell, &handle);

    let ctx = SessionCtx {
        shell: w.shell,
        handle: handle.clone(),
        model: w.model,
        provider: w.provider,
        config: w.config,
        settings: AppSettings::load(),
        resumed: false,
        shell_cwd: crate::tools::bash::ShellCwd::new(w.cwd.clone()),
        cwd: w.cwd,
        unsupported_server_tools: w.unsupported_server_tools,
        // 侧边会话：不派生子 agent、不连 MCP、不抓包。见 `side.rs` 的模块头。
        include_task: false,
        capture: None,
        fast: None,
        fast_ref: None,
        // 同上：侧边聊天也可能起后台命令，句柄上那份要跟着变。
        shells: handle.background.clone(),
        running: crate::tools::control::RunningTools::new(),
    };
    let runner = Runner {
        meta: side_meta(&handle),
        ctx,
        thread: w.thread,
        cmd_rx,
        tool_tx: None,
        ask_tx: None,
        mcp_tools: Vec::new(),
        activity: Arc::new(AtomicU32::new(0)),
        goal_signal: Arc::new(GoalSignal::default()),
        // 侧边会话不推目标：它是「临时问一句」，不该被一个长任务目标牵着走。
        goal: None,
        downgraded_goal: false,
        queued: VecDeque::new(),
        fixed_system: Some(w.system),
        // 侧边聊天不落盘，也就没有「会话列表里那一行」要起名。
        title_pending: false,
        title_seed: None,
        last_message_at: None,
        changes: None,
        changes_turn: 0,
        overhead: 0,
        reported: Arc::default(),
    };
    tokio::spawn(runner.run());
    handle
}

/// 历史里第一条用户消息的文本。给「续接一条没标题的老会话」补标题用。
///
/// 只要文本块：老会话的第一条完整消息可能带图片 / 工具结果，标题只该看字。
fn first_user_prompt(messages: &[carter::provider::WireMessage]) -> Option<String> {
    use carter::provider::{Block, Role};
    messages
        .iter()
        .find(|m| m.role == Role::User)
        .and_then(|m| {
            let text = m
                .content
                .iter()
                .filter_map(Block::as_text)
                .collect::<Vec<_>>()
                .join(" ");
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        })
}

/// 侧边会话没有会话文件，但 `SessionMeta` 在几处被读（git 快照、模型名）。
/// 这里只求填得合理 —— 它永远不会被写进磁盘。
fn side_meta(handle: &SessionHandle) -> session::SessionMeta {
    session::SessionMeta {
        id: handle.id.clone(),
        format_version: session::CURRENT_FORMAT_VERSION,
        parent_id: handle.parent_id.clone(),
        forked_from: None,
        cwd: handle.cwd.to_string_lossy().to_string(),
        git: None,
        title: Some("侧边聊天".to_string()),
        carter_version: env!("CARGO_PKG_VERSION").to_string(),
        model: handle.model.load().key.clone(),
        created_at: crate::protocol::now_ms(),
    }
}

/// 会话任务需要的一整套东西。打包成一个结构体是为了 `Runner` 的构造不至于十来个参数。
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
    ///
    /// `None` = 这条会话不抓包（侧边聊天，见 `side.rs`）。
    capture: Option<Arc<crate::capture::CaptureStore>>,
    /// 是否挂 `task`（派生子 agent）。侧边聊天不挂。
    include_task: bool,
    /// 副模型（`agent.fast_model`）解析出来的句柄，配了才算数。
    /// 见 [`SessionCtx::fast_model`] —— 压缩与标题走它。
    fast: Option<CompactModel>,
    /// 上面那份是按哪个引用解析出来的 —— 用户在设置页改了副模型，下一轮要重解析。
    fast_ref: Option<String>,
    /// 后台命令（`bash` 的 `run_in_background`）。一条会话一份，关会话时整组收掉。
    shells: Arc<crate::tools::background::BackgroundShells>,
    /// bash 此刻所在的目录。工具每轮重建，`cd` 的结果得放在会话这一层才留得住。
    shell_cwd: Arc<crate::tools::bash::ShellCwd>,
    /// 正在跑的工具表：界面上的「中断命令 / 转后台」靠它定位到具体那一次调用。
    running: Arc<crate::tools::control::RunningTools>,
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

    /// 副模型（`agent.fast_model`）：压缩上下文、生成标题、总结用它。
    ///
    /// 解析结果**缓存住**：解析一次要建一个 HTTP 客户端，而压缩判断每轮都要问一次。
    /// 缓存的键是引用串本身 —— 用户在设置页改了副模型，下一轮就重解析。
    ///
    /// 解析失败**不报错**（只 warn 一条）：副模型是锦上添花，为它把主对话带崩是错的。
    /// 调用方拿到 `None` 就回落主模型。
    fn fast_model(&mut self) -> Option<CompactModel> {
        let want = self
            .config
            .agent
            .fast_model
            .clone()
            .filter(|s| !s.trim().is_empty());
        if want.as_deref() == self.fast_ref.as_deref() {
            return self.fast.clone();
        }
        self.fast_ref = want.clone();
        let Some(reference) = want else {
            // 副模型被清掉了：回到「没配」的状态。
            self.fast = None;
            return None;
        };
        // 副模型就是这条会话正在用的模型：直接复用主 provider —— 再解析一遍
        // 要多建一个 HTTP 客户端，而它们会打出两个一模一样的请求。
        if reference == self.model.key {
            self.fast = Some(CompactModel {
                provider: self.provider.clone(),
                model: self.model.clone(),
            });
            return self.fast.clone();
        }

        // 副模型发的请求（压缩、标题）同样挂抓包钩子 —— 与 `switch_model` 一个道理：
        // 那些请求也是这条对话的一部分，静默不抓会让人以为「报文面板漏了」。
        // 它们没带 `WireRequest.meta`（carter 的压缩与标题不标会话），所以落进
        // `_orphan.jsonl` 而不是会话文件里。
        let hook = self
            .capture
            .as_ref()
            .map(|store| Arc::new(crate::capture::CaptureHook::new(store.clone())));
        let hooks: Vec<Arc<dyn llm_adapt_core::RequestHook>> = hook
            .iter()
            .map(|h| h.clone() as Arc<dyn llm_adapt_core::RequestHook>)
            .collect();

        let cache = match crate::catalog::read_cache() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("副模型 `{reference}` 用不上：读模型缓存失败（{e}）");
                return None;
            }
        };
        self.fast = match crate::catalog::resolve_provider(&self.config, &cache, &reference, hooks)
        {
            Ok(r) => {
                // 协议按**副模型自己那个渠道**标（主模型可能在另一个渠道、另一个协议上）。
                if let Some(h) = &hook {
                    label_capture(h, &self.config, &r.model);
                }
                Some(CompactModel {
                    provider: r.provider,
                    model: r.model,
                })
            }
            Err(e) => {
                tracing::warn!("副模型 `{reference}` 解析失败，这一轮回落主模型：{e}");
                None
            }
        };
        self.fast.clone()
    }

    /// 当前的系统提示（含技能目录）。技能开关与扫描路径都会变，所以每轮重算。
    ///
    /// 技能读的是 `self.settings`（每轮 [`SessionCtx::refresh`] 重读一遍）—— 用户在技能页
    /// 加了扫描路径，**下一轮就生效**，不需要重开会话。
    ///
    /// 项目级技能由会话的 cwd 定位：同一个设置在别的项目里打开的会话看到的是那个项目
    /// 的 `.carter/skills`，所以这里**必须**带上 cwd，不能复用一份全局扫描。
    fn system(&self, git: Option<&carter::session::GitInfo>) -> Vec<String> {
        let skill_metas = if self.config.skills.enabled {
            crate::skills::union_metas(&crate::skills::Scan::from_settings_at(
                &self.settings,
                Some(&self.cwd),
            ))
        } else {
            Vec::new()
        };
        let mut system = carter::prompt::assemble_system(
            &self.config,
            &self.cwd,
            &self.model.key,
            git,
            &skill_metas,
            true,
        );
        system.push(GUI_INSTRUCTIONS.to_string());
        system
    }
}

/// 告诉模型「这个界面能直接看见东西」。
///
/// 不给这段说明，模型只会把图存下来、把 HTML 写进文件，然后说一句「已生成，你去
/// 打开看看」—— 而界面上本来就是能直接看的（见 `src/components/Media.tsx`）。
/// 终端里那套「用户只看得到你的文本输出」的旧假设，在这里不成立。
///
/// 刻意写得短：这是一条**能力**说明，不是风格指令，每轮都随系统提示发出去。
pub(crate) const GUI_INSTRUCTIONS: &str = "\
## 这个界面能直接看见图与网页

用户用的是桌面图形界面，不是终端 —— 下面这些写了就**直接显示出来**，不要只说「已生成」：

- `![](相对或绝对路径)`：本地图片（png/jpg/gif/webp/bmp/svg）按会话工作目录解析，
  点击可放大。`read_file` 读到的图片会给出 `[img:…]` 引用，也能直接写进正文。
- 你自己要**看**一张图时直接 `read_file` 它，不要写脚本（PIL 等）去读。
- 用 ```html 或 ```svg 包起来的代码块会渲染成**可预览的页面**（预览 / 源码两个页签），
  所以图表、报告这类东西直接以 HTML 给出即可，不必先写文件再让用户自己去打开。
- 相对路径的链接按同一个工作目录解析；本地 `.html` 链接点了会就地渲染。";

// ---------------------------------------------------------------------------
// 运行器
// ---------------------------------------------------------------------------

/// 一轮跑完之后的账。自动继续的判定全看这几个数（见 `goal.rs::decide_after_turn`）。
struct TurnResult {
    /// 这一轮以错误收场（上游报错、输出被截断）。
    failed: bool,
    /// 这一轮调用过工具。
    used_tools: bool,
    /// 用户在这一轮里点过「停止」。
    user_cancelled: bool,
    /// 这一轮开始时历史里有几轮（含这一轮自己）。carter 中途压缩过的话，轮末靠它重排。
    turns_before: u32,
    /// carter 在这一轮**中途**自己压缩过时的摘要档位（见 [`Runner::after_turn`]）。
    mid_compact: Option<String>,
}

struct Runner {
    ctx: SessionCtx,
    /// 这份历史。会话任务**独占**它（`carter::agent::run_turn` 要 `&mut Thread`
    /// 一整轮攥着），别处要看进展走 [`SessionHandle::history`] 那份快照。
    ///
    /// 曾经包在 `Arc<Mutex<..>>` 里，为的是让自动压缩跑到后台。代价是压缩与这一轮
    /// 各改各的历史：压缩把结果写回共享那份，轮末又把整份换回去，压缩白做。现在
    /// 压缩在轮末**就地**做（见 [`Runner::compact`]）。
    thread: Thread,
    meta: session::SessionMeta,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    /// 工具卡片的出口（**同步**调用，与正文增量同一条时间线，见 [`ToolTx`]）。
    tool_tx: Option<crate::tools::record::ToolTx>,
    /// 工具发起的提问：接收端在 `forward_events` 任务手里。`None` = 还没建。
    ask_tx: Option<mpsc::UnboundedSender<AskRequest>>,
    mcp_tools: Vec<Arc<dyn Tool>>,
    activity: Arc<AtomicU32>,
    goal_signal: Arc<GoalSignal>,
    goal: Option<Goal>,
    /// 启动时盘上的目标从 `active` 降级成了 `paused`，要向用户说一声。
    downgraded_goal: bool,
    /// 生成中收到的、要等这一轮跑完再处理的命令。
    queued: VecDeque<Command>,
    /// 侧边会话的系统提示固定部分（见 [`SideWiring`]），主会话每轮现算。
    fixed_system: Option<Vec<String>>,
    /// 还欠这条会话一个自动标题（首轮开始时去生成，见 [`Runner::spawn_title`]）。
    title_pending: bool,
    /// 起标题用的那句话。`None` = 用这一轮用户刚发的那句。
    ///
    /// 续接一条**没有标题**的老会话时，拿用户**现在**说的这句话去起名是错的
    /// （这条对话早就不是那个话题了）；该用历史里的第一句。所以那条路把这里
    /// 填成历史里的第一句用户消息。
    title_seed: Option<String>,
    /// 最近一次有消息进历史的时间。助理对话靠它判断「空闲多久了」该不该切期。
    /// 启动时取会话文件的修改时间（重启之后也能接上空闲时长）。
    last_message_at: Option<u64>,
    /// 正在收集的这一轮的文件变更。轮首建、轮末收（见 `changes.rs`）。
    ///
    /// `None` = 不在轮内（空闲时），或监听器没建起来 —— 两种情况都只是「这一轮
    /// 没有清单可列」，不该影响对话本身。
    changes: Option<ChangeWatch>,
    /// 系统提示 + 工具声明的 token 数（见 [`Runner::measure_overhead`]）。
    overhead: u64,
    /// 最近一次主对话请求、上游回报的上下文占用（见 [`Runner::run_one_turn`] 里
    /// 处理 `StreamChunk::Usage` 那一段）。0 = 还没有
    /// （刚打开 / 刚续接，或历史刚被改写过），这时百分比环回落到估算值。
    reported: Arc<AtomicU64>,
    /// 上面那个监听器对应哪一轮。
    ///
    /// 必须在轮首记下来：`carter::agent::run_turn` 一进去就把 `thread.user_turns`
    /// 加一，轮末再去读就成了**下一轮**的号 —— 历史回看时清单会挂错回答。
    changes_turn: u32,
}

impl Runner {
    async fn run(mut self) {
        let session_id = self.ctx.handle.id.clone();

        // 异步的两条事件来源，最终都汇到 AppEvent 上：
        // * `tool_tx` —— 我们自己那层记录包装（**完整**的参数与结果）
        // * `ask_tx`  —— 工具发起的提问，需要等前端作答
        //
        // 正文增量不在这里：它们与工具卡片要严格保序，所以在产生的那一刻直接
        // `emit`（见模块文档）。工具记录与提问本来就不是 carter 的概念，走各自的
        // 通道由转发任务收口。
        let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
        let tx = emit_tool_event(self.ctx.shell.clone(), session_id.clone());
        self.tool_tx = Some(tx);
        self.ask_tx = Some(ask_tx);
        let forwarder = tokio::spawn(forward_events(
            self.ctx.shell.clone(),
            session_id.clone(),
            self.ctx.handle.asks.clone(),
            ask_rx,
        ));

        // MCP server 启动是异步的（要连子进程 / 网络）。失败不致命：
        // carter 的 McpManager 会跳过连不上的 server。
        //
        // 它在会话开头连一次就不再重连 —— 所以界面上改了 MCP 配置要**新开会话**
        // 才生效，这与「工具开关下一条消息就生效」不同，界面上分开提示。
        // 助理对话不连：它只有助理那一套工具（见 `tools::build_tools`），连上也用不到。
        let mcp_mgr =
            if self.ctx.include_task && self.ctx.handle.id != crate::assistant::CHAT_SESSION_ID {
                let (mgr, tools) = carter::mcp::McpManager::start(&self.ctx.config.mcp).await;
                self.mcp_tools = tools;
                Some(mgr)
            } else {
                None
            };

        self.emit(SessionEvent::Ready {
            session_id: session_id.clone(),
            model: self.ctx.model.api_name.clone(),
            model_ref: self.ctx.model.key.clone(),
            cwd: self.ctx.cwd.to_string_lossy().to_string(),
            resumed: self.ctx.resumed,
        });

        // 上下文占用也紧跟着发一次：百分比环要在第一条消息之前就有数（续接一条长会话时
        // 尤其要紧 —— 用户得先知道它已经快满了）。系统提示与工具声明这时先量一遍。
        let system = self.turn_system(self.meta.git.as_ref());
        let tools = self.turn_tools(&system);
        self.measure_overhead(&system, &tools);
        drop(tools);
        self.emit_context();

        // 目标状态紧跟 ready 发一次：界面上那条目标条靠它画出来。
        self.emit(SessionEvent::Goal {
            goal: self.goal.as_ref().map(goal::view),
        });
        if self.downgraded_goal {
            self.notice(
                "上次退出时这个目标还在进行中，已改为暂停。点目标条上的「继续」可以接着推进。",
            );
        }

        // 声明不出去的服务端工具**必须说出来**。静默丢弃是最坏的选项：
        // 用户以为开了联网搜索，模型其实一个结果都拿不到，而模型自己也不知道，
        // 只会一本正经地编。
        if !self.ctx.unsupported_server_tools.is_empty() {
            self.notice(&format!(
                "当前渠道不支持这些服务端工具，已忽略：{}",
                self.ctx.unsupported_server_tools.join("、")
            ));
        }

        // 后台命令自己结束时的通知。只在**空闲**时取：正在跑的那一轮里它就留在通道里，
        // 等这一轮（以及排在前面的用户消息）跑完再起新的一轮 —— 与 Claude Code 一样，
        // 模型起完后台命令就可以收工，结束了再被叫回来，而不是守着它轮询。
        let mut done_rx = self.ctx.shells.subscribe();
        // 别的会话发来的信。同样只在空闲时取；正在跑的那一轮里由工具结果捎带（见 `peers.rs`）。
        let inbox = self.ctx.handle.inbox.clone();

        loop {
            let cmd = match self.queued.pop_front() {
                Some(c) => c,
                None => {
                    tokio::select! {
                        cmd = self.cmd_rx.recv() => match cmd {
                            Some(c) => c,
                            // 命令通道关了（句柄全 drop）→ 会话结束。
                            None => break,
                        },
                        Some(id) = done_rx.recv() => {
                            // `None` = 模型这期间已经用 bash_output 看到它结束了，不必再报。
                            match self.ctx.shells.completion(&id) {
                                Some(text) => Command::SendPrompt { text, id: None, interject: false },
                                None => continue,
                            }
                        },
                        _ = inbox.notified() => match inbox.take_prompt() {
                            Some(text) => Command::SendPrompt { text, id: None, interject: false },
                            // 这一轮里已经搭着工具结果送过了。
                            None => continue,
                        },
                        // 显式关闭。光靠上面那条 `None` 是不够的 —— 会话任务自己攥着一个
                        // `Arc<SessionHandle>`，它持有发送端，所以 `recv()` 永远不会返回
                        // `None`。见 `shell.rs::close_session`。
                        _ = self.ctx.handle.close.cancelled() => break,
                    }
                }
            };
            self.handle_command(cmd).await;
            if self.ctx.handle.close.is_cancelled() {
                break;
            }
        }

        // 后台命令跟着会话走：会话没了，它们就不该继续占着端口 / CPU。
        // 这一步要在等转发任务之前做 —— 那些子进程的日志不会再有人读。
        self.ctx.shells.kill_all();
        if let Some(mgr) = mcp_mgr {
            mgr.shutdown().await;
        }
        self.ctx.handle.asks.clear();
        self.ctx.handle.close.set();
        drop(self.tool_tx.take());
        drop(self.ask_tx.take());
        let _ = forwarder.await;
        // 没读的信退回去、等着回执的告诉一声 —— 否则发信方会一直以为对方会回。
        crate::peers::on_exit(&self.ctx.shell, &self.ctx.handle);
        // 只摘**自己**那一条。按 id 摘的话，`close_session` 等超时之后已经开起来的
        // 接班 runner 会被一起摘掉 —— 注册表里看不到它，别人又按 id 开出第三个，
        // 同一个会话文件就有了多个写者。
        self.ctx
            .shell
            .sessions
            .remove_if(&session_id, |_, h| Arc::ptr_eq(h, &self.ctx.handle));
        self.ctx.shell.mark_closed(&session_id);
        // **最后**一句：告诉 `close_session` 那个等待方「文件与 Thread 都放掉了」。
        // 放在 remove 之后，保证它醒来时注册表里已经没有这条会话。
        self.ctx.handle.done.set();
    }

    async fn handle_command(&mut self, cmd: Command) {
        match cmd {
            // 没有正在跑的轮次时取消是空操作。
            Command::Cancel => {}
            Command::AnswerForm { id, answers } => {
                self.ctx.handle.asks.answer(id, answers);
            }
            Command::Goal { action } => {
                let was_active = self
                    .goal
                    .as_ref()
                    .is_some_and(|g| g.status == GoalStatus::Active);
                let mut goal = self.goal.clone();
                apply_goal_action(
                    &mut goal,
                    &self.ctx.handle.id.clone(),
                    &self.ctx.shell.clone(),
                    action,
                );
                let now_active = goal
                    .as_ref()
                    .is_some_and(|g| g.status == GoalStatus::Active);
                self.goal = goal;
                // 刚刚从「不推进」变成「推进」（设定了新目标、或点了继续）→ 立刻开干。
                // 不这么做的话，按了「继续」却什么都不发生，得再发一条消息才动 ——
                // 那颗按钮就没有意义了。
                if now_active && !was_active {
                    self.kick_goal().await;
                }
            }
            // 先重读配置再解析：`ctx.config` 是开会话时的快照，而换模型常常紧跟在
            // 「在设置页改了默认模型 / 加了个模型」之后 —— 不刷新的话，新加的模型
            // 会在旧配置里查不到，或者渠道改过的地址还用着旧的。
            Command::SetModel { model } => {
                self.ctx.refresh();
                match switch_model(&self.ctx, &model) {
                    Ok(resolved) => {
                        self.ctx.model = resolved.model;
                        self.ctx.provider = resolved.provider;
                        self.ctx.unsupported_server_tools = resolved.unsupported_server_tools;
                        self.ctx
                            .handle
                            .model
                            .store(Arc::new(self.ctx.model.clone()));
                        // 换模型要同时告诉前端两件事：真正发给上游的名字（`Notice` 里
                        // 那句话用它），以及 `渠道/显示名`（界面上的模型标签用它）。
                        self.emit(SessionEvent::ModelChanged {
                            model: self.ctx.model.api_name.clone(),
                            model_ref: self.ctx.model.key.clone(),
                        });
                        self.notice(&format!("模型已切换为 {}", self.ctx.model.api_name));
                        // 窗口大小跟着模型变，百分比要重算。
                        self.emit_context();
                    }
                    Err(e) => self.error(&format!("切换模型失败：{e}")),
                }
            }
            // 排队中的消息真正轮到它时走到这里。`interject` 在这里没有意义
            // （已经没有正在跑的轮次可以打断了），直接当普通消息处理。
            Command::SendPrompt { text, id, .. } => self.submit(text, id).await,
            // 历史里存的是展开后的正文，原样重发 —— 不再过一次展开，
            // 否则 `@文件` 的内容会被贴第二遍。
            Command::Regenerate { turn } => self.regenerate(turn).await,
            Command::RetryReply { turn } => self.retry_reply(turn).await,
            // 空闲时「插队」= 把那条消息挪到队首。下一轮循环就会取到它。
            Command::PromotePrompt { id } => promote(&mut self.queued, id),
            Command::DropPrompt { id } => drop_queued(&mut self.queued, id),
            // 空闲时按「中断命令」= 那条命令已经结束了，什么都不用做。
            Command::ToolAction { .. } => {}
            Command::Compact => {
                self.ctx.refresh();
                self.compact(false).await;
            }
            Command::Record { text, report } => self.record_only(text, report),
            Command::NewEpisode => self.close_episode(true).await,
            // 空闲时才走到这里（生成中收到的排队等这一轮跑完），所以下一轮的号
            // 就是 `user_turns` —— 起点从它开始，已经跑完的轮次都算「清空之前」。
            Command::ClearChanges => {
                let since = self.thread.user_turns;
                if let Err(e) = crate::changes::clear(&self.ctx.handle.id, since) {
                    tracing::warn!("清空变更记录失败：{e}");
                }
                self.emit(SessionEvent::ChangesCleared { since });
            }
        }
    }

    /// 重试：历史回退到这一轮之前，把它原样再发一遍。
    ///
    /// **只回滚对话，不动磁盘**。文件已经改过、命令已经跑过的事不会撤销 ——
    /// 界面上那句确认里也这么说，用户想要的是「让它重来一遍」，不是「把世界推回去」
    /// （后者是 carter 的 `/rewind`，走的是另一套检查点）。
    ///
    /// **改写提问不走这里**：那是开一条新分支（见 `sessions::fork`），原会话
    /// 一个字都不动 —— 改写过的对话本来就与原来那句提问对不上，塞回同一份历史
    /// 只会让它自相矛盾。
    ///
    /// 回退与重发是同一件事的两半：中间不插任何 `Idle`，否则界面会先看到一路空下去、
    /// 再慢慢长出来。`TurnRetracted` 紧跟着的不带 `prompt` 的 `turn_started`
    /// 复用的是界面上原本那条用户气泡。
    async fn regenerate(&mut self, turn: u32) {
        let git = self.meta.git.clone();
        // 上一轮可能有悬空的 tool_use；回退前先收口，免得把断口留在截断点之前。
        self.settle_history();
        let stored = match crate::history::retract_turn(&mut self.thread, turn) {
            Ok(t) => t,
            Err(e) => {
                self.error(&e);
                return;
            }
        };
        self.forget_reported();
        // 变更清单按轮存着，一起退 —— 否则被回退掉的那些回答的文件清单会留下来。
        // 重跑出来的那一轮会重新记自己那条（同一个轮次号）。
        if let Err(e) = crate::changes::retract(&self.ctx.handle.id, turn) {
            tracing::warn!("回退轮次变更记录失败：{e}");
        }
        // 原文只退这一轮**之后**的：这一轮的原文要留着（「编辑」框还得能回填）。
        if let Err(e) = crate::prompts::retract(&self.ctx.handle.id, turn + 1) {
            tracing::warn!("回退用户原文失败：{e}");
        }
        // 时刻退到这一轮**之前**：重跑出来的这一轮会重新记自己的开始时刻。
        if let Err(e) = crate::timeline::retract(&self.ctx.handle.id, turn) {
            tracing::warn!("回退时刻记录失败：{e}");
        }
        self.ctx.handle.publish_history(&self.thread.messages);
        self.emit(SessionEvent::TurnRetracted { turn });
        // `echo` 一律给 `None`：这一轮的用户气泡界面上已经有了，`TurnRetracted`
        // 刚刚交代过它 —— 再带一次 `prompt` 会多画一条。
        self.run_sequence(stored, git, None, None).await;
    }

    /// 就地重试一条回答：历史退到这一轮最后一条助手消息之前，**不重发提问**，
    /// 直接拿同一份上下文再请求一次。
    ///
    /// 与 [`Self::regenerate`] 的区别见 `history::rewind_reply`。截断点落在一轮
    /// 中间，界面没法自己删准，所以发一份 `HistoryReplaced` 让它整个重建，再接一条
    /// 不带 `prompt` 的 `turn_started` —— 新的增量接在重建出来的那一轮末尾。
    async fn retry_reply(&mut self, turn: u32) {
        let git = self.meta.git.clone();
        self.settle_history();
        if let Err(e) = crate::history::rewind_reply(&mut self.thread, turn) {
            self.error(&e);
            return;
        }
        self.forget_reported();
        // 这一轮自己的变更清单与原文都留着（前面几次往返改过的文件还算这一轮的），
        // 只退后面的。
        let id = self.ctx.handle.id.clone();
        if let Err(e) = crate::changes::retract(&id, turn + 1) {
            tracing::warn!("回退轮次变更记录失败：{e}");
        }
        if let Err(e) = crate::prompts::retract(&id, turn + 1) {
            tracing::warn!("回退用户原文失败：{e}");
        }
        if let Err(e) = crate::timeline::retract(&id, turn + 1) {
            tracing::warn!("回退时刻记录失败：{e}");
        }
        self.ctx.handle.publish_history(&self.thread.messages);
        self.emit(SessionEvent::HistoryReplaced {
            history: crate::view::to_views(&self.thread.messages),
            prompts: crate::prompts::load(&id),
            changes: crate::changes::load(&id),
            changes_since: crate::changes::since(&id),
            times: crate::timeline::load(&id),
        });

        self.ctx.refresh();
        self.begin_changes();
        let started_at = crate::protocol::now_ms();
        if let Err(e) = crate::timeline::started(&id, turn, started_at) {
            tracing::warn!("记录本轮开始时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnStarted {
            turn,
            prompt_id: None,
            prompt: None,
            started_at,
            report: None,
        });
        let result = self.run_one_turn(git.clone()).await;
        self.settle_history();
        self.finish_changes(result.used_tools).await;
        self.emit_turn_ended(turn, &result);
        self.after_turn(&result).await;
        self.drive_goal(result, git).await;
        self.last_message_at = Some(crate::protocol::now_ms());
        self.emit(SessionEvent::Idle);
    }

    /// 只往历史里记一条消息，不跑模型（见 `Command::Record`）。
    ///
    /// 界面上与一轮的开头一样画出来：`turn_started` 带原文，紧跟一条 `idle`。
    /// 它在历史里算一轮用户输入（正文非空的 user 消息），号要占上，否则之后的轮次号
    /// 与 `view.rs` 从历史数出来的对不上。
    ///
    /// `report` 跟着这一轮存进 `timeline`，并随 `turn_started` 发给界面。
    fn record_only(&mut self, text: String, report: Option<crate::timeline::ReportTag>) {
        self.settle_history();
        self.thread.append_user(text.clone());
        let turn = self.thread.user_turns;
        self.thread.user_turns = turn.saturating_add(1);
        self.ctx.handle.publish_history(&self.thread.messages);
        let started_at = crate::protocol::now_ms();
        if let Err(e) =
            crate::timeline::started_tagged(&self.ctx.handle.id, turn, started_at, report.clone())
        {
            tracing::warn!("记录本轮开始时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnStarted {
            turn,
            prompt_id: None,
            prompt: Some(text),
            started_at,
            report,
        });
        // 这条不跑模型，写完就算结束 —— 时间要立刻收口，否则界面上它会一直
        // 停在没有结束时刻的状态。
        let at = crate::protocol::now_ms();
        if let Err(e) = crate::timeline::ended(&self.ctx.handle.id, turn, at) {
            tracing::warn!("记录本轮结束时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnEnded {
            turn,
            at,
            failed: false,
            cancelled: false,
        });
        self.emit(SessionEvent::Idle);
        self.last_message_at = Some(crate::protocol::now_ms());
    }

    /// 助理对话：这一句话到来之前，看看这一期该不该结束（见 `assistant/memory.rs`）。
    ///
    /// 结束一期 = 原始消息另存进往期存档、用副模型整理出纪要与记忆、历史清空。
    /// 清空走 carter 的 `Compacted` 记录（空快照），会话文件只追加不改写，重启时
    /// 照样只加载本期。整理失败不挡这句话：纪要退成一句「整理失败」，历史照样切。
    ///
    /// `force`：用户在面板上点了「开新一期」（`Command::NewEpisode`），不看切期条件。
    async fn close_episode(&mut self, force: bool) {
        use crate::assistant::memory;
        if self.ctx.handle.id != crate::assistant::CHAT_SESSION_ID {
            return;
        }
        if self.thread.messages.is_empty() {
            if force {
                self.notice("这一期还没有对话，不用切");
            }
            return;
        }
        let now = crate::protocol::now_ms() + self.ctx.handle.clock_skew_ms.load(Ordering::SeqCst);
        let store = self.ctx.shell.assistant.clone();
        let started_at = store
            .read(|s| s.episodes.last().map(|e| e.ended_at))
            .unwrap_or(self.meta.created_at);
        let last_at = self.last_message_at.unwrap_or(now);
        let turns = crate::view::user_turn_count(&self.thread.messages);
        let used = self.history_tokens() + self.overhead;
        let window = u64::from(self.ctx.model.context_window);
        let why = if force {
            "手动开新一期"
        } else {
            let Some(why) = memory::should_close(last_at, now, turns, used, window) else {
                return;
            };
            why
        };
        self.settle_history();
        let messages = self.thread.messages.clone();
        let number = store.read(|s| s.episodes.last().map_or(1, |e| e.number + 1));
        let dir = memory::archive_dir();
        if let Err(e) = memory::save_archive(&dir, number, &messages) {
            // 存档写不下就不切：切了的话这一期的原话就只剩一行纪要。
            tracing::warn!(error = %e, "助理对话：往期存档写不下，这一期先不切");
            return;
        }
        self.notice(&format!(
            "{why}：整理第 {number} 期对话，长期记忆与纪要会保留"
        ));

        let user = memory::user_lines(&messages);
        let existing = store.read(|s| s.memories.clone());
        let prompt =
            memory::consolidate_prompt(&existing, &user, &memory::transcript(&messages, 24_000));
        let model = self.ctx.fast_model().unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        });
        let consolidated = crate::assistant::eval::ask_with(
            &*model.provider,
            &model.model,
            memory::CONSOLIDATE_SYSTEM,
            prompt,
            1500,
        )
        .await
        .and_then(|a| memory::parse_consolidated(&a));
        let (summary, ops) = match consolidated {
            Ok(c) if !c.summary.trim().is_empty() => (c.summary.trim().to_string(), c.memory),
            Ok(_) => ("（整理没给出纪要）".to_string(), Vec::new()),
            Err(e) => {
                tracing::warn!(error = %e, "助理对话：整理失败，只存档不提取记忆");
                (
                    format!("（整理失败：{e}。原话在第 {number} 期存档里）"),
                    Vec::new(),
                )
            }
        };
        let changed = store.with(|s| {
            let n = memory::apply_ops(&mut s.memories, &ops, &user, now);
            s.episodes.push(memory::Episode {
                number,
                started_at,
                ended_at: last_at,
                summary: summary.clone(),
                turns,
            });
            n
        });
        if let Err(e) = &changed {
            tracing::warn!(error = %e, "助理对话：纪要写不进 assistant.json，这一期先不切");
            return;
        }

        if let Some(rec) = self.thread.recorder() {
            rec.record(carter::session::RecordKind::Compacted {
                tier: "episode".into(),
                messages: Vec::new(),
            });
        }
        self.thread.messages.clear();
        self.thread.user_turns = 0;
        self.thread.turns = 0;
        let id = self.ctx.handle.id.clone();
        crate::prompts::remove(&id);
        crate::changes::remove(&id);
        crate::timeline::remove(&id);
        self.forget_reported();
        self.ctx.handle.publish_history(&self.thread.messages);
        self.emit(SessionEvent::HistoryReplaced {
            history: Vec::new(),
            prompts: Vec::new(),
            changes: Vec::new(),
            changes_since: 0,
            times: Vec::new(),
        });
        self.notice(&format!(
            "第 {number} 期已整理：{}{}",
            summary.lines().next().unwrap_or(""),
            match changed {
                Ok(n) if n > 0 => format!("（长期记忆改了 {n} 条）"),
                _ => String::new(),
            }
        ));
        self.emit(SessionEvent::AssistantEpisode { number });
        self.emit_context();
    }

    /// 用户发来一条消息：展开、落进历史、跑一轮，再按目标决定要不要自动继续。
    async fn submit(&mut self, raw: String, prompt_id: Option<u64>) {
        // 助理对话：用户开口前先看这一期该不该收。汇报（`record_only`）不走这里 ——
        // 只有用户的话才开新的一期。
        if !crate::assistant::is_injected(&raw) {
            self.close_episode(false).await;
        }
        // 用户亲口说话了：这一轮不再是哪封信的回信，跨会话的跳数从头数。
        if !crate::assistant::is_injected(&raw)
            && !raw.starts_with(crate::tools::background::DONE_MARK)
            && !raw.starts_with(crate::peers::PEER_MARK)
        {
            self.ctx.handle.inbox.reset_hops();
        }
        // 展开之后发给模型的是另一段文本，但界面上要显示用户**打的字**。
        let echo = Some(raw.clone());
        // 热重载落点：每轮开始前重读配置与应用设置。
        // 界面上改完工具开关、换完渠道、动过 system.md，下一条消息就按新的来。
        self.ctx.refresh();
        let git = self.meta.git.clone();

        // 输入框里的 `/命令`、`@文件`、`@图片` 在这里展开。
        //
        // 放在后端而不是前端：展开后的文本（几百行文件内容）不该出现在输入框
        // 里让用户看着它发出去。放在这里还有一个好处 —— 与 CLI 走的是同一套
        // 规则（自定义命令用 carter 的 `commands::expand`，图片用 carter 的
        // `media::inline_user_attachments`），两边行为对得上。
        //
        // 助理的汇报不展开：里面有外部事件的文字，写一个 `@~/.ssh/id_rsa` 不该把文件读进来。
        // 后台命令的完成通知同理：里面是命令输出。别的会话的来信也是别人写的字。
        let expansion = if crate::assistant::is_injected(&raw)
            || raw.starts_with(crate::tools::background::DONE_MARK)
            || raw.starts_with(crate::peers::PEER_MARK)
        {
            Ok((raw.clone(), None))
        } else {
            expand_input(&self.ctx.cwd, &raw)
        };
        let text = match expansion {
            Ok((expanded, note)) => {
                if let Some(note) = note {
                    self.notice(&note);
                }
                expanded
            }
            Err(e) => {
                // 展开失败不吞掉这条消息：把原文发出去，但要说清发生了什么。
                self.notice(&format!("{e}（已按原文发送）"));
                raw
            }
        };

        // 用户自己说话了 —— 上一轮「原地打转」的判定就此作废。
        if let Some(g) = self.goal.as_mut()
            && g.idle_streak != 0
        {
            g.idle_streak = 0;
            self.publish_goal();
        }

        self.run_sequence(text, git, prompt_id, echo).await;
    }

    /// 目标从「不推进」变成「推进」时踢一脚：不等用户再说话，直接开跑。
    ///
    /// 走的是与自动继续**同一条**消息与同一条路（`CONTINUE_PROMPT` + `run_sequence`），
    /// 所以界面上呈现成「自动继续」的分隔条而不是用户气泡 —— 它确实不是用户说的。
    async fn kick_goal(&mut self) {
        let git = self.meta.git.clone();
        self.run_sequence(goal::CONTINUE_PROMPT.to_string(), git, None, None)
            .await;
    }

    /// 把一段文本作为一轮的用户输入跑完（含它引发的自动继续）。
    ///
    /// `prompt_id` 是排队队列里那条消息的编号：`turn_started` 会原样带回，界面
    /// 据此把「排队中」的那一条挪进对话。自动继续（`/goal`）没有编号。
    async fn run_sequence(
        &mut self,
        text: String,
        git: Option<carter::session::GitInfo>,
        prompt_id: Option<u64>,
        echo: Option<String>,
    ) {
        // 上一轮如果以「模型发完 tool_use 就被中断」收场，历史里会留一个断口。
        // **必须在追加这条用户消息之前补上** —— 补在它之后，普通用户消息就会
        // 插在 tool_use 与 tool_result 中间，那仍然是一份非法历史。
        self.settle_history();
        let turn_start = self.thread.messages.len();
        self.thread.append_user(text.clone());
        // 标题在这一刻并行去生成，**不挡这一轮**（见 [`Runner::spawn_title`]）。
        // 放在 append 之后：carter 的 Recorder 是惰性建文件的，第一条消息落盘
        // 才让会话文件存在，而写标题要往那个文件里追加。
        if std::mem::take(&mut self.title_pending) {
            let seed = self.title_seed.take().unwrap_or_else(|| text.clone());
            self.spawn_title(&seed);
        }
        // 轮次开头也发布一次：这一轮可能很长，侧边聊天在这期间至少要能看到
        // 「用户提了什么要求」（之后的进展由 `live` 补上）。
        self.ctx.handle.publish_history(&self.thread.messages);
        self.begin_changes();
        // 用户**打的原文**按轮存一份：会话文件里是展开后的正文（`@文件` 的内容
        // 也可能在里面），拿它回填「编辑」框等于把整份文件摊在输入框里。
        // 自动继续推起来的那一轮没有原文（`echo` 是 `None`），本来也不给它编辑。
        if let Some(raw) = echo.as_deref()
            && let Err(e) = crate::prompts::record(&self.ctx.handle.id, self.thread.user_turns, raw)
        {
            tracing::warn!("记录用户原文失败：{e}");
        }
        // 轮末的记忆建议要看用户**打的原文**（`@文件` 展开前）。
        let raw = echo.clone();
        // 轮次号在**进 run_turn 之前**记下：carter 的 `run_turn` 一进去就把
        // `thread.user_turns` 加一（见 `changes_turn` 的同一段说明），轮末再读就成了
        // 下一轮的号 —— 完成时刻会被记到下一轮上，这一轮的回答永远等不到它。
        let turn = self.thread.user_turns;
        let started_at = crate::protocol::now_ms();
        // 时间按轮落盘：切走再回来、重启之后显示的还是当时的那个数，而不是
        // 「重开会话的时刻」。老会话没有这份记录，界面就不显示时间。
        if let Err(e) = crate::timeline::started(&self.ctx.handle.id, turn, started_at) {
            tracing::warn!("记录本轮开始时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnStarted {
            turn,
            prompt_id,
            prompt: echo,
            started_at,
            report: None,
        });

        let result = self.run_one_turn(git.clone()).await;
        // 一轮结束立刻收口：中断留下的断口现在就补进历史（并落盘），
        // 不必等用户下一次发消息。
        self.settle_history();
        self.finish_changes(result.used_tools).await;
        self.emit_turn_ended(turn, &result);
        // 压缩放在 `turn_ended` 与 `Idle` **之间**，而不是更早：助理之类的监听者
        // 一收到 `turn_ended` 就开始处理收尾（判「没汇报」、发通知、排下一轮），
        // 而压缩要发模型请求、要几秒。按源顺序处理事件的监听者会被它拖住，
        // 那几秒里什么都干不了；排进去的新消息还会因为这条会话还「忙」而多等一层。
        self.after_turn(&result).await;
        if !result.failed
            && !result.user_cancelled
            && let Some(raw) = raw.as_deref()
        {
            self.spawn_memory_suggest(raw, turn_start);
        }
        self.drive_goal(result, git).await;
        self.last_message_at = Some(crate::protocol::now_ms());
        self.emit(SessionEvent::Idle);
    }

    /// 记下这一轮跑完的时刻并发给界面。
    ///
    /// `turn` 必须由调用方在**进 `run_turn` 之前**取好传进来：carter 一进去就把
    /// `thread.user_turns` 加一，在这里现读会拿到下一轮的号（见 `run_sequence`）。
    ///
    /// 三条路径（正常响应、失败、被中断）都走这里：回答是不完整的也要显示时间 ——
    /// 界面上少一处时间比显示一个不完整回答的结束时刻更让人困惑。
    fn emit_turn_ended(&mut self, turn: u32, result: &TurnResult) {
        let at = crate::protocol::now_ms();
        if let Err(e) = crate::timeline::ended(&self.ctx.handle.id, turn, at) {
            tracing::warn!("记录本轮结束时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnEnded {
            turn,
            at,
            failed: result.failed,
            cancelled: result.user_cancelled,
        });
    }

    /// 历史自愈 + 界面收尾。
    ///
    /// 补上结果的那几条工具调用，界面上可能正悬着一张没有结果的卡片 ——
    /// 这里补发一对 `tool_started` / `tool_finished` 把它画完整，否则界面显示的
    /// 历史与磁盘上的历史会不一样（下次打开时才突然多出一张卡）。
    fn settle_history(&mut self) {
        let repaired = crate::history::repair_dangling_tool_uses(&mut self.thread);
        if repaired.is_empty() {
            return;
        }
        for call in repaired {
            self.emit(SessionEvent::ToolStarted {
                call_id: call.id.clone(),
                name: call.name,
                args: call.input,
            });
            self.emit(SessionEvent::ToolFinished {
                call_id: call.id,
                ok: false,
                content: crate::history::INTERRUPTED_RESULT.to_string(),
                elapsed_ms: 0,
            });
        }
        self.ctx.handle.publish_history(&self.thread.messages);
    }

    /// 首轮之后并行生成会话标题。
    ///
    /// ## 为什么是另一条任务
    ///
    /// 生成标题是一次完整的模型调用（哪怕用的是副模型也要几百毫秒到几秒）。串在
    /// `run_sequence` 里就会**挡住这一轮**：用户盯着的是第一句回答，而这里在等一个
    /// 只影响侧栏一行的东西。串在轮末也一样 —— 会话任务同时在处理队列里的命令，
    /// 标题那几秒里用户发第二条消息就得排队。
    ///
    /// ## 与手动改名的先后
    ///
    /// 写回走 [`crate::sessions::auto_title_allowed`]：**只在还没有标题时写**。用户在这几百
    /// 毫秒里自己改了名字的话（侧栏行内重命名），机器起的这个名字不该把它盖掉。
    ///
    /// ## 失败也不静默
    ///
    /// 副模型配错、网络不通、上游拒答 —— 重试之后仍然拿不到，就用首句话截出来的
    /// 兜底标题（见 [`crate::title::generate`]）。侧栏绝不能停在「未命名会话」上：
    /// 一排没名字的会话，用户分不清哪条是哪条。但兜底也不是完整的成功，所以
    /// 顺带发一条提示 —— 不弹错误条，只在对话里一行字。
    fn spawn_title(&mut self, first_prompt: &str) {
        // 侧边会话没有 recorder（不落盘），也就没有「列表里那一行」要起名。
        let Some(recorder) = self.thread.recorder() else {
            return;
        };
        // 与 carter CLI 同一个函数（`generate_title`）：3-6 词中文、取首行、去引号。
        //
        // 这里显式解析一次副模型（不能靠 `run_one_turn` 里那份缓存：标题跑在它前面，
        // 那时缓存还没填）。解析过一次之后就是同一份缓存。
        let (provider, model) = match self.ctx.fast_model() {
            Some(f) => (f.provider, f.model),
            // 没配副模型（或它解析不出来）：用主模型起标题，也好过没有标题。
            None => (self.ctx.provider.clone(), self.ctx.model.clone()),
        };
        // 渠道种类决定用哪种写法关思考（见 `title.rs`：推理模型会把 32 个 token
        // 全花在思考上，正文一个字都不留）。副模型可能挂在另一个渠道上，所以取
        // **它自己那个** kind。
        let kind = self.ctx.config.provider_for(&model.provider).kind;
        let shell = self.ctx.shell.clone();
        let session_id = self.ctx.handle.id.clone();
        let prompt = first_prompt.to_string();

        tokio::spawn(async move {
            let outcome =
                crate::title::generate(&prompt, &session_id, provider, &model, &kind).await;
            let title = outcome.title.trim().to_string();
            if title.is_empty() {
                return;
            }
            // 用户在这几秒里自己改过名字的话，机器起的名字（哪怕是兜底的）不该盖掉它。
            if !crate::sessions::auto_title_allowed(
                crate::sessions::title_of(&session_id).as_deref(),
            ) {
                return;
            }
            // 用**会话自己那个 recorder** 追加，而不是新开一个句柄：同一个文件上的
            // 两个追加句柄，写一行会拆成两次 write（正文 + 换行），并发时可能把两条
            // 记录绞在一起。这个 recorder 内部有 Mutex，是唯一安全的那条路。
            //
            // 文件还没建出来也不怕：carter 的 recorder 是惰性的，第一次写会先把
            // 文件头（SessionMeta）落下去 —— 第一条消息早就写过了，这一步只是兜底。
            recorder.record(carter::session::RecordKind::Title {
                title: title.clone(),
            });
            if outcome.degraded {
                emit(
                    &shell,
                    &session_id,
                    SessionEvent::Notice {
                        message: "自动命名没成功，先用首句话当了标题，可以右键重命名".into(),
                    },
                );
            }
            emit(&shell, &session_id, SessionEvent::Title { title });
        });
    }

    /// 轮末看看用户这句话里有没有值得长期记住的东西，有就交给界面请用户确认
    /// （见 `memory_suggest.rs`）。与标题一样**不挡下一轮**、失败就静默。
    ///
    /// 只在用户亲自打字的主会话里做：侧边聊天、助理对话、工单会话里的话不是
    /// 「用户对这个项目的长期要求」，后两者还常常没人在看，弹了也没人点。
    fn spawn_memory_suggest(&mut self, user_text: &str, turn_start: usize) {
        use crate::memory_suggest as ms;
        if self.fixed_system.is_some()
            || self.ctx.handle.id == crate::assistant::CHAT_SESSION_ID
            || crate::assistant::runtime::ticket_for_session(&self.ctx.shell, &self.ctx.handle)
                .is_some()
            || user_text.starts_with(crate::peers::PEER_MARK)
            || !ms::worth_checking(user_text)
        {
            return;
        }
        let guard = &self.thread;
        let turn = guard.messages.get(turn_start..).unwrap_or_default();
        // 模型自己已经记过了：同一件事再问一遍是打扰。
        if ms::saved_in(turn) {
            return;
        }
        let reply = ms::reply_text(turn);
        let model = self.ctx.fast_model().unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        });
        let existing = carter::memory::load(&self.ctx.cwd);
        let shell = self.ctx.shell.clone();
        let session_id = self.ctx.handle.id.clone();
        let user_text = user_text.to_string();
        tokio::spawn(async move {
            match ms::suggest(
                &*model.provider,
                &model.model,
                &session_id,
                &user_text,
                &reply,
                &existing,
            )
            .await
            {
                Ok(candidates) if !candidates.is_empty() => {
                    emit(
                        &shell,
                        &session_id,
                        SessionEvent::MemorySuggestion { candidates },
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("记忆建议没生成出来：{e}"),
            }
        });
    }

    /// 这一轮的系统提示。主会话每轮现算（技能会变、目标会变），侧边会话是固定部分
    /// 加上主对话的最新状态。
    fn turn_system(&self, git: Option<&carter::session::GitInfo>) -> Vec<String> {
        match &self.fixed_system {
            Some(s) => {
                let mut s = s.clone();
                // 侧边会话：主对话的最新状态（含进行中的那一轮）每轮现取。
                // 放在最后 —— 它是最「当下」的东西，也是每轮都会变的那段。
                if let Some(parent) = self.ctx.handle.parent_id.as_deref() {
                    match crate::side::parent_context(&self.ctx.shell, parent) {
                        Some(c) => s.push(c),
                        None => s.push("## 主对话\n\n（主对话已经关闭，看不到了。）".into()),
                    }
                }
                s
            }
            None => {
                let mut s = self.ctx.system(git);
                if self.ctx.handle.id == crate::assistant::CHAT_SESSION_ID {
                    s.push(crate::assistant::chat_segment(&self.ctx.shell.assistant));
                }
                // 目标段接在最后：它是最「当下」的指令，而且（将来 carter 打开
                // cache 断点后）末段正是断点位置 —— 所以它必须是稳定的，
                // 见 `goal.rs` 模块头。
                if let Some(seg) = self.goal.as_ref().and_then(goal::system_segment) {
                    s.push(seg);
                }
                s
            }
        }
    }

    /// 这一轮的工具表（工具开关、MCP、子 agent 都在这里装配）。
    fn turn_tools(&self, system: &[String]) -> carter::tools::ToolRegistry {
        let is_chat = self.ctx.handle.id == crate::assistant::CHAT_SESSION_ID;
        // 汇报推起来的那一轮只给一个只读工具：汇报里有外部事件的文字，这一轮只该转述。
        // 不能一个都不给 —— 历史里有 tool_use 时，Anthropic 协议要求请求必须声明 tools。
        let report_turn = crate::assistant::chat::is_report_turn(&self.thread.messages);
        if is_chat && report_turn {
            let mut bare = carter::tools::ToolRegistry::empty();
            bare.push(Arc::new(
                crate::tools::assistant_ticket::AssistantStatusTool::new(
                    self.ctx.shell.assistant.clone(),
                ),
            ));
            return self.recorded(bare);
        }
        // 常驻助理的工单会话：挂上 `assistant_report`；助理在后台推着跑、没人在看时，
        // 不给提问表单（没人回答，这一轮会永远卡住）。用户自己打开会话接着聊时照常给。
        let ticket =
            crate::assistant::runtime::ticket_for_session(&self.ctx.shell, &self.ctx.handle);
        let unattended = ticket.as_ref().is_some_and(|(_, u)| *u);
        // 助理对话同样可能没人在看（汇报推起来的那一轮）：表单没人答，这一轮卡住，
        // 后面的汇报就全堵在队列里。要问什么直接写在回复里。
        let bare = build_tools(
            ToolBuildArgs {
                provider: self.ctx.provider.clone(),
                model: self.ctx.model.clone(),
                agent_cfg: self.ctx.config.agent.clone(),
                base_system: system.join("\n"),
                cancel: self.ctx.handle.cancel.clone(),
                mcp_tools: self.mcp_tools.clone(),
                cwd: self.ctx.cwd.clone(),
                skills_enabled: self.ctx.config.skills.enabled,
                tool_tx: self.tool_tx.clone().expect("事件通道在 run() 里建好"),
                ask_tx: if self.ctx.include_task && !unattended && !is_chat {
                    self.ask_tx.clone()
                } else {
                    // 侧边聊天不挂 ask_user：抽屉里再套一层提问表单，与「临时问一句」
                    // 的定位不符，而且那套回答链路要单独接一遍。
                    None
                },
                include_task: self.ctx.include_task,
                goal_signal: self.goal_signal.clone(),
                schedules: self.ctx.shell.schedules.clone(),
                activity: self.activity.clone(),
                live: self.live(),
                shells: Some(self.ctx.shells.clone()),
                session_id: self.ctx.handle.id.clone(),
                shell_cwd: Some(self.ctx.shell_cwd.clone()),
                running: Some(self.ctx.running.clone()),
                assistant: ticket.map(|(id, _)| (self.ctx.shell.assistant.clone(), id)),
                assistant_chat: is_chat.then(|| self.ctx.shell.clone()),
                peers: (self.ctx.include_task && !is_chat).then(|| self.ctx.shell.clone()),
            },
            &self.ctx.settings,
        );
        // 主会话这一层统一套记录包装：一次调用只画一张卡，而且编号用**模型给的**
        // `call_id` —— 界面上的「中断 / 转后台」按它找到那条命令，历史里的卡片也按它
        // 配对。子 agent 那份快照在 `build_tools` 里已经套过了（它们模型给的编号可能
        // 跨 agent 重复，所以现编）。
        self.recorded(bare)
    }

    /// 把工具池里的每个工具套进 [`crate::tools::record::Recorded`]。
    fn recorded(&self, bare: carter::tools::ToolRegistry) -> carter::tools::ToolRegistry {
        wrap_recorded(
            bare,
            self.tool_tx.clone().expect("事件通道在 run() 里建好"),
            self.live(),
            Some(self.ctx.running.clone()),
            self.activity.clone(),
        )
    }

    /// 这一轮的用户说了什么 —— `UserPromptSubmit` 可阻断（拦下就不发请求）。
    ///
    /// carter 的 `run_turn` 不管这个 hook（它是宿主自己的「用户提交了一句话」语义），
    /// 所以放在进 `run_turn` 之前。返回 `None` = 放行。
    async fn dispatch_prompt_hook(&self, run_opts: &carter::agent::RunOptions) -> Option<String> {
        if !run_opts.hooks.has(HookEvent::UserPromptSubmit) {
            return None;
        }
        let last = self
            .thread
            .messages
            .last()
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|b| match b {
                        llm_adapt_core::Block::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        match run_opts
            .hooks
            .dispatch(
                HookEvent::UserPromptSubmit,
                serde_json::json!({ "prompt": last }),
            )
            .await
        {
            HookDecision::Block { reason } => Some(reason),
            _ => None,
        }
    }

    /// 记下「历史之外」每次请求都要带的那部分 token：系统提示 + 工具声明。
    ///
    /// 百分比环要的是**整个请求**的大小，只数历史会在工具很多（MCP 动辄几十个）时
    /// 偏小一大截。这两样每轮才变一次，所以在装配时顺手量一下存起来。
    fn measure_overhead(&mut self, system: &[String], tools: &carter::tools::ToolRegistry) {
        let tokenizer = &self.ctx.model.tokenizer;
        let specs = serde_json::to_string(&tools.specs()).unwrap_or_default();
        self.overhead = carter::tokens::count_text(&system.join("\n"), tokenizer)
            + carter::tokens::count_text(&specs, tokenizer);
    }

    /// 跑一轮：交给 carter 的 `run_turn`（发请求 → 流式 → 执行工具 → 回灌），
    /// 期间把界面上那几条「立刻生效」的命令读掉。
    ///
    /// 请求、工具调度、hook（`PreToolUse` / `PostToolUse` / `PostTurn` / `Stop`…）、
    /// 中途压缩、写前快照都在 carter 那一层 —— 这里**不再自己驱动循环**。曾经为了
    /// 「一条工具结果落地就发下一次请求」把整套循环搬过来，代价是一堆 carter 的行为
    /// 要在这里重新实现一遍（中途压缩的判定、todo 落盘与复诵、跨会话来信、检查点…），
    /// 漏掉哪一样都只在很远的表象上看得出来。而那个目标本身也做不到：协议要求同一条
    /// assistant 消息里的每个 `tool_use` 都在下一条 user 消息里有 `tool_result`。
    ///
    /// `git` 只有主会话有（侧边会话不重算系统提示），这里收一个 `Option` 是为了
    /// 让两条路共用同一个函数体。
    async fn run_one_turn(&mut self, git: Option<carter::session::GitInfo>) -> TurnResult {
        let system = self.turn_system(git.as_ref());
        // 只有状态为 active 时模型才该调 goal_update。
        self.goal_signal.set_armed(
            self.goal
                .as_ref()
                .is_some_and(|g| g.status == GoalStatus::Active),
        );

        let hooks = Arc::new(HookRegistry::from_configs(self.ctx.config.hooks.clone()));
        let tools = self.turn_tools(&system);
        self.measure_overhead(&system, &tools);
        // 这一轮开始时历史里有几轮（**含**这一轮自己 —— 用户消息在 `run_sequence`
        // 里已经 append 了）。carter 中途压过的话，轮后靠它重排轮次号；与
        // `TurnRemap` 的口径必须是同一个数。
        let turns_before = crate::view::user_turn_count(&self.thread.messages);

        // 压缩用副模型：这是「把长历史揉短」的活儿，要的是便宜和快。
        let fast = self.ctx.fast_model();
        let compact_model = Some(fast.unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        }));

        // 一轮里能调多少次模型由模型自己决定（不再要工具就收尾）。给 carter 一个用不完
        // 的上限 —— `max_turns` 是它 CLI 里的防失控保险，而工单里「翻几十个文档、逐个
        // 核对」的活很容易超过 50 次，撞上之后剩下的工具一条都不执行。
        let mut agent = self.ctx.config.agent.clone();
        agent.max_turns = u32::MAX;

        let run_opts = carter::agent::RunOptions {
            show_thinking: true,
            system,
            compact_model,
            hooks,
            purpose: carter::provider::RequestPurpose::MainTurn,
        };

        // `thread.turns` 在 carter 里是「整个会话累计的模型调用次数」，每轮清零让它
        // 回到名字上的意思：**一次用户输入**内最多几次模型调用。
        self.thread.turns = 0;
        // 上一轮可能残留了取消标记（用户在流结束后、select 收尾前点的停止）。
        self.ctx.handle.cancel.reset();
        let cancel = self.ctx.handle.cancel.clone();
        let activity_before = self.activity.load(Ordering::Relaxed);
        let mut user_cancelled = false;
        let mut closed = false;

        // 这一轮用户说了什么 —— `UserPromptSubmit` 可阻断。拦下时照样占掉轮次号：
        // 那条消息已经进历史了，不占的话之后的轮次号与 `view` 数出来的对不上。
        let blocked = self.dispatch_prompt_hook(&run_opts).await;
        // carter 在这一轮**中途**自己压过上下文时，由 sink 记下摘要档位。
        let mid = Arc::new(std::sync::Mutex::new(None));
        let outcome = if let Some(reason) = blocked {
            self.thread.user_turns = self.thread.user_turns.saturating_add(1);
            Err(carter::CarterError::Config(format!(
                "被 hook 拦下：{reason}"
            )))
        } else {
            let mut sink = TxSink {
                shell: self.ctx.shell.clone(),
                session_id: self.ctx.handle.id.clone(),
                live: self.live(),
                todos: self.ctx.handle.todos.clone(),
                mid: mid.clone(),
            };
            let tap = ContextTap {
                inner: self.ctx.provider.clone(),
                shell: self.ctx.shell.clone(),
                session_id: self.ctx.handle.id.clone(),
                window: self.ctx.model.context_window,
                reported: self.reported.clone(),
            };
            // 包一层收件箱：这一轮里每次工具返回都顺手把新到的信送进去。
            let turn = crate::peers::scope(
                self.ctx.handle.inbox.clone(),
                carter::agent::run_turn(
                    &mut self.thread,
                    &tap,
                    &self.ctx.model,
                    &agent,
                    &run_opts,
                    &tools,
                    &mut sink,
                    &cancel,
                ),
            );
            tokio::pin!(turn);

            let out = loop {
                tokio::select! {
                    out = &mut turn => break out,
                    // 生成中能**立刻**生效的命令；其余进队列，这一轮跑完再处理。
                    cmd = self.cmd_rx.recv() => match cmd {
                        Some(cmd) => {
                            user_cancelled |= handle_live_command(
                                cmd,
                                &self.ctx,
                                &mut self.queued,
                                &mut self.goal,
                                &mut self.changes,
                                self.changes_turn,
                            );
                        }
                        // 命令通道关了 = 句柄都没了，按关会话收尾。
                        None => {
                            closed = true;
                            cancel.set();
                        }
                    },
                    // 关会话（换走时空闲就关、应用退出）要立刻断流。
                    //
                    // **只响应一次**（`!closed`）：`close` 置位之后 `cancelled()` 每次都
                    // 立即就绪，不加这道闸的话 select 每圈都选中它，一个 worker 线程被
                    // 忙转占满，而 carter 收到第一次取消就 `reset()` 了。
                    _ = self.ctx.handle.close.cancelled(), if !closed => {
                        closed = true;
                        cancel.set();
                    }
                }
            };
            out.map(|(o, _)| o)
        };

        self.ctx.handle.publish_history(&self.thread.messages);

        let used_tools = self.activity.load(Ordering::Relaxed) > activity_before;
        // 「失败」的几种来源，对自动继续是同一件事：**别接着推**。
        let failed = match &outcome {
            Err(_) => true,
            // 被中断：用户自己点的停止不算失败（那条规则在 `decide_after_turn` 里
            // 优先级更高）；自己断的（关会话等）算，免得在这儿打转。
            Ok(carter::agent::TurnOutcome::Cancelled) => !user_cancelled,
            // 只剩「输出被截断」会走到这里（轮数不设限了）。
            Ok(carter::agent::TurnOutcome::Limit) => true,
            Ok(carter::agent::TurnOutcome::Error(_)) => true,
            Ok(carter::agent::TurnOutcome::Assistant) => false,
        };
        if let Err(e) = &outcome {
            // 关会话造成的断流不是错误；用户点了停止也一样（取消之后上游那次请求
            // 常以「已取消」收场，当成错误弹出来会让人以为「停止」弄坏了什么）。
            if !closed && !user_cancelled {
                self.error(&format!("{e}"));
            }
        }
        let mid_compact = mid.lock().ok().and_then(|mut t| t.take());
        TurnResult {
            failed,
            used_tools,
            user_cancelled,
            turns_before,
            mid_compact,
        }
    }

    /// 一轮跑完后按目标状态决定要不要接着推。
    ///
    /// 判定全在 `goal.rs::decide_after_turn`（纯函数、逐条测过），这里只负责
    /// 「照做」：落盘、发事件、追加那条「继续」再跑一轮。
    async fn drive_goal(&mut self, first: TurnResult, git: Option<carter::session::GitInfo>) {
        let mut result = first;
        loop {
            let Some(mut g) = self.goal.clone() else {
                return;
            };
            if g.status != GoalStatus::Active {
                return;
            }

            // 含本轮的连续无工具轮数。有工具调用就是「在干活」，清零。
            g.idle_streak = if result.used_tools {
                0
            } else {
                g.idle_streak.saturating_add(1)
            };
            let outcome = TurnOutcome {
                user_cancelled: result.user_cancelled,
                failed: result.failed,
                used_tools: result.used_tools,
                idle_streak: g.idle_streak,
                signal: self.goal_signal.take(),
            };

            match goal::decide_after_turn(&g, &outcome) {
                goal::Decision::None => {
                    // 无事发生，但空转计数变了，落一下盘。
                    self.set_goal(Some(g));
                    return;
                }
                goal::Decision::Set {
                    status,
                    note,
                    notice,
                } => {
                    g.status = status;
                    g.note = note;
                    g.updated_at = crate::protocol::now_ms();
                    self.set_goal(Some(g));
                    self.notice(&notice);
                    return;
                }
                goal::Decision::Continue => {
                    g.auto_continues = g.auto_continues.saturating_add(1);
                    self.set_goal(Some(g));

                    // 给「停止」留一点手感窗口，也避免退化成 tight loop。
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    if self.ctx.handle.close.is_cancelled() {
                        return;
                    }

                    // 这条消息会进历史（会话文件里也留一份，resume 后模型看得到自己
                    // 被推过；界面把它渲染成「自动继续」的分隔条而不是用户气泡）。
                    self.thread.append_user(goal::CONTINUE_PROMPT);
                    self.ctx.handle.publish_history(&self.thread.messages);
                    self.begin_changes();
                    // 同 `run_sequence`：`user_turns` 在进 `run_turn` 之后就被加一了。
                    let turn = self.thread.user_turns;
                    let started_at = crate::protocol::now_ms();
                    if let Err(e) = crate::timeline::started(&self.ctx.handle.id, turn, started_at)
                    {
                        tracing::warn!("记录本轮开始时刻失败：{e}");
                    }
                    self.emit(SessionEvent::TurnStarted {
                        turn,
                        // 自动继续没有用户消息编号 —— 界面据此画分隔条而不是气泡。
                        prompt_id: None,
                        prompt: None,
                        started_at,
                        report: None,
                    });
                    result = self.run_one_turn(git.clone()).await;
                    self.settle_history();
                    self.finish_changes(result.used_tools).await;
                    self.emit_turn_ended(turn, &result);
                    if self.ctx.handle.close.is_cancelled() {
                        return;
                    }
                    self.after_turn(&result).await;
                    if self.ctx.handle.close.is_cancelled() {
                        return;
                    }
                }
            }
        }
    }

    /// 一轮收尾时的上下文管理：发一次最新的占用；超线了就自动压缩。
    ///
    /// 先处理 carter 在这一轮**中途**自己做的那次压缩：它换掉的是更早那段历史，
    /// 轮次号、用户原文、变更清单、时刻记录都得跟着重排，界面也要整个重建一次
    /// （见 [`Runner::apply_compaction`]）。
    async fn after_turn(&mut self, result: &TurnResult) {
        if let Some(tier) = result.mid_compact.clone() {
            // carter 中途压完的历史已经在手上了，压之前多大已经量不到 —— 用此刻的
            // 估算当「之后」那一半，界面上那两个数只影响提示语的措辞。
            let now = self.history_tokens();
            self.apply_compaction(result.turns_before, now, tier, true);
        }
        let ctx = self.context_view();
        self.emit(SessionEvent::Context(ctx));
        if ctx.auto && ctx.threshold > 0 && ctx.used > ctx.threshold && !result.user_cancelled {
            self.compact(true).await;
        }
    }

    /// 压缩上下文。`auto` = 占用超线自动触发；否则是用户的 `/compact`。
    ///
    /// 两条路用同一个 carter 函数，差别只在目标值：自动压缩的目标是触发线（折叠旧工具
    /// 输出就够的话不必摘要），手动压缩的目标是 0（用户明说了要压，一定走到摘要）。
    ///
    /// **就地跑，不丢后台**：历史归会话任务独占（见 [`Runner::thread`]），后台那份
    /// 改完也没人接得住。摘要要几秒，这几秒里仍然读命令通道，停止 / 插队 / 作答都
    /// 照常送达；按了停止则**这一轮的下一次请求**不再发（压缩本身跑完，历史才不会
    /// 停在半截）。
    async fn compact(&mut self, auto: bool) {
        // 带着断口去摘要的话，摘要请求本身就会被上游拒掉。
        self.settle_history();
        let before = self.history_tokens();
        // 与 `TurnRemap` 同一口径：历史里数出来的轮数。
        let turns_before = crate::view::user_turn_count(&self.thread.messages);
        self.emit(SessionEvent::CompactStarted { auto });

        let fast = self.ctx.fast_model();
        let cm = fast.unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        });
        let target = if auto {
            self.context_view().threshold
        } else {
            0
        }
        .saturating_sub(self.overhead);

        let mut sink = CompactSink::default();
        let cancel = self.ctx.handle.cancel.clone();
        let mut stopped = false;
        let mut closed = false;
        let res = {
            let compact = carter::agent::context::compact(
                &mut self.thread,
                &*cm.provider,
                &cm.model,
                target,
                &mut sink,
            );
            tokio::pin!(compact);
            loop {
                tokio::select! {
                    r = &mut compact => break r,
                    cmd = self.cmd_rx.recv() => match cmd {
                        Some(cmd) => {
                            stopped |= handle_live_command(
                                cmd,
                                &self.ctx,
                                &mut self.queued,
                                &mut self.goal,
                                &mut self.changes,
                                self.changes_turn,
                            );
                        }
                        None => {
                            closed = true;
                            break Err(carter::CarterError::Config("会话已结束".into()));
                        }
                    },
                    _ = self.ctx.handle.close.cancelled(), if !closed => {
                        closed = true;
                        break Err(carter::CarterError::Config("会话已结束".into()));
                    }
                }
            }
        };
        // 取消标记在这一段里被消费掉了，下一轮不要一进去就判成取消。
        if stopped || cancel.is_cancelled() {
            cancel.reset();
        }
        if closed {
            return;
        }
        let after = self.history_tokens();
        self.forget_reported();

        match (res, sink.tier) {
            (Err(e), _) => self.error(&format!("压缩上下文失败：{e}")),
            (Ok(()), Some(tier)) => {
                self.apply_compaction(turns_before, before, tier, auto);
            }
            (Ok(()), None) => {
                // 没摘要成：要么折叠旧工具输出就够了（自动压缩），要么历史太短切不出
                // 中段，要么摘要请求失败（carter 已降级成只折叠）。三种都要说清。
                self.ctx.handle.publish_history(&self.thread.messages);
                let msg = if let Some(why) = sink.failures.last() {
                    format!(
                        "摘要没生成出来（{why}），只折叠了较早的工具输出：~{before} → ~{after} tokens"
                    )
                } else if after < before {
                    format!("已折叠较早的工具输出：~{before} → ~{after} tokens")
                } else {
                    "对话还很短，没有可以压缩的内容".to_string()
                };
                self.notice(&msg);
            }
        }
        self.emit_context();
    }

    /// 摘要压缩之后的善后：轮次号重排（历史、用户原文、变更清单、时刻四处一致），
    /// 发布新历史，让界面整个重建。
    ///
    /// `turns_before` 是压缩**之前**的轮数 —— `TurnRemap` 靠它把旧号映到新号，
    /// 被摘要吃掉的那几轮映不到（`None`）。
    fn apply_compaction(&mut self, turns_before: u32, before: u64, tier: String, auto: bool) {
        let remap = crate::history::TurnRemap::new(turns_before, &self.thread.messages);
        self.thread.user_turns = remap.after();
        let id = self.ctx.handle.id.clone();
        if !remap.is_identity() {
            if let Err(e) = crate::prompts::remap(&id, |t| remap.map(t)) {
                tracing::warn!("压缩后重排用户原文失败：{e}");
            }
            if let Err(e) = crate::changes::remap(&id, &remap) {
                tracing::warn!("压缩后重排变更记录失败：{e}");
            }
            if let Err(e) = crate::timeline::remap(&id, |t| remap.map(t)) {
                tracing::warn!("压缩后重排时刻记录失败：{e}");
            }
        }
        self.ctx.handle.publish_history(&self.thread.messages);
        self.forget_reported();
        let after = self.history_tokens();
        self.emit(SessionEvent::Compacted {
            history: crate::view::to_views(&self.thread.messages),
            prompts: crate::prompts::load(&id),
            changes: crate::changes::load(&id),
            changes_since: crate::changes::since(&id),
            times: crate::timeline::load(&id),
            before,
            after,
            tier,
            auto,
        });
    }

    /// 历史消息部分的估算 token 数。
    fn history_tokens(&self) -> u64 {
        carter::agent::context::estimate_tokens_for(
            &self.thread.messages,
            &self.ctx.model.tokenizer,
        )
    }

    /// 当前的上下文占用（见 [`ContextView`]）。
    ///
    /// 以上游回报的为准；还没有回报（刚打开、刚压缩完）时才用估算。
    fn context_view(&self) -> ContextView {
        let reported = self.reported.load(Ordering::Relaxed);
        context_view(
            if reported > 0 {
                reported
            } else {
                self.history_tokens() + self.overhead
            },
            self.ctx.model.context_window,
            &self.ctx.config.agent,
        )
    }

    /// 历史被我们自己改写过（压缩、回退）：上游上次回报的占用不再对得上，
    /// 回落到估算，直到下一次主对话请求带回新数。
    fn forget_reported(&self) {
        self.reported.store(0, Ordering::Relaxed);
    }

    fn emit_context(&self) {
        self.emit(SessionEvent::Context(self.context_view()));
    }

    /// 轮首：开始监听 cwd 的文件变更。
    ///
    /// 监听器只是**观察**，不参与工具执行 —— 它建不起来也不影响这一轮。
    fn begin_changes(&mut self) {
        // 上一轮理论上已经收走了；真剩下来也别把两轮的变更混在一起。
        self.changes = None;
        self.changes_turn = self.thread.user_turns;
        self.changes = ChangeWatch::start(&self.ctx.cwd, &self.ctx.settings.changes_ignore);
    }

    /// 轮末：收回这一轮真实变更的文件，落盘并推给界面。
    ///
    /// 空清单不发事件 —— 前端仍会从成功的 `write_file` / `edit_file` 参数里补，
    /// 那些落在 cwd 之外的写入（比如写 `/tmp`）监听器本来就看不见。
    async fn finish_changes(&mut self, used_tools: bool) {
        let Some(watch) = self.changes.take() else {
            return;
        };
        // 这一轮一个工具都没调，就不可能有人动文件 —— 连那 150ms 的收尾等待一起省掉。
        if !used_tools {
            return;
        }
        let done = watch.finish().await;
        if done.files.is_empty() {
            return;
        }
        let turn = self.changes_turn;
        if let Err(e) = crate::changes::record(
            &self.ctx.handle.id,
            turn,
            &done.files,
            done.baseline.as_deref(),
        ) {
            // 落不了盘只影响历史回看，实时清单照发。
            tracing::warn!("记录本轮变更文件失败：{e}");
        }
        self.emit(SessionEvent::FilesChanged {
            turn,
            files: done.files,
        });
    }

    /// 进行中那一轮要不要记下来：只有会被侧边聊天看的主会话才记。
    fn live(&self) -> Option<Arc<crate::shell::LiveLog>> {
        self.ctx
            .handle
            .parent_id
            .is_none()
            .then(|| self.ctx.handle.live.clone())
    }

    /// 改目标：落内存、落盘、发事件。三件事必须一起做 —— 少落盘的话重启就丢，
    /// 少发事件的话界面上的目标条会和实际状态对不上。
    fn set_goal(&mut self, goal: Option<Goal>) {
        self.goal = goal;
        self.publish_goal();
    }

    fn publish_goal(&self) {
        publish_goal(&self.goal, &self.ctx.handle.id, &self.ctx.shell);
    }

    fn emit(&self, event: SessionEvent) {
        match event {
            SessionEvent::TurnStarted { .. } => self.ctx.handle.busy.store(true, Ordering::SeqCst),
            SessionEvent::Idle => {
                self.ctx.handle.busy.store(false, Ordering::SeqCst);
                // 这一轮读过信、发信方要回执的，现在送（历史快照此时已经是这一轮的结尾）。
                crate::peers::on_idle(&self.ctx.shell, &self.ctx.handle);
                // 这一轮动过团队数据（`.carter/`）就收成一条提交。放在 idle 而不是
                // 每次工具调用：一轮里建三个任务改两个状态是**一件事**，应该是一条
                // 提交。侧边聊天不碰团队数据（它没挂 `team` 工具），提交是空转。
                if self.ctx.include_task {
                    crate::team::git::commit_turn_if_team(&self.ctx.cwd, self.thread.user_turns);
                }
            }
            _ => {}
        }
        emit(&self.ctx.shell, &self.ctx.handle.id, event);
    }

    fn notice(&self, message: &str) {
        self.emit(SessionEvent::Notice {
            message: message.to_string(),
        });
    }

    fn error(&self, message: &str) {
        self.emit(SessionEvent::Error {
            message: message.to_string(),
        });
    }
}

/// 把目标落盘并广播给界面。
fn publish_goal(goal: &Option<Goal>, session_id: &str, shell: &AppShell) {
    match goal {
        Some(g) => {
            if let Err(e) = goal::save(session_id, g) {
                emit(
                    shell,
                    session_id,
                    SessionEvent::Error {
                        message: format!("目标没能写进磁盘：{e}"),
                    },
                );
            }
        }
        None => {
            goal::remove(session_id);
        }
    }
    emit(
        shell,
        session_id,
        SessionEvent::Goal {
            goal: goal.as_ref().map(goal::view),
        },
    );
}

/// 执行一条 `/goal` 动作。**自由函数**，不碰 `Runner`：它要在跑一轮的中途被调用，
/// 而那时 `self.thread` 正被 `run_turn` 的可变借用占着 —— 只有「不碰 self」的代码
/// 才进得去那个 select 分支。
fn apply_goal_action(
    goal: &mut Option<Goal>,
    session_id: &str,
    shell: &AppShell,
    action: GoalCommand,
) {
    let Some(next) = goal::apply_action(goal.clone(), &action) else {
        emit(
            shell,
            session_id,
            SessionEvent::Notice {
                message: goal_noop_note(&action, goal.as_ref()).to_string(),
            },
        );
        return;
    };
    *goal = next;
    publish_goal(goal, session_id, shell);

    let message = match (goal.as_ref(), &action) {
        (Some(_), GoalCommand::Set { .. }) => "目标已设定，开始推进。".to_string(),
        (Some(_), GoalCommand::Resume) => "目标已继续。".to_string(),
        (Some(_), GoalCommand::Pause) => {
            "目标已暂停：不会再有自动继续（正在跑的那一轮会跑完）。".to_string()
        }
        (Some(_), GoalCommand::Complete) => "目标已标记为完成。".to_string(),
        _ => "目标已清除。".to_string(),
    };
    emit(shell, session_id, SessionEvent::Notice { message });
}

/// 排队操作的结果必须能一眼看出来：插队要真的到队首，撤回要真的删掉，
/// 而且都不能误伤别的命令。
#[cfg(test)]
mod queue_tests {
    use super::*;

    fn prompt(id: u64) -> Command {
        Command::SendPrompt {
            text: format!("m{id}"),
            id: Some(id),
            interject: false,
        }
    }

    fn ids(q: &VecDeque<Command>) -> Vec<Option<u64>> {
        q.iter()
            .map(|c| match c {
                Command::SendPrompt { id, .. } => *id,
                _ => None,
            })
            .collect()
    }

    #[test]
    fn promoting_moves_only_that_message_to_the_front() {
        let mut q = VecDeque::from(vec![prompt(1), Command::Cancel, prompt(2), prompt(3)]);
        promote(&mut q, 3);
        assert_eq!(ids(&q), vec![Some(3), Some(1), None, Some(2)]);
    }

    #[test]
    fn dropping_removes_exactly_one_and_is_harmless_if_unknown() {
        let mut q = VecDeque::from(vec![prompt(1), prompt(2), prompt(3)]);
        drop_queued(&mut q, 2);
        assert_eq!(ids(&q), vec![Some(1), Some(3)]);
        // 已经开始跑（或已经撤回过）的编号再点一次不该出错。
        drop_queued(&mut q, 99);
        assert_eq!(ids(&q), vec![Some(1), Some(3)]);
    }
}

/// 动作在当前状态下没意义时，说清是为什么 —— 静默无事发生最难查。
fn goal_noop_note(action: &GoalCommand, current: Option<&Goal>) -> &'static str {
    match (action, current) {
        (_, None) => "还没有设定目标。用 `/goal <目标>` 设一个。",
        (GoalCommand::Pause, _) => "目标不在进行中，无需暂停。",
        (GoalCommand::Resume, _) => "目标已经在推进了。",
        (GoalCommand::Complete, _) => "目标已经是完成状态。",
        (GoalCommand::Set { .. }, _) => "目标正文不能是空的。",
        (GoalCommand::Clear, _) => "没有可清除的目标。",
    }
}

/// 把排队中的某条用户消息提到最前面。
///
/// 只在队列里找 `SendPrompt`：其它命令（换模型、目标操作）没有「下一条用户输入」
/// 的语义，把它们提到队首只会让「插队」变得莫名其妙。
fn promote(queue: &mut VecDeque<Command>, id: u64) {
    let pos = queue
        .iter()
        .position(|c| matches!(c, Command::SendPrompt { id: Some(q), .. } if *q == id));
    if let Some(pos) = pos
        && let Some(cmd) = queue.remove(pos)
    {
        queue.push_front(cmd);
    }
}

/// 撤回一条还没开始跑的消息。找不到就当作已经跑了，什么都不做。
fn drop_queued(queue: &mut VecDeque<Command>, id: u64) {
    queue.retain(|c| !matches!(c, Command::SendPrompt { id: Some(q), .. } if *q == id));
}

/// 一轮**正在跑**时到达的命令：立刻生效的当场处理，其余进 `queued` 等这一轮跑完。
///
/// 读命令通道的地方有三个（等模型首字、读流、跑工具），它们都在 `run_one_turn`
/// 里，而那时 `self.ctx` 正被工具调用的 `CallCtx` 借着 —— 所以这里做成自由函数、
/// 只拿互不重叠的几个字段，而不是 `&mut Runner` 的方法。
///
/// **少了这套即时处理会出现什么**：用户点「转后台」之后，命令要等这一轮（以及
/// 那条命令）跑完才被读到；那时 `call_id` 早就失效了，`RunningTools::apply` 找不到
/// 它 —— 界面上就是「点了没反应」。停止、插队同样是这个道理。
///
/// 返回 `true` = 这条命令是用户在要求停下这一轮（停止 / 插队 / 把排队消息提到最前）。
/// 调用方靠它区分「用户主动停」与「会话被关掉」—— 前者不该报成失败（见 `run_one_turn`）。
fn handle_live_command(
    cmd: Command,
    ctx: &SessionCtx,
    queued: &mut VecDeque<Command>,
    goal: &mut Option<Goal>,
    changes: &mut Option<ChangeWatch>,
    changes_turn: u32,
) -> bool {
    match cmd {
        // 停止整轮：只置取消标记，流与工具都在听它，这一轮自己收拾。
        Command::Cancel => {
            ctx.handle.cancel.set();
            return true;
        }
        // 生成中也能发消息：默认排队，勾了「插队」就中断本轮、排到队首。
        Command::SendPrompt {
            text,
            id,
            interject,
        } => {
            let cmd = Command::SendPrompt {
                text,
                id,
                interject: false,
            };
            if interject {
                ctx.handle.cancel.set();
                queued.push_front(cmd);
                return true;
            }
            queued.push_back(cmd);
        }
        Command::PromotePrompt { id } => {
            promote(queued, id);
            ctx.handle.cancel.set();
            return true;
        }
        Command::DropPrompt { id } => drop_queued(queued, id),
        // 单条工具调用的「中断 / 转后台」。立刻打到那条命令上，不影响这一轮本身 ——
        // 工具会把结果回灌给模型，模型接着往下走。
        Command::ToolAction { call_id, action } => {
            // 找不到说明它刚好跑完了。不是错误，静默即可 —— 用户点按钮与命令结束
            // 本来就可能撞在同一毫秒。
            ctx.running.apply(&call_id, action);
        }
        Command::AnswerForm { id, answers } => {
            ctx.handle.asks.answer(id, answers);
        }
        // `/goal pause` 必须立刻生效：否则一条正在自动继续的长任务要等整串跑完
        // 才停得下来，而用户按暂停正是因为不想再等了。
        Command::Goal { action } => {
            let mut next = goal.clone();
            apply_goal_action(&mut next, &ctx.handle.id, &ctx.shell, action);
            *goal = next;
        }
        // 清空也要立刻生效：起点就是**正在跑的这一轮**，监听器重建一个 —— 新基线、
        // 空清单，这一轮清空之前动过的文件不再算。
        Command::ClearChanges => {
            if let Err(e) = crate::changes::clear(&ctx.handle.id, changes_turn) {
                tracing::warn!("清空变更记录失败：{e}");
            }
            // 只有正在记这一轮时才重建监听器 —— 空闲时没有可清的那一份，凭空起一个
            // 只会在下一次轮首被丢掉。
            if changes.is_some() {
                *changes = ChangeWatch::start(&ctx.cwd, &ctx.settings.changes_ignore);
            }
            emit(
                &ctx.shell,
                &ctx.handle.id,
                SessionEvent::ChangesCleared {
                    since: changes_turn,
                },
            );
        }
        other => queued.push_back(other),
    }
    false
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
        if !name.is_empty()
            && carter::commands::discover(cwd)
                .iter()
                .any(|c| c.name == name)
        {
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

/// 按 id 找会话，**跨所有项目**。
///
/// 不按 cwd 过滤：界面上能点到的会话（包括「会话」页里别的目录下的那些）都应该能
/// 打开 —— 否则会出现「列表里有、点开却是另一个会话」这种最令人困惑的失败。
/// 按 id 找是 uuid，全局唯一，让调用方传对目录只会引入「传错了所以打不开」
/// 这类沉默的失败。
fn find_by_id(id: &str) -> Option<session::SessionEntry> {
    session::list(std::path::Path::new("."), true)
        .into_iter()
        .find(|e| e.meta.id == id)
}

/// 续接一条历史会话时，这一轮该在哪个目录里跑。
///
/// 优先用会话文件里记的那份：从「会话」页点开别的目录下的会话时，工具、系统提示、
/// git 快照都该落在那条对话自己的目录里，而不是启动应用时的家目录。
///
/// 目录已经被删掉（换机器、清了盘）时退回请求里的 cwd —— 宁可在一个尚可的目录里
/// 继续聊，也不要因为一条历史会话打不开。抽成纯函数是为了给这条兜底一个能测的落点。
fn resumed_cwd(recorded: &str, requested: &std::path::Path) -> PathBuf {
    let recorded = PathBuf::from(recorded);
    if recorded.is_dir() {
        recorded
    } else {
        requested.to_path_buf()
    }
}

fn switch_model(
    ctx: &SessionCtx,
    model_ref: &str,
) -> carter::Result<carter::provider::resolve::ResolvedProvider> {
    let cache_json = catalog::read_cache().map_err(carter::CarterError::Config)?;
    // 换模型时也保留抓包 —— 否则一切换就静默停止记录了。
    let hook = ctx
        .capture
        .as_ref()
        .map(|store| Arc::new(crate::capture::CaptureHook::new(store.clone())));
    let hooks: Vec<Arc<dyn llm_adapt_core::RequestHook>> = hook
        .iter()
        .map(|h| h.clone() as Arc<dyn llm_adapt_core::RequestHook>)
        .collect();
    let resolved = crate::catalog::resolve_provider(&ctx.config, &cache_json, model_ref, hooks)?;
    // 换模型可能换协议（anthropic 渠道 → deepseek 渠道），钩子的协议要跟着走，
    // 否则新报文会按旧协议的语法去解。
    if let Some(h) = hook {
        label_capture(&h, &ctx.config, &resolved.model);
    }
    Ok(resolved)
}

/// 把「这条请求走的是哪条路线」告诉抓包钩子：协议、渠道、模型。
///
/// 钩子拿不到这些（它只看得见最终字节），而它们是解析 provider 时才定下来的 ——
/// 所以是「先建钩子、解析完再回填」。回填在任何请求之前完成。
///
/// 渠道（provider 名）必须记：同一个模型可能同时配在多个渠道上（官方源、中转、
/// 自建镜像），事后光看 URL 未必分得出来。
fn label_capture(hook: &crate::capture::CaptureHook, config: &Config, model: &ModelInfo) {
    hook.set_route(
        config.provider_for(&model.provider).protocol().as_str(),
        model.provider.as_str(),
        model.key.as_str(),
    );
}

fn emit(shell: &AppShell, session_id: &str, event: SessionEvent) {
    shell.emit(AppEvent::Session {
        session_id: session_id.to_string(),
        event,
    });
}

/// 把两条异步来源翻译成统一的 IPC 事件。
///
/// 正文增量不经过这里，见模块文档：它们要跟工具卡片严格保序，所以同步 `emit`。
///
/// carter 的 `ToolCallStarted` / `ToolResult` / `AskUser` **刻意丢弃**：
/// 前两者只带截断的预览（结果只有第一行 120 字符），界面上画不出有意义的卡片；
/// 后者是 carter 那个一问一答的 ask_user 工具，已经被我们自己的实现顶掉了。
/// 留着它们只会让每个工具出现两张卡片。
async fn forward_events(
    shell: Arc<AppShell>,
    session_id: String,
    asks: Arc<AskRegistry>,
    mut ask_rx: mpsc::UnboundedReceiver<AskRequest>,
) {
    let mut ask_open = true;

    let send = |shell: &AppShell, event: SessionEvent| {
        shell.emit(AppEvent::Session {
            session_id: session_id.clone(),
            event,
        });
    };

    while ask_open {
        tokio::select! {
            req = ask_rx.recv(), if ask_open => match req {
                Some(AskRequest::Questions { id, questions, reply }) => {
                    // 发送端存住：前端要跨一次进程边界才回得来，而 oneshot 只能发一次。
                    asks.insert(id, questions.clone(), reply);
                    send(&shell, SessionEvent::AskForm { id, questions });
                }
                None => ask_open = false,
            },
        }
    }
}

/// 给工具池里的每个工具套一层记录包装（完整参数与结果 → 界面）。
///
/// 主会话用它（编号取模型给的 `call_id`，见 [`crate::tools::record::Recorded`]）。
/// 抽成自由函数是为了能直接测「一次调用只画一张卡」。
fn wrap_recorded(
    bare: carter::tools::ToolRegistry,
    tx: crate::tools::record::ToolTx,
    live: Option<Arc<crate::shell::LiveLog>>,
    running: Option<Arc<crate::tools::control::RunningTools>>,
    activity: Arc<AtomicU32>,
) -> carter::tools::ToolRegistry {
    let seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut out = carter::tools::ToolRegistry::empty();
    for t in bare.tools() {
        out.push(Arc::new(
            crate::tools::record::Recorded::new(
                t.clone(),
                tx.clone(),
                seq.clone(),
                activity.clone(),
            )
            .with_live(live.clone())
            .with_running(running.clone())
            .with_model_call_id(),
        ));
    }
    out
}

/// 工具卡片 → IPC 事件。**同步**：与正文增量走同一条时间线，顺序才不会反过来。
fn emit_tool_event(shell: Arc<AppShell>, session_id: String) -> crate::tools::record::ToolTx {
    Arc::new(move |ev| {
        let event = match ev {
            ToolEvent::Started {
                call_id,
                name,
                args,
            } => SessionEvent::ToolStarted {
                call_id,
                name,
                args,
            },
            ToolEvent::Finished {
                call_id,
                ok,
                content,
                elapsed_ms,
            } => SessionEvent::ToolFinished {
                call_id,
                ok,
                content,
                elapsed_ms,
            },
            ToolEvent::Agent { call_id, agents } => SessionEvent::AgentProgress { call_id, agents },
        };
        shell.emit(AppEvent::Session {
            session_id: session_id.clone(),
            event,
        });
    })
}

/// carter `run_turn` 的渲染出口 → IPC 事件。
///
/// 工具卡片不从这里走：那一路是 [`crate::tools::record::Recorded`] 发的（带完整的
/// 参数与结果），carter 那两条只有截断的预览。
struct TxSink {
    shell: Arc<AppShell>,
    session_id: String,
    /// 进行中那一轮的记录，见 `shell.rs::LiveLog`。
    live: Option<Arc<crate::shell::LiveLog>>,
    /// 待办落在这里（`thread.todos` 由 carter 自己维护并复诵给模型）。
    todos: Arc<arc_swap::ArcSwap<Vec<carter::tools::TodoItem>>>,
    /// carter 在一轮**中途**自己压缩过时的摘要档位（见 [`Runner::after_turn`]）。
    mid: Arc<std::sync::Mutex<Option<String>>>,
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
struct ContextTap {
    inner: Arc<dyn carter::provider::LlmProvider>,
    shell: Arc<AppShell>,
    session_id: String,
    window: u32,
    reported: Arc<AtomicU64>,
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
fn context_view(used: u64, window: u32, agent: &carter::config::AgentConfig) -> ContextView {
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
fn summary_tier(notice: &str) -> Option<String> {
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
struct CompactSink {
    tier: Option<String>,
    failures: Vec<String>,
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

#[cfg(test)]
mod live_close {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_close_kills_a_running_bash_promptly() {
        assert!(std::env::var_os("CARTER_HOME").is_some());
        let shell = AppShell::new(carter::config::Config::load().unwrap());
        let dir =
            std::env::temp_dir().join(format!("carter-live-close-{}", crate::protocol::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut rx = shell.subscribe();
        let started = start_session(
            shell.clone(),
            SessionOptions {
                cwd: dir.clone(),
                model: None,
                resume_latest: false,
                resume_id: None,
                new_id: None,
                title: None,
            },
        )
        .unwrap();
        let id = started.handle.id.clone();
        started.handle.send(Command::SendPrompt {
            text: "用 bash 工具执行 `sleep 40 && echo done`，前台执行，不要后台。".into(),
            id: None,
            interject: false,
        });
        loop {
            match tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                AppEvent::Session {
                    event: SessionEvent::ToolStarted { name, .. },
                    ..
                } if name == "bash" => break,
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let t = Instant::now();
        shell.close_session(&id).await;
        let waited = t.elapsed();
        println!(
            "close_session 返回用了 {waited:?}，done={}",
            started.handle.done.is_cancelled()
        );
        let t = Instant::now();
        while !started.handle.done.is_cancelled() && t.elapsed() < Duration::from_secs(60) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!("runner 真正退出又过了 {:?}", t.elapsed());
        assert!(started.handle.done.is_cancelled());
        assert!(
            waited < Duration::from_secs(5),
            "关会话要在 5 秒内收掉正在跑的 bash"
        );
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
            new_id: None,
            title: None,
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

    /// 动作在当前状态下没有意义时，提示必须说清是哪种「没意义」。
    #[test]
    fn a_goal_no_op_explains_itself() {
        let none = goal_noop_note(&GoalCommand::Pause, None);
        assert!(none.contains("/goal"), "要告诉用户怎么设：{none}");

        let g = Goal::new("x");
        assert!(goal_noop_note(&GoalCommand::Pause, Some(&g)).contains("不在进行中"));
        let paused = {
            let mut g = g.clone();
            g.status = GoalStatus::Paused;
            g
        };
        assert!(goal_noop_note(&GoalCommand::Resume, Some(&paused)).contains("已经在推进"));
    }

    /// 续接历史会话时按会话自己记的目录跑；目录没了就退回请求里的 cwd。
    ///
    /// 不这么做的话，从「会话」页点开一条别的项目下的会话，会拿家目录去当工作目录 ——
    /// 工具、系统提示、git 快照全错位，而且界面上只表现为「工作目录跳了」。
    #[test]
    fn a_resumed_session_keeps_its_own_directory_and_falls_back_when_its_gone() {
        let mine = std::env::temp_dir().join(format!(
            "carter-app-resume-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&mine).unwrap();

        let requested = std::path::Path::new("/definitely/not/here");
        let recorded = mine.to_string_lossy().to_string();
        assert_eq!(resumed_cwd(&recorded, requested), mine);

        // 记下来的目录已经不在了：别把会话卡死，退回去继续能用。
        let gone = mine.join("deleted-just-now");
        assert_eq!(
            resumed_cwd(&gone.to_string_lossy(), requested),
            requested.to_path_buf()
        );

        std::fs::remove_dir_all(&mine).unwrap();
    }

    /// 一次工具调用只能画**一张**卡，编号用模型给的那个。
    ///
    /// 守的是「明明只调了一次，界面上却有两行一模一样的 `bash`」那个 bug：工具池与
    /// 回调层各套一层 `Recorded` 的话，同一句命令会画两张卡，一张用模型的 `call_id`、
    /// 一张用现编的号，两张还都能拿到结果、都打勾。主会话只在 `wrap_recorded` 这一处
    /// 套（`build_tools` 返回的是裸工具），所以这里数一遍就够。
    #[tokio::test]
    async fn one_dispatch_draws_exactly_one_card() {
        struct Echo;
        #[async_trait::async_trait]
        impl Tool for Echo {
            fn name(&self) -> &str {
                "bash"
            }
            fn description(&self) -> &str {
                "echo"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(&self, _args: serde_json::Value) -> carter::tools::ToolResult {
                carter::tools::ToolResult::ok("ok")
            }
        }

        let mut bare = carter::tools::ToolRegistry::empty();
        bare.push(Arc::new(Echo));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink: crate::tools::record::ToolTx = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });
        let pool = wrap_recorded(bare, sink, None, None, Arc::new(AtomicU32::new(0)));
        assert_eq!(pool.tools().len(), 1, "一个工具只该被套一层");
        let _ = pool.dispatch("bash", serde_json::json!({})).await;

        let mut started = Vec::new();
        let mut finished = 0;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ToolEvent::Started { call_id, .. } => started.push(call_id),
                ToolEvent::Finished { .. } => finished += 1,
                ToolEvent::Agent { .. } => {}
            }
        }
        assert_eq!(started.len(), 1, "一次调用只该画一张卡：{started:?}");
        assert_eq!(finished, 1);
    }
}

#[cfg(test)]
mod no_turn_cap {
    /// 一轮里能调多少次模型，不该由配置拍脑袋定：翻几十个文档的工单很容易超过
    /// carter 默认的 50，撞上之后剩下的工具一条都不执行。
    ///
    /// 现在不再自己驱动循环，这条守的是「交给 carter 的上限被抬掉了」。
    #[test]
    fn the_running_loop_has_no_model_call_cap() {
        let src = include_str!("session.rs");
        let body = src
            .split("async fn run_one_turn(")
            .nth(1)
            .and_then(|s| s.split("async fn drive_goal(").next())
            .expect("找得到 run_one_turn");
        assert!(
            body.contains("agent.max_turns = u32::MAX"),
            "交给 carter 的 agent 配置必须把轮数上限抬掉"
        );
        assert!(
            !body.contains("self.ctx.config.agent.max_turns"),
            "run_one_turn 里不该读配置里的上限"
        );
    }
}
#[cfg(test)]
mod live_commands {
    use super::*;
    use crate::protocol::ToolActionKind;

    /// 一条能调 [`handle_live_command`] 的最小会话上下文：不碰网络、不建工具池。
    fn live_ctx() -> SessionCtx {
        struct Quiet;
        #[async_trait::async_trait]
        impl carter::provider::LlmProvider for Quiet {
            async fn stream(
                &self,
                _req: carter::provider::ChatRequest,
            ) -> carter::Result<carter::provider::EventStream> {
                unreachable!("这条测试不发请求")
            }
        }

        let dir = std::env::temp_dir();
        let model = carter::registry::ModelInfo {
            key: "k".into(),
            provider: "p".into(),
            api_name: "m".into(),
            context_window: 100_000,
            max_output_tokens: 8_000,
            tokenizer: "cl100k".into(),
            capabilities: vec![],
            pricing: carter::registry::Pricing {
                input: 0.0,
                output: 0.0,
                cache_write: None,
                cache_read: None,
            },
            default_reasoning: None,
        };
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let handle = SessionHandle::new(
            "live-cmd-test".into(),
            dir.clone(),
            model.clone(),
            None,
            cmd_tx,
        );
        SessionCtx {
            shell: AppShell::new(Config::default()),
            handle: handle.clone(),
            model,
            provider: Arc::new(Quiet),
            config: Arc::new(Config::default()),
            settings: AppSettings::default(),
            resumed: false,
            cwd: dir.clone(),
            unsupported_server_tools: Vec::new(),
            capture: None,
            include_task: false,
            fast: None,
            fast_ref: None,
            shells: handle.background.clone(),
            shell_cwd: crate::tools::bash::ShellCwd::new(dir),
            running: crate::tools::control::RunningTools::new(),
        }
    }

    fn prompt(id: u64, interject: bool) -> Command {
        Command::SendPrompt {
            text: format!("m{id}"),
            id: Some(id),
            interject,
        }
    }

    fn ids(q: &VecDeque<Command>) -> Vec<Option<u64>> {
        q.iter()
            .map(|c| match c {
                Command::SendPrompt { id, .. } => *id,
                _ => None,
            })
            .collect()
    }

    /// 这一条守的就是「界面上点『转后台』没反应」：在**一轮正在跑**的时候把命令
    /// 送进 `handle_live_command`，它必须当场打到那条正在执行的工具调用上。
    #[tokio::test]
    async fn a_tool_action_mid_turn_reaches_the_running_call() {
        let ctx = live_ctx();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let running = ctx.running.clone();
        let task = tokio::spawn(async move {
            running
                .scope("call_1", async move {
                    let control = crate::tools::control::current().expect("scope 里应有控制句柄");
                    tx.send(()).ok();
                    control.backgrounded().await;
                    control.call_id().to_string()
                })
                .await
        });

        // 等工具真的注册进去，再按按钮 —— 与界面上那条命令跑起来之后才出现按钮一样。
        rx.await.unwrap();
        let mut queued = VecDeque::new();
        let (mut goal, mut changes) = (None, None);
        let cancellable = handle_live_command(
            Command::ToolAction {
                call_id: "call_1".into(),
                action: ToolActionKind::Background,
            },
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );

        assert_eq!(task.await.unwrap(), "call_1", "按钮要打到正在跑的那一条上");
        assert!(!cancellable, "工具操作不是「停下这一轮」");
        assert!(queued.is_empty(), "工具操作不该进排队");
    }

    /// 找不到对应调用的按钮不算错误：用户点下去与命令结束可能撞在同一毫秒。
    #[test]
    fn a_tool_action_for_a_finished_call_is_dropped_quietly() {
        let ctx = live_ctx();
        let mut queued = VecDeque::new();
        let (mut goal, mut changes) = (None, None);
        handle_live_command(
            Command::ToolAction {
                call_id: "never-existed".into(),
                action: ToolActionKind::Interrupt,
            },
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(queued.is_empty());
    }

    /// 生成中点「停止」要立刻断流（置取消标记），不能等这一轮跑完。
    #[test]
    fn cancel_mid_turn_sets_the_token_immediately() {
        let ctx = live_ctx();
        assert!(!ctx.handle.cancel.is_cancelled());
        let mut queued = VecDeque::new();
        let (mut goal, mut changes) = (None, None);
        let cancellable = handle_live_command(
            Command::Cancel,
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(cancellable, "停止是「停下这一轮」");
        assert!(ctx.handle.cancel.is_cancelled());
        assert!(queued.is_empty(), "取消不是一条要排队的消息");
    }

    /// 插队 = 中断本轮 + 排到队首；普通发送只是排队。
    #[test]
    fn interjecting_cancels_and_jumps_the_queue() {
        let ctx = live_ctx();
        let mut queued = VecDeque::from(vec![prompt(1, false)]);
        let (mut goal, mut changes) = (None, None);

        let cancellable = handle_live_command(
            prompt(2, true),
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(cancellable, "插队要打断这一轮");
        assert_eq!(ids(&queued), vec![Some(2), Some(1)]);
        assert!(ctx.handle.cancel.is_cancelled());

        ctx.handle.cancel.reset();
        let mut queued = VecDeque::from(vec![prompt(1, false)]);
        let cancellable = handle_live_command(
            prompt(3, false),
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(!cancellable, "普通发送不该打断这一轮");
        assert_eq!(ids(&queued), vec![Some(1), Some(3)]);
        assert!(!ctx.handle.cancel.is_cancelled(), "普通发送不该打断这一轮");
    }

    /// 其它命令（压缩、开新一期…）照旧排队，等这一轮跑完再处理。
    #[test]
    fn everything_else_waits_for_the_turn_to_end() {
        let ctx = live_ctx();
        let mut queued = VecDeque::new();
        let (mut goal, mut changes) = (None, None);
        handle_live_command(
            Command::Compact,
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(matches!(queued.front(), Some(Command::Compact)));
    }
}

#[cfg(test)]
mod live_turn {
    //! 一轮**真的在跑**的时候，界面上的命令要当场生效。
    //!
    //! 这里驱动的是完整的 `run_one_turn`（内部就是 carter 的 `run_turn`），只是不走
    //! `run_sequence` —— 于是不碰标题、`prompts/`、`timeline/` 这些落盘的东西，测试
    //! 可以随便跑，也不会在用户机器上留下文件。
    //!
    //! 守的是「点『转后台』没反应」：那条命令必须打到正在跑的工具调用上，工具立刻
    //! 返回 `shell_id` 而不是等满整条命令。

    use super::*;
    use carter::provider::{ChatRequest, EventStream, FinishReason, StreamChunk};
    use std::time::Duration;

    /// 第一次请求：让模型调 `bash` 跑一条很久的命令。第二次：说一句话收尾。
    struct Scripted(std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl carter::provider::LlmProvider for Scripted {
        async fn stream(&self, _req: ChatRequest) -> carter::Result<EventStream> {
            use std::sync::atomic::Ordering;
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            let chunks: Vec<carter::Result<StreamChunk>> = if n == 0 {
                vec![
                    Ok(StreamChunk::ToolCallDelta {
                        index: 0,
                        id: "call_from_model".into(),
                        name_delta: Some("bash".into()),
                        arguments_delta: Some(
                            serde_json::json!({ "command": "sleep 60", "timeout": 120 })
                                .to_string(),
                        ),
                    }),
                    Ok(StreamChunk::Finish {
                        reason: FinishReason::ToolCall,
                    }),
                ]
            } else {
                vec![
                    Ok(StreamChunk::text("好了")),
                    Ok(StreamChunk::Finish {
                        reason: FinishReason::Stop,
                    }),
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    fn model() -> ModelInfo {
        ModelInfo {
            key: "k".into(),
            provider: "p".into(),
            api_name: "m".into(),
            context_window: 100_000,
            max_output_tokens: 8_000,
            tokenizer: "cl100k".into(),
            capabilities: vec![carter::registry::Capability::Tools],
            pricing: carter::registry::Pricing {
                input: 0.0,
                output: 0.0,
                cache_write: None,
                cache_read: None,
            },
            default_reasoning: None,
        }
    }

    /// 等一条事件，超时就失败 —— 卡住的测试要自己说清卡在哪一步。
    async fn wait_for(
        events: &mut tokio::sync::broadcast::Receiver<AppEvent>,
        what: &str,
        mut hit: impl FnMut(&SessionEvent) -> bool,
    ) -> SessionEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ev = tokio::time::timeout(left, events.recv())
                .await
                .unwrap_or_else(|_| panic!("等「{what}」超时"))
                .expect("事件通道关了");
            if let AppEvent::Session { event, .. } = ev
                && hit(&event)
            {
                return event;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_background_action_reaches_the_command_running_inside_the_turn() {
        use std::sync::atomic::AtomicU32;

        let dir = std::env::temp_dir().join(format!(
            "carter-app-live-turn-{}-{}",
            std::process::id(),
            crate::protocol::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let shell = AppShell::new(Config::default());
        let mut events = shell.subscribe();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let m = model();
        let handle = SessionHandle::new(
            "live-turn-test".into(),
            dir.clone(),
            m.clone(),
            None,
            cmd_tx,
        );

        let ctx = SessionCtx {
            shell: shell.clone(),
            handle: handle.clone(),
            model: m.clone(),
            provider: Arc::new(Scripted(std::sync::atomic::AtomicUsize::new(0))),
            config: Arc::new(Config::default()),
            settings: AppSettings::default(),
            resumed: false,
            cwd: dir.clone(),
            unsupported_server_tools: Vec::new(),
            capture: None,
            include_task: false,
            fast: None,
            fast_ref: None,
            shells: handle.background.clone(),
            shell_cwd: crate::tools::bash::ShellCwd::new(dir.clone()),
            running: crate::tools::control::RunningTools::new(),
        };
        let mut thread = Thread::new_empty();
        thread.append_user("跑一条很久的命令");
        let meta = session::SessionMeta {
            id: handle.id.clone(),
            format_version: session::CURRENT_FORMAT_VERSION,
            parent_id: None,
            forked_from: None,
            cwd: dir.to_string_lossy().to_string(),
            git: None,
            title: Some("测试".into()),
            carter_version: env!("CARGO_PKG_VERSION").to_string(),
            model: m.key.clone(),
            created_at: crate::protocol::now_ms(),
        };
        let mut runner = Runner {
            ctx,
            thread,
            meta,
            cmd_rx,
            tool_tx: Some(emit_tool_event(shell.clone(), handle.id.clone())),
            ask_tx: None,
            mcp_tools: Vec::new(),
            activity: Arc::new(AtomicU32::new(0)),
            goal_signal: Arc::new(GoalSignal::default()),
            goal: None,
            downgraded_goal: false,
            queued: VecDeque::new(),
            // 固定系统提示：不去读用户机器上的 `~/.carter/system.md` / 技能目录。
            fixed_system: Some(vec!["测试系统提示".into()]),
            title_pending: false,
            title_seed: None,
            last_message_at: None,
            changes: None,
            changes_turn: 0,
            overhead: 0,
            reported: Arc::default(),
        };

        let turn = tokio::spawn(async move { runner.run_one_turn(None).await });

        // 工具卡片上的编号必须是**模型给的**那个 —— 界面靠它把按钮送回后端。
        let started = wait_for(&mut events, "工具开始", |e| {
            matches!(e, SessionEvent::ToolStarted { .. })
        })
        .await;
        let SessionEvent::ToolStarted { call_id, name, .. } = started else {
            unreachable!()
        };
        assert_eq!(name, "bash");
        assert_eq!(call_id, "call_from_model", "卡片编号要用模型给的那个");

        // 界面上就是这一刻点「转后台」。
        assert!(handle.send(Command::ToolAction {
            call_id: call_id.clone(),
            action: crate::protocol::ToolActionKind::Background,
        }));

        let finished = wait_for(&mut events, "工具结束", |e| {
            matches!(e, SessionEvent::ToolFinished { .. })
        })
        .await;
        let SessionEvent::ToolFinished {
            call_id: done_id,
            content,
            ..
        } = finished
        else {
            unreachable!()
        };
        assert_eq!(done_id, call_id);
        assert!(content.contains("shell_id="), "应当转成后台：{content}");

        // 这一轮照常收尾（模型拿到了结果、说了最后一句话）。
        let result = tokio::time::timeout(Duration::from_secs(30), turn)
            .await
            .expect("转后台之后这一轮应当立刻收尾")
            .unwrap();
        assert!(!result.failed, "转后台不该把这一轮弄失败");
        assert!(result.used_tools);

        handle.background.kill_all();
        std::fs::remove_dir_all(&dir).ok();
    }
}
