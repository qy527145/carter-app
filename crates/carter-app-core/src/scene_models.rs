//! 场景模型：每一种「要调一次模型」的场合各用哪个模型，以及没指定时怎么回落。
//!
//! 以前这些规则散在八处代码里（会话入口、压缩、标题、记忆、助理评估……），用户只看得到
//! 「主模型 / 副模型」两个开关，猜不出某个场景到底用了谁。这里把它们收成一张表：
//! **设置页展示的就是运行时用的同一份回落链**，两边不会各说各的。
//!
//! 存在哪儿：
//!
//! | 场景 | 绑定存在 | 为什么 |
//! |---|---|---|
//! | 普通对话 | `config.toml` 的 `agent.model` | 它就是主模型，carter CLI 也认 |
//! | 工单执行 / 评估 | 助理设置的 `work_model` / `eval_model` | 早就有，不另存第二份 |
//! | 其余五个 | `settings.json` 的 `scene_models` | 只有本应用有这些场景（或这层覆盖） |
//!
//! 每个场景的取值是 `@main`（主模型）、`@fast`（副模型）或一个具体的 `渠道/模型`；
//! 没单独设过就是场景默认。回落链由取值展开（见 [`chain`]）。
//!
//! 回落链里的「会话模型」指**这条会话自己正在用的模型**（可能中途切过），不是全局主模型。

use std::sync::Arc;

use carter::config::Config;
use serde::Serialize;

use crate::app_settings::AppSettings;
use crate::protocol::{AppEvent, Command};
use crate::shell::AppShell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    Chat,
    AssistantChat,
    TicketWork,
    TicketEval,
    Compact,
    Title,
    MemorySuggest,
    MemoryConsolidate,
}

/// 回落链上的一环。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// 这个场景自己的绑定。
    Scene,
    /// 副模型（`agent.fast_model`）。
    Fast,
    /// 主模型（`agent.model`）。
    Main,
    /// 发起这次调用的那条会话当前用的模型。
    Session,
}

impl Scene {
    pub const ALL: [Scene; 8] = [
        Scene::Chat,
        Scene::AssistantChat,
        Scene::TicketWork,
        Scene::TicketEval,
        Scene::Compact,
        Scene::Title,
        Scene::MemorySuggest,
        Scene::MemoryConsolidate,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Scene::Chat => "chat",
            Scene::AssistantChat => "assistant_chat",
            Scene::TicketWork => "ticket_work",
            Scene::TicketEval => "ticket_eval",
            Scene::Compact => "compact",
            Scene::Title => "title",
            Scene::MemorySuggest => "memory_suggest",
            Scene::MemoryConsolidate => "memory_consolidate",
        }
    }

    pub fn from_id(id: &str) -> Option<Scene> {
        Scene::ALL.into_iter().find(|s| s.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Scene::Chat => "普通对话",
            Scene::AssistantChat => "助理对话",
            Scene::TicketWork => "工单执行",
            Scene::TicketEval => "工单评估",
            Scene::Compact => "上下文压缩",
            Scene::Title => "会话标题",
            Scene::MemorySuggest => "记忆候选提取",
            Scene::MemoryConsolidate => "助理记忆整理",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Scene::Chat => "新开会话时的模型。已开的会话不跟着变，可在会话里单独切换",
            Scene::AssistantChat => "常驻助理的对话。改了会当场切换正在跑的那条",
            Scene::TicketWork => "助理开工后，执行每张工单的会话",
            Scene::TicketEval => "读观察源的新事件，决定建单、挂到已有工单还是忽略",
            Scene::Compact => "上下文超阈值或 /compact 时，把较早的对话揉成摘要",
            Scene::Title => "首轮之后给会话起名",
            Scene::MemorySuggest => "用户纠正或表达偏好的那一轮之后，提出值得记住的条目",
            Scene::MemoryConsolidate => "助理对话按期整理长期记忆与纪要",
        }
    }

    /// 没单独设置时的取值：`@main` 或 `@fast`。普通对话没有默认 —— 它就是主模型本身。
    pub fn default_choice(self) -> Option<&'static str> {
        match self {
            Scene::Chat => None,
            Scene::AssistantChat | Scene::TicketWork => Some(MAIN),
            Scene::TicketEval
            | Scene::Compact
            | Scene::Title
            | Scene::MemorySuggest
            | Scene::MemoryConsolidate => Some(FAST),
        }
    }

    /// 回落链的最后一环。辅助场景兜到**发起调用的那条会话**自己的模型：会话中途切过模型的话，
    /// 压缩该用它切过去的那个，而不是全局主模型。其余场景没有「那条会话」，兜到主模型。
    fn terminal(self) -> Step {
        match self {
            Scene::Compact | Scene::Title | Scene::MemorySuggest | Scene::MemoryConsolidate => {
                Step::Session
            }
            _ => Step::Main,
        }
    }

    /// 绑定存在哪里：`config` | `assistant` | `app`。界面据此说明「CLI 认不认」。
    pub fn storage(self) -> &'static str {
        match self {
            Scene::Chat => "config",
            Scene::TicketWork | Scene::TicketEval => "assistant",
            _ => "app",
        }
    }
}

/// 取值里代表「主模型」「副模型」的记号。具体模型一律是 `渠道/模型`，含 `/`，不会撞上。
pub const MAIN: &str = "@main";
pub const FAST: &str = "@fast";

/// 场景此刻的取值：单独设过的，或场景默认。普通对话恒为主模型的具体引用。
pub fn choice(
    scene: Scene,
    config: &Config,
    app: &AppSettings,
    shell: Option<&AppShell>,
) -> String {
    let raw = match scene {
        Scene::Chat => return config.agent.model.clone(),
        Scene::TicketWork => {
            shell.and_then(|s| s.assistant.read(|a| a.settings.work_model.clone()))
        }
        Scene::TicketEval => {
            shell.and_then(|s| s.assistant.read(|a| a.settings.eval_model.clone()))
        }
        _ => app.scene_models.get(scene.id()).cloned(),
    };
    raw.map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .or_else(|| scene.default_choice().map(str::to_string))
        .unwrap_or_else(|| MAIN.to_string())
}

/// 回落链上的一格：哪一环、此刻指向哪个模型（`None` = 没设，或是「会话模型」）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Link {
    pub step: Step,
    pub model: Option<String>,
}

/// 由取值展开回落链。取值本质上要么是**指针**（`@main` / `@fast`），要么是**具体模型**：
///
/// - 指针 → 它指向的那个 → 最后一环；
/// - 具体模型 → 它自己 → 场景默认那个指针 → 最后一环（模型被删了也不至于没得用）。
///
/// 最后一环：辅助场景是「会话模型」，其余是主模型。同一环不重复出现。
pub fn chain(scene: Scene, choice: &str, config: &Config) -> Vec<Link> {
    let link = |step: Step, model: Option<String>| Link { step, model };
    let pointer = |c: &str| match c {
        FAST => Some(link(
            Step::Fast,
            config
                .agent
                .fast_model
                .clone()
                .filter(|r| !r.trim().is_empty()),
        )),
        MAIN => Some(link(Step::Main, Some(config.agent.model.clone()))),
        _ => None,
    };
    let mut out = Vec::new();
    match pointer(choice) {
        Some(l) => out.push(l),
        None => {
            out.push(link(Step::Scene, Some(choice.to_string())));
            out.extend(scene.default_choice().and_then(pointer));
        }
    }
    let tail = match scene.terminal() {
        Step::Main => link(Step::Main, Some(config.agent.model.clone())),
        step => link(step, None),
    };
    if !out.iter().any(|l| l.step == tail.step) {
        out.push(tail);
    }
    out
}

/// 沿链找第一个用得上的。走到「会话模型」就停 —— 那一环由调用方用会话自己的模型补上。
/// 返回下标；`None` = 一个都用不上（只有主模型也指不到时才会发生）。
pub fn pick(links: &[Link], usable: impl Fn(&str) -> bool) -> Option<usize> {
    links.iter().position(|l| match (&l.step, &l.model) {
        (Step::Session, _) => true,
        (_, Some(r)) => usable(r),
        (_, None) => false,
    })
}

/// 场景此刻实际会用的引用。`None` = 用发起调用的那条会话自己的模型（或什么都用不上）。
pub fn resolve_ref(
    scene: Scene,
    config: &Config,
    app: &AppSettings,
    shell: Option<&AppShell>,
) -> Option<String> {
    let links = chain(scene, &choice(scene, config, app, shell), config);
    pick(&links, |r| model_exists(config, r)).and_then(|i| links[i].model.clone())
}

/// 引用在当前配置里指得到一个模型。
pub fn model_exists(config: &Config, reference: &str) -> bool {
    reference.split_once('/').is_some_and(|(p, m)| {
        config
            .providers
            .get(p)
            .is_some_and(|pc| pc.models.contains_key(m))
    })
}

/// 设置页上的一行。
#[derive(Debug, Clone, Serialize)]
pub struct SceneRow {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// 此刻的取值：`@main` | `@fast` | `渠道/模型`。
    pub choice: String,
    /// 场景默认的取值；普通对话是 `None`（它就是主模型本身，没有「恢复默认」）。
    pub default_choice: Option<&'static str>,
    /// 回落链，从取值开始。
    pub chain: Vec<Link>,
    /// 链上实际生效的那一格；`None` = 一个都用不上。
    pub effective: Option<usize>,
    /// 绑定存在哪：`config` | `assistant` | `app`。
    pub storage: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct SceneModelsView {
    pub main: String,
    pub fast: Option<String>,
    pub scenes: Vec<SceneRow>,
}

pub fn view(shell: &AppShell) -> SceneModelsView {
    let config = shell.config.load_full();
    let app = AppSettings::load();
    let scenes = Scene::ALL
        .into_iter()
        .map(|s| {
            let c = choice(s, &config, &app, Some(shell));
            let links = chain(s, &c, &config);
            SceneRow {
                id: s.id(),
                label: s.label(),
                description: s.description(),
                effective: pick(&links, |r| model_exists(&config, r)),
                choice: c,
                default_choice: s.default_choice(),
                chain: links,
                storage: s.storage(),
            }
        })
        .collect();
    SceneModelsView {
        main: config.agent.model.clone(),
        fast: config
            .agent
            .fast_model
            .clone()
            .filter(|r| !r.trim().is_empty()),
        scenes,
    }
}

/// 改一个场景的取值：`@main` / `@fast` / `渠道/模型`。`None`（或等于场景默认）= 恢复默认，
/// 存储里删掉这一项 —— 以后改了场景默认，没单独设过的场景跟着走。
///
/// 回传新的应用设置：`set_app_settings` 是整体覆盖写入，前端 store 要对齐，
/// 否则下一次改主题会把这里写进去的绑定抹掉。
pub fn set_scene(
    shell: &Arc<AppShell>,
    id: &str,
    value: Option<&str>,
) -> Result<AppSettings, String> {
    let scene = Scene::from_id(id).ok_or_else(|| format!("不认识的场景 `{id}`"))?;
    let value = value
        .map(str::trim)
        .filter(|v| !v.is_empty() && Some(*v) != scene.default_choice());
    if let Some(v) = value {
        let config = shell.config.load_full();
        let token = v == MAIN || v == FAST;
        if scene == Scene::Chat && token {
            return Err("普通对话就是主模型本身，只能选具体的模型".into());
        }
        if !token && !model_exists(&config, v) {
            return Err(format!("模型 `{v}` 不在任何渠道里"));
        }
    }
    let mut app = AppSettings::load();
    match scene {
        Scene::Chat => {
            let r = value.ok_or("普通对话必须有模型（它就是主模型）")?;
            crate::config_edit::set_default_model_reload(shell, r)?;
        }
        Scene::TicketWork | Scene::TicketEval => {
            let v = value.map(str::to_string);
            shell.assistant.with(|s| match scene {
                Scene::TicketWork => s.settings.work_model = v,
                _ => s.settings.eval_model = v,
            })?;
            shell.emit(AppEvent::AssistantChanged);
        }
        _ => {
            match value {
                Some(r) => app
                    .scene_models
                    .insert(scene.id().to_string(), r.to_string()),
                None => app.scene_models.remove(scene.id()),
            };
            app.save()?;
        }
    }
    // 主模型、副模型一变，助理对话的取值可能指向别处了；它自己的取值变了更不用说。
    if matches!(scene, Scene::Chat | Scene::AssistantChat) {
        switch_assistant_chat(shell, &app);
    }
    Ok(app)
}

/// 助理对话是常驻的：它的模型变了就当场让正在跑的那条换过去，
/// 否则要等到下次重启应用才生效，而用户没有别的办法让它重开。
pub fn switch_assistant_chat(shell: &Arc<AppShell>, app: &AppSettings) {
    let Some(handle) = shell.session(crate::assistant::CHAT_SESSION_ID) else {
        return;
    };
    let config = shell.config.load_full();
    let Some(target) = resolve_ref(Scene::AssistantChat, &config, app, Some(shell)) else {
        return;
    };
    if handle.model.load().key != target {
        handle.send(Command::SetModel { model: target });
    }
}

/// 不依附于某条会话的场景（助理对话、工单执行、工单评估）该用的引用。
pub fn standalone_ref(shell: &AppShell, scene: Scene) -> Option<String> {
    let config = shell.config.load_full();
    resolve_ref(scene, &config, &AppSettings::load(), Some(shell))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(main: &str, fast: Option<&str>) -> Config {
        let mut c = Config::default();
        c.agent.model = main.into();
        c.agent.fast_model = fast.map(str::to_string);
        c
    }

    fn steps(links: &[Link]) -> Vec<Step> {
        links.iter().map(|l| l.step).collect()
    }

    #[test]
    fn helper_scenes_default_to_fast_then_the_session_model() {
        let c = config("p/main", Some("p/fast"));
        let links = chain(Scene::Title, FAST, &c);
        assert_eq!(steps(&links), [Step::Fast, Step::Session]);
        assert_eq!(pick(&links, |_| true), Some(0));
        // 没配副模型：不回落到全局主模型，而是会话自己的模型。
        let c = config("p/main", None);
        let links = chain(Scene::Compact, FAST, &c);
        assert_eq!(pick(&links, |_| true), Some(1));
    }

    #[test]
    fn a_concrete_choice_falls_back_through_the_scene_default() {
        let c = config("p/main", Some("p/fast"));
        let links = chain(Scene::MemorySuggest, "gone/x", &c);
        assert_eq!(steps(&links), [Step::Scene, Step::Fast, Step::Session]);
        // 绑定的模型被删了：跳过它，与运行时一致。
        assert_eq!(pick(&links, |r| r != "gone/x"), Some(1));
        let links = chain(Scene::TicketWork, "p/x", &c);
        assert_eq!(steps(&links), [Step::Scene, Step::Main]);
    }

    #[test]
    fn ticket_eval_defaults_to_fast_then_main() {
        let c = config("p/main", None);
        let links = chain(Scene::TicketEval, FAST, &c);
        assert_eq!(steps(&links), [Step::Fast, Step::Main]);
        assert_eq!(pick(&links, |_| true), Some(1));
    }

    #[test]
    fn choosing_main_for_a_helper_scene_still_ends_at_the_session() {
        let c = config("p/main", Some("p/fast"));
        let links = chain(Scene::Title, MAIN, &c);
        // 指针只指向它自己那一个，不再绕回场景默认的副模型。
        assert_eq!(steps(&links), [Step::Main, Step::Session]);
    }

    #[test]
    fn ticket_work_and_assistant_chat_default_to_main() {
        let c = config("p/main", Some("p/fast"));
        for s in [Scene::TicketWork, Scene::AssistantChat] {
            assert_eq!(s.default_choice(), Some(MAIN));
            assert_eq!(steps(&chain(s, MAIN, &c)), [Step::Main]);
        }
    }

    #[test]
    fn scene_ids_round_trip() {
        for s in Scene::ALL {
            assert_eq!(Scene::from_id(s.id()), Some(s));
        }
        assert_eq!(Scene::from_id("nope"), None);
    }
}
