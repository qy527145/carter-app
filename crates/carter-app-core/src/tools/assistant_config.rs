//! 助理对话里的配置工具：让用户用一句话就能让助理把观察源与规则建出来。
//!
//! ## 为什么要有这组工具
//!
//! 原型里主 Session 就是一个可以对话的会话，它带着 `orchestrator_settings_save` /
//! `orchestrator_read` / `orchestrator_prompts_save` 一组工具 —— 「加一个来源、改一条
//! 策略」本来就是通过对话让 agent 调工具做的，面板只是同一份数据的另一个入口。
//! 少了这一层，用户就只能自己跑到设置页里手填命令与规则，那正是原型一直在避免的
//! 「把配置工作丢回给人」。
//!
//! ## 为什么按对象拆成几个工具，而不是一个 `assistant_config(action, ...)`
//!
//! 与 `tools/schedule.rs` 同一个理由：每个动作的参数差别很大，塞进同一个 schema 之后
//! 模型经常填一串不相干的字段，而必填项要到执行时才报错。拆开之后每个 schema 只说
//! 自己的事。
//!
//! ## 只在助理对话里挂
//!
//! 普通对话、工单的执行会话都拿不到它们：一条工单不该顺手改掉自己的接单规则。
//! 见 `tools/mod.rs` 里的装配顺序。

use std::sync::Arc;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::assistant::{AssistantStore, Mode, Rule, Source, new_id, source::run_command};
use crate::protocol::now_ms;

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    SourceSave,
    SourceDelete,
    RuleSave,
    RuleDelete,
    Mode,
}

pub struct AssistantConfigTool {
    store: Arc<AssistantStore>,
    kind: Kind,
}

impl AssistantConfigTool {
    pub fn new(store: Arc<AssistantStore>, kind: Kind) -> Self {
        Self { store, kind }
    }
}

/// 按名字找（模型回的多半是名字）。找不到再按 id。
fn find_source(store: &AssistantStore, key: &str) -> Option<Source> {
    let key = key.trim();
    store.read(|s| {
        s.sources
            .iter()
            .find(|x| x.name.trim() == key)
            .or_else(|| s.sources.iter().find(|x| x.id == key))
            .cloned()
    })
}

fn find_rule(store: &AssistantStore, key: &str) -> Option<Rule> {
    let key = key.trim();
    store.read(|s| {
        s.settings
            .rules
            .iter()
            .find(|r| r.name.trim() == key)
            .or_else(|| s.settings.rules.iter().find(|r| r.id == key))
            .cloned()
    })
}

fn field(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 列表用的一行。
fn describe_source(s: &Source) -> String {
    format!(
        "- {}（{}，每 {} 分钟，{}）：`{}`",
        s.name,
        s.cwd,
        s.every_minutes,
        if s.enabled { "启用" } else { "停用" },
        s.command
    )
}

#[async_trait::async_trait]
impl Tool for AssistantConfigTool {
    fn name(&self) -> &str {
        match self.kind {
            Kind::SourceSave => "assistant_source_save",
            Kind::SourceDelete => "assistant_source_delete",
            Kind::RuleSave => "assistant_rule_save",
            Kind::RuleDelete => "assistant_rule_delete",
            Kind::Mode => "assistant_mode",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            Kind::SourceSave => {
                "新增或修改一个观察源：一条定时跑的命令，输出里每行一件事，新出现的行就是事件。\n\
                 按名字找现有的那条：同名就更新，没有就新建。\n\
                 命令要能反复跑、输出稳定（`gh ...`、`git ...`、`dws ...`）。写完会自动试跑一次，\
                 试跑失败会原样把错误回给你 —— 先把命令修对再告诉用户建好了。"
            }
            Kind::SourceDelete => "删掉一个观察源（按名字）。已经开出来的工单不受影响。",
            Kind::RuleSave => {
                "新增或修改一条接单规则。按名字找：同名就更新。\n\
                 规则是写给评估模型看的一句话：什么样的事可以替用户做、做到什么程度、什么不能做。\n\
                 没有规则时助理什么都不接，所以用户说「帮我盯着 X 就看」时，至少要建一条规则。"
            }
            Kind::RuleDelete => "删掉一条接单规则（按名字）。",
            Kind::Mode => {
                "设置接单模式或暂停。\n\
                 - mode = observe：只记录「本来会接」，等用户批准（新装好时的默认）；\n\
                 - mode = auto：命中规则就开工；\n\
                 - paused = true/false：暂停 / 恢复（暂停时不再检查观察源，也不开工）。\n\
                 切到 auto 之前先确认用户明确要求过 —— 那是让它自己动起来的开关。"
            }
        }
    }

    fn parameters(&self) -> Value {
        match self.kind {
            Kind::SourceSave => json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "短名字，界面上显示就用它；同名视为修改" },
                    "command": { "type": "string", "description": "要反复跑的命令，每行输出一件事" },
                    "cwd": { "type": "string", "description": "命令在哪个目录跑；不给就沿用原来的，新建时必填" },
                    "every_minutes": { "type": "integer", "description": "多久检查一次（分钟，1 ~ 1440），不给就用 10" },
                    "enabled": { "type": "boolean", "description": "是否启用，不给就是启用" }
                },
                "required": ["name"]
            }),
            Kind::SourceDelete => json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "观察源的名字" } },
                "required": ["name"]
            }),
            Kind::RuleSave => json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "短规则名，评估按名字引用；同名视为修改" },
                    "instruction": { "type": "string", "description": "一句话：什么情况下可以替用户做什么、做到什么程度" },
                    "enabled": { "type": "boolean", "description": "是否启用，不给就是启用" }
                },
                "required": ["name", "instruction"]
            }),
            Kind::RuleDelete => json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "规则名" } },
                "required": ["name"]
            }),
            Kind::Mode => json!({
                "type": "object",
                "properties": {
                    "mode": { "type": "string", "enum": ["observe", "auto"], "description": "接单模式" },
                    "paused": { "type": "boolean", "description": "暂停 / 恢复" }
                }
            }),
        }
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match self.kind {
            Kind::SourceSave => self.save_source(args).await,
            Kind::SourceDelete => {
                let Some(name) = field(&args, "name") else {
                    return ToolResult::err("assistant_source_delete: 缺少 name");
                };
                let Some(s) = find_source(&self.store, &name) else {
                    return ToolResult::err(format!("assistant_source_delete: 没有叫「{name}」的观察源"));
                };
                match self.store.with(|st| st.sources.retain(|x| x.id != s.id)) {
                    Ok(()) => ToolResult::ok(format!("已删除观察源「{}」。", s.name)),
                    Err(e) => ToolResult::err(format!("assistant_source_delete: 写盘失败：{e}")),
                }
            }
            Kind::RuleSave => {
                let (Some(name), Some(instruction)) =
                    (field(&args, "name"), field(&args, "instruction"))
                else {
                    return ToolResult::err("assistant_rule_save: 缺少 name 或 instruction");
                };
                let enabled = args.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                let existing = find_rule(&self.store, &name);
                let clash = self.store.read(|s| {
                    s.settings
                        .rules
                        .iter()
                        .any(|r| r.name.trim() == name && existing.as_ref().is_none_or(|e| e.id != r.id))
                });
                if clash {
                    return ToolResult::err(format!("assistant_rule_save: 已经有叫「{name}」的规则了"));
                }
                let rule = Rule {
                    id: existing.as_ref().map(|r| r.id.clone()).unwrap_or_else(|| new_id("rule", now_ms())),
                    name: name.clone(),
                    instruction,
                    enabled,
                };
                let id = rule.id.clone();
                let res = self.store.with(|s| {
                    match s.settings.rules.iter_mut().find(|r| r.id == id) {
                        Some(slot) => *slot = rule.clone(),
                        None => s.settings.rules.push(rule.clone()),
                    }
                });
                match res {
                    Ok(()) => ToolResult::ok(format!(
                        "已{}接单规则「{}」：{}",
                        if existing.is_some() { "更新" } else { "新建" },
                        rule.name,
                        rule.instruction
                    )),
                    Err(e) => ToolResult::err(format!("assistant_rule_save: 写盘失败：{e}")),
                }
            }
            Kind::RuleDelete => {
                let Some(name) = field(&args, "name") else {
                    return ToolResult::err("assistant_rule_delete: 缺少 name");
                };
                let Some(r) = find_rule(&self.store, &name) else {
                    return ToolResult::err(format!("assistant_rule_delete: 没有叫「{name}」的规则"));
                };
                let id = r.id.clone();
                match self
                    .store
                    .with(|s| s.settings.rules.retain(|x| x.id != id))
                {
                    Ok(()) => ToolResult::ok(format!("已删除接单规则「{}」。", r.name)),
                    Err(e) => ToolResult::err(format!("assistant_rule_delete: 写盘失败：{e}")),
                }
            }
            Kind::Mode => {
                let mode = field(&args, "mode");
                let paused = args.get("paused").and_then(Value::as_bool);
                if mode.is_none() && paused.is_none() {
                    return ToolResult::err("assistant_mode: mode 与 paused 至少给一个");
                }
                let mode = match mode.as_deref() {
                    None => None,
                    Some("observe") => Some(Mode::Observe),
                    Some("auto") => Some(Mode::Auto),
                    Some(other) => {
                        return ToolResult::err(format!(
                            "assistant_mode: mode 只能是 observe / auto，收到 `{other}`"
                        ));
                    }
                };
                match self.store.with(|s| {
                    if let Some(m) = mode {
                        s.settings.mode = m;
                    }
                    if let Some(p) = paused {
                        s.settings.paused = p;
                    }
                }) {
                    Ok(()) => {
                        let now = self.store.read(|s| (s.settings.mode, s.settings.paused));
                        ToolResult::ok(format!(
                            "已设置：{}，{}。",
                            match now.0 {
                                Mode::Observe => "只观察（命中只记录，等用户批准）",
                                Mode::Auto => "自动接单",
                            },
                            if now.1 { "已暂停" } else { "运行中" }
                        ))
                    }
                    Err(e) => ToolResult::err(format!("assistant_mode: 写盘失败：{e}")),
                }
            }
        }
    }
}

impl AssistantConfigTool {
    async fn save_source(&self, args: Value) -> ToolResult {
        let Some(name) = field(&args, "name") else {
            return ToolResult::err("assistant_source_save: 缺少 name");
        };
        let existing = find_source(&self.store, &name);
        let command = field(&args, "command")
            .or_else(|| existing.as_ref().map(|s| s.command.clone()))
            .unwrap_or_default();
        let cwd = field(&args, "cwd")
            .or_else(|| existing.as_ref().map(|s| s.cwd.clone()))
            .unwrap_or_default();
        let every = args
            .get("every_minutes")
            .and_then(Value::as_u64)
            .or_else(|| existing.as_ref().map(|s| s.every_minutes))
            .unwrap_or(10);
        let enabled = args
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| existing.as_ref().is_none_or(|s| s.enabled));

        let src = Source {
            id: existing
                .as_ref()
                .map(|s| s.id.clone())
                .unwrap_or_else(|| new_id("src", now_ms())),
            name: name.clone(),
            command,
            cwd,
            every_minutes: every,
            enabled,
            state: existing.as_ref().map(|s| s.state.clone()).unwrap_or_default(),
        };
        if let Err(e) = src.validate() {
            return ToolResult::err(format!("assistant_source_save: {e}"));
        }
        if !std::path::Path::new(&src.cwd).is_dir() {
            return ToolResult::err(format!(
                "assistant_source_save: 工作目录 {} 不存在",
                src.cwd
            ));
        }

        // 先试跑一次再存：命令写错的话，用户会得到一个永远跑不出东西的观察源，
        // 而界面上只显示一句「上次失败」—— 不如现在就告诉模型哪里不对。
        if let Err(e) = run_command(&src.command, std::path::Path::new(&src.cwd)).await {
            return ToolResult::err(format!(
                "assistant_source_save: 试跑失败，先修命令再保存：{e}"
            ));
        }

        let updated = existing.is_some();
        match self.store.with(|s| s.upsert_source(src.clone())) {
            Ok(()) => ToolResult::ok(format!(
                "已{}观察源：\n{}\n\n第一次检查只记下现状，之后新出现的行才算事件。",
                if updated { "更新" } else { "新建" },
                describe_source(&src)
            )),
            Err(e) => ToolResult::err(format!("assistant_source_save: 写盘失败：{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Arc<AssistantStore> {
        let dir = std::env::temp_dir().join(format!(
            "carter-configtool-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        AssistantStore::open(dir.join("assistant.json"))
    }


    #[tokio::test]
    async fn a_rule_can_be_created_updated_and_deleted_by_name() {
        let st = store();
        let save = AssistantConfigTool::new(st.clone(), Kind::RuleSave);
        let r = save
            .execute(json!({"name": "修 CI", "instruction": "主干挂了就修"}))
            .await;
        assert!(r.ok, "{}", r.content);
        assert!(st.read(|s| s.settings.rules.len()) == 1);

        let r = save
            .execute(json!({"name": "修 CI", "instruction": "主干 CI 挂了就修，不推送"}))
            .await;
        assert!(r.ok);
        assert_eq!(st.read(|s| s.settings.rules.len()), 1, "同名是更新不是新增");
        assert!(st.read(|s| s.settings.rules[0].instruction.contains("不推送")));

        let del = AssistantConfigTool::new(st.clone(), Kind::RuleDelete);
        assert!(del.execute(json!({"name": "修 CI"})).await.ok);
        assert!(st.read(|s| s.settings.rules.is_empty()));
    }

    #[tokio::test]
    async fn a_broken_command_is_refused_before_it_is_stored() {
        let st = store();
        let tool = AssistantConfigTool::new(st.clone(), Kind::SourceSave);
        let r = tool
            .execute(json!({
                "name": "坏的",
                "command": "exit 7",
                "cwd": std::env::temp_dir().to_string_lossy(),
            }))
            .await;
        assert!(!r.ok);
        assert!(r.content.contains("试跑失败"), "{}", r.content);
        assert!(st.read(|s| s.sources.is_empty()), "没存进去");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_working_command_is_stored_and_can_be_deleted() {
        let st = store();
        let tool = AssistantConfigTool::new(st.clone(), Kind::SourceSave);
        let r = tool
            .execute(json!({
                "name": "时间",
                "command": "date +%s",
                "cwd": std::env::temp_dir().to_string_lossy(),
                "every_minutes": 3,
            }))
            .await;
        assert!(r.ok, "{}", r.content);
        let s = st.read(|s| s.sources[0].clone());
        assert_eq!(s.every_minutes, 3);
        assert!(s.enabled);

        let del = AssistantConfigTool::new(st.clone(), Kind::SourceDelete);
        assert!(del.execute(json!({"name": "时间"})).await.ok);
        assert!(st.read(|s| s.sources.is_empty()));
    }

    #[tokio::test]
    async fn mode_and_pause_are_settable_in_one_call() {
        let st = store();
        let tool = AssistantConfigTool::new(st.clone(), Kind::Mode);
        let r = tool.execute(json!({"mode": "auto", "paused": true})).await;
        assert!(r.ok, "{}", r.content);
        assert!(st.read(|s| s.settings.mode == Mode::Auto && s.settings.paused));
        assert!(!tool.execute(json!({})).await.ok, "什么都不给要报错");
        assert!(!tool.execute(json!({"mode": "yolo"})).await.ok);
    }
}
