//! 助理对话里的工单工具：在对话里回答工单的问题、给它收尾。
//!
//! ## 为什么必须在这里
//!
//! 只有配置工具时，这条对话是**单向**的：助理能把汇报推进来，用户回一句「用 A 方案」
//! 却什么也不会发生 —— 那句话只有模型自己看得见，到不了工单的执行会话。原型里这是
//! 通的：人在工单订阅的线程里回话，消息直接送到工人手上（Subscription fan-out）。
//! 桌面应用没有那些线程，所以这条对话本身必须能投递。
//!
//! 与面板上的按钮走同一套逻辑（`runtime::ticket_action`），不是另写一份：两处各写一遍
//! 的话，「叫停」迟早会在其中一处漏掉打断正在跑的那一轮。
//!
//! ## 只在助理对话里挂
//!
//! 工单的执行会话自己有 `assistant_report`，不需要、也不该能替别的工单投递消息。
//! 见 `tools/mod.rs` 的装配顺序。

use std::sync::Arc;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::assistant::runtime::{TicketAction, approve, dismiss, ticket_action};
use crate::assistant::{AssistantStore, TicketStatus};
use crate::shell::AppShell;

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Reply,
    Act,
    /// 观察模式下「本来会接」的提议：批准开工或忽略。
    Proposal,
}

pub struct AssistantTicketTool {
    shell: Arc<AppShell>,
    kind: Kind,
}

impl AssistantTicketTool {
    pub fn new(shell: Arc<AppShell>, kind: Kind) -> Self {
        Self { shell, kind }
    }
}

/// 按 `#编号`、编号或 id 找工单。模型手里拿到的是编号，所以先认编号。
fn resolve(store: &AssistantStore, key: &str) -> Option<(String, String)> {
    let key = key.trim();
    let number = key.trim_start_matches('#').parse::<u32>().ok();
    store.read(|s| {
        let t = match number {
            Some(n) => s.tickets.iter().find(|t| t.number == n),
            None => s
                .tickets
                .iter()
                .find(|t| t.id == key)
                .or_else(|| s.tickets.iter().find(|t| t.title.trim() == key)),
        }?;
        Some((t.id.clone(), t.label()))
    })
}

fn field(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[async_trait::async_trait]
impl Tool for AssistantTicketTool {
    fn name(&self) -> &str {
        match self.kind {
            Kind::Reply => "assistant_ticket_reply",
            Kind::Act => "assistant_ticket_act",
            Kind::Proposal => "assistant_proposal",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            Kind::Reply => {
                "把用户对一张工单的回答或补充要求送进它的执行会话，让它接着做。\n\
                 用户在对话里回答工单的问题时**必须**用这个工具，否则那句话只会留在这条对话里，\
                 工单永远停在等你拍板。ticket 用编号（`#3` 或 `3`）。\n\
                 只转述用户说的内容，不要替用户做技术决定、也不要加实现方案。"
            }
            Kind::Act => {
                "对一张工单做收尾动作：\n\
                 - retry —— 从停下的地方再来一次（受阻、失败、跑完未汇报时用）；\n\
                 - done —— 用户说看过了 / 不用管了，结束这张工单；\n\
                 - stop —— 叫停，打断正在跑的那一轮，之后不再收任何事件。\n\
                 用户没有明确要求时不要调用。"
            }
            Kind::Proposal => {
                "处理一条「本来会接」的提议（观察模式下命中规则、等用户拍板的那种）：\n\
                 - approve —— 用户同意，开一张工单去做；\n\
                 - dismiss —— 用户说不用，忽略它。\n\
                 id 是汇报里「提议编号」后面那串。用户没表态时不要替用户决定。"
            }
        }
    }

    fn parameters(&self) -> Value {
        match self.kind {
            Kind::Reply => json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string", "description": "工单编号，如 `#3` 或 `3`" },
                    "text": { "type": "string", "description": "用户的原话，或对它的一句忠实转述" }
                },
                "required": ["ticket", "text"]
            }),
            Kind::Act => json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string", "description": "工单编号，如 `#3` 或 `3`" },
                    "action": { "type": "string", "enum": ["retry", "done", "stop"] }
                },
                "required": ["ticket", "action"]
            }),
            Kind::Proposal => json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "提议编号（`dec-...`）" },
                    "action": { "type": "string", "enum": ["approve", "dismiss"] }
                },
                "required": ["id", "action"]
            }),
        }
    }

    async fn execute(&self, args: Value) -> ToolResult {
        if let Kind::Proposal = self.kind {
            return self.proposal(args).await;
        }
        let Some(key) = field(&args, "ticket") else {
            return ToolResult::err(format!("{}: 缺少 ticket", self.name()));
        };
        let Some((id, label)) = resolve(&self.shell.assistant, &key) else {
            let live: Vec<String> = self
                .shell
                .assistant
                .read(|s| {
                    s.tickets
                        .iter()
                        .filter(|t| !t.status.ended())
                        .map(|t| format!("{}（{}）", t.label(), t.status.label()))
                        .collect()
                });
            return ToolResult::err(format!(
                "{}: 找不到工单 `{key}`。现在有的：{}",
                self.name(),
                if live.is_empty() {
                    "（没有进行中的工单）".to_string()
                } else {
                    live.join("、")
                }
            ));
        };

        let action = match self.kind {
            Kind::Reply => {
                let Some(text) = field(&args, "text") else {
                    return ToolResult::err("assistant_ticket_reply: 缺少 text");
                };
                TicketAction::Reply { text }
            }
            Kind::Proposal => unreachable!("上面已经分流"),
            Kind::Act => {
                let Some(action) = field(&args, "action") else {
                    return ToolResult::err("assistant_ticket_act: 缺少 action");
                };
                match action.as_str() {
                    "retry" => TicketAction::Retry,
                    "done" => TicketAction::Done,
                    "stop" => TicketAction::Stop,
                    other => {
                        return ToolResult::err(format!(
                            "assistant_ticket_act: action 只能是 retry / done / stop，收到 `{other}`"
                        ));
                    }
                }
            }
        };

        match ticket_action(&self.shell, &id, action).await {
            Ok(()) => {
                let (status, question) = self
                    .shell
                    .assistant
                    .read(|s| s.ticket(&id).map(|t| (t.status, t.question.clone())))
                    .unwrap_or((TicketStatus::Queued, None));
                let mut out = format!("已处理 {label}，现在是{}。", status.label());
                if let Some(q) = question {
                    out.push_str(&format!("它还等着用户回答：{q}"));
                }
                ToolResult::ok(out)
            }
            Err(e) => ToolResult::err(format!("{}: {e}", self.name())),
        }
    }
}

/// 只读地看一眼现状。汇报推起来的那一轮只有它，见 `session.rs::turn_tools`。
pub struct AssistantStatusTool {
    store: Arc<AssistantStore>,
}

impl AssistantStatusTool {
    pub fn new(store: Arc<AssistantStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl Tool for AssistantStatusTool {
    fn name(&self) -> &str {
        "assistant_status"
    }

    fn description(&self) -> &str {
        "查看助理现状：模式、观察源、规则、进行中的工单。只读。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::ok(crate::assistant::status_text(&self.store))
    }
}

impl AssistantTicketTool {
    async fn proposal(&self, args: Value) -> ToolResult {
        let (Some(id), Some(action)) = (field(&args, "id"), field(&args, "action")) else {
            return ToolResult::err("assistant_proposal: 缺少 id 或 action");
        };
        match action.as_str() {
            "approve" => match approve(&self.shell, &id).await {
                Ok(ticket_id) => {
                    let label = self
                        .shell
                        .assistant
                        .read(|s| s.ticket(&ticket_id).map(|t| t.label()))
                        .unwrap_or(ticket_id);
                    ToolResult::ok(format!("已开工：{label}。做完会汇报回这里。"))
                }
                Err(e) => ToolResult::err(format!("assistant_proposal: {e}")),
            },
            "dismiss" => match dismiss(&self.shell, &id) {
                Ok(()) => ToolResult::ok("已忽略这条提议。"),
                Err(e) => ToolResult::err(format!("assistant_proposal: {e}")),
            },
            other => ToolResult::err(format!(
                "assistant_proposal: action 只能是 approve / dismiss，收到 `{other}`"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_found_by_number_or_title() {
        let dir = std::env::temp_dir().join(format!(
            "carter-tickettool-{}-{}",
            std::process::id(),
            crate::protocol::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AssistantStore::open(dir.join("assistant.json"));
        store
            .with(|s| {
                s.open_ticket(&crate::assistant::tests::source(), "r", "修主干 CI", "x", vec![])
            })
            .unwrap();
        let (id, label) = resolve(&store, "#1").expect("按编号");
        assert_eq!(label, "#1 修主干 CI");
        assert_eq!(resolve(&store, "1").map(|x| x.0), Some(id.clone()), "不带 # 也认");
        assert_eq!(resolve(&store, "修主干 CI").map(|x| x.0), Some(id));
        assert!(resolve(&store, "#9").is_none());
    }
}
