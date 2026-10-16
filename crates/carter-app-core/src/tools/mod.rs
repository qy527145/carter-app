//! 工具池装配：在 carter 的装配结果之上做三件事 —— 换掉两个实现、套一层用户开关、
//! 外面再包一层记录。
//!
//! ## 为什么不是在 carter 里加开关
//!
//! carter 0.1.4 的 `ToolRegistry` 是「全部常开」，`Config` 里也没有任何开关字段。
//! 本应用不改 carter（它已经发布、CLI 也在用），而是在**装配之后**按名单挑一遍 ——
//! `ToolRegistry::empty/push/tools` 与 `TaskTool::new` 都是 crate 公开 API，这条路成立。
//!
//! ## 为什么 `task` 要单独重建
//!
//! `build_tools` 产出的 `task` 工具内部握着一个「工厂闭包」，子 agent 派生时用它
//! 重建工具池。若把 `task` 原样搬过来，那个闭包里装的还是**没过滤**的全量工具：
//! 用户关掉 `bash`，主 agent 拿不到，子 agent 却照跑。
//!
//! 所以这里重新造一个 `task`，工厂里放过滤后的快照 —— 而那份快照与主工具池
//! **是同一份值**（见 [`filter_tools`]）。这是「关掉的工具对子 agent 同样失效」
//! 的唯一保证，也正因为如此两条路径必须共用这一个函数的返回值。
//!
//! ## 换掉 / 包住哪些实现
//!
//! * **`bash`** —— carter 那个硬编码 `sh -c`，Windows 上从 GUI 启动时 PATH 里多半
//!   没有 `sh`，模型每次都拿到「program not found」而且看不出原因。换成
//!   [`bash::PlatformBash`]：探测 shell、显式设工作目录、描述里写明当前平台。
//! * **文件工具**（`read_file` / `write_file` / `edit_file` / `glob` / `grep`）——
//!   包一层 [`cwd::CwdScoped`]，把相对路径按会话的 cwd 解析。
//! * **`ask_user_question`** —— carter 那个一次只能问一个问题、选项只有标签没有说明。
//!   界面里「问三件事」要来回三轮，而每轮都是一次完整的模型请求。换成
//!   [`ask_user::AskUserTool`]：一次问一组、支持多选。
//! * **`skill`** —— carter 那个只认单根（构造期写死的 `~/.carter/skills`）。技能页能加
//!   额外的扫描路径，换成 [`multi_skill::MultiRootSkill`] 之后那些根里的技能才加载得到。
//!   刻意走「换实现」而不是「再 push 一个」：工具池里两个同名工具会让 `specs()` 下发
//!   两条，上游直接 400。
//!
//! ## 路径解析
//!
//! carter 的文件工具**不解析相对路径** —— 它们直接 `Path::new(&path)`，于是相对路径
//! 落到进程的 cwd 上。命令行下这是对的（进程 cwd 就是用户所在目录），桌面应用下必然
//! 错：进程 cwd 取决于从哪启动，而会话有自己的工作目录。而且它**不报错** ——
//! 系统提示里写着「工作目录：…」，文件却读写到了别处。
//!
//! ## 记录包装
//!
//! 每个工具外面再包一层 [`record::Recorded`]，把**完整的参数与结果**送给界面。
//! carter 的 `UiEvent` 只给截断的预览 —— 数据不够，界面上就只能给所有工具画同一种
//! 卡片。这是「工具看起来都一样」的根因。
//!
//! ## 这份开关只影响本应用
//!
//! 名单存在 `~/.carter-app/settings.json`，不是 `~/.carter/config.toml`。
//! carter CLI 读不到它，也不会受它影响 —— 界面上必须把这句话写出来。

pub mod ask_user;
pub mod bash;
pub mod cwd;
pub mod goal_update;
pub mod multi_skill;
pub mod record;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64};

use carter::agent::bootstrap::build_tools as carter_build_tools;
use carter::agent::{CancelToken, TaskTool, ToolFactory};
use carter::config::AgentConfig;
use carter::provider::LlmProvider;
use carter::registry::ModelInfo;
use carter::tools::{Tool, ToolRegistry};
use tokio::sync::mpsc;

use crate::app_settings::AppSettings;
use crate::goal::GoalSignal;
use crate::skills::Scan;
use ask_user::{AskRequest, AskUserTool};
use bash::PlatformBash;
use cwd::{CwdScoped, ProjectMemoryGuard};
use goal_update::GoalUpdateTool;
use multi_skill::MultiRootSkill;
use record::{Recorded, ToolEvent};

/// 装配工具池需要的一整套东西。
///
/// 打包成结构体是因为裸参数有十几个，而其中四个在重建 `task` 时要各用两次 ——
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
    /// 工具事件（完整参数与结果）的出口。
    pub tool_tx: mpsc::UnboundedSender<ToolEvent>,
    /// 提问请求的出口。`None` = 不给这个会话注册 `ask_user_question`。
    pub ask_tx: Option<mpsc::UnboundedSender<AskRequest>>,
    /// 是否挂上 `task`（派生子 agent 的入口）。侧边聊天不挂。
    pub include_task: bool,
    /// 长任务目标的信号格子。恒常传 —— `goal_update` 是恒常注册的，见 `goal_update.rs`。
    pub goal_signal: Arc<GoalSignal>,
    /// 工具调用计数。runner 靠它判断「这一轮到底动没动手」（自动继续的空转检测）。
    pub activity: Arc<AtomicU32>,
    /// 进行中那一轮的记录。`None` = 没人要看（侧边会话自己就不需要被别人看）。
    pub live: Option<Arc<crate::shell::LiveLog>>,
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
pub fn substitute(t: Arc<dyn Tool>, settings: &AppSettings, cwd: &Path) -> Arc<dyn Tool> {
    match t.name() {
        // `bash` 换成跨平台实现。
        "bash" => Arc::new(PlatformBash::new(cwd.to_path_buf())),
        // 文件工具：相对路径按会话的 cwd 解析。原实现（行号、图片、截断…）全部保留，
        // 只是在执行前把 `path` 改写成绝对路径。
        name if CwdScoped::applies_to(name) => Arc::new(CwdScoped::new(t, cwd.to_path_buf())),
        // save_memory 没有 path 参数可改写，目标路径是内部按 scope 算的 —— 项目级作用域
        // 在桌面应用里必然是错的，明确拒绝掉。
        "save_memory" => Arc::new(ProjectMemoryGuard::new(t)),
        // `skill` 换成多根实现：技能页加的扫描路径就在这里生效。
        "skill" => Arc::new(MultiRootSkill::new(Scan::from_settings(settings))),
        _ => t,
    }
}

/// 装配工具池。
///
/// 顺序：carter 装配 → 换实现 → 过滤 → 记录包装 → 快照 → 挂上 `task`。
/// `task` 必须最后挂：子 agent 的工厂闭包捕获的是「此刻已有的工具」，
/// 这个顺序就是递归守卫（工厂里不含 `task`，子 agent 不能再派生）。
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
        goal_signal,
        activity,
        live,
    } = args;

    // ui_tx 传 None：carter 的 ask_user 会一并注册进来，而我们用自己的那个
    // （见模块头的说明）。传 None 之后 carter 就不加它了。
    let full = carter_build_tools(
        skills_enabled,
        provider.clone(),
        model.clone(),
        agent_cfg.clone(),
        base_system.clone(),
        cancel.clone(),
        mcp_tools,
        None,
    );

    let seq = Arc::new(AtomicU64::new(0));
    let mut tools = ToolRegistry::empty();

    let push = |t: Arc<dyn Tool>, tools: &mut ToolRegistry| {
        tools.push(Arc::new(
            Recorded::new(t, tool_tx.clone(), seq.clone(), activity.clone())
                .with_live(live.clone()),
        ));
    };

    for t in filter_tools(&full, settings) {
        push(substitute(t, settings, &cwd), &mut tools);
    }

    // 我们自己那个一次问一组问题的 ask_user。
    if let Some(ask_tx) = ask_tx {
        if settings.tool_enabled("ask_user_question") {
            push(Arc::new(AskUserTool::new(ask_tx)), &mut tools);
        }
    }

    // 子 agent 工厂：产出「主 registry 中的所有工具（去掉 task）」。这个快照必须在
    // 挂 `goal_update` **之前**取，否则子 agent 也拿到 goal_update，一次
    // status = completed 就能把父会话的目标标完成 —— 而这种错不会报错，只会让
    // 长任务悄悄停下。
    let snapshot: Vec<Arc<dyn Tool>> = tools.tools().to_vec();
    if include_task {
        let factory: ToolFactory = Arc::new(move || snapshot.clone());
        tools.push(Arc::new(TaskTool::new(
            provider,
            model,
            agent_cfg,
            base_system,
            cancel,
            factory,
            0, // 主 agent depth = 0
        )));
    }

    push(Arc::new(GoalUpdateTool::new(goal_signal)), &mut tools);

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
            .map(|t| substitute(t, &settings, std::path::Path::new("/tmp")))
            .collect();
        let skills: Vec<_> = swapped.iter().filter(|t| t.name() == "skill").collect();
        assert_eq!(skills.len(), 1, "必须恰好一个");
        // 给模型看的文档不变 —— 换的是实现，不是契约。
        assert!(skills[0].description().contains("Skill"), "{}", skills[0].description());
        assert_eq!(
            skills[0].parameters()["required"][0],
            serde_json::json!("name"),
            "参数 schema 不变"
        );
    }
}
