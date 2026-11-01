//! `schedule`：让模型通过对话创建 / 查看 / 删除定时任务。
//!
//! ## 为什么分成四件事、而不是一个 `schedule` 带 `action` 参数
//!
//! 每个动作的参数差别很大（创建要时间与正文，删除只要一个名字），塞进同一个
//! schema 之后模型经常把不相关的字段也填上，而省略掉的必填字段要到执行时才报错。
//! 分成 `schedule_create` / `schedule_list` / `schedule_update` / `schedule_delete`
//! 之后，每个 schema 都只说它自己那点事，工具名字本身就是一次明确的承诺。
//!
//! ## 时间怎么给
//!
//! 一律用**本机时间**字符串（`2026-03-01 09:30`），不用 UTC、也不用相对秒数。
//! 用户说的是「每天早上九点」，模型把这句话翻成结构化字段；至于「现在是几点、
//! 明天几点」，那是 [`crate::schedules`] 的事。让模型算时间，错一小时是常态。
//!
//! ## 为什么不在 `agent` 的子 agent 里给你
//!
//! 子 agent 是并行干活的，四个子 agent 各建一条定时任务，用户在设置页里会看到
//! 四条重复的，还得自己去重。创建定时任务是主对话的事（它知道整段对话的意图），
//! 与 `goal_update` 同一个理由，见 `tools/mod.rs` 里快照的取法。

use std::sync::Arc;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::schedules::{self, Schedule, ScheduleKind, ScheduleStore};

/// 创建可用 `at`（本机时间字符串）或 `every_minutes` 或 `daily_at`（`HH:MM`）。
fn parse_kind(args: &Value) -> Result<ScheduleKind, String> {
    let at = args.get("at").and_then(Value::as_str).map(str::trim);
    let every = args.get("every_minutes").and_then(Value::as_u64);
    let daily = args.get("daily_at").and_then(Value::as_str).map(str::trim);

    let given = [at.is_some(), every.is_some(), daily.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if given == 0 {
        return Err(
            "缺少时间：`at`（一次性，写 `2026-03-01 09:30`）／`every_minutes`（每隔多少分钟）／\
             `daily_at`（每天，写 `09:00`）三选一"
                .into(),
        );
    }
    if given > 1 {
        return Err("`at` / `every_minutes` / `daily_at` 只能给一个 —— 给多个没法判断你要哪个".into());
    }

    if let Some(at) = at {
        return Ok(ScheduleKind::Once {
            at_ms: schedules::parse_local(at)?,
        });
    }
    if let Some(m) = every {
        return Ok(ScheduleKind::Interval { every_minutes: m });
    }
    let daily = daily.expect("前面已经确认过它存在");
    let (h, m) = daily
        .split_once(':')
        .ok_or_else(|| format!("`daily_at` 要写成 `09:00` 这种 24 小时制，收到 `{daily}`"))?;
    let hour: u32 = h
        .trim()
        .parse()
        .map_err(|_| format!("`daily_at` 里的小时认不出来：`{daily}`"))?;
    let minute: u32 = m
        .trim()
        .parse()
        .map_err(|_| format!("`daily_at` 里的分钟认不出来：`{daily}`"))?;
    Ok(ScheduleKind::Daily { hour, minute })
}

/// 一条任务给人（和模型）看的一行。
pub fn describe(s: &Schedule) -> String {
    let mut line = format!(
        "- [{}] {}（{}）· {}\n  下次：{}",
        if s.enabled { "启用" } else { "已停用" },
        s.name,
        s.kind.label(),
        s.cwd,
        if s.enabled {
            schedules::fmt_local(s.next_at)
        } else {
            "—".to_string()
        },
    );
    if let Some(at) = s.last_fired_at {
        line.push_str(&format!(
            "\n  上次：{}（已跑 {} 次）",
            schedules::fmt_local(at),
            s.runs
        ));
    }
    if let Some(err) = &s.last_error {
        line.push_str(&format!("\n  ⚠ 上次失败：{err}"));
    }
    line.push_str(&format!("\n  id: {}", s.id));
    line
}

pub struct ScheduleTool {
    store: Arc<ScheduleStore>,
    /// 会话的工作目录：新建任务时默认在这里跑。
    cwd: String,
}

impl ScheduleTool {
    pub fn new(store: Arc<ScheduleStore>, cwd: String) -> Self {
        Self { store, cwd }
    }
}

#[async_trait::async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }

    fn description(&self) -> &str {
        "管理定时任务：到点自动**新开一条对话**并发出你写的那段话。\n\
         \n\
         动作（`action`）：\n\
         * `create` —— 需要 `name`、`prompt`，以及时间三选一：\n\
           `at`（一次性，`2026-03-01 09:30` 本机时间）、\n\
           `every_minutes`（每隔 N 分钟）、\n\
           `daily_at`（每天，`09:00`）。可选 `cwd`、`model`。\n\
         * `list` —— 列出全部定时任务与下次触发时间。\n\
         * `update` —— 用 `id`（或 `name`）指定一条，改 `enabled` / `name` / `prompt` / 时间。\n\
         * `delete` —— 用 `id`（或 `name`）删掉。\n\
         \n\
         什么时候该用：用户说「每天早上…」「过一会儿…」「每周一…」这类**将来才做**的事。\n\
         什么时候不该用：用户说「现在就跑一下」—— 那是直接做，不是定时任务。\n\
         \n\
         `prompt` 是到点时原样发出去的那句话。它必须**自给自足**：定时会话看不到\n\
         这段对话的历史，所以要写清楚做什么、看哪里、产出成什么样。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "update", "delete"]
                },
                "id": {
                    "type": "string",
                    "description": "update / delete 时用它指定任务（也可以直接给 name）"
                },
                "name": {
                    "type": "string",
                    "description": "短标题，也用来在 update / delete 里指定任务"
                },
                "prompt": {
                    "type": "string",
                    "description": "到点发出去的正文。写清楚做什么、看哪里、要什么结果"
                },
                "at": {
                    "type": "string",
                    "description": "一次性任务的本机时间，如 `2026-03-01 09:30`"
                },
                "every_minutes": {
                    "type": "integer",
                    "description": "每隔多少分钟跑一次（1 到 43200）"
                },
                "daily_at": {
                    "type": "string",
                    "description": "每天的触发时间（24 小时制，如 `09:00`）"
                },
                "cwd": {
                    "type": "string",
                    "description": "触发时在哪个目录开新对话。默认是当前会话的目录"
                },
                "model": {
                    "type": "string",
                    "description": "`渠道/显示名`。默认用配置里当前的默认模型"
                },
                "enabled": {
                    "type": "boolean",
                    "description": "update 时用它暂停 / 恢复"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match action.as_str() {
            "create" => self.create(&args),
            "list" => self.list(),
            "update" => self.update(&args),
            "delete" => self.delete(&args),
            other => ToolResult::err(format!(
                "action 只能是 create / list / update / delete，收到 `{other}`"
            )),
        }
    }
}

impl ScheduleTool {
    fn create(&self, args: &Value) -> ToolResult {
        let name = str_arg(args, "name");
        let prompt = str_arg(args, "prompt");
        let (name, prompt) = match (name, prompt) {
            (Some(n), Some(p)) => (n, p),
            (None, _) => return ToolResult::err("create 需要 name（短标题）"),
            (_, None) => {
                return ToolResult::err("create 需要 prompt（到点时原样发出去的那句话）")
            }
        };
        let kind = match parse_kind(args) {
            Ok(k) => k,
            Err(e) => return ToolResult::err(e),
        };
        let cwd = str_arg(args, "cwd").unwrap_or_else(|| self.cwd.clone());
        let model = str_arg(args, "model");

        let mut s = match Schedule::new(&name, &prompt, kind, cwd, model) {
            Ok(s) => s,
            Err(e) => return ToolResult::err(e),
        };
        // 同名的旧任务不删：名字不是唯一键，删掉用户一条精心配过的任务
        // 比多一条重复的坏得多。界面上两行挨着，用户自己看得见。
        if let Err(e) = self.store.upsert(s.clone()) {
            return ToolResult::err(format!("存不下来：{e}"));
        }
        s.runs = 0;
        ToolResult::ok(format!(
            "已创建定时任务「{}」，{}，下次触发：{}。\n\
             每次触发会**新开一条对话**（标题用任务名），在那里执行 prompt。",
            s.name,
            s.kind.label(),
            schedules::fmt_local(s.next_at),
        ))
    }

    fn list(&self) -> ToolResult {
        let all = self.store.list();
        if all.is_empty() {
            return ToolResult::ok("当前没有定时任务。".to_string());
        }
        let body: Vec<String> = all.iter().map(describe).collect();
        ToolResult::ok(format!(
            "共 {} 条定时任务：\n{}",
            all.len(),
            body.join("\n")
        ))
    }

    fn update(&self, args: &Value) -> ToolResult {
        let key = match str_arg(args, "id").or_else(|| str_arg(args, "name")) {
            Some(k) => k,
            None => return ToolResult::err("update 需要 id 或 name 指明是哪一条"),
        };
        let Some(mut s) = self.store.find(&key) else {
            return ToolResult::err(format!("找不到定时任务 `{key}`"));
        };

        let mut changed = Vec::new();
        if let Some(name) = str_arg(args, "name") {
            if name != s.name {
                s.name = name;
                changed.push("name");
            }
        }
        if let Some(prompt) = str_arg(args, "prompt") {
            s.prompt = prompt;
            changed.push("prompt");
        }
        if let Some(enabled) = args.get("enabled").and_then(Value::as_bool) {
            s.enabled = enabled;
            changed.push("enabled");
        }
        // 给了任何一个时间字段就重算节奏。`parse_kind` 在三个都没给时会报错，
        // 所以先判断一下再调。
        let has_time = ["at", "every_minutes", "daily_at"]
            .iter()
            .any(|k| args.get(*k).is_some_and(|v| !v.is_null()));
        if has_time {
            match parse_kind(args) {
                Ok(kind) => {
                    s.kind = kind;
                    s.reschedule();
                    changed.push("时间");
                }
                Err(e) => return ToolResult::err(e),
            }
        }
        if changed.is_empty() {
            return ToolResult::err("没有给出任何要改的字段（name / prompt / enabled / 时间）");
        }
        // 改了节奏、或停用了一阵之后重新打开：下一次已经过期就按现在重算，
        // 否则它会在下一次轮询里立刻触发一次 —— 「恢复」不该等于「立刻补跑」。
        if s.enabled && s.next_at <= crate::protocol::now_ms() {
            s.reschedule();
        }
        if let Err(e) = self.store.upsert(s.clone()) {
            return ToolResult::err(format!("存不下来：{e}"));
        }
        let now = self.store.get(&s.id).unwrap_or(s);
        ToolResult::ok(format!(
            "已更新「{}」（改了 {}）。\n{}",
            now.name,
            changed.join("、"),
            describe(&now)
        ))
    }

    fn delete(&self, args: &Value) -> ToolResult {
        let key = match str_arg(args, "id").or_else(|| str_arg(args, "name")) {
            Some(k) => k,
            None => return ToolResult::err("delete 需要 id 或 name 指明删哪一条"),
        };
        let Some(s) = self.store.find(&key) else {
            return ToolResult::err(format!("找不到定时任务 `{key}`"));
        };
        match self.store.remove(&s.id) {
            Ok(true) => ToolResult::ok(format!(
                "已删除定时任务「{}」。已经跑出来的会话不会跟着删。",
                s.name
            )),
            Ok(false) => ToolResult::err(format!("定时任务「{}」已经不存在了", s.name)),
            Err(e) => ToolResult::err(format!("删不掉：{e}")),
        }
    }
}

fn str_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::now_ms;

    fn tmp_store(tag: &str) -> Arc<ScheduleStore> {
        let path = std::env::temp_dir().join(format!(
            "carter-schedtool-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        ScheduleStore::open(path)
    }

    fn tool(tag: &str) -> ScheduleTool {
        ScheduleTool::new(tmp_store(tag), "/tmp/project".into())
    }

    #[tokio::test]
    async fn create_then_list_shows_the_task() {
        let t = tool("create");
        let r = t
            .execute(json!({ "action": "create", "name": "早报", "prompt": "看一遍 CI",
                             "daily_at": "09:00" }))
            .await;
        assert!(r.ok, "{}", r.content);
        assert!(r.content.contains("早报"));

        let list = t.execute(json!({ "action": "list" })).await;
        assert!(list.content.contains("早报"));
        assert!(list.content.contains("每天 09:00"));
        assert!(list.content.contains("/tmp/project"), "默认用会话目录");
    }

    #[tokio::test]
    async fn create_rejects_zero_or_two_time_fields() {
        let t = tool("badtime");
        let none = t
            .execute(json!({ "action": "create", "name": "x", "prompt": "y" }))
            .await;
        assert!(none.ok == false);
        assert!(none.content.contains("三选一"), "{}", none.content);

        let two = t
            .execute(json!({ "action": "create", "name": "x", "prompt": "y",
                             "daily_at": "09:00", "every_minutes": 30 }))
            .await;
        assert!(two.ok == false);
        assert!(two.content.contains("只能给一个"), "{}", two.content);
    }

    #[tokio::test]
    async fn create_rejects_a_time_in_the_past() {
        let t = tool("past");
        let r = t
            .execute(json!({ "action": "create", "name": "x", "prompt": "y",
                             "at": "2020-01-01 09:00" }))
            .await;
        assert!(r.ok == false);
        assert!(r.content.contains("过去了"), "{}", r.content);
    }

    #[tokio::test]
    async fn update_changes_only_what_was_given() {
        let t = tool("update");
        t.execute(json!({ "action": "create", "name": "早报", "prompt": "旧正文",
                           "every_minutes": 60, "model": "ws/sonnet" }))
            .await;

        let r = t
            .execute(json!({ "action": "update", "name": "早报", "prompt": "新正文",
                             "enabled": false }))
            .await;
        assert!(r.ok, "{}", r.content);

        let s = t.store.find("早报").unwrap();
        assert_eq!(s.prompt, "新正文");
        assert!(!s.enabled);
        assert_eq!(s.model.as_deref(), Some("ws/sonnet"), "没提到的字段要留着");
        assert_eq!(s.kind, ScheduleKind::Interval { every_minutes: 60 });
    }

    #[tokio::test]
    async fn update_with_nothing_to_change_says_so() {
        let t = tool("noop");
        t.execute(json!({ "action": "create", "name": "a", "prompt": "b",
                           "every_minutes": 5 }))
            .await;
        let r = t.execute(json!({ "action": "update", "name": "a" })).await;
        assert!(r.ok == false);
        assert!(r.content.contains("没有给出"), "{}", r.content);
    }

    #[tokio::test]
    async fn delete_by_name_then_it_is_gone() {
        let t = tool("delete");
        t.execute(json!({ "action": "create", "name": "一次性", "prompt": "b",
                           "every_minutes": 5 }))
            .await;
        let ok = t.execute(json!({ "action": "delete", "name": "一次性" })).await;
        assert!(ok.ok, "{}", ok.content);
        let again = t.execute(json!({ "action": "delete", "name": "一次性" })).await;
        assert!(again.ok == false, "删第二次要说找不到");
    }

    #[tokio::test]
    async fn an_unknown_action_explains_the_options() {
        let t = tool("badaction");
        let r = t.execute(json!({ "action": "run" })).await;
        assert!(r.ok == false);
        assert!(r.content.contains("create / list / update / delete"));
    }

    #[test]
    fn the_schema_only_requires_action() {
        let t = ScheduleTool::new(tmp_store("schema"), "/tmp".into());
        let p = t.parameters();
        assert_eq!(p["required"][0], json!("action"));
        assert_eq!(t.name(), "schedule");
    }
}
