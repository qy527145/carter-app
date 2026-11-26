//! 工具池装配：在 carter 的装配结果之上做三件事 —— 按会话 cwd 包一层路径解析、
//! 套一层用户开关、外面再包一层记录。
//!
//! ## 基础能力在 carter，这里只做界面与扩展
//!
//! `bash`（shell 探测、`cd` 跟随、后台执行）、`grep`（内置 ripgrep）、
//! `ask_user_question`（一次问一组）、`bash_output` / `kill_shell`、工具开关、插话
//! 都已经在 carter 里（它们本来就不是界面能力）。这里**不再**各留一份实现：
//! carter 的 `build_tools` 直接装配它们，本模块只负责把几样桌面场景才有的东西接上：
//!
//! * **路径解析** —— carter 的文件工具不解析相对路径（直接 `Path::new(&path)`，落到
//!   进程 cwd 上）。命令行下这是对的，桌面应用下必然错：进程 cwd 取决于从哪启动。
//!   所以文件工具包一层 [`cwd::CwdScoped`]，相对路径按会话的 cwd 解析。
//! * **`bash` 的界面控制** —— carter 的 `PlatformBash` 留了一个 `CommandControl`
//!   接口（单条命令的「中断 / 转后台」与实时输出），这里用 [`control`] 的 task-local
//!   句柄接上。
//! * **`skill`** —— carter 那个只认单根（`~/.carter/skills`）。技能页能加额外的扫描
//!   路径，换成 [`multi_skill::MultiRootSkill`] 之后那些根里的技能才加载得到。
//!   刻意走「换实现」而不是「再 push 一个」：工具池里两个同名工具会让 `specs()` 下发
//!   两条，上游直接 400。
//! * **`save_memory`** —— 项目级记忆默认跟随进程 cwd，桌面里换成绑定会话 cwd 的那份。
//!
//! ## 为什么不用 carter 自带的 `task`
//!
//! `build_tools` 产出的 `task` 内部握着一个「工厂闭包」，子 agent 派生时用它重建
//! 工具池。若把 `task` 原样搬过来，闭包里装的还是**没过滤**的全量工具：用户关掉
//! `bash`，主 agent 拿不到，子 agent 却照跑。而且它在整段执行期间一个事件都不发 ——
//! 界面上就是个转圈的卡片，跑二十分钟看不出里面有几个、到哪一步了。
//!
//! 所以换成 [`agent::AgentTool`]：一次收 `tasks: [...]` 并发派生，进度实时推给界面；
//! 工具池直接用过滤后的快照（见 [`filter_tools`]），"关掉的工具对子 agent 同样失效"。
//!
//! ## 记录包装
//!
//! 每个工具外面包一层 [`record::Recorded`]，把**完整的参数与结果**送给界面。
//! carter 的 `UiEvent` 只给截断的预览 —— 数据不够，界面上就只能给所有工具画同一种
//! 卡片。这是「工具看起来都一样」的根因。主会话的包装在 `session/turn.rs::turn_tools`
//! 里统一做（那里还知道用模型给的 `call_id`）；子 agent 的快照在这里套。
//!
//! ## 两份开关
//!
//! carter 自己有 `[agent].disabled_tools`（CLI 与本应用共用，装配期就筛掉）；
//! 本应用还有一份界面上的开关，存在 `~/.carter-app/settings.json`，在装配之后再挑一遍
//! （[`filter_tools`]）。界面上要写清楚：这一份 carter CLI 读不到。

pub mod assistant_config;
pub mod assistant_decide;
pub mod assistant_report;
pub mod assistant_ticket;
pub mod control;
pub mod cwd;
pub mod guard;
pub mod multi_skill;
pub mod peer;
pub mod record;
pub mod schedule;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64};

use carter::agent::CancelToken;
use carter::agent::bootstrap::build_tools as carter_build_tools;
use carter::config::AgentConfig;
use carter::provider::LlmProvider;
use carter::registry::ModelInfo;
use carter::tools::{Tool, ToolRegistry};
use tokio::sync::mpsc;

use carter::tools::{BackgroundShells, EmbeddedGrep, PlatformBash, ShellCwd};

use crate::app_settings::AppSettings;
use crate::skills::Scan;
use assistant_config::{AssistantConfigTool, Kind as ConfigKind};
use assistant_report::AssistantReportTool;
use assistant_ticket::{
    AssistantMemoryTool, AssistantRecallTool, AssistantTicketTool, Kind as TicketKind,
};
use carter::goal::GoalSignal;
use carter::tools::GoalUpdateTool;
use control::RunningTools;
use cwd::CwdScoped;
use multi_skill::MultiRootSkill;
use record::{Recorded, ToolTx};
use schedule::ScheduleTool;

/// 装配工具池需要的一整套东西。
///
/// 打包成结构体是因为裸参数有十几个，而其中几个装配 `agent` 时要再用一次 ——
/// 那点位置参数摆在那里，早晚会有人传错顺序。
pub struct ToolBuildArgs {
    pub provider: Arc<dyn LlmProvider>,
    pub model: ModelInfo,
    pub agent_cfg: AgentConfig,
    pub base_system: String,
    pub cancel: CancelToken,
    pub mcp_tools: Vec<Arc<dyn Tool>>,
    /// 会话的工作目录。bash 会在这个目录里跑。
    pub cwd: std::path::PathBuf,
    pub skills_enabled: bool,
    /// 工具事件（完整参数与结果）的出口。同步调用，见 [`ToolTx`]。
    pub tool_tx: ToolTx,
    /// 提问请求的出口（carter 的 `UiEvent::AskUser` 走这里）。
    /// `None` = 不给这个会话注册 `ask_user_question`。
    pub ask_tx: Option<mpsc::UnboundedSender<carter::agent::UiEvent>>,
    /// 是否挂上 `agent`（派生子 agent 的入口）。侧边聊天不挂。
    pub include_task: bool,
    /// 后台命令表。`Some` 时才会挂上 `bash_output` / `kill_shell`。
    ///
    /// 侧边聊天也给一份：它跑 `bash` 时同样可能起服务，没有这两个工具的话
    /// 模型只能把命令跑成前台、把抽屉卡住。
    pub shells: Option<Arc<BackgroundShells>>,
    /// 后台命令的日志按会话分目录。
    pub session_id: String,
    /// 后台子 agent 表。`Some` 时 `task` 才声明 `run_in_background`。
    ///
    /// 侧边聊天不给（它本来就不挂 `task`）；助理对话也不给（它不动手）。
    pub background_agents: Option<Arc<carter::agent::BackgroundAgents>>,
    /// bash 此刻所在的目录。由会话持有，`cd` 跨调用、跨轮保留。见 [`ShellCwd`]。
    pub shell_cwd: Option<Arc<ShellCwd>>,
    /// 正在跑的工具表。界面按 `call_id` 对单条命令做「中断 / 转后台」。
    pub running: Option<Arc<RunningTools>>,
    /// 长任务目标的信号格子。恒常传 —— `goal_update` 是恒常注册的（carter 的实现）。
    pub goal_signal: Arc<GoalSignal>,
    /// 定时任务表。`schedule` 工具通过对话增删改它。
    pub schedules: Arc<crate::schedules::ScheduleStore>,
    /// 工具调用计数。runner 靠它判断「这一轮到底动没动手」（自动继续的空转检测）。
    pub activity: Arc<AtomicU32>,
    /// 进行中那一轮的记录。`None` = 没人要看（侧边会话自己就不需要被别人看）。
    pub live: Option<Arc<crate::shell::LiveLog>>,
    /// 这条会话是常驻助理某张工单的执行会话：挂上 `assistant_report`。
    pub assistant: Option<(Arc<crate::assistant::AssistantStore>, String)>,
    /// 这条会话就是「助理对话」本身：只挂助理那一套工具，其余一概不给（见 [`build_tools`]）。
    ///
    /// 给的是整个外壳而不是 `AssistantStore`：派单、收尾、给工单写信都要开会话、
    /// 找会话，那些路需要会话注册表。
    pub assistant_chat: Option<Arc<crate::shell::AppShell>>,
    /// 这条会话是「值守」本身：只挂值守那一套工具（读、验证、判断、请示、收尾）。
    /// 与 `assistant_chat` 互斥 —— 值守不跟人说话，也不直接改配置。
    pub supervise: Option<Arc<crate::shell::AppShell>>,
    /// 挂上跨会话的三个工具（`list_sessions` / `send_message` / `read_session`）。
    /// 只有主对话给：子 agent、侧边聊天都不该替这条会话对外说话。
    pub peers: Option<Arc<crate::shell::AppShell>>,
}

/// 按设置挑出要保留的工具（不含 `task`、不含 `ask_user_question`）。
///
/// `ask_user_question` 单独处理：carter 的 `build_tools` 只在给了 `ui_tx` 时才注册
/// 它，而我们不用 carter 那个实现，所以这里传 `None`、自己加。
pub fn filter_tools(full: &ToolRegistry, settings: &AppSettings) -> Vec<Arc<dyn Tool>> {
    full.tools()
        .iter()
        // `task` 不参与筛选：它不是「一个工具」，是派生子 agent 的入口。
        // 把它关掉等于砍掉子 agent 能力，那是另一个开关该管的事。
        .filter(|t| t.name() != "task" && settings.tool_enabled(t.name()))
        .cloned()
        .collect()
}

/// 按名字把 carter 的实现换成本应用自己的那一份。
///
/// 抽成函数是为了让**两处共用同一份判断**：真正装配工具池的 [`build_tools`]，与设置页
/// 那份只用来展示的 registry（`src-tauri` 的 `list_tools`）。后者此前手抄了一遍 `bash`
/// 的构造，而设置页显示一份与真实行为对不上的说明，比不显示更坏。
pub fn substitute(
    t: Arc<dyn Tool>,
    settings: &AppSettings,
    cwd: &Path,
    timeout: BashTimeout,
) -> Arc<dyn Tool> {
    // 只在设置页「工具」列表里用到：那里只展示名字与描述，不会真的执行 ——
    // 于是给一个永远不会置位的取消令牌、不给后台表。
    let cancel = CancelToken::new();
    substitute_with(
        t,
        settings,
        cwd,
        BashRuntime {
            cancel: &cancel,
            shells: None,
            session_id: "",
            shell_cwd: None,
            timeout,
        },
    )
}

/// bash 运行期要的东西：取消令牌、后台表、当前目录、超时策略。打包是为了
/// [`substitute_with`] 的参数不至于一长串（展示用的那份全给空的）。
pub struct BashRuntime<'a> {
    pub cancel: &'a CancelToken,
    pub shells: Option<&'a Arc<BackgroundShells>>,
    pub session_id: &'a str,
    pub shell_cwd: Option<&'a Arc<ShellCwd>>,
    pub timeout: BashTimeout,
}

/// bash 的超时策略（`agent.bash_timeout_secs` / `agent.bash_on_timeout`）。
/// 设置页那份只读 registry 也要带上：工具描述里写着超时之后会发生什么。
#[derive(Debug, Clone, Copy)]
pub struct BashTimeout {
    pub secs: u64,
    pub on_timeout: carter::config::OnTimeout,
}

impl BashTimeout {
    pub fn of(agent: &AgentConfig) -> Self {
        Self {
            secs: agent.bash_timeout_secs,
            on_timeout: agent.bash_on_timeout,
        }
    }
}

/// [`substitute`] 的完整版。真正装配工具池时走这条，带上取消令牌与后台命令表。
pub fn substitute_with(
    t: Arc<dyn Tool>,
    settings: &AppSettings,
    cwd: &Path,
    rt: BashRuntime<'_>,
) -> Arc<dyn Tool> {
    let BashRuntime {
        cancel,
        shells,
        session_id,
        shell_cwd,
        timeout,
    } = rt;
    match t.name() {
        // `bash` 换成跨平台实现。
        // 当前目录由会话持有（`cd` 要跨轮保留）；没给就从工作目录起一份新的。
        "bash" => Arc::new(
            PlatformBash::new(
                shell_cwd
                    .cloned()
                    .unwrap_or_else(|| ShellCwd::new(cwd.to_path_buf())),
                cancel.clone(),
                shells.cloned(),
                session_id,
            )
            .with_timeout(timeout.secs, timeout.on_timeout)
            // 界面卡片上的「中断命令 / 转后台」与实时输出：从 task-local 取控制句柄。
            .with_control(Arc::new(|| {
                control::current().map(|c| c as Arc<dyn carter::tools::CommandControl>)
            })),
        ),
        // `grep` 换成内置 ripgrep 引擎（不依赖系统 `rg`），仍然走 `CwdScoped`
        // 解析相对路径。构造时把 cwd 也传给它 —— 结果里的文件路径要显示成相对路径。
        "grep" => Arc::new(CwdScoped::new(
            Arc::new(EmbeddedGrep::new(cwd.to_path_buf())),
            cwd.to_path_buf(),
        )),
        // 文件工具：相对路径按会话的 cwd 解析。原实现（行号、图片、截断…）全部保留，
        // 只是在执行前把 `path` 改写成绝对路径。
        name if CwdScoped::applies_to(name) => Arc::new(CwdScoped::new(t, cwd.to_path_buf())),
        // save_memory 的项目级记忆默认跟随**进程** cwd（命令行下就是项目根），桌面应用
        // 里那是启动目录（从 Finder 启动就是 `/`）—— 换成绑定会话 cwd 的那一份。
        "save_memory" => Arc::new(carter::tools::SaveMemory::new(cwd.to_path_buf())),
        // `skill` 换成多根实现：技能页加的扫描路径与项目级目录就在这里生效。
        // cwd 是构造期传入的会话工作目录，项目根由它往上找。
        "skill" => Arc::new(MultiRootSkill::new(Scan::from_settings_at(
            settings,
            Some(cwd),
        ))),
        _ => t,
    }
}

/// 装配工具池。
///
/// 顺序：carter 装配 → 换实现 → 过滤 → 快照 → 挂上 `agent`。
/// `agent` 拿到的是「此刻已有的工具」的快照，所以子 agent 那里没有 `agent`
/// 自身 —— 这个顺序就是递归守卫。
///
/// 池子里放的是**裸工具**，不套 `Recorded`：主会话那一层由 `session/turn.rs::turn_tools`
/// 统一套上（编号用模型给的 `call_id`，界面上的「中断 / 转后台」才找得到那条命令）。
/// 在这里先套一层的话，派发时会被再套一层 —— 同一次调用画两张卡，一张用模型的
/// `call_id`、一张用这里现编的号。
pub fn build_tools(args: ToolBuildArgs, settings: &AppSettings) -> ToolRegistry {
    let ToolBuildArgs {
        provider,
        model,
        agent_cfg,
        base_system,
        cancel,
        mcp_tools,
        cwd,
        skills_enabled,
        tool_tx,
        ask_tx,
        include_task,
        shells,
        session_id,
        background_agents,
        shell_cwd,
        running,
        goal_signal,
        schedules,
        activity,
        live,
        assistant,
        assistant_chat,
        supervise,
        peers,
    } = args;

    let seq = Arc::new(AtomicU64::new(0));
    let mut tools = ToolRegistry::empty();

    // 直接放进注册表 —— 别在这里套 `Recorded`，理由见本函数的文档。
    let push = |t: Arc<dyn Tool>, tools: &mut ToolRegistry| tools.push(t);

    // 值守是「幕僚」：只读不写。给它查看、验证、判断、请示、收尾这一套，
    // 不给 bash / 文件 / MCP / 配置 —— 动手仍然是工单的事。
    if let Some(shell) = supervise {
        for kind in assistant_decide::Kind::ALL {
            push(
                Arc::new(assistant_decide::SupervisorTool::new(shell.clone(), kind)),
                &mut tools,
            );
        }
        return tools;
    }

    // 助理对话是大脑，不是手：只给配置、派单、收尾、记忆和与工单通信的工具。
    // 没有 bash / 文件 / `agent` / MCP —— 要动手的事派成工单，由独立的执行会话去做
    // （见 `assistant_ticket.rs` 模块头）。工具开关管不到这几个：关掉它们助理就瘫了。
    if let Some(shell) = assistant_chat {
        let store = shell.assistant.clone();
        for kind in [
            ConfigKind::SourceSave,
            ConfigKind::SourceDelete,
            ConfigKind::RuleSave,
            ConfigKind::RuleDelete,
            ConfigKind::Mode,
            // 面板上的其余配置（同时跑几张、工作约定、两个模型）也能在对话里改：
            // 用户希望只跟助理打交道，不该被赶回设置页自己点。
            ConfigKind::Settings,
        ] {
            push(
                Arc::new(AssistantConfigTool::new(store.clone(), kind)),
                &mut tools,
            );
        }
        for kind in [
            TicketKind::Dispatch,
            TicketKind::Act,
            TicketKind::Proposal,
            TicketKind::Answer,
        ] {
            push(
                Arc::new(AssistantTicketTool::new(shell.clone(), kind)),
                &mut tools,
            );
        }
        // 长期记忆与往期检索（见 `assistant/memory.rs`）。
        push(
            Arc::new(AssistantMemoryTool::new(store.clone())),
            &mut tools,
        );
        push(Arc::new(AssistantRecallTool::new(store)), &mut tools);
        // 与工单通信：给工单补话（`to="#3"`）、看它做到哪了。不给 `list_sessions` ——
        // 工单一览已经在系统提示的「现状」里，别的会话也不是助理该联系的。
        for kind in [peer::Kind::Send, peer::Kind::Read] {
            push(
                Arc::new(peer::PeerTool::new(shell.clone(), session_id.clone(), kind)),
                &mut tools,
            );
        }
        return tools;
    }

    // `ask_user_question` 由 carter 注册（`ui_tx` 给它一条通道，`UiEvent::AskUser`
    // 由会话的转发任务接成界面表单）；后台命令表与 cwd 也交给它 —— carter 自己的
    // `bash` / `bash_output` / `kill_shell` 就是这么装起来的，这里不再重复一遍。
    // `ui_tx` 同时是 `task` 的进度出口：会话的转发任务把 `UiEvent::AgentProgress`
    // 接成界面上那张会动的卡片。`ask_user_question` 不该挂的会话（侧边、无人值守）
    // 仍要给通道 —— 只按开关把它从结果里筛掉。
    let full = carter_build_tools(
        skills_enabled,
        provider.clone(),
        model.clone(),
        agent_cfg.clone(),
        base_system.clone(),
        cancel.clone(),
        mcp_tools,
        ask_tx.clone(),
        shells.clone(),
        Some(cwd.clone()),
        // `goal_update` 这里自己挂（要在子 agent 快照之后）。
        None,
        // carter 的那个 `task` 下面会被丢掉、按自己的规矩重建，登记表挂在那一份上。
        None,
    );
    let ask_allowed = ask_tx.is_some() && settings.tool_enabled("ask_user_question");

    for t in filter_tools(&full, settings) {
        if t.name() == "ask_user_question" && !ask_allowed {
            continue;
        }
        // carter 的 `task` 握着的是**未过开关**的全量快照：这里按自己的规矩重建一个，
        // 让子 agent 也遵守开关并带上记录包装（见下面的 `snapshot`）。
        if t.name() == "task" {
            continue;
        }
        push(
            substitute_with(
                t,
                settings,
                &cwd,
                BashRuntime {
                    cancel: &cancel,
                    shells: shells.as_ref(),
                    session_id: &session_id,
                    shell_cwd: shell_cwd.as_ref(),
                    timeout: BashTimeout::of(&agent_cfg),
                },
            ),
            &mut tools,
        );
    }

    // 子 agent 的工具池：主 registry 里的全部工具，去掉 `agent`（不能再派生）与
    // `ask_user_question`（几个子 agent 同时弹表单，人会疯）、`goal_update`（子 agent
    // 一次 status = completed 就能把父会话的目标标完成，而这种错不报错，只会让长任务
    // 悄悄停下）。这个快照必须在挂 `goal_update` **之前**取。
    //
    // 子 agent 的调用直接在 carter 的 `run_turn` 里派发，没有主会话那层统一包装
    // （见 `session/turn.rs::turn_tools`），这里给它们套上，否则它们敲的命令在界面上一步
    // 都看不到。编号按到达次序现编 —— 几个子 agent 模型给的编号可能重复。
    let snapshot: Vec<Arc<dyn Tool>> = tools
        .tools()
        .iter()
        .filter(|t| !matches!(t.name(), "task" | "ask_user_question" | "goal_update"))
        .map(|t| {
            Arc::new(
                Recorded::new(t.clone(), tool_tx.clone(), seq.clone(), activity.clone())
                    .with_live(live.clone())
                    .with_running(running.clone()),
            ) as Arc<dyn Tool>
        })
        .collect();
    if include_task {
        let factory: carter::agent::ToolFactory = Arc::new(move || snapshot.clone());
        let mut task = carter::agent::TaskTool::new(
            provider,
            model,
            agent_cfg,
            base_system,
            cancel,
            factory,
            0,
        );
        if let Some(tx) = &ask_tx {
            task = task.with_progress(tx.clone());
        }
        if let Some(agents) = &background_agents {
            task = task.with_background(agents.clone());
        }
        push(Arc::new(task), &mut tools);
    }

    push(Arc::new(GoalUpdateTool::new(goal_signal)), &mut tools);

    // 定时任务。挂在**快照之后**：子 agent 各自建定时任务会造出一堆重复的，
    // 与 `goal_update` 同一个理由。
    if settings.tool_enabled("schedule") {
        push(
            Arc::new(ScheduleTool::new(
                schedules,
                cwd.to_string_lossy().to_string(),
            )),
            &mut tools,
        );
    }

    // 工单的结局。同样挂在快照之后：交不交工是主会话的决定，不是某个子 agent 的。
    if let Some((store, ticket_id)) = assistant {
        push(
            Arc::new(AssistantReportTool::new(store, ticket_id)),
            &mut tools,
        );
    }

    // 团队目录（目标 / 任务 / 看板）。在快照之后：子 agent 拿的是主对话工具表的
    // 快照，让它们顺手改团队账本等于「派出去的人各自改同一本账」——
    // 而它们本来就该把结果交回来由主对话记。侧边聊天同样不该挂（见 `team::tool::attach`）。
    // 只在初始化过团队目录的项目里挂：普通项目里它只是一段每轮下发、永远用不上的说明，
    // 模型还会被它引着去建团队目标。初始化是在界面上点的，下一轮装配时就能看到。
    if settings.tool_enabled("team") && crate::team::store::Store::discover(&cwd).is_some() {
        push(crate::team::tool::attach(&cwd), &mut tools);
    }

    // 跨会话消息。在快照之后：子 agent 看不到主对话，替它对外说话只会说半截情况。
    if let Some(shell) = peers {
        for kind in [peer::Kind::List, peer::Kind::Send, peer::Kind::Read] {
            let t = peer::PeerTool::new(shell.clone(), session_id.clone(), kind);
            if settings.tool_enabled(t.name()) {
                push(Arc::new(t), &mut tools);
            }
        }
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[Arc<dyn Tool>]) -> Vec<String> {
        let mut v: Vec<String> = list.iter().map(|t| t.name().to_string()).collect();
        v.sort();
        v
    }

    /// 默认全开。用 `ToolRegistry::builtin()` 而不是真的走 `build_tools`：
    /// 后者要一个 `LlmProvider`，而本模块的筛选逻辑根本不关心 provider。
    #[test]
    fn nothing_is_filtered_by_default() {
        let kept = filter_tools(&ToolRegistry::builtin(), &AppSettings::default());
        let n = names(&kept);
        assert_eq!(n.len(), 8, "内置工具应当一个不少：{n:?}");
        assert!(n.contains(&"bash".to_string()));
    }

    #[test]
    fn disabled_tools_are_dropped_and_nothing_else_is() {
        let settings = AppSettings {
            disabled_tools: vec!["bash".into(), "write_file".into()],
            ..Default::default()
        };
        let n = names(&filter_tools(&ToolRegistry::builtin(), &settings));
        assert!(!n.contains(&"bash".to_string()));
        assert!(!n.contains(&"write_file".to_string()));
        assert!(n.contains(&"read_file".to_string()), "不该误伤别的工具");
        assert_eq!(n.len(), 6);
    }

    /// 名单里写了个不存在的工具名，不该把装配搞崩 —— 用户可能刚从界面上
    /// 删掉一个 MCP server，它的工具名还留在开关列表里。
    #[test]
    fn an_unknown_name_in_the_disabled_list_is_harmless() {
        let settings = AppSettings {
            disabled_tools: vec!["no_such_tool".into()],
            ..Default::default()
        };
        let n = names(&filter_tools(&ToolRegistry::builtin(), &settings));
        assert_eq!(n.len(), 8);
    }

    /// `task` 永远不进筛选结果 —— `build_tools` 会另造一个。
    ///
    /// 摆在这条上的是「子 agent 也遵守开关」这个保证：`build_tools` 里
    /// `task` 的工厂闭包捕获的就是过滤之后的工具表，所以只要这个函数
    /// 把被关掉的工具筛掉了，子 agent 那边也一定拿不到。
    #[test]
    fn the_task_tool_never_comes_through_the_filter() {
        let mut full = ToolRegistry::builtin();
        full.push(Arc::new(carter::agent::AskUserQuestionTool::new(
            tokio::sync::mpsc::unbounded_channel().0,
        )));
        let kept = names(&filter_tools(&full, &AppSettings::default()));
        assert!(!kept.contains(&"task".to_string()));
        assert!(kept.contains(&"ask_user_question".to_string()));
    }

    /// `grep` 换成本应用自己的实现，名字与参数 schema 不变 —— 上游按名字下发，
    /// 名字一变 `is_concurrent_safe` 与子 agent 的工具白名单也跟着变。
    #[test]
    fn the_grep_tool_is_replaced_and_keeps_its_contract() {
        let full = ToolRegistry::builtin();
        let settings = AppSettings::default();
        let swapped: Vec<_> = filter_tools(&full, &settings)
            .into_iter()
            .map(|t| {
                substitute(
                    t,
                    &settings,
                    Path::new("/tmp"),
                    BashTimeout::of(&AgentConfig::default()),
                )
            })
            .collect();
        let greps: Vec<_> = swapped.iter().filter(|t| t.name() == "grep").collect();
        assert_eq!(greps.len(), 1, "必须恰好一个");
        assert_eq!(
            greps[0].parameters()["required"][0],
            serde_json::json!("pattern"),
            "参数 schema 不变"
        );
        // 换的是实现：描述里不该再让用户去装系统二进制。
        assert!(!greps[0].description().contains("已安装 rg"));
        assert!(greps[0].description().contains("内置"));
    }

    /// `skill` 是**就地换实现**，不是再多挂一个：池子里两个同名工具会让 `specs()`
    /// 下发两条，上游直接 400。名字也必须原样保留 —— 并发安全名单按名字判。
    #[test]
    fn the_skill_tool_is_replaced_in_place_and_keeps_its_name() {
        let mut full = ToolRegistry::builtin();
        full.push(std::sync::Arc::new(carter::skills::SkillTool::new(
            std::path::PathBuf::from("/tmp/skills"),
        )));
        let settings = AppSettings::default();

        let swapped: Vec<_> = filter_tools(&full, &settings)
            .into_iter()
            .map(|t| {
                substitute(
                    t,
                    &settings,
                    std::path::Path::new("/tmp"),
                    BashTimeout::of(&AgentConfig::default()),
                )
            })
            .collect();
        let skills: Vec<_> = swapped.iter().filter(|t| t.name() == "skill").collect();
        assert_eq!(skills.len(), 1, "必须恰好一个");
        // 给模型看的文档不变 —— 换的是实现，不是契约。
        assert!(
            skills[0].description().contains("Skill"),
            "{}",
            skills[0].description()
        );
        assert_eq!(
            skills[0].parameters()["required"][0],
            serde_json::json!("name"),
            "参数 schema 不变"
        );
    }
}
