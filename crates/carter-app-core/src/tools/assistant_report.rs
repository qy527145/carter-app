//! `assistant_report`：执行会话向常驻助理汇报一张工单的结局。
//!
//! 与 `goal_update` 同一个理由：结局要**可判定**。从正文里捞「做完了」靠不住 ——
//! 模型会复述、会写「还没做完」。`status` 是枚举，助理据此把工单挪到「待查看」
//! 「等你拍板」「受阻」……并决定要不要发系统通知。
//!
//! 只在属于某张工单的会话里挂（见 `tools/mod.rs`）。普通对话看不到它。
//! 挂在子 agent 快照之后：子 agent 一句 ready 就把整张工单交出去，那不是它该做的决定。

use std::sync::Arc;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::assistant::{AssistantStore, TicketStatus};
use crate::protocol::now_ms;

pub struct AssistantReportTool {
    store: Arc<AssistantStore>,
    ticket_id: String,
}

impl AssistantReportTool {
    pub fn new(store: Arc<AssistantStore>, ticket_id: String) -> Self {
        Self { store, ticket_id }
    }
}

/// 候选答案最多几个。再多就不是「拍板」了，是让用户读一份方案对比。
const MAX_OPTIONS: usize = 4;

struct Report {
    status: TicketStatus,
    summary: String,
    question: Option<String>,
    options: Vec<String>,
}

fn parse(args: &Value) -> Result<Report, String> {
    let status = args
        .get("status")
        .and_then(Value::as_str)
        .ok_or("缺少 status")?
        .trim()
        .to_ascii_lowercase();
    let summary = args
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("缺少 summary：一两句话说清结果")?
        .to_string();
    let question = args
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let status = match status.as_str() {
        "ready" => TicketStatus::Ready,
        "needs_user" | "needs-user" => TicketStatus::NeedsUser,
        "blocked" => TicketStatus::Blocked,
        "failed" => TicketStatus::Failed,
        "dropped" => TicketStatus::Dropped,
        other => {
            return Err(format!(
                "status 只能是 ready / needs_user / blocked / failed / dropped，收到 `{other}`"
            ));
        }
    };
    if status == TicketStatus::NeedsUser && question.is_none() {
        return Err("status = needs_user 时必须用 question 问一个具体问题".into());
    }
    let mut options: Vec<String> = Vec::new();
    for o in args
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(o) = o.as_str().map(str::trim).filter(|o| !o.is_empty()) else {
            continue;
        };
        if !options.iter().any(|x| x == o) {
            options.push(o.to_string());
        }
    }
    if !options.is_empty() && status != TicketStatus::NeedsUser {
        return Err("options 只在 status = needs_user 时给".into());
    }
    if options.len() == 1 || options.len() > MAX_OPTIONS {
        return Err(format!("options 给 2 到 {MAX_OPTIONS} 个，或者不给"));
    }
    Ok(Report {
        status,
        summary,
        question,
        options,
    })
}

#[async_trait::async_trait]
impl Tool for AssistantReportTool {
    fn name(&self) -> &str {
        "assistant_report"
    }

    fn description(&self) -> &str {
        "向常驻助理汇报这张工单的结局。这一轮结束前必须调用一次。\n\
         * ready —— 有结果可看了（改好并验证过、查清楚了、草稿写好了）；\n\
         * needs_user —— 必须用户决定才能往下走，用 question 问**一个**具体问题；有几种明确走法时用 options 列出来，用户点一下就能答；\n\
         * blocked —— 缺权限、缺依赖、外部系统不通，做不下去；\n\
         * failed —— 试过了，做不成；\n\
         * dropped —— 核实后发现不需要做（复现不了、已经有人做了、不是问题）。\n\
         如实汇报：没做完不是 ready。summary 只写结果、实际跑过的检查和剩下的事。\n\
         除了 dropped，用户都会收到一条系统通知。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["ready", "needs_user", "blocked", "failed", "dropped"]
                },
                "summary": {
                    "type": "string",
                    "description": "一两句话：结果是什么、在哪看、验证过什么、还剩什么"
                },
                "question": {
                    "type": "string",
                    "description": "needs_user 时必填：要用户回答的那一个问题"
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "needs_user 时可选：2 到 4 个候选答案，每个一句话、自成一体（如「用 A：改接口，兼容旧调用」）。用户点哪个就原样送回给你；用户也可以不选、自己写"
                }
            },
            "required": ["status", "summary"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Report {
            status,
            summary,
            question,
            options,
        } = match parse(&args) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("assistant_report: {e}")),
        };
        let id = self.ticket_id.clone();
        let res = self.store.with(|s| {
            let Some(t) = s.ticket_mut(&id) else {
                return Err("这张工单已经不在了".to_string());
            };
            if t.status == TicketStatus::Stopped {
                return Err("这张工单已经被用户叫停，不用再汇报".to_string());
            }
            t.status = status;
            t.summary = Some(summary.clone());
            t.question = question.clone();
            t.options = options.clone();
            t.updated_at = now_ms();
            Ok(t.label())
        });
        match res {
            Ok(Ok(label)) => ToolResult::ok(format!(
                "已记录：{label} → {}。现在结束这一轮，不要再做别的事。",
                status.label()
            )),
            Ok(Err(e)) => ToolResult::err(format!("assistant_report: {e}")),
            Err(e) => ToolResult::err(format!("assistant_report: 写盘失败：{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_user_requires_a_question() {
        assert!(parse(&json!({"status": "needs_user", "summary": "x"})).is_err());
        let r =
            parse(&json!({"status": "needs_user", "summary": "x", "question": "用 A 还是 B？"}))
                .unwrap();
        assert_eq!(r.status, TicketStatus::NeedsUser);
        assert_eq!(r.question.as_deref(), Some("用 A 还是 B？"));
        assert!(r.options.is_empty());
    }

    #[test]
    fn options_are_two_to_four_and_only_with_needs_user() {
        let ask = |opts: Value| {
            parse(
                &json!({"status": "needs_user", "summary": "x", "question": "q", "options": opts}),
            )
        };
        let r = ask(json!(["用 A", " 用 B ", "用 A", ""])).unwrap();
        assert_eq!(r.options, vec!["用 A", "用 B"], "去空白、去重、丢空的");
        assert!(ask(json!(["只有一个"])).is_err());
        assert!(ask(json!(["1", "2", "3", "4", "5"])).is_err());
        assert!(parse(&json!({"status": "ready", "summary": "x", "options": ["a", "b"]})).is_err());
    }

    #[test]
    fn a_summary_is_required_and_unknown_statuses_are_refused() {
        assert!(
            parse(&json!({"status": "ready"}))
                .err()
                .unwrap()
                .contains("summary")
        );
        assert!(
            parse(&json!({"status": "done", "summary": "x"}))
                .err()
                .unwrap()
                .contains("done")
        );
        assert_eq!(
            parse(&json!({"status": "DROPPED", "summary": "x"}))
                .unwrap()
                .status,
            TicketStatus::Dropped
        );
    }

    #[tokio::test]
    async fn a_report_moves_the_ticket_and_a_stopped_one_refuses() {
        let dir =
            std::env::temp_dir().join(format!("carter-report-{}-{}", std::process::id(), now_ms()));
        let store = AssistantStore::open(dir.join("assistant.json"));
        let id = store
            .with(|s| s.open_ticket(&crate::assistant::tests::source(), "r", "t", "x", vec![]))
            .unwrap();
        let tool = AssistantReportTool::new(store.clone(), id.clone());

        let r = tool
            .execute(json!({"status": "ready", "summary": "改好了"}))
            .await;
        assert!(r.ok, "{}", r.content);
        store.read(|s| {
            let t = s.ticket(&id).unwrap();
            assert_eq!(t.status, TicketStatus::Ready);
            assert_eq!(t.summary.as_deref(), Some("改好了"));
        });

        store
            .with(|s| s.ticket_mut(&id).unwrap().status = TicketStatus::Stopped)
            .unwrap();
        let r = tool
            .execute(json!({"status": "ready", "summary": "x"}))
            .await;
        assert!(!r.ok);
    }
}
