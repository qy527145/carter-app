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
    // 首次启动只有空的起步渠道：carter 的报错（「格式应为 provider/model」）说不清该干什么。
    if !crate::config_edit::model_configured(&config, &model_ref) {
        return Err(carter::CarterError::Config(
            "还没有配置模型，请到「设置 → 渠道」里添加模型".into(),
        ));
    }
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

mod compact;
mod events;
mod helpers;
mod runner;
mod sinks;
mod turn;

#[cfg(test)]
mod tests;

use helpers::*;
use runner::Runner;
use sinks::*;
