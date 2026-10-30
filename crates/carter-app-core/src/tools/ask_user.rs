//! `ask_user_question`：一次性问一组问题，支持多选。
//!
//! ## 为什么不用 carter 那个
//!
//! carter 的 `AskUserQuestionTool` 的 schema 是 `{ question: string, options: string[] }`
//! —— **一个问题、一个答案**。想收集三件事就得调三次工具、来回三轮，而每一轮都要
//! 走一遍完整的模型请求。而且 `options` 只有标签，没有说明，用户看到「选项 A」不知道
//! 它意味着什么。
//!
//! 这里改成一次问一组：每个问题可以带说明的选项、可以多选、可以带一个短标签。
//! 用户填完一次性提交，模型拿到的是**一份完整答案**而不是挤牙膏的结果。
//!
//! ## 兼容旧的调用形态
//!
//! 模型见过的语料里 `ask_user_question` 大多长成 `{question, options:[string]}`。
//! 只认新 schema 的话，那些调用会得到一个「参数错误」，而模型多半会**重试同样的形状**
//! —— 白烧几轮。所以这里两种形态都收。

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use carter::agent::CancelToken;
use carter::tools::{Tool, ToolResult};

use crate::protocol::AskQuestion;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// 工具要向界面提问，就得有个把请求送出去、再等回复的通道。
///
/// 与 carter 的 `UiEvent::AskUser` 是同一个套路（oneshot 发送端放在事件里，
/// 收事件的一方取出来存住），只是事件类型是我们自己的 —— 因为 `UiEvent` 是
/// carter 的封闭枚举，加不了变体。
pub enum AskRequest {
    Questions {
        id: u64,
        questions: Vec<AskQuestion>,
        reply: oneshot::Sender<Vec<Vec<String>>>,
    },
}

pub struct AskUserTool {
    tx: mpsc::UnboundedSender<AskRequest>,
    /// 用户按停止 / 关会话时置位。提问**也要**能被打断 —— 否则界面上按了停止，
    /// 这一轮却还卡在一个永远等不到答案的 oneshot 上（最多等 120 秒的工具超时
    /// 也救不了它），关掉的会话还得等它醒来才能退出。
    cancel: CancelToken,
}

impl AskUserTool {
    pub fn new(tx: mpsc::UnboundedSender<AskRequest>, cancel: CancelToken) -> Self {
        Self { tx, cancel }
    }
}

/// 把参数解析成问题列表。新旧两种形态都收，见模块头。
fn parse_questions(args: &Value) -> Result<Vec<AskQuestion>, String> {
    // 形态一（新）：{ questions: [ { question, header?, multi_select?, options? } ] }
    if let Some(arr) = args.get("questions").and_then(Value::as_array) {
        let mut out = Vec::new();
        for (i, q) in arr.iter().enumerate() {
            out.push(parse_one(q).map_err(|e| format!("第 {} 个问题：{e}", i + 1))?);
        }
        if out.is_empty() {
            return Err("questions 是空的".into());
        }
        return Ok(out);
    }

    // 形态二（旧）：{ question, options: [string] }
    if args.get("question").is_some() {
        return Ok(vec![parse_one(args)?]);
    }

    Err("缺少 questions（要问的问题列表）".into())
}

fn parse_one(q: &Value) -> Result<AskQuestion, String> {
    let question = q
        .get("question")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("缺少 question")?
        .to_string();

    let options: Vec<crate::protocol::AskOption> = q
        .get("options")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|o| match o {
                    // 新形态：{ label, description }
                    Value::Object(_) => o
                        .get("label")
                        .and_then(Value::as_str)
                        .map(|l| crate::protocol::AskOption {
                            label: l.to_string(),
                            description: o
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        }),
                    // 旧形态：裸字符串
                    Value::String(s) => Some(crate::protocol::AskOption {
                        label: s.clone(),
                        description: None,
                    }),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(AskQuestion {
        question,
        header: q.get("header").and_then(Value::as_str).map(str::to_string),
        multi_select: q
            .get("multi_select")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        options,
    })
}

/// 把答案渲染成回灌给模型的一段文本。
///
/// 多个问题时必须把问题也带上 —— 否则模型拿到的是一串没有主语的答案，
/// 上下文一长就分不清哪句答的是哪问。
fn render_answers(pairs: &[(AskQuestion, Vec<String>)]) -> String {
    if pairs.len() == 1 {
        let (q, a) = &pairs[0];
        if a.is_empty() {
            return format!("问题：{}\n用户没有作答。", q.question);
        }
        return format!("问题：{}\n用户选择：{}", q.question, a.join("、"));
    }
    let mut out = String::from("用户对以下问题作出了回答：\n");
    for (i, (q, a)) in pairs.iter().enumerate() {
        let answer = if a.is_empty() {
            "（未作答）".to_string()
        } else {
            a.join("、")
        };
        let head = q.header.as_deref().unwrap_or("");
        out.push_str(&format!(
            "{}. {}：{}\n   答：{}\n",
            i + 1,
            if head.is_empty() { "问题" } else { head },
            q.question,
            answer
        ));
    }
    out
}

#[async_trait::async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user_question"
    }

    fn description(&self) -> &str {
        "向用户提问并等待回答。**一次可以问多个问题**，每个问题可选多选。\
         适合：需求有歧义、需要用户在方案间取舍、敏感操作前确认。\n\
         一次问完比分几轮问好 —— 每多一轮就多一次完整的模型请求。\n\
         用户也可能不作答（跳过），这时要能继续往下走。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "要问的问题（1-4 个）。一次问完，不要分多轮。",
                    "minItems": 1,
                    "maxItems": 4,
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "问题本身，一句话说清楚"
                            },
                            "header": {
                                "type": "string",
                                "description": "极短标签（最多 12 字），界面上当标题用，如「数据库」「部署方式」"
                            },
                            "multi_select": {
                                "type": "boolean",
                                "description": "是否允许多选（缺省 false = 单选）"
                            },
                            "options": {
                                "type": "array",
                                "description": "候选项。给了就渲染成选项按钮，用户仍可自由输入。",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "选项文字" },
                                        "description": {
                                            "type": "string",
                                            "description": "这个选项意味着什么（可选，短）"
                                        }
                                    },
                                    "required": ["label"]
                                }
                            }
                        },
                        "required": ["question"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let questions = match parse_questions(&args) {
            Ok(q) => q,
            Err(e) => return ToolResult::err(format!("ask_user_question: {e}")),
        };

        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let (reply, rx) = oneshot::channel::<Vec<Vec<String>>>();
        if self
            .tx
            .send(AskRequest::Questions {
                id,
                questions: questions.clone(),
                reply,
            })
            .is_err()
        {
            return ToolResult::err("ask_user_question: 界面通道已关闭");
        }

        // `[]` = 用户跳过了。不是错误 —— 模型应当能继续往下走。
        //
        // 但「中断」和「跳过」不是一回事：中断之后这一轮本来就要收尾了，给模型
        // 一段明确的说明，比让它以为用户什么都没选要好。
        let answers = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                return ToolResult::err("这一轮被用户中断了，问题没有作答。");
            }
            a = rx => a.unwrap_or_default(),
        };
        let pairs: Vec<(AskQuestion, Vec<String>)> = questions
            .into_iter()
            .enumerate()
            .map(|(i, q)| {
                let a = answers.get(i).cloned().unwrap_or_default();
                (q, a)
            })
            .collect();

        let text = render_answers(&pairs);
        if pairs.iter().all(|(_, a)| a.is_empty()) {
            return ToolResult::ok(format!("{text}\n（用户跳过了，请按最合理的默认继续）"));
        }
        ToolResult::ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_new_multi_question_shape() {
        let args = json!({
            "questions": [
                { "question": "用哪个数据库？", "header": "数据库", "multi_select": false,
                  "options": [{"label": "Postgres", "description": "已有实例"}] },
                { "question": "要哪些功能？", "multi_select": true,
                  "options": [{"label": "登录"}, {"label": "导出"}] }
            ]
        });
        let qs = parse_questions(&args).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].header.as_deref(), Some("数据库"));
        assert!(!qs[0].multi_select);
        assert!(qs[1].multi_select);
        assert_eq!(qs[1].options.len(), 2);
        assert_eq!(qs[0].options[0].description.as_deref(), Some("已有实例"));
    }

    /// 模型见过的语料里这个工具大多是旧形态。只认新 schema 的话，它会拿到参数错误、
    /// 然后**重试同样的形状**，白烧几轮。
    #[test]
    fn the_old_single_question_shape_still_works() {
        let args = json!({ "question": "继续吗？", "options": ["继续", "停下"] });
        let qs = parse_questions(&args).unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].question, "继续吗？");
        assert_eq!(qs[0].options.len(), 2);
        // 裸字符串选项也要能给说明字段留空。
        assert!(qs[0].options[0].description.is_none());
    }

    #[test]
    fn missing_questions_is_a_clear_error() {
        assert!(parse_questions(&json!({})).is_err());
        assert!(parse_questions(&json!({"questions": []})).is_err());
    }

    #[test]
    fn a_bad_entry_names_which_question_is_wrong() {
        let args = json!({ "questions": [{ "question": "好的" }, { "header": "缺了 question" }] });
        let err = parse_questions(&args).unwrap_err();
        assert!(err.contains("第 2 个问题"), "{err}");
    }

    /// 多个问题的答案必须带上问题本身，否则模型拿到的是一串没有主语的答案。
    #[test]
    fn multiple_answers_are_rendered_with_their_questions() {
        let pairs = vec![
            (
                AskQuestion {
                    question: "用哪个数据库？".into(),
                    header: Some("数据库".into()),
                    multi_select: false,
                    options: vec![],
                },
                vec!["Postgres".to_string()],
            ),
            (
                AskQuestion {
                    question: "要哪些功能？".into(),
                    header: None,
                    multi_select: true,
                    options: vec![],
                },
                vec!["登录".to_string(), "导出".to_string()],
            ),
        ];
        let out = render_answers(&pairs);
        assert!(out.contains("用哪个数据库？"), "{out}");
        assert!(out.contains("Postgres"), "{out}");
        assert!(out.contains("登录、导出"), "多选要连起来：{out}");
    }

    #[test]
    fn a_skipped_question_says_so_instead_of_becoming_empty_text() {
        let pairs = vec![(
            AskQuestion {
                question: "继续吗？".into(),
                header: None,
                multi_select: false,
                options: vec![],
            },
            vec![],
        )];
        let out = render_answers(&pairs);
        assert!(out.contains("没有作答"), "{out}");
    }

    #[tokio::test]
    async fn the_tool_round_trips_an_answer_through_the_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel::<AskRequest>();
        let tool = AskUserTool::new(tx, CancelToken::new());

        tokio::spawn(async move {
            if let Some(AskRequest::Questions { reply, .. }) = rx.recv().await {
                let _ = reply.send(vec![vec!["Postgres".to_string()]]);
            }
        });

        let res = tool
            .execute(json!({ "questions": [{ "question": "用哪个数据库？", "options": [{"label":"Postgres"}] }] }))
            .await;
        assert!(res.ok, "{}", res.content);
        assert!(res.content.contains("Postgres"), "{}", res.content);
    }
}
