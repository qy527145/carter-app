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
//!
//! ## 长任务目标（`/goal`）住在哪
//!
//! 目标的**判定**在 `goal.rs`（纯函数），**执行**在这里：一轮跑完后问
//! `decide_after_turn`，要继续就再追加一条「继续」消息接着跑。所以「跑一轮」被抽成了
//! [`Runner::run_one_turn`]，用户发消息与自动继续走的是同一条路 —— 两条路各写一遍的话，
//! 自动继续迟早会漏掉某一轮里新加的东西（工具开关、热重载、抓包…）。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use carter::agent::{run_turn, CompactModel, RunOptions, Thread, UiEvent, UiSink};
use carter::config::Config;
use carter::hooks::HookRegistry;
use carter::registry::{fetch, ModelInfo};
use carter::session::{self, SessionOpts};
use carter::tools::Tool;

use crate::app_settings::AppSettings;
use crate::changes::ChangeWatch;
use crate::goal::{self, Goal, GoalSignal, GoalStatus, TurnOutcome};
use crate::protocol::{AppEvent, Command, GoalCommand, GoalView, SessionEvent, TodoView, UsageView};
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
    if let Some(id) = opts.resume_id.as_deref() {
        if shell.session(id).is_some() || shell.is_closing(id) {
            return Err(carter::CarterError::Config(format!(
                "会话 {id} 还在收尾（上一轮的命令没有响应中断），请稍后再打开。"
            )));
        }
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
        vec![capture_hook.clone()],
    )?;
    let (model, provider) = (resolved.model, resolved.provider);
    let unsupported_server_tools = resolved.unsupported_server_tools;
    // 协议回填给钩子：抓包存的是最终字节，解码要先知道按哪套语法解。
    label_capture(&capture_hook, &config, &model);

    // 会话文件：按 id 打开 → 续接最近的 → 起一个新的。
    let existing = match &opts.resume_id {
        Some(id) => find_by_id(id),
        None if opts.resume_latest => session::latest(&opts.cwd),
        None => None,
    };
    let (mut thread, meta, resumed) = match existing {
        Some(entry) => {
            let (mut t, m) = session::load(&entry)?;
            // 续接时把用户轮序号接到历史后面。
            //
            // carter 的 `Thread::from_parts` 把它从 0 重数，而抓包（`RequestMeta.turn`）、
            // 界面上的「第 N 轮」、历史消息上的轮次号全都按这一个数对齐 —— 从 0 重数
            // 会让重启后的第一轮与历史里的第一轮撞号，报文面板上一个筛选条件就套住
            // 了两条不相干的请求。压缩过的会话这里会数小（早期消息已被摘要替换），
            // 那是压缩本身的固有损失，与这里无关。
            t.user_turns = crate::view::user_turn_count(&t.messages);
            (t, m, true)
        }
        None => {
            let sess_opts = SessionOpts { session_id: None };
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
    let handle = SessionHandle::new(
        meta.id.clone(),
        cwd.clone(),
        model.clone(),
        None,
        cmd_tx,
    );
    // 侧边聊天要靠这份快照拿父会话的上下文，所以一开始就得有值。
    handle.publish_history(&thread.messages);
    shell.sessions.insert(meta.id.clone(), handle.clone());
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
        cwd,
        unsupported_server_tools,
        capture: Some(capture_store),
        include_task: true,
        fast: None,
        fast_ref: None,
        shells: crate::tools::background::BackgroundShells::new(),
        running: crate::tools::control::RunningTools::new(),
    };
    // 会话文件里还没标题（`meta` 下面要整个移进 Runner，先问一句）。
    let unnamed = meta.title.is_none();
    let runner = Runner {
        ctx,
        thread,
        meta,
        cmd_rx,
        ui_tx: None,
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
        // 只有「新起的会话」才配自动标题：续接一条已经在聊的会话，标题早该有了；
        // 而它没有的时候（老版本建的），也不该拿用户**现在**说的这句话去给它起名。
        title_pending: !resumed && unnamed,
        changes: None,
        changes_turn: 0,
    };
    tokio::spawn(runner.run());
    let changes = crate::changes::load(&handle.id);
    Ok(StartedSession {
        handle,
        history,
        goal: goal_view,
        changes,
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

    let ctx = SessionCtx {
        shell: w.shell,
        handle: handle.clone(),
        model: w.model,
        provider: w.provider,
        config: w.config,
        settings: AppSettings::load(),
        resumed: false,
        cwd: w.cwd,
        unsupported_server_tools: w.unsupported_server_tools,
        // 侧边会话：不派生子 agent、不连 MCP、不抓包。见 `side.rs` 的模块头。
        include_task: false,
        capture: None,
        fast: None,
        fast_ref: None,
        shells: crate::tools::background::BackgroundShells::new(),
        running: crate::tools::control::RunningTools::new(),
    };
    let runner = Runner {
        meta: side_meta(&handle),
        ctx,
        thread: w.thread,
        cmd_rx,
        ui_tx: None,
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
        changes: None,
        changes_turn: 0,
    };
    tokio::spawn(runner.run());
    handle
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

        let cache = match fetch::read_cache() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("副模型 `{reference}` 用不上：读模型缓存失败（{e}）");
                return None;
            }
        };
        self.fast = match carter::provider::resolve_provider(&self.config, &cache, &reference, hooks)
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
    fn system(&self, git: Option<&carter::session::GitInfo>) -> Vec<String> {
        let skill_metas = if self.config.skills.enabled {
            crate::skills::union_metas(&crate::skills::Scan::from_settings(&self.settings))
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

// ---------------------------------------------------------------------------
// 运行器
// ---------------------------------------------------------------------------

/// 一轮跑完之后的账。自动继续的判定全看这几个数（见 `goal.rs::decide_after_turn`）。
struct TurnResult {
    /// 这一轮以错误收场（上游报错、或撞上 `max_turns`）。
    failed: bool,
    /// 这一轮调用过工具。
    used_tools: bool,
    /// 用户在这一轮里点过「停止」。
    user_cancelled: bool,
}

struct Runner {
    ctx: SessionCtx,
    thread: Thread,
    meta: session::SessionMeta,
    cmd_rx: mpsc::UnboundedReceiver<Command>,
    /// 三条事件通道的发送端。接收端在 `forward_events` 任务手里，所以这里存 `None`
    /// 就表示「还没建」—— 见 [`Runner::run`]。
    ui_tx: Option<mpsc::UnboundedSender<UiEvent>>,
    tool_tx: Option<mpsc::UnboundedSender<ToolEvent>>,
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
    /// 正在收集的这一轮的文件变更。轮首建、轮末收（见 `changes.rs`）。
    ///
    /// `None` = 不在轮内（空闲时），或监听器没建起来 —— 两种情况都只是「这一轮
    /// 没有清单可列」，不该影响对话本身。
    changes: Option<ChangeWatch>,
    /// 上面那个监听器对应哪一轮。
    ///
    /// 必须在轮首记下来：`carter::agent::run_turn` 一进去就把 `thread.user_turns`
    /// 加一，轮末再去读就成了**下一轮**的号 —— 历史回看时清单会挂错回答。
    changes_turn: u32,
}

impl Runner {
    async fn run(mut self) {
        let session_id = self.ctx.handle.id.clone();

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
        self.ui_tx = Some(ui_tx);
        self.tool_tx = Some(tool_tx);
        self.ask_tx = Some(ask_tx);
        let forwarder = tokio::spawn(forward_events(
            self.ctx.shell.clone(),
            session_id.clone(),
            self.ctx.handle.asks.clone(),
            ui_rx,
            tool_rx,
            ask_rx,
        ));

        // MCP server 启动是异步的（要连子进程 / 网络）。失败不致命：
        // carter 的 McpManager 会跳过连不上的 server。
        //
        // 它在会话开头连一次就不再重连 —— 所以界面上改了 MCP 配置要**新开会话**
        // 才生效，这与「工具开关下一条消息就生效」不同，界面上分开提示。
        let mcp_mgr = if self.ctx.include_task {
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
        drop(self.ui_tx.take());
        drop(self.tool_tx.take());
        drop(self.ask_tx.take());
        let _ = forwarder.await;
        self.ctx.shell.sessions.remove(&session_id);
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
            Command::SetModel { model } => match switch_model(&self.ctx, &model) {
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
                }
                Err(e) => self.error(&format!("切换模型失败：{e}")),
            },
            // 排队中的消息真正轮到它时走到这里。`interject` 在这里没有意义
            // （已经没有正在跑的轮次可以打断了），直接当普通消息处理。
            Command::SendPrompt { text, id, .. } => self.submit(text, id).await,
            Command::Regenerate { turn } => {
                // 历史里存的是展开后的正文，原样重发 —— 不再过一次展开，
                // 否则 `@文件` 的内容会被贴第二遍。
                self.rewind_turn(turn, None).await;
            }
            Command::EditPrompt { turn, text } => {
                // 这是用户**新打的字**，照常展开（`/命令`、`@文件`、图片引用）。
                self.rewind_turn(turn, Some(text)).await;
            }
            // 空闲时「插队」= 把那条消息挪到队首。下一轮循环就会取到它。
            Command::PromotePrompt { id } => promote(&mut self.queued, id),
            Command::DropPrompt { id } => drop_queued(&mut self.queued, id),
            // 空闲时按「中断命令」= 那条命令已经结束了，什么都不用做。
            Command::ToolAction { .. } => {}
        }
    }

    /// 回退到某一轮之前再重跑它 —— 「重试」与「编辑并重发」共用这一段。
    ///
    /// `replacement` 是用户改写后的**原文**（未展开）；`None` = 原样重跑。
    /// 两条路的差别只有一处：原样重跑直接用历史里那段（已经展开过），改写过的
    /// 则要重新走一遍 `expand_input`。
    ///
    /// **只回滚对话，不动磁盘**。文件已经改过、命令已经跑过的事不会撤销 ——
    /// 界面上那句确认里也这么说，用户想要的是「让它重来一遍」，不是「把世界推回去」
    /// （后者是 carter 的 `/rewind`，走的是另一套检查点）。
    ///
    /// 回退与重发是同一件事的两半：中间不插任何 `Idle`，否则界面会先看到一路空下去、
    /// 再慢慢长出来。`TurnRetracted` 紧跟着的不带 `prompt` 的 `turn_started`
    /// 复用的是界面上原本那条用户气泡。
    async fn rewind_turn(&mut self, turn: u32, replacement: Option<String>) {
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
        // 改写后的提问要重新展开，再落进历史（下面 `run_sequence` 会 append）。
        // 展开失败不吞掉这条：按原文发出去，并说清发生了什么（与 `submit` 同一套）。
        // 克隆一份留给事件：`replacement` 下面会被 move 进 `text`。
        let echo_prompt = replacement.clone();
        let text = match replacement {
            None => stored,
            Some(raw) => match expand_input(&self.ctx.cwd, &raw) {
                Ok((expanded, note)) => {
                    if let Some(note) = note {
                        self.notice(&note);
                    }
                    expanded
                }
                Err(e) => {
                    self.notice(&format!("{e}（已按原文发送）"));
                    raw
                }
            },
        };
        // 变更清单按轮存着，一起退 —— 否则被回退掉的那些回答的文件清单会留下来。
        if let Err(e) = crate::changes::retract(&self.ctx.handle.id, turn) {
            tracing::warn!("回退轮次变更记录失败：{e}");
        }
        self.ctx.handle.publish_history(&self.thread.messages);
        // 改写过的提问要把新正文带给界面（那条气泡要跟着变）；原样重跑不用。
        self.emit(SessionEvent::TurnRetracted {
            turn,
            prompt: echo_prompt,
        });
        // `echo` 一律给 `None`：这一轮的用户气泡界面上已经有了，`TurnRetracted`
        // 刚刚交代过它该是什么样 —— 再带一次 `prompt` 会多画一条。
        self.run_sequence(text, git, None, None).await;
    }

    /// 用户发来一条消息：展开、落进历史、跑一轮，再按目标决定要不要自动继续。
    async fn submit(&mut self, raw: String, prompt_id: Option<u64>) {
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
        let text = match expand_input(&self.ctx.cwd, &raw) {
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
        if let Some(g) = self.goal.as_mut() {
            if g.idle_streak != 0 {
                g.idle_streak = 0;
                self.publish_goal();
            }
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
        self.thread.append_user(text.clone());
        // 标题在这一刻并行去生成，**不挡这一轮**（见 [`Runner::spawn_title`]）。
        // 放在 append 之后：carter 的 Recorder 是惰性建文件的，第一条消息落盘
        // 才让会话文件存在，而写标题要往那个文件里追加。
        if std::mem::take(&mut self.title_pending) {
            self.spawn_title(&text);
        }
        // 轮次开头也发布一次：这一轮可能很长，侧边聊天在这期间至少要能看到
        // 「用户提了什么要求」（之后的进展由 `live` 补上）。
        self.ctx.handle.publish_history(&self.thread.messages);
        self.begin_changes();
        self.emit(SessionEvent::TurnStarted {
            turn: self.thread.user_turns,
            prompt_id,
            prompt: echo,
        });

        let result = self.run_one_turn(git.clone()).await;
        // 一轮结束立刻收口：中断留下的断口现在就补进历史（并落盘），
        // 不必等用户下一次发消息。
        self.settle_history();
        self.finish_changes(result.used_tools).await;
        self.drive_goal(result, git).await;
        self.emit(SessionEvent::Idle);
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
    /// 写回走 [`sessions::title_if_unnamed`]：**只在还没有标题时写**。用户在这几百
    /// 毫秒里自己改了名字的话（侧栏行内重命名），机器起的这个名字不该把它盖掉。
    ///
    /// ## 失败就静默
    ///
    /// 副模型配错、网络不通、上游拒答 —— 都只让这条会话暂时没有标题，侧栏显示
    /// 「未命名会话」。为它弹一个错误条，比没有标题更打扰人。
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
        // 渠道种类决定要不要关思考（见 `title.rs`：推理模型会把 32 个 token
        // 全花在思考上，正文一个字都不留）。副模型可能挂在另一个渠道上，所以取
        // **它自己那个** kind。
        let kind = self.ctx.config.provider_for(&model.provider).kind;
        let shell = self.ctx.shell.clone();
        let session_id = self.ctx.handle.id.clone();
        let prompt = first_prompt.to_string();

        tokio::spawn(async move {
            let Ok(title) = crate::title::generate(&prompt, provider, &model, &kind).await else {
                return;
            };
            if title.trim().is_empty() {
                return;
            }
            // 用**会话自己那个 recorder** 追加，而不是新开一个句柄：同一个文件上的
            // 两个追加句柄，写一行会拆成两次 write（正文 + 换行），并发时可能把两条
            // 记录绞在一起。这个 recorder 内部有 Mutex，是唯一安全的那条路。
            //
            // 文件还没建出来也不怕：carter 的 recorder 是惰性的，第一次写会先把
            // 文件头（SessionMeta）落下去 —— 第一条消息早就写过了，这一步只是兜底。
            if !crate::sessions::auto_title_allowed(
                crate::sessions::title_of(&session_id).as_deref(),
            ) {
                return;
            }
            recorder.record(carter::session::RecordKind::Title { title: title.clone() });
            emit(&shell, &session_id, SessionEvent::Title { title });
        });
    }

    /// 跑一轮：装配工具、发请求、执行工具、回灌，直到模型不再调工具。
    ///
    /// `git` 只有主会话有（侧边会话不重算系统提示），这里收一个 `Option` 是为了
    /// 让两条路共用同一个函数体。
    async fn run_one_turn(&mut self, git: Option<carter::session::GitInfo>) -> TurnResult {
        let system = match &self.fixed_system {
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
                let mut s = self.ctx.system(git.as_ref());
                // 目标段接在最后：它是最「当下」的指令，而且（将来 carter 打开
                // cache 断点后）末段正是断点位置 —— 所以它必须是稳定的，
                // 见 `goal.rs` 模块头。
                if let Some(seg) = self.goal.as_ref().and_then(goal::system_segment) {
                    s.push(seg);
                }
                s
            }
        };
        // 只有状态为 active 时模型才该调 goal_update。
        self.goal_signal
            .set_armed(self.goal.as_ref().is_some_and(|g| g.status == GoalStatus::Active));

        let hooks = Arc::new(HookRegistry::from_configs(self.ctx.config.hooks.clone()));
        let tools = build_tools(
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
                ask_tx: if self.ctx.include_task {
                    self.ask_tx.clone()
                } else {
                    // 侧边聊天不挂 ask_user：抽屉里再套一层提问表单，与「临时问一句」
                    // 的定位不符，而且那套回答链路要单独接一遍。
                    None
                },
                include_task: self.ctx.include_task,
                goal_signal: self.goal_signal.clone(),
                activity: self.activity.clone(),
                live: self.live(),
                shells: Some(self.ctx.shells.clone()),
                session_id: self.ctx.handle.id.clone(),
                running: Some(self.ctx.running.clone()),
            },
            &self.ctx.settings,
        );
        // 压缩用副模型（`agent.fast_model`）：压缩是"把长历史揉短"的活儿，
        // 要的是便宜和快。没配副模型就回落主模型。
        let fast = self.ctx.fast_model();
        let compact_model = Some(fast.unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        }));

        let run_opts = RunOptions {
            show_thinking: true,
            system,
            compact_model,
            hooks,
            // 单会话里只有主对话这一种请求（压缩与子 agent 也会发，
            // 但它们的 purpose 由各自那条路径设置）。
            purpose: carter::provider::RequestPurpose::MainTurn,
        };

        // `thread.turns` 是**整个会话**累计的模型调用次数，而 carter 的
        // `max_turns`（默认 50）拿它当上限 —— 累计口径下，一条聊了很久的会话
        // 到第 50 次模型调用之后，每一轮都会被判成 Limit 而立刻返回、什么都不做。
        // 在自动继续下这尤其致命（目标还在，每轮却零成本空转）。每轮开头清零，
        // 把 `max_turns` 恢复成它名字上的意思：**一次用户输入**内最多几次模型调用。
        self.thread.turns = 0;
        // 上一轮可能残留了取消标记（用户在流结束后、select 收尾前点的停止）。
        // 不清的话，这一轮一进去就立刻被判成取消。
        self.ctx.handle.cancel.reset();

        let activity_before = self.activity.load(Ordering::Relaxed);
        let mut user_cancelled = false;
        let mut closed = false;
        let cancel = self.ctx.handle.cancel.clone();

        let outcome = {
            let mut sink = TxSink {
                tx: self.ui_tx.clone().expect("事件通道在 run() 里建好"),
                live: self.live(),
            };
            let turn = run_turn(
                &mut self.thread,
                &*self.ctx.provider,
                &self.ctx.model,
                &self.ctx.config.agent,
                &run_opts,
                &tools,
                &mut sink,
                &cancel,
            );
            tokio::pin!(turn);

            loop {
                tokio::select! {
                    res = &mut turn => break res,
                    Some(inner) = self.cmd_rx.recv() => match inner {
                        // 生成中能**立刻**生效的几条命令。其余排队等这一轮跑完。
                        Command::Cancel => {
                            user_cancelled = true;
                            cancel.set();
                        }
                        // 用户又发了一条消息。默认排队（这一轮跑完就轮到它），
                        // 勾了「插队」则立刻中断本轮、插到队首。
                        Command::SendPrompt { text, id, interject } => {
                            let cmd = Command::SendPrompt {
                                text,
                                id,
                                interject: false,
                            };
                            if interject {
                                user_cancelled = true;
                                cancel.set();
                                self.queued.push_front(cmd);
                            } else {
                                self.queued.push_back(cmd);
                            }
                        }
                        // 界面上对一条**已经排进队**的消息点「插队」。
                        Command::PromotePrompt { id } => {
                            promote(&mut self.queued, id);
                            user_cancelled = true;
                            cancel.set();
                        }
                        Command::DropPrompt { id } => drop_queued(&mut self.queued, id),
                        // 单条工具调用的「中断 / 转后台」。立刻生效，也不影响
                        // 这一轮本身 —— 工具会把结果回灌给模型，模型接着往下走。
                        Command::ToolAction { call_id, action } => {
                            if !self.ctx.running.apply(&call_id, action) {
                                // 找不到说明它刚好跑完了。不是错误，静默即可 ——
                                // 用户点按钮与命令结束本来就可能撞在同一毫秒。
                            }
                        }
                        Command::AnswerForm { id, answers } => {
                            self.ctx.handle.asks.answer(id, answers);
                        }
                        // `/goal pause` 必须立刻生效：否则一条正在自动继续的长任务
                        // 要等整串跑完才停得下来，而用户按暂停正是因为不想再等了。
                        Command::Goal { action } => {
                            let mut goal = self.goal.clone();
                            apply_goal_action(
                                &mut goal,
                                &self.ctx.handle.id.clone(),
                                &self.ctx.shell.clone(),
                                action,
                            );
                            self.goal = goal;
                        }
                        other => self.queued.push_back(other),
                    },
                    // 关会话（关掉侧边抽屉、换会话）要立刻断流，而不是等这一轮
                    // 自然跑完 —— 「关掉即丢」的前提就是这个。
                    _ = self.ctx.handle.close.cancelled() => {
                        closed = true;
                        cancel.set();
                    }
                }
            }
        };

        // 历史快照：侧边聊天靠它拿父会话的上下文。**在每一轮之后都发布**
        // （同时清空 `live` —— 这一轮的进展已经进了快照）。
        self.ctx.handle.publish_history(&self.thread.messages);

        let used_tools = self.activity.load(Ordering::Relaxed) > activity_before;
        // 「失败」的三种来源，对自动继续是同一件事：**别接着推**。
        // 尤其是 `Limit` —— 模型输出被截断、或这一轮撞上 `max_turns` 时，
        // 接着推只会再撞一次，而每撞一次都是零成本的，于是变成死循环。
        let failed = match &outcome {
            Err(_) => true,
            // 被中断：用户自己点的停止不算失败（那条规则在 `decide_after_turn` 里
            // 优先级更高）；自己断的（关会话等）算，免得在这儿打转。
            Ok((carter::agent::TurnOutcome::Cancelled, _)) => !user_cancelled,
            Ok((carter::agent::TurnOutcome::Limit, _)) => true,
            // carter 目前不构造这个变体，但枚举里有 —— 真出现了就是「这一轮废了」。
            Ok((carter::agent::TurnOutcome::Error(_), _)) => true,
            Ok((carter::agent::TurnOutcome::Assistant, _)) => false,
        };
        if let Err(e) = &outcome {
            // 关会话造成的断流不是错误：用户本来就让这条对话结束，
            // 报一条「已取消」只会在他按下关闭之后再弹一句废话。
            //
            // 用户点了停止也一样：carter 在取消之后还会再发一次请求（它的循环
            // 顶部不检查取消），那一次多半会以「请求被取消」收场 —— 把这条当成
            // 错误弹出来，用户会以为是「停止」弄坏了什么。
            if !closed && !user_cancelled {
                self.error(&format!("{e}"));
            }
        }
        TurnResult {
            failed,
            used_tools,
            user_cancelled,
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
                    self.emit(SessionEvent::TurnStarted {
                        turn: self.thread.user_turns,
                        // 自动继续没有用户消息编号 —— 界面据此画分隔条而不是气泡。
                        prompt_id: None,
                        prompt: None,
                    });
                    result = self.run_one_turn(git.clone()).await;
                    self.settle_history();
                    self.finish_changes(result.used_tools).await;
                    if self.ctx.handle.close.is_cancelled() {
                        return;
                    }
                }
            }
        }
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
    if let Some(pos) = pos {
        if let Some(cmd) = queue.remove(pos) {
            queue.push_front(cmd);
        }
    }
}

/// 撤回一条还没开始跑的消息。找不到就当作已经跑了，什么都不做。
fn drop_queued(queue: &mut VecDeque<Command>, id: u64) {
    queue.retain(|c| !matches!(c, Command::SendPrompt { id: Some(q), .. } if *q == id));
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
    let cache_json = fetch::read_cache()?;
    // 换模型时也保留抓包 —— 否则一切换就静默停止记录了。
    let hook = ctx
        .capture
        .as_ref()
        .map(|store| Arc::new(crate::capture::CaptureHook::new(store.clone())));
    let hooks: Vec<Arc<dyn llm_adapt_core::RequestHook>> = hook
        .iter()
        .map(|h| h.clone() as Arc<dyn llm_adapt_core::RequestHook>)
        .collect();
    let resolved = carter::provider::resolve_provider(&ctx.config, &cache_json, model_ref, hooks)?;
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

    let send = |shell: &AppShell, event: SessionEvent| {
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
                        send(&shell, out);
                    }
                }
                None => ui_open = false,
            },
            ev = tool_rx.recv(), if tool_open => match ev {
                Some(ToolEvent::Started { call_id, name, args }) => {
                    send(&shell, SessionEvent::ToolStarted { call_id, name, args });
                }
                Some(ToolEvent::Finished { call_id, ok, content, elapsed_ms }) => {
                    send(&shell, SessionEvent::ToolFinished { call_id, ok, content, elapsed_ms });
                }
                None => tool_open = false,
            },
            req = ask_rx.recv(), if ask_open => match req {
                Some(AskRequest::Questions { id, questions, reply }) => {
                    // 发送端存住：前端要跨一次进程边界才回得来，而 oneshot 只能发一次。
                    asks.insert(id, reply);
                    send(&shell, SessionEvent::AskForm { id, questions });
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
struct TxSink {
    tx: mpsc::UnboundedSender<UiEvent>,
    /// 进行中那一轮的记录，见 `shell.rs::LiveLog`。
    live: Option<Arc<crate::shell::LiveLog>>,
}

impl UiSink for TxSink {
    fn emit(&mut self, ev: UiEvent) {
        if let (Some(live), UiEvent::AssistantTextDelta(t)) = (&self.live, &ev) {
            live.text(t);
        }
        // 接收端没了说明会话正在退出，丢弃即可。
        let _ = self.tx.send(ev);
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
}
