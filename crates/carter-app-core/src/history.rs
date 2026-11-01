//! 历史自愈：把「有 tool_use 却没有 tool_result」的断口补上。
//!
//! ## 为什么需要它
//!
//! 一条工具调用在会话历史里是**两条消息**：助手消息里的 `tool_use`，和紧随其后的
//! 用户消息里的 `tool_result`。上游（Anthropic 系尤其严格）要求这两半必须相邻且
//! 一一对应 —— 中间夹一条普通的用户消息、或者干脆少了 `tool_result`，整个请求直接
//! 400。而这条历史**已经落盘**了，于是之后每一条消息都失败：界面看起来就是
//! 「异常中断之后消息对不上，无论如何都没法继续了」。
//!
//! 断口是怎么产生的：用户在模型刚流式输出完一个 `tool_use`、工具还没开始执行时
//! 点了停止（或关掉会话）。carter 的 `run_turn` 在这种情况下会把已经装配好的块
//! 原样追加进历史然后返回 `Cancelled` —— 工具那一半永远不会到来。
//!
//! ## 做法
//!
//! 在「要拿这份历史发请求」之前，按顺序扫一遍：
//!
//! * 助手消息里出现 `tool_use` → 记进待配对表；
//! * 后续用户消息里的 `tool_result` → 从待配对表里划掉；
//! * 出现一条**带正文的用户消息**（新的一轮），或者又一条助手消息，而待配对表
//!   非空 → 在这条消息**之前**插一条合成结果，说明「这一轮被中断了」；
//! * 扫到末尾还有剩余 → 追加一条合成结果。
//!
//! 合成结果的正文要写清楚「没有执行」，否则模型会以为工具真的跑过、只是没输出。
//!
//! ## 落盘与内存的区别
//!
//! 会话文件是**只追加**的（carter 的 `Recorder`），历史中段插进去的那条合成结果
//! 没法原地写回文件。所以：
//!
//! * 末尾补全（最常见的情况：中断发生在最后一轮）走 `Thread::append`，真正落盘；
//! * 中段补全只在内存里改，发出去的是修好的历史；下次加载时会再修一遍。
//!
//! 这不是妥协而是唯一正确的做法 —— 为了「文件顺序也好看」去重写整个会话文件，
//! 会在任何一次写入失败时把一条能救回来的对话彻底弄丢。

use llm_adapt_core::{Block, Role, ToolResultContent, WireMessage};

use carter::agent::Thread;

/// 补上的那条结果里的正文。**必须**让模型知道工具没跑过。
pub const INTERRUPTED_RESULT: &str = "这一轮被中断了，工具没有执行。";

/// 被补上结果的那一次工具调用（界面据此把悬着的卡片收尾）。
#[derive(Debug, Clone, PartialEq)]
pub struct RepairedCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

/// 扫一遍历史，返回其中断口被补齐的调用。没有断口时什么也不做。
///
/// 见模块头：末尾的断口走 `Thread::append`（落盘），中段的只在内存里修。
pub fn repair_dangling_tool_uses(thread: &mut Thread) -> Vec<RepairedCall> {
    let mut pending: Vec<(String, String, serde_json::Value)> = Vec::new();
    let mut repaired: Vec<RepairedCall> = Vec::new();
    // 自愈后的消息序列。没有断口时它与原序列逐条相同 —— 那时不替换，
    // 免得白白克隆一遍整条历史。
    let mut out: Vec<WireMessage> = Vec::with_capacity(thread.messages.len() + 1);
    let mut changed = false;

    let flush = |out: &mut Vec<WireMessage>,
                 pending: &mut Vec<(String, String, serde_json::Value)>,
                 repaired: &mut Vec<RepairedCall>| {
        if pending.is_empty() {
            return;
        }
        let blocks: Vec<Block> = pending
            .drain(..)
            .map(|(id, name, input)| {
                repaired.push(RepairedCall {
                    id: id.clone(),
                    name,
                    input,
                });
                Block::ToolResult {
                    tool_use_id: id,
                    content: ToolResultContent::text(INTERRUPTED_RESULT),
                    is_error: true,
                    cache: None,
                }
            })
            .collect();
        out.push(WireMessage::new(Role::User, blocks));
    };

    for msg in thread.messages.iter() {
        match msg.role {
            Role::Assistant => {
                // 又一条助手消息 → 上一条里没配对的调用永远等不到结果了。
                if !pending.is_empty() {
                    flush(&mut out, &mut pending, &mut repaired);
                    changed = true;
                }
                for b in &msg.content {
                    if let Block::ToolUse {
                        id, name, input, ..
                    } = b
                    {
                        pending.push((id.clone(), name.clone(), input.clone()));
                    }
                }
                out.push(msg.clone());
            }
            Role::User => {
                let has_text = msg.content.iter().any(|b| match b {
                    Block::Text { text, .. } => !text.trim().is_empty(),
                    _ => false,
                });
                // 带正文的用户消息 = 新的一轮。断口必须压在它前面。
                if has_text && !pending.is_empty() {
                    flush(&mut out, &mut pending, &mut repaired);
                    changed = true;
                }
                for b in &msg.content {
                    if let Block::ToolResult { tool_use_id, .. } = b {
                        pending.retain(|(id, _, _)| id != tool_use_id);
                    }
                }
                out.push(msg.clone());
            }
            Role::System => {
                if !pending.is_empty() {
                    flush(&mut out, &mut pending, &mut repaired);
                    changed = true;
                }
                out.push(msg.clone());
            }
        }
    }

    // 末尾还有没配对的：这是中断最常见的形态。走 append，真正写进会话文件。
    if !pending.is_empty() {
        let before = out.len();
        flush(&mut out, &mut pending, &mut repaired);
        // 只有当整条历史逐条不变、只是尾部多了一条合成结果时，才能安全 append。
        if !changed && out.len() == before + 1 {
            if let Some(tail) = out.last() {
                thread.append(tail.clone());
            }
        } else {
            thread.messages = out;
        }
        return repaired;
    }

    // 中段补过：内存里的这一份才是自洽的。
    if changed {
        thread.messages = out;
    }
    repaired
}

/// 合成结果里给界面看的说明（与 [`INTERRUPTED_RESULT`] 逐字对应）。
pub fn interrupted_note() -> &'static str {
    INTERRUPTED_RESULT
}

/// 只留到第 `turn` 轮**为止**（含这一轮的回答），之后的全部丢掉。
///
/// 与 [`retract_turn`] 差一轮：那个是「这一轮要重跑，所以切在它之前」，这个是
/// 「从这里往后分叉，所以这一轮要留住」。同样写一条 `Compacted` 快照落盘。
///
/// 找不到这一轮时什么都不做（fork 出来的那份已经有别的轮次了，硬切只会切坏它）。
pub fn truncate_after_turn(thread: &mut Thread, turn: u32) -> Result<(), String> {
    let mut seen = 0u32;
    let mut cut = thread.messages.len();
    for (i, msg) in thread.messages.iter().enumerate() {
        if !crate::view::is_turn_start(msg) {
            continue;
        }
        if seen > turn {
            cut = i;
            break;
        }
        seen += 1;
    }
    if cut == thread.messages.len() {
        return Ok(());
    }
    thread.messages.truncate(cut);
    thread.user_turns = turn + 1;
    if let Some(rec) = thread.recorder() {
        rec.record(carter::session::RecordKind::Compacted {
            tier: "fork".into(),
            messages: thread.messages.clone(),
        });
    }
    Ok(())
}

/// 重试：把历史回退到第 `turn` 轮**之前**，返回那一轮用户输入的正文。
///
/// ## 返回的是「已经展开过的」原文
///
/// 历史里存的是展开 `/命令`、`@文件` 之后的正文（见 `session.rs::expand_input`）。
/// 所以重跑时**不能再过一次展开** —— 否则文件内容会被再贴一遍，两遍之后就很难看。
/// 调用方拿到什么就原样重发。
///
/// ## 落盘靠一条全量快照
///
/// 会话文件是只追加的，回退没法原地改写。写一条 `Compacted` 快照（与 `/clear`、
/// 上下文压缩走同一条路），续接时按它整体替换 —— 等价于把文件截断到这一轮之前，
/// 而且旧的原始记录还在文件里，真要审计也翻得到。
///
/// `thread.user_turns` 一起退回去：重跑这一轮应当拿到**同一个**轮次号，界面上的
/// 「第 N 轮」与报文筛选才不会错位。
pub fn retract_turn(thread: &mut Thread, turn: u32) -> Result<String, String> {
    let mut seen = 0u32;
    let mut start = None;
    for (i, msg) in thread.messages.iter().enumerate() {
        // 与界面、抓包同一个判据 —— 两处用两套数法的话，回退点迟早对不上。
        if !crate::view::is_turn_start(msg) {
            continue;
        }
        if seen == turn {
            start = Some(i);
            break;
        }
        seen += 1;
    }
    let Some(i) = start else {
        return Err(format!(
            "第 {} 轮不在历史里了（可能已被压缩），没法重试",
            turn + 1
        ));
    };

    let text: String = thread.messages[i]
        .content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return Err(format!("第 {} 轮没有正文，没法重试", turn + 1));
    }

    thread.messages.truncate(i);
    thread.user_turns = turn;
    if let Some(rec) = thread.recorder() {
        rec.record(carter::session::RecordKind::Compacted {
            tier: "retry".into(),
            messages: thread.messages.clone(),
        });
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use carter::provider::{ToolResultContent, WireMessage};
    use serde_json::json;

    fn tool_use(id: &str, name: &str) -> Block {
        Block::ToolUse {
            id: id.into(),
            name: name.into(),
            input: json!({"command": "sleep 999"}),
            cache: None,
        }
    }

    fn tool_result(id: &str) -> Block {
        Block::ToolResult {
            tool_use_id: id.into(),
            content: ToolResultContent::text("ok"),
            is_error: false,
            cache: None,
        }
    }

    fn thread(messages: Vec<WireMessage>) -> Thread {
        let mut t = Thread::new_empty();
        t.messages = messages;
        t
    }

    /// 中断留下的断口：最后一条助手消息里 tool_use 没有结果。修好后应多出一条
    /// 用户消息，里面的 tool_result 与调用 id 一一对应。
    #[test]
    fn a_trailing_orphan_call_gets_a_synthetic_result() {
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
        ]);
        let repaired = repair_dangling_tool_uses(&mut t);
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].id, "call_1");
        assert_eq!(repaired[0].name, "bash");
        assert_eq!(t.messages.len(), 3);
        let last = t.messages.last().unwrap();
        assert_eq!(last.role, Role::User);
        match &last.content[0] {
            Block::ToolResult {
                tool_use_id,
                is_error,
                content,
                ..
            } => {
                assert_eq!(tool_use_id, "call_1");
                assert!(is_error, "中断不是成功");
                assert_eq!(content.as_text(), INTERRUPTED_RESULT);
            }
            other => panic!("期望 tool_result，得到 {other:?}"),
        }
    }

    /// 正常配对的历史一个字节都不该动 —— 修复必须是无副作用的。
    #[test]
    fn a_healthy_history_is_left_untouched() {
        let messages = vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
            WireMessage::new(Role::User, vec![tool_result("call_1")]),
        ];
        let mut t = thread(messages.clone());
        let repaired = repair_dangling_tool_uses(&mut t);
        assert!(repaired.is_empty());
        assert_eq!(t.messages.len(), messages.len());
    }

    /// 断口后面已经有下一条用户消息（双写、或中断后又被追加过）：合成结果必须
    /// **插在它前面**，不能追加到末尾 —— 追加到末尾仍然是一份非法历史。
    #[test]
    fn an_orphan_in_the_middle_is_repaired_before_the_next_turn() {
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
            WireMessage::user_text("还在吗"),
        ]);
        let repaired = repair_dangling_tool_uses(&mut t);
        assert_eq!(repaired.len(), 1);
        assert_eq!(t.messages.len(), 4);
        let mid = &t.messages[2];
        assert_eq!(mid.role, Role::User);
        assert!(matches!(mid.content[0], Block::ToolResult { .. }));
        assert_eq!(t.messages[3].concat_text(), "还在吗");
    }

    /// 一次中断里可能有好几个并发调用，全都要补。
    #[test]
    fn every_orphan_in_one_message_is_repaired() {
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(
                Role::Assistant,
                vec![tool_use("a", "read_file"), tool_use("b", "grep")],
            ),
        ]);
        let repaired = repair_dangling_tool_uses(&mut t);
        assert_eq!(repaired.len(), 2);
        match &t.messages.last().unwrap().content[..] {
            [
                Block::ToolResult { tool_use_id: a, .. },
                Block::ToolResult { tool_use_id: b, .. },
            ] => {
                assert_eq!((a.as_str(), b.as_str()), ("a", "b"), "顺序要原样");
            }
            other => panic!("期望两条 tool_result，得到 {other:?}"),
        }
    }

    /// 只有被回答掉的那一个算数：另一个仍然是断口。
    #[test]
    fn partially_answered_calls_only_repair_the_missing_ones() {
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(
                Role::Assistant,
                vec![tool_use("a", "read_file"), tool_use("b", "grep")],
            ),
            WireMessage::new(Role::User, vec![tool_result("a")]),
        ]);
        let repaired = repair_dangling_tool_uses(&mut t);
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].id, "b");
    }

    /// 重试把历史回退到这一轮**之前**，返回时的正文正好是这一轮的提问。
    ///
    /// 注意要连工具往返一起切掉：一次用户输入在历史里是「用户消息 + 若干条
    /// 助手/工具结果」，只切到用户消息后面那一半，会把没有结果的 tool_use 留在末尾
    ///（下一次请求直接 400）。
    #[test]
    fn retracting_a_turn_rewinds_to_before_its_user_message() {
        let mut t = thread(vec![
            WireMessage::user_text("第一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("第一答")]),
            WireMessage::user_text("第二问"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
            WireMessage::new(Role::User, vec![tool_result("call_1")]),
            WireMessage::new(Role::Assistant, vec![Block::text("第二答")]),
        ]);

        let text = retract_turn(&mut t, 1).unwrap();
        assert_eq!(text, "第二问");
        assert_eq!(t.messages.len(), 2, "后面那一整段都要切掉");
        assert_eq!(t.messages[0].concat_text(), "第一问");
        // 轮次号退回这一轮 —— 重跑时拿到的还是同一个号，报文筛选不会错位。
        assert_eq!(t.user_turns, 1);
    }

    /// 重跑的是**展开之后**的正文：历史里存的就是它，再展开一遍会把文件内容贴两次。
    /// 这里验证取出来的正是历史里那一段原样。
    #[test]
    fn retracting_returns_the_text_exactly_as_stored() {
        let mut t = thread(vec![
            WireMessage::user_text("第一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("答")]),
            WireMessage::user_text("--- src/a.rs ---\nfn main() {}"),
        ]);
        let text = retract_turn(&mut t, 1).unwrap();
        assert_eq!(text, "--- src/a.rs ---\nfn main() {}");
        // 只剩第一轮那两条；这一轮的提问本身也切掉了（重跑时会原样追加回来）。
        assert_eq!(t.messages.len(), 2);
        assert_eq!(t.messages[0].concat_text(), "第一问");
    }

    /// 轮次号不在历史里（被压缩掉了、或前端算错）：报一句能读懂的话，不改历史。
    #[test]
    fn retracting_an_unknown_turn_is_an_error_and_changes_nothing() {
        let mut t = thread(vec![
            WireMessage::user_text("只有一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("答")]),
        ]);
        let err = retract_turn(&mut t, 3).unwrap_err();
        assert!(err.contains("没法重试"), "{err}");
        assert_eq!(t.messages.len(), 2);
    }

    /// 分叉截断与重试截断差一轮：分叉要**留住**这一轮的提问与回答。
    #[test]
    fn truncating_after_a_turn_keeps_that_turn() {
        let mut t = thread(vec![
            WireMessage::user_text("第一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("第一答")]),
            WireMessage::user_text("第二问"),
            WireMessage::new(Role::Assistant, vec![Block::text("第二答")]),
            WireMessage::user_text("第三问"),
            WireMessage::new(Role::Assistant, vec![Block::text("第三答")]),
        ]);
        truncate_after_turn(&mut t, 1).unwrap();
        assert_eq!(t.messages.len(), 4);
        assert_eq!(t.messages[3].concat_text(), "第二答");
        // 新的轮次数要接上：从这条会话继续提问时不能与已有的轮次撞号。
        assert_eq!(t.user_turns, 2);
    }

    /// 已经是最后 (或更短) 的一轮：什么都不切，也不报错。
    #[test]
    fn truncating_past_the_last_turn_is_a_noop() {
        let mut t = thread(vec![
            WireMessage::user_text("只有一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("答")]),
        ]);
        truncate_after_turn(&mut t, 5).unwrap();
        assert_eq!(t.messages.len(), 2);
    }
}
