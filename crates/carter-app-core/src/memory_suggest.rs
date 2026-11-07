//! 记忆建议：一轮结束后，从用户这句话里挑出值得长期记住的东西，交给用户点一下确认。
//!
//! ## 为什么不全靠模型自己调 `save_memory`
//!
//! 模型在干活时注意力全在任务上，「顺手记一笔」是它最容易漏掉的一步 —— 实测翻遍
//! 历史会话，`save_memory` 一次都没被调用过。提示词能把调用率往上推一点，但推不到
//! 「用户纠正过一次就不会再犯」。所以在轮末补一道：副模型专门看这一轮，提出候选，
//! 用户确认后才落盘。
//!
//! ## 为什么要用户确认
//!
//! 记忆每一轮都会进系统提示。记错一条的代价是之后**每一轮**都被它带偏，而且用户
//! 多半不知道它在那里。所以模型提的只是候选，写不写由人决定。
//!
//! ## 为什么先过关键词
//!
//! 绝大多数轮次（「帮我改个 bug」「跑一下测试」）里根本没有值得记的东西。每轮都调
//! 一次副模型，是为极少数轮次付全部轮次的钱。偏好与纠正几乎总带着几个固定说法
//! （「以后」「别再」「我习惯」…），先用它们筛一遍，漏掉的那些还有模型自己的
//! `save_memory` 兜着。

use std::path::Path;

use serde::{Deserialize, Serialize};

use futures::StreamExt;

use carter::provider::{
    Block, ChatRequest, LlmProvider, RequestMeta, RequestPurpose, Role, StreamChunk, WireMessage,
    WireRequest,
};
use carter::registry::ModelInfo;

/// 一条候选记忆（IPC 用）。字段与 `save_memory` 的参数同名同义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCandidate {
    pub content: String,
    /// `facts` | `profile`
    pub kind: String,
    /// `project` | `user`。`profile` 时恒为 `user`。
    pub scope: String,
}

/// 用户这句话里出现这些说法，才值得花一次副模型去看。
///
/// 只收「表达长期意愿」的说法。「不对」「错了」这类单次纠正不在里面：它们大多只是
/// 针对这一次的结果，真要变成规矩，用户通常会接着说「以后」「每次」。
const SIGNALS: &[&str] = &[
    "以后",
    "今后",
    "下次",
    "往后",
    "别再",
    "不要再",
    "每次",
    "总是",
    "一律",
    "永远",
    "始终",
    "一直用",
    "记住",
    "记一下",
    "记得",
    "我习惯",
    "我喜欢",
    "我偏好",
    "我更喜欢",
    "我不喜欢",
    "我讨厌",
    "我们团队",
    "我们项目",
    "我们这边",
    "约定",
    "规范是",
    "统一用",
    "优先用",
    "默认用",
    "不要用",
    "别用",
    "改用",
    "from now on",
    "always",
    "never",
    "remember",
    "i prefer",
    "don't ever",
];

/// 候选最多几条。一轮话里能提炼出的长期规矩不会多，多了基本是噪音。
const MAX_CANDIDATES: usize = 3;
/// 给副模型看的：用户原话、助手回答、已有记忆各自的上限（字符）。
const MAX_USER_CHARS: usize = 4000;
const MAX_REPLY_CHARS: usize = 2000;
const MAX_EXISTING_CHARS: usize = 8000;
/// 推理模型先思考再输出、两边共用这一个数（见 `title.rs`）。
const MAX_TOKENS: u32 = 2048;

/// 这句话值不值得去提取。
pub fn worth_checking(user_text: &str) -> bool {
    let lower = user_text.to_lowercase();
    SIGNALS.iter().any(|s| lower.contains(s))
}

/// 这一轮里模型是不是已经自己记过了。记过就不再提建议 —— 同一件事问两遍是打扰。
pub fn saved_in(messages: &[WireMessage]) -> bool {
    messages.iter().any(|m| {
        m.role == Role::Assistant
            && m.content
                .iter()
                .any(|b| matches!(b, Block::ToolUse { name, .. } if name == "save_memory"))
    })
}

/// 这一轮助手说的话（只取正文，不要思考与工具往返）。
pub fn reply_text(messages: &[WireMessage]) -> String {
    let mut out = String::new();
    for m in messages.iter().filter(|m| m.role == Role::Assistant) {
        for b in &m.content {
            if let Block::Text { text, .. } = b {
                out.push_str(text);
                out.push('\n');
            }
        }
    }
    out.trim().to_string()
}

const SYSTEM: &str = "\
你负责给一个编程助手维护长期记忆。下面是用户在一轮对话里说的话、助手的回答，以及已有的记忆。\
判断用户这句话里有没有**以后每次都该遵守**的东西，有就提炼出来。

值得记的：
- 用户对做事方式的长期要求或纠正（「以后提交信息用中文」「别再自动建分支」）→ kind=facts；\
只对这个项目成立的 scope=project，到哪都成立的 scope=user
- 用户本人的身份、角色、偏好、沟通习惯（「我是后端，前端不熟」「回答简短点」）→ kind=profile
- 用户告诉你的项目约定、命令、环境事实 → kind=facts, scope=project

不值得记的：只针对这一次任务的要求、已有记忆里说过的、从代码里就看得出的、密钥凭据、你的猜测。\
拿不准就不记 —— 宁可漏，不可错。

每条写成一句能脱离这段对话看懂的话，用用户的语言，带上原因更好。最多 3 条。
只回一个 JSON 对象，不要代码块、不要解释：
{\"memories\": [{\"content\": \"…\", \"kind\": \"facts\" | \"profile\", \"scope\": \"project\" | \"user\"}]}
没有值得记的就回 {\"memories\": []}。";

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("…（已截断）");
    out
}

/// 拼给副模型的用户消息。
pub fn prompt(user_text: &str, reply: &str, existing: &str) -> String {
    format!(
        "## 已有记忆\n\n{}\n\n## 用户这一轮说的\n\n{}\n\n## 助手的回答\n\n{}",
        if existing.trim().is_empty() {
            "（没有）".to_string()
        } else {
            clip(existing, MAX_EXISTING_CHARS)
        },
        clip(user_text, MAX_USER_CHARS),
        if reply.is_empty() {
            "（没有正文）".to_string()
        } else {
            clip(reply, MAX_REPLY_CHARS)
        },
    )
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Raw {
    memories: Vec<RawItem>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawItem {
    content: String,
    kind: String,
    scope: String,
}

/// 解析副模型的回答，并把说不通的条目丢掉。
///
/// 模型给的 kind/scope 不认识时按最保守的落点处理（项目事实）：那是影响范围最小的地方。
pub fn parse(text: &str) -> Result<Vec<MemoryCandidate>, String> {
    let start = text.find('{').ok_or("回答里没有 JSON 对象")?;
    let end = text.rfind('}').ok_or("回答里的 JSON 不完整")?;
    if end < start {
        return Err("回答里的 JSON 不完整".into());
    }
    let raw: Raw = serde_json::from_str(&text[start..=end])
        .map_err(|e| format!("回答不是合法的 JSON：{e}"))?;
    let mut out: Vec<MemoryCandidate> = Vec::new();
    for item in raw.memories {
        let content = item
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if content.is_empty() || out.iter().any(|c| c.content == content) {
            continue;
        }
        let (kind, scope) = match (item.kind.trim(), item.scope.trim()) {
            ("profile", _) => ("profile", "user"),
            (_, "user" | "global") => ("facts", "user"),
            _ => ("facts", "project"),
        };
        out.push(MemoryCandidate {
            content,
            kind: kind.into(),
            scope: scope.into(),
        });
        if out.len() >= MAX_CANDIDATES {
            break;
        }
    }
    Ok(out)
}

/// 问一次副模型，拿回候选。`existing` 是当前生效的记忆全文（拿来去重）。
///
/// `turn` 是这次建议**针对哪一轮用户输入**。抓包面板按轮分组，不带的话它会落在
/// 第 0 轮 —— 会话跑到第 7 轮时，列表里凭空多出一条「第 0 轮」的请求，还把那一组
/// 拆成两段（分组按「同轮连续」切）。
pub async fn suggest(
    provider: &dyn LlmProvider,
    model: &ModelInfo,
    session_id: &str,
    turn: u32,
    user_text: &str,
    reply: &str,
    existing: &str,
) -> Result<Vec<MemoryCandidate>, String> {
    let mut wire = WireRequest::new(model.api_name.clone());
    wire.system = vec![Block::text(SYSTEM)];
    wire.messages = vec![WireMessage::user_text(prompt(user_text, reply, existing))];
    wire.max_tokens = Some(MAX_TOKENS.min(model.max_output_tokens.max(1)));
    // 带上会话 id：网关靠它把这次请求归进同一条对话。
    wire.meta = RequestMeta {
        session_id: Some(session_id.to_string()),
        turn,
        round: 0,
        // 与主对话同用途：它确实是「一轮对话的附带调用」，而且 `RequestPurpose`
        // 没有更贴切的变体（见 llm_adapt_core 的枚举）。
        purpose: RequestPurpose::MainTurn,
        ..Default::default()
    };
    let req = ChatRequest {
        protocol: None,
        wire,
    };
    let mut stream = provider.stream(req).await.map_err(|e| e.to_string())?;
    let mut text = String::new();
    while let Some(ev) = stream.next().await {
        if let StreamChunk::TextDelta { text: t, .. } = ev.map_err(|e| e.to_string())? {
            text.push_str(&t);
        }
    }
    parse(&text)
}

/// 把用户确认过的一条写进去。项目级记忆落在 `cwd/CARTER.md`（与模型自己调
/// `save_memory` 同一个位置、同一套去重与修订快照）。
pub fn accept(cwd: &Path, c: &MemoryCandidate) -> Result<String, String> {
    let kind = carter::memory::Kind::parse(&c.kind)?;
    let scope = carter::memory::Scope::parse(&c.scope)?;
    match carter::memory::save(&c.content, kind, scope, cwd)? {
        carter::memory::Saved::Written(p) => Ok(format!("已记住（写入 {}）", p.display())),
        carter::memory::Saved::Duplicate(p) => Ok(format!("{} 里已有这条", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_long_term_phrasing_triggers() {
        assert!(worth_checking("以后提交信息都用中文"));
        assert!(worth_checking("别再给我建新分支了"));
        assert!(worth_checking("我习惯用 bun"));
        assert!(worth_checking("From now on use tabs"));
        assert!(!worth_checking("帮我修一下这个 bug"));
        assert!(!worth_checking("不对，是另一个文件"));
    }

    #[test]
    fn detects_save_memory_in_turn() {
        let used = vec![WireMessage::new(
            Role::Assistant,
            vec![Block::ToolUse {
                id: "1".into(),
                name: "save_memory".into(),
                input: json!({}),
                cache: None,
            }],
        )];
        assert!(saved_in(&used));
        let other = vec![WireMessage::new(Role::Assistant, vec![Block::text("好的")])];
        assert!(!saved_in(&other));
    }

    #[test]
    fn parse_normalizes_and_caps() {
        let text = r#"好的：{"memories":[
            {"content":"提交信息用中文","kind":"facts","scope":"user"},
            {"content":"  提交信息用中文 ","kind":"facts","scope":"user"},
            {"content":"我是后端","kind":"profile","scope":"project"},
            {"content":"测试用 nextest","kind":"weird","scope":"weird"},
            {"content":"第四条","kind":"facts","scope":"project"}
        ]}"#;
        let out = parse(text).unwrap();
        assert_eq!(out.len(), 3, "去重后再截到 3 条：{out:?}");
        assert_eq!(out[0].scope, "user");
        assert_eq!(
            (out[1].kind.as_str(), out[1].scope.as_str()),
            ("profile", "user")
        );
        assert_eq!(
            (out[2].kind.as_str(), out[2].scope.as_str()),
            ("facts", "project")
        );
    }

    #[test]
    fn parse_empty_and_garbage() {
        assert!(parse(r#"{"memories":[]}"#).unwrap().is_empty());
        assert!(parse("没有").is_err());
    }

    #[test]
    fn accept_writes_project_memory_into_cwd() {
        let dir =
            std::env::temp_dir().join(format!("carter-app-memsug-{}", crate::protocol::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = MemoryCandidate {
            content: "这个项目用 bun 不用 npm".into(),
            kind: "facts".into(),
            scope: "project".into(),
        };
        accept(&dir, &c).unwrap();
        let body = std::fs::read_to_string(dir.join("CARTER.md")).unwrap();
        assert!(body.contains("- 这个项目用 bun 不用 npm"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
