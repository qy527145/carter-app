//! 工具池装配：在 carter 的装配结果之上套一层用户开关。
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
//! ## 这份开关只影响本应用
//!
//! 名单存在 `~/.carter-app/settings.json`，不是 `~/.carter/config.toml`。
//! carter CLI 读不到它，也不会受它影响 —— 界面上必须把这句话写出来。

use std::sync::Arc;

use carter::agent::bootstrap::build_tools as carter_build_tools;
use carter::agent::{CancelToken, TaskTool, ToolFactory, UiEvent};
use carter::config::AgentConfig;
use carter::provider::LlmProvider;
use carter::registry::ModelInfo;
use carter::tools::{Tool, ToolRegistry};

use crate::app_settings::AppSettings;

/// 装配工具池需要的一整套东西。
///
/// 打包成结构体是因为裸参数有八个，而其中四个在重建 `task` 时要各用两次 ——
/// 八个位置参数摆在那里，早晚会有人传错顺序。
pub struct ToolBuildArgs {
    pub provider: Arc<dyn LlmProvider>,
    pub model: ModelInfo,
    pub agent_cfg: AgentConfig,
    pub base_system: String,
    pub cancel: CancelToken,
    pub mcp_tools: Vec<Arc<dyn Tool>>,
    pub ui_tx: Option<tokio::sync::mpsc::UnboundedSender<UiEvent>>,
    pub skills_enabled: bool,
}

/// 按设置挑出要保留的工具，**不含 `task`**。
///
/// 返回值同时是主工具池的内容与子 agent 工厂的快照 —— `build_tools` 里
/// 那两处用的是同一个值，不是各自算一遍。
pub fn filter_tools(full: &ToolRegistry, settings: &AppSettings) -> Vec<Arc<dyn Tool>> {
    full.tools()
        .iter()
        // `task` 不参与筛选：它不是「一个工具」，是派生子 agent 的入口。
        // 把它关掉等于砍掉子 agent 能力，那是另一个开关该管的事。
        .filter(|t| t.name() != "task" && settings.tool_enabled(t.name()))
        .cloned()
        .collect()
}

/// 装配工具池，并按设置里的名单过滤掉被关掉的工具。
pub fn build_tools(args: ToolBuildArgs, settings: &AppSettings) -> ToolRegistry {
    let ToolBuildArgs {
        provider,
        model,
        agent_cfg,
        base_system,
        cancel,
        mcp_tools,
        ui_tx,
        skills_enabled,
    } = args;

    // 先按 carter 的标准方式装一遍：内置 8 个 + skill + ask_user + MCP 工具 + task。
    // 递归守卫与装配顺序都留在 carter 那侧，我们只做筛选、不重写装配逻辑。
    let full = carter_build_tools(
        skills_enabled,
        provider.clone(),
        model.clone(),
        agent_cfg.clone(),
        base_system.clone(),
        cancel.clone(),
        mcp_tools,
        ui_tx,
    );

    let kept = filter_tools(&full, settings);

    let mut tools = ToolRegistry::empty();
    for t in &kept {
        tools.push(t.clone());
    }

    // 工厂闭包里放的正是上面那份 `kept` —— 与主工具池同源。
    let snapshot: Vec<Arc<dyn Tool>> = kept;
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
    /// `task` 的工厂闭包捕获的就是 `filter_tools` 的返回值，所以只要这个函数
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
}
