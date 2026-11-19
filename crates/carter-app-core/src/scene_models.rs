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

    /// 没绑定时依次试的后备（不含 [`Step::Scene`] 本身）。
    pub fn fallback(self) -> &'static [Step] {
        match self {
            // 普通对话的绑定就是主模型本身，没有后备可言。
            Scene::Chat => &[],
            Scene::AssistantChat | Scene::TicketWork => &[Step::Main],
            Scene::TicketEval => &[Step::Fast, Step::Main],
            Scene::Compact | Scene::Title | Scene::MemorySuggest | Scene::MemoryConsolidate => {
                &[Step::Fast, Step::Session]
            }
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

/// 某个场景此刻的绑定（只看这个场景自己，不回落）。空串当没配。
pub fn binding(
    scene: Scene,
    config: &Config,
    app: &AppSettings,
    shell: Option<&AppShell>,
) -> Option<String> {
    let raw = match scene {
        Scene::Chat => Some(config.agent.model.clone()),
        Scene::TicketWork => {
            shell.and_then(|s| s.assistant.read(|a| a.settings.work_model.clone()))
        }
        Scene::TicketEval => {
            shell.and_then(|s| s.assistant.read(|a| a.settings.eval_model.clone()))
        }
        _ => app.scene_models.get(scene.id()).cloned(),
    };
    raw.filter(|r| !r.trim().is_empty())
}

/// 按回落链定下用哪个引用。`Session` 一环不在这里展开（那要看具体哪条会话）：
/// 走到它时返回 `(None, Step::Session)`，调用方用会话自己的模型。
///
/// `usable` 用来跳过解析不出来的引用（渠道被删了之类）——与运行时「解析失败就回落」一致。
pub fn pick(
    scene: Scene,
    bound: Option<String>,
    config: &Config,
    usable: impl Fn(&str) -> bool,
) -> (Option<String>, Step) {
    let fast = config
        .agent
        .fast_model
        .clone()
        .filter(|r| !r.trim().is_empty());
    let candidates =
        std::iter::once((Step::Scene, bound)).chain(scene.fallback().iter().map(|s| {
            (
                *s,
                match s {
                    Step::Fast => fast.clone(),
                    Step::Main => Some(config.agent.model.clone()),
                    Step::Scene | Step::Session => None,
                },
            )
        }));
    for (step, r) in candidates {
        if step == Step::Session {
            return (None, Step::Session);
        }
        if let Some(r) = r.filter(|r| usable(r)) {
            return (Some(r), step);
        }
    }
    (None, Step::Session)
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
    /// 这个场景自己的绑定；`None` = 走回落链。
    pub binding: Option<String>,
    /// 回落链（不含场景自身）。
    pub fallback: Vec<Step>,
    /// 实际会用的模型；`None` = 用发起调用的那条会话自己的模型。
    pub effective: Option<String>,
    /// `effective` 来自回落链的哪一环。
    pub effective_from: Step,
    /// 绑定存在哪：`config` | `assistant` | `app`。
    pub storage: &'static str,
    /// 绑定指不到任何模型（渠道或模型被删了）。运行时会跳过它往下回落。
    pub binding_broken: bool,
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
            let bound = binding(s, &config, &app, Some(shell));
            let (effective, from) = pick(s, bound.clone(), &config, |r| model_exists(&config, r));
            SceneRow {
                id: s.id(),
                label: s.label(),
                description: s.description(),
                binding_broken: bound.as_deref().is_some_and(|r| !model_exists(&config, r)),
                binding: bound,
                fallback: s.fallback().to_vec(),
                effective,
                effective_from: from,
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

/// 改一个场景的绑定。`None` = 清掉，回到回落链。普通对话不能清（主模型必须有）。
///
/// 回传新的应用设置：`set_app_settings` 是整体覆盖写入，前端 store 要对齐，
/// 否则下一次改主题会把这里写进去的绑定抹掉。
pub fn set_scene(
    shell: &Arc<AppShell>,
    id: &str,
    model_ref: Option<&str>,
) -> Result<AppSettings, String> {
    let scene = Scene::from_id(id).ok_or_else(|| format!("不认识的场景 `{id}`"))?;
    let model_ref = model_ref.map(str::trim).filter(|r| !r.is_empty());
    if let Some(r) = model_ref {
        let config = shell.config.load_full();
        if !model_exists(&config, r) {
            return Err(format!("模型 `{r}` 不在任何渠道里"));
        }
    }
    let mut app = AppSettings::load();
    match scene {
        Scene::Chat => {
            let r = model_ref.ok_or("普通对话必须有模型（它就是主模型）")?;
            crate::config_edit::set_default_model_reload(shell, r)?;
        }
        Scene::TicketWork | Scene::TicketEval => {
            let v = model_ref.map(str::to_string);
            shell.assistant.with(|s| match scene {
                Scene::TicketWork => s.settings.work_model = v,
                _ => s.settings.eval_model = v,
            })?;
            shell.emit(AppEvent::AssistantChanged);
        }
        _ => {
            match model_ref {
                Some(r) => app
                    .scene_models
                    .insert(scene.id().to_string(), r.to_string()),
                None => app.scene_models.remove(scene.id()),
            };
            app.save()?;
            if scene == Scene::AssistantChat {
                switch_assistant_chat(shell, &app);
            }
        }
    }
    Ok(app)
}

/// 助理对话是常驻的：改了它的场景模型，当场让正在跑的那条换过去，
/// 否则要等到下次重启应用才生效，而用户没有别的办法让它重开。
fn switch_assistant_chat(shell: &Arc<AppShell>, app: &AppSettings) {
    let Some(handle) = shell.session(crate::assistant::CHAT_SESSION_ID) else {
        return;
    };
    let config = shell.config.load_full();
    let bound = binding(Scene::AssistantChat, &config, app, Some(shell));
    let (Some(target), _) = pick(Scene::AssistantChat, bound, &config, |r| {
        model_exists(&config, r)
    }) else {
        return;
    };
    if handle.model.load().key != target {
        handle.send(Command::SetModel { model: target });
    }
}

/// 助理对话开新会话时用的模型（界面没显式指定时）。`None` = 交给会话入口用主模型。
pub fn assistant_chat_model(shell: &AppShell) -> Option<String> {
    let config = shell.config.load_full();
    binding(
        Scene::AssistantChat,
        &config,
        &AppSettings::load(),
        Some(shell),
    )
    .filter(|r| model_exists(&config, r))
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

    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn helper_scenes_prefer_their_own_binding_then_fast_then_the_session() {
        let c = config("p/main", Some("p/fast"));
        assert_eq!(
            pick(Scene::Title, Some("p/t".into()), &c, all),
            (Some("p/t".into()), Step::Scene)
        );
        assert_eq!(
            pick(Scene::Title, None, &c, all),
            (Some("p/fast".into()), Step::Fast)
        );
        let no_fast = config("p/main", None);
        // 不回落到全局主模型：会话中途切过模型的话，压缩该用它切过去的那个。
        assert_eq!(
            pick(Scene::Compact, None, &no_fast, all),
            (None, Step::Session)
        );
    }

    #[test]
    fn ticket_eval_falls_back_to_fast_then_main() {
        let c = config("p/main", None);
        assert_eq!(
            pick(Scene::TicketEval, None, &c, all),
            (Some("p/main".into()), Step::Main)
        );
        let c = config("p/main", Some("p/fast"));
        assert_eq!(
            pick(Scene::TicketEval, None, &c, all),
            (Some("p/fast".into()), Step::Fast)
        );
    }

    #[test]
    fn ticket_work_and_assistant_chat_skip_the_fast_model() {
        let c = config("p/main", Some("p/fast"));
        for s in [Scene::TicketWork, Scene::AssistantChat] {
            assert_eq!(pick(s, None, &c, all), (Some("p/main".into()), Step::Main));
        }
    }

    #[test]
    fn a_broken_binding_is_skipped_like_at_runtime() {
        let c = config("p/main", Some("p/fast"));
        let usable = |r: &str| r != "gone/x";
        assert_eq!(
            pick(Scene::MemorySuggest, Some("gone/x".into()), &c, usable),
            (Some("p/fast".into()), Step::Fast)
        );
    }

    #[test]
    fn scene_ids_round_trip() {
        for s in Scene::ALL {
            assert_eq!(Scene::from_id(s.id()), Some(s));
        }
        assert_eq!(Scene::from_id("nope"), None);
    }
}
