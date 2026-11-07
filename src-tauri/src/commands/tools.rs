use crate::prelude::*;

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct ToolRow {
    name: String,
    description: String,
    /// 参数 JSON Schema，供界面展开查看。
    parameters: serde_json::Value,
    enabled: bool,
    /// `false` = 界面上不提供开关（只有 `task` 一例，它承载子 agent 能力）。
    toggleable: bool,
    /// 说明为什么不可关，或这个工具从哪来。
    note: Option<String>,
}

/// 本地会执行的工具清单。
///
/// 这是**本应用**装配出来的那一套（内置 + skill + ask_user + task）；
/// 厂商在服务端执行的工具（web_search 等）不在这里，它们是渠道能力，
/// 见 `list_channels`。MCP 工具也不在这里 —— 它们的名字要连上 server 才知道，
/// 见 `list_mcp_servers`。
#[tauri::command]
pub(crate) fn list_tools(shell: State<'_, Arc<AppShell>>, cwd: Option<String>) -> Vec<ToolRow> {
    let settings = AppSettings::load();
    let skills_on = shell.config.load().skills.enabled;

    let mut registry = carter::tools::ToolRegistry::builtin();
    if skills_on {
        registry.push(Arc::new(carter::skills::SkillTool::new(
            carter::config::paths::skills_dir(),
        )));
    }

    // 每个工具都过一遍 `carter_app_core::tools::substitute` —— 真正装配工具池时用的是
    // **同一个函数**。此前这里手抄了一遍 `bash` 与 `skill` 的构造，两处的说明迟早会漂，
    // 而设置页显示一份与真实行为对不上的文档，比不显示更坏（这是本文件自己的注释里
    // 早已写下的理由，现在才真正落实）。
    //
    // 这里不需要真的能跑：`substitute` 只读构造期参数，不碰 provider、不碰会话。
    // 会话 cwd 只用来让 `skill` 的说明指出项目级目录；没有会话时退回一个占位路径。
    let cwd = cwd
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(carter::config::paths::carter_home);
    let mut rows: Vec<ToolRow> = Vec::new();
    for t in registry.tools() {
        let t = carter_app_core::tools::substitute(t.clone(), &settings, &cwd);
        let name = t.name().to_string();
        let note = match name.as_str() {
            "bash" => Some(carter_app_core::platform::os_label().to_string()),
            // `note` 就是为「这个工具从哪来」准备的 —— 技能多了附加扫描路径之后，
            // 「它到底扫哪几个目录」是用户会问的第一个问题。
            "skill" => {
                let scan = Scan::from_settings_at(&settings, Some(&cwd));
                let extra = match settings.skill_roots.len() {
                    0 => String::new(),
                    n => format!(" 与 {n} 条附加扫描路径"),
                };
                Some(match &scan.project_enabled {
                    Some(p) => format!("扫 ~/.carter/skills、{} 里的项目技能{extra}", p.display()),
                    None => format!(
                        "扫 ~/.carter/skills{extra}（当前会话的项目里若有 .carter/skills 也会扫）"
                    ),
                })
            }
            _ => None,
        };
        rows.push(ToolRow {
            enabled: settings.tool_enabled(&name),
            toggleable: true,
            note,
            description: t.description().to_string(),
            parameters: t.parameters(),
            name,
        });
    }

    // ask_user_question 的 schema 也来自本应用的实现（给一个没人读的通道即可）。
    {
        use carter::tools::Tool as _;
        let a = carter_app_core::tools::ask_user::AskUserTool::new(
            tokio::sync::mpsc::unbounded_channel().0,
            carter::agent::CancelToken::new(),
        );
        rows.push(ToolRow {
            enabled: settings.tool_enabled("ask_user_question"),
            toggleable: true,
            note: None,
            name: a.name().to_string(),
            description: a.description().to_string(),
            parameters: a.parameters(),
        });
    }

    // 后台命令的两个配套工具。它们跟着 `bash` 的 `run_in_background` 一起用，
    // 所以也要出现在开关列表里 —— 用户关掉 bash 时，这两个留着也没有意义，
    // 但那是用户自己的选择，界面上说清楚即可。
    {
        use carter_app_core::tools::background::{BackgroundShells, BashOutputTool, KillShellTool};
        let shells = BackgroundShells::new();
        let extras: Vec<(std::sync::Arc<dyn carter::tools::Tool>, &str)> = vec![
            (
                std::sync::Arc::new(BashOutputTool::new(shells.clone())),
                "读取 `bash` 以后台方式启动的命令的输出。",
            ),
            (
                std::sync::Arc::new(KillShellTool::new(shells)),
                "结束一条后台命令。",
            ),
        ];
        for (t, note) in extras {
            rows.push(ToolRow {
                enabled: settings.tool_enabled(t.name()),
                toggleable: true,
                note: Some(note.to_string()),
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            });
        }
    }

    // 跨会话的三个工具。只展示说明，不会真的执行 —— 发信方 id 给空串即可。
    {
        use carter::tools::Tool as _;
        use carter_app_core::tools::peer::{Kind, PeerTool};
        for (kind, note) in [
            (Kind::List, "列出本机可以发信的其他对话"),
            (
                Kind::Send,
                "给另一条对话发一段话，对方空闲时立刻处理、忙时在下次工具返回时读到",
            ),
            (Kind::Read, "旁观另一条对话最近说了什么，不打扰它"),
        ] {
            let t = PeerTool::new(shell.inner().clone(), String::new(), kind);
            rows.push(ToolRow {
                enabled: settings.tool_enabled(t.name()),
                toggleable: true,
                note: Some(note.into()),
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            });
        }
    }

    // 定时任务工具。它要一份任务表 —— 用壳里那份**同一个** `Arc`，而不是新开一个：
    // 设置页与模型改的必须是同一份数据。
    {
        use carter::tools::Tool as _;
        let t = carter_app_core::tools::schedule::ScheduleTool::new(
            shell.schedules.clone(),
            cwd.to_string_lossy().to_string(),
        );
        rows.push(ToolRow {
            enabled: settings.tool_enabled(t.name()),
            toggleable: true,
            note: Some("到点自动新开一条对话".into()),
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters(),
        });
    }

    // `agent` 构造要一个 provider，这里拿不到 —— 单独补一行，标明它不可关。
    // 子 agent 的能力开关该是独立的一项，而不是混在「工具开关」里顺手关掉。
    rows.push(ToolRow {
        name: "agent".into(),
        description: "一次派生多个隔离子 agent 并行干活。每个子 agent 有独立上下文与工具池\
             （不含 agent 自身，不会再往下派生），并遵守同一份工具开关。"
            .into(),
        parameters: serde_json::Value::Null,
        enabled: true,
        toggleable: false,
        note: Some("承载子 agent 能力，暂不提供开关".into()),
    });

    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}
