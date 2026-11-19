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

use crate::assistant::wait::{DEFAULT_TIMEOUT_MINUTES, MAX_TIMEOUT_MINUTES};
use crate::assistant::{AssistantStore, TicketStatus, Wait};
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
    /// `waiting` 时：等什么。来源还没解析（工具里才有状态可查），先攒着原始参数。
    wait: Option<WaitArgs>,
}

/// `status = waiting` 的参数。`source` 是观察源的名字（或 id），不给就用开单的那个来源。
#[derive(Debug, Clone, PartialEq, Eq)]
struct WaitArgs {
    note: String,
    keywords: Vec<String>,
    source: Option<String>,
    timeout_minutes: u64,
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
        "waiting" => TicketStatus::Waiting,
        other => {
            return Err(format!(
                "status 只能是 ready / needs_user / blocked / failed / dropped / waiting，收到 `{other}`"
            ));
        }
    };
    if status == TicketStatus::NeedsUser && question.is_none() {
        return Err("status = needs_user 时必须用 question 问一个具体问题".into());
    }
    let wait = if status == TicketStatus::Waiting {
        Some(parse_wait(args)?)
    } else {
        if args.get("wait_for").is_some() || args.get("match").is_some() {
            return Err("wait_for / match 只在 status = waiting 时给".into());
        }
        None
    };
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
        wait,
    })
}

fn parse_wait(args: &Value) -> Result<WaitArgs, String> {
    let note = args
        .get("wait_for")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("status = waiting 时必须用 wait_for 说清在等什么")?
        .to_string();
    let mut keywords: Vec<String> = Vec::new();
    for k in args
        .get("match")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(k) = k.as_str().map(str::trim).filter(|k| !k.is_empty()) else {
            continue;
        };
        if !keywords.iter().any(|x| x.eq_ignore_ascii_case(k)) {
            keywords.push(k.to_string());
        }
    }
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let timeout_minutes = match args.get("timeout_minutes") {
        None | Some(Value::Null) => DEFAULT_TIMEOUT_MINUTES,
        Some(v) => {
            let n = v
                .as_u64()
                .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
                .ok_or("timeout_minutes 要是一个分钟数")?;
            if n == 0 || n > MAX_TIMEOUT_MINUTES {
                return Err(format!(
                    "timeout_minutes 应在 1 到 {MAX_TIMEOUT_MINUTES}（7 天）之间"
                ));
            }
            n
        }
    };
    Ok(WaitArgs {
        note,
        keywords,
        source,
        timeout_minutes,
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
         * dropped —— 核实后发现不需要做（复现不了、已经有人做了、不是问题）；\n\
         * waiting —— 已经回复了对方、要等对方的下文才能继续（群里 @ 你的人、私聊你的人、PR 作者）。\
         用 wait_for 说清在等什么，用 match 给几个能认出那条回复的关键词（对方的名字、会话 id、单号）；\
         观察源里一出现匹配的行，这条会话会被直接叫醒并收到那几行，不用自己轮询。\
         到了 timeout_minutes（默认一天）还没等到也会被叫醒一次。\n\
         如实汇报：没做完不是 ready。summary 只写结果、实际跑过的检查和剩下的事。\n\
         除了 dropped 和 waiting，用户都会收到一条系统通知。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["ready", "needs_user", "blocked", "failed", "dropped", "waiting"]
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
                },
                "wait_for": {
                    "type": "string",
                    "description": "waiting 时必填：在等什么，一句话（如「张三对方案的答复」）"
                },
                "match": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "waiting 时给：观察源新行里含任一关键词（不分大小写）就算等到了。写能认出那条回复的东西：对方名字、会话 / 群 id、PR 号。不给 = 这个来源的任何新行都算，一般太宽"
                },
                "source": {
                    "type": "string",
                    "description": "waiting 时可选：盯哪个观察源（名字）。不给 = 开这张工单的那个来源"
                },
                "timeout_minutes": {
                    "type": "integer",
                    "description": "waiting 时可选：最多等多少分钟，默认 1440（一天），最多 7 天"
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
            wait,
        } = match parse(&args) {
            Ok(v) => v,
            Err(e) => return ToolResult::err(format!("assistant_report: {e}")),
        };
        let id = self.ticket_id.clone();
        let res = self.store.with(|s| {
            // 先解析来源：要用到全部状态（来源表），得在拿工单的可变引用之前做。
            let wait = match &wait {
                Some(w) => Some(resolve_wait(s, &id, w)?),
                None => None,
            };
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
            t.wait = wait;
            t.updated_at = now_ms();
            Ok((t.label(), t.wait.clone()))
        });
        match res {
            Ok(Ok((label, Some(w)))) => ToolResult::ok(format!(
                "已记录：{label} → 等回复（{}；最多等到 {}）。现在结束这一轮，不要再做别的事 —— \
                 等到了会把那几行直接送回这条会话。",
                if w.keywords.is_empty() {
                    "来源的任何新行".to_string()
                } else {
                    format!("关键词「{}」", w.keywords.join("」「"))
                },
                crate::schedules::fmt_local(w.until)
            )),
            Ok(Ok((label, None))) => ToolResult::ok(format!(
                "已记录：{label} → {}。现在结束这一轮，不要再做别的事。",
                status.label()
            )),
            Ok(Err(e)) => ToolResult::err(format!("assistant_report: {e}")),
            Err(e) => ToolResult::err(format!("assistant_report: 写盘失败：{e}")),
        }
    }
}

/// 把 `waiting` 的参数落成 [`Wait`]：来源按名字或 id 找；没指定就用开单的那个来源。
/// 用户在助理对话里直接指派的工单没有来源，必须指明。
fn resolve_wait(
    s: &crate::assistant::State,
    ticket_id: &str,
    w: &WaitArgs,
) -> Result<Wait, String> {
    let t = s.ticket(ticket_id).ok_or("这张工单已经不在了")?;
    let source_id = match &w.source {
        Some(key) => s
            .sources
            .iter()
            .find(|x| x.name.trim() == key.trim() || x.id == key.trim())
            .map(|x| x.id.clone())
            .ok_or_else(|| {
                let names: Vec<&str> = s.sources.iter().map(|x| x.name.as_str()).collect();
                format!(
                    "没有叫「{key}」的观察源。现在有的：{}",
                    if names.is_empty() {
                        "（没有）".to_string()
                    } else {
                        names.join("、")
                    }
                )
            })?,
        None if !t.source_id.is_empty() && s.sources.iter().any(|x| x.id == t.source_id) => {
            t.source_id.clone()
        }
        None => {
            return Err(
                "这张工单不是观察源开的（或那个来源已删），waiting 时要用 source 指明盯哪个观察源"
                    .into(),
            );
        }
    };
    Ok(Wait {
        source_id,
        keywords: w.keywords.clone(),
        until: now_ms() + w.timeout_minutes * 60_000,
        note: w.note.clone(),
    })
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

    /// `waiting` 要说清等什么；关键词去空白去重；超时有默认、有上限；
    /// 其它状态不许带 wait_for / match。
    #[test]
    fn waiting_needs_a_note_and_bounds_its_timeout() {
        assert!(
            parse(&json!({"status": "waiting", "summary": "回了"}))
                .err()
                .unwrap()
                .contains("wait_for")
        );
        let r = parse(&json!({
            "status": "waiting", "summary": "回了", "wait_for": "张三的答复",
            "match": [" 张三 ", "张三", "", "cid=42"], "timeout_minutes": 30
        }))
        .unwrap();
        let w = r.wait.unwrap();
        assert_eq!(w.note, "张三的答复");
        assert_eq!(w.keywords, vec!["张三", "cid=42"]);
        assert_eq!(w.timeout_minutes, 30);
        assert!(w.source.is_none());

        let r = parse(&json!({"status": "waiting", "summary": "x", "wait_for": "y"})).unwrap();
        assert_eq!(r.wait.unwrap().timeout_minutes, DEFAULT_TIMEOUT_MINUTES);
        assert!(
            parse(
                &json!({"status": "waiting", "summary": "x", "wait_for": "y", "timeout_minutes": 0})
            )
            .is_err()
        );
        assert!(
            parse(&json!({
                "status": "waiting", "summary": "x", "wait_for": "y",
                "timeout_minutes": MAX_TIMEOUT_MINUTES + 1
            }))
            .is_err()
        );
        assert!(parse(&json!({"status": "ready", "summary": "x", "wait_for": "y"})).is_err());
    }

    /// 来源：不给就用开单的那个；助理直接指派的工单没有来源，必须指明；指了不存在的要报错。
    #[tokio::test]
    async fn waiting_resolves_the_source_or_asks_for_one() {
        let dir = std::env::temp_dir().join(format!(
            "carter-wait-src-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let store = AssistantStore::open(dir.join("assistant.json"));
        let source = crate::assistant::tests::source();
        let (from_source, assigned) = store
            .with(|s| {
                s.sources.push(source.clone());
                (
                    s.open_ticket(&source, "r", "t", "x", vec![]),
                    s.assign_ticket("/tmp", "u", "y"),
                )
            })
            .unwrap();

        let r = AssistantReportTool::new(store.clone(), from_source.clone())
            .execute(json!({"status": "waiting", "summary": "回了", "wait_for": "答复", "match": ["张三"]}))
            .await;
        assert!(r.ok, "{}", r.content);
        assert!(r.content.contains("等回复"), "{}", r.content);
        store.read(|s| {
            let t = s.ticket(&from_source).unwrap();
            assert_eq!(t.status, TicketStatus::Waiting);
            let w = t.wait.as_ref().unwrap();
            assert_eq!(w.source_id, source.id);
            assert!(w.until > now_ms());
        });

        let tool = AssistantReportTool::new(store.clone(), assigned.clone());
        let r = tool
            .execute(json!({"status": "waiting", "summary": "回了", "wait_for": "答复"}))
            .await;
        assert!(!r.ok);
        assert!(r.content.contains("source"), "{}", r.content);
        let r = tool
            .execute(json!({"status": "waiting", "summary": "回了", "wait_for": "答复", "source": "不存在"}))
            .await;
        assert!(!r.ok);
        assert!(
            r.content.contains("GitHub 通知"),
            "要列出现有的来源：{}",
            r.content
        );
        let r = tool
            .execute(json!({"status": "waiting", "summary": "回了", "wait_for": "答复", "source": "GitHub 通知"}))
            .await;
        assert!(r.ok, "{}", r.content);
        assert_eq!(
            store.read(|s| s
                .ticket(&assigned)
                .and_then(|t| t.wait.clone())
                .map(|w| w.source_id)),
            Some(source.id.clone())
        );
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
