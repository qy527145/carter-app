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
//! 在「要拿这份历史发请求」之前，按顺序扫一遍，把历史**归一化**成一串自洽的
//! 「助手调用 → 紧跟着的结果」：
//!
//! * 助手消息里出现 `tool_use` → 记进待配对表，继续往下；
//! * 用户消息里的 `tool_result`，id 还挂在待配对表上才算数（并从表里划掉）；
//! * 出现一条**带正文的用户消息**（新的一轮），或者又一条助手消息、系统消息，
//!   而待配对表非空 → 就在这条消息**之前**把欠的结果补齐；
//! * 扫到末尾还有剩余 → 追加一条。
//!
//! ## 补什么、丢什么
//!
//! 只补「没结果」是不够的 —— 上游报错的形态往往是反过来的那两种，它们同样会让
//! **之后每一条消息**都发不出去：
//!
//! * **多出来的结果**（`tool_result` 的 id 在待配对表里找不到）：说明它对应的调用
//!   已经被回答过了，或者压根不在这一段历史里（两条 runner 交错写、旧版本修复过
//!   一次又追加了真结果）。留着它就是一条「不是在回答上面那次调用」的 tool 消息，
//!   Chat 协议直接 400 —— 所以丢掉。
//! * **来晚了的真结果**：调用在 A 处、结果写到了后面（中间夹了一条事件或用户消息）。
//!   这时**把真结果搬到调用后面**，而不是另补一条合成的 —— 搬完原处那条自然成了
//!   「多出来的结果」，按上一条丢掉。搬运比合成重要得多：合成那句「工具没有执行」
//!   会把真的执行结果盖掉，模型于是重跑一遍已经做过的动作。
//!
//! 一条用户消息里既有正文又有结果时，拆成「结果一条、正文一条」：Chat 协议把结果
//! 编成独立的 `role:"tool"` 消息，夹在正文后面就不再紧跟着那次调用了。
//!
//! 合成结果的正文要写清楚「没有执行」，否则模型会以为工具真的跑过、只是没输出。
//!
//! ## 为什么要扫两遍
//!
//! 「来晚了的真结果」要求在第 i 条消息处就知道它后面还有没有结果，所以先把**整条
//! 历史**里的结果收成一张 id → 位置的索引，再按顺序拼新的历史。合成的只留给索引里
//! 也找不到的那些。
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

use std::collections::{HashMap, HashSet};

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
/// 见模块头：末尾的断口走 `Thread::append`（落盘），中段的、以及多余/错位的结果
/// 只在内存里修（会话文件只追加，写不回中段）。
pub fn repair_dangling_tool_uses(thread: &mut Thread) -> Vec<RepairedCall> {
    // 第一遍：整条历史里**真正存在**的结果，id → 块。用来把「来晚了的真结果」搬到
    // 调用后面 —— 合成一条假的会把真的执行结果盖掉，模型于是重跑已做过的动作。
    //
    // 同一个 id 出现多次时取**最后**一个：交错写入留下的旧副本通常在真结果前面。
    let mut known: HashMap<String, Block> = HashMap::new();
    for msg in thread.messages.iter() {
        for b in &msg.content {
            if let Block::ToolResult { tool_use_id, .. } = b {
                known.insert(tool_use_id.clone(), b.clone());
            }
        }
    }

    let mut pending: Vec<(String, String, serde_json::Value)> = Vec::new();
    let mut repaired: Vec<RepairedCall> = Vec::new();
    // 已经用掉的 id。一个结果只能回答一次调用，第二次出现就是多余的。
    let mut answered: HashSet<String> = HashSet::new();
    // 自愈后的消息序列。没有断口时它与原序列逐条相同 —— 那时不替换，
    // 免得白白克隆一遍整条历史。
    let mut out: Vec<WireMessage> = Vec::with_capacity(thread.messages.len() + 1);
    let mut changed = false;

    /// 把欠下的结果补齐：优先用历史里那个真结果，没有才合成「被中断」。
    fn flush(
        out: &mut Vec<WireMessage>,
        pending: &mut Vec<(String, String, serde_json::Value)>,
        repaired: &mut Vec<RepairedCall>,
        known: &mut HashMap<String, Block>,
        answered: &mut HashSet<String>,
    ) {
        if pending.is_empty() {
            return;
        }
        let blocks: Vec<Block> = pending
            .drain(..)
            .map(|(id, name, input)| {
                answered.insert(id.clone());
                if let Some(real) = known.remove(&id) {
                    return real;
                }
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
    }

    for msg in thread.messages.iter() {
        match msg.role {
            Role::Assistant => {
                // 又一条助手消息 → 上一条里没配对的调用等不到结果了。
                if !pending.is_empty() {
                    flush(
                        &mut out,
                        &mut pending,
                        &mut repaired,
                        &mut known,
                        &mut answered,
                    );
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
                // 带正文的用户消息 = 新的一轮。断口必须压在它前面 —— 否则结果会被
                // 编到新那一轮的话后面去，而它回答的是上一轮的调用。
                if has_text && !pending.is_empty() {
                    flush(
                        &mut out,
                        &mut pending,
                        &mut repaired,
                        &mut known,
                        &mut answered,
                    );
                    changed = true;
                }

                let mut results: Vec<Block> = Vec::new();
                let mut others: Vec<Block> = Vec::new();
                for b in &msg.content {
                    match b {
                        // 只有还挂在待配对表上、且没被用过的才算数。其余都是「不是在
                        // 回答上面那次调用」的孤儿结果，留着整条请求就发不出去。
                        Block::ToolResult { tool_use_id, .. }
                            if pending.iter().any(|(id, _, _)| id == tool_use_id)
                                && !answered.contains(tool_use_id) =>
                        {
                            answered.insert(tool_use_id.clone());
                            pending.retain(|(id, _, _)| id != tool_use_id);
                            known.remove(tool_use_id);
                            results.push(b.clone());
                        }
                        Block::ToolResult { .. } => changed = true,
                        other => others.push(other.clone()),
                    }
                }

                if !results.is_empty() {
                    out.push(WireMessage::new(Role::User, results));
                    // 正文与结果混在一条消息里：Chat 协议把结果编成独立的 tool 消息，
                    // 夹在正文后面就不再紧跟着那次调用了 —— 拆成两条。
                    if others.is_empty() {
                        continue;
                    }
                    changed = true;
                }
                if others.is_empty() {
                    continue;
                }
                out.push(WireMessage::new(Role::User, others));
            }
            Role::System => {
                if !pending.is_empty() {
                    flush(
                        &mut out,
                        &mut pending,
                        &mut repaired,
                        &mut known,
                        &mut answered,
                    );
                    changed = true;
                }
                out.push(msg.clone());
            }
        }
    }

    // 末尾还有没配对的：这是中断最常见的形态。走 append，真正写进会话文件。
    if !pending.is_empty() {
        let before = out.len();
        flush(
            &mut out,
            &mut pending,
            &mut repaired,
            &mut known,
            &mut answered,
        );
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

/// 只保留第 `turn` 轮**之前**的历史（不含这一轮），后面的全部丢掉。
///
/// 分叉与「编辑历史消息」共用它：
/// * 从某条回答之后分叉 → 保留到 `turn + 1`；
/// * 改写某句话再问 → 保留到 `turn`（这句话本身要换掉）。
///
/// `turn` 是 0 时截成空历史 —— 那是「同一句话重新开一条对话」，是合法用法。
/// 越界（已经是这么短了）时什么都不做。
///
/// 会话文件只能追加，所以截断靠一条 `Compacted` 快照整体替换（与 `/clear`、
/// 上下文压缩同一条路）。它写在新文件上：调用方 fork 出来的是一份自包含副本，
/// 原会话一个字都不会被动到。
pub fn truncate_before_turn(thread: &mut Thread, turn: u32) -> Result<(), String> {
    let mut seen = 0u32;
    let mut cut = thread.messages.len();
    for (i, msg) in thread.messages.iter().enumerate() {
        if !crate::view::is_turn_start(msg) {
            continue;
        }
        if seen == turn {
            cut = i;
            break;
        }
        seen += 1;
    }
    if cut == thread.messages.len() {
        return Ok(());
    }
    thread.messages.truncate(cut);
    thread.user_turns = turn;
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

/// 就地重试一条回答：把历史回退到第 `turn` 轮**最后一条助手消息之前**。
///
/// 与 [`retract_turn`] 的区别在落点：那边连这一轮的提问一起切掉、再原样重发；
/// 这里提问与这一轮前面几次工具往返都留着，只把最后那条回答（连同它之后的一切）
/// 拿掉 —— 模型拿到的上下文与它当初写这条回答时一模一样，重写的只是这一条。
///
/// 这一轮里一条助手消息都没有（上一次请求直接失败了）时，切掉的只是后面那几轮。
/// 落盘与 `user_turns` 的处理同 [`retract_turn`]。
pub fn rewind_reply(thread: &mut Thread, turn: u32) -> Result<(), String> {
    let mut seen = 0u32;
    let mut start = None;
    let mut end = thread.messages.len();
    for (i, msg) in thread.messages.iter().enumerate() {
        if !crate::view::is_turn_start(msg) {
            continue;
        }
        if start.is_some() {
            end = i;
            break;
        }
        if seen == turn {
            start = Some(i);
        }
        seen += 1;
    }
    let Some(start) = start else {
        return Err(format!(
            "第 {} 轮不在历史里了（可能已被压缩），没法重试",
            turn + 1
        ));
    };
    let cut = thread.messages[start + 1..end]
        .iter()
        .rposition(|m| m.role == Role::Assistant)
        .map_or(end, |k| start + 1 + k);

    thread.messages.truncate(cut);
    thread.user_turns = turn;
    if let Some(rec) = thread.recorder() {
        rec.record(carter::session::RecordKind::Compacted {
            tier: "retry".into(),
            messages: thread.messages.clone(),
        });
    }
    Ok(())
}

/// 压缩之后，旧轮次号 → 新轮次号。
///
/// carter 的摘要压缩把历史重排成 `[首条, 摘要, 最近 8 条]`：首条（原始任务）还是第 0 轮，
/// 中段那些轮被摘要吃掉，最近几轮整体往前挪。轮次号是界面「重试 / 编辑」、用户原文、
/// 变更清单共同的钥匙，所以压缩之后这三样都要按同一张表重排 —— 只改历史不改另外
/// 两份的话，「编辑」框会回填成别的轮的原文，文件清单会挂到别的回答下面。
///
/// 只靠「压缩前有几轮、压缩后剩几轮、首条是不是一轮」就能算出来：被保留下来的永远是
/// 首轮加上**最后那几轮**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnRemap {
    /// 首条消息是不是一轮用户输入（几乎总是；空历史或首条是工具结果时不是）。
    head: bool,
    /// 压缩前的轮数。
    before: u32,
    /// 压缩后的轮数。
    after: u32,
}

impl TurnRemap {
    /// `before` = 压缩前的轮数；`messages` = 压缩后的历史。
    pub fn new(before: u32, messages: &[WireMessage]) -> Self {
        Self {
            head: messages.first().is_some_and(crate::view::is_turn_start),
            before,
            after: crate::view::user_turn_count(messages),
        }
    }

    /// 压缩后的轮数（`thread.user_turns` 要改成它）。
    pub fn after(&self) -> u32 {
        self.after
    }

    /// 什么都没丢（只折叠了旧工具输出，或根本没压）。
    pub fn is_identity(&self) -> bool {
        self.after >= self.before
    }

    /// 旧轮次号 → 新轮次号。`None` = 这一轮已经被摘要吃掉了。
    pub fn map(&self, turn: u32) -> Option<u32> {
        if self.is_identity() {
            return Some(turn);
        }
        if self.head && turn == 0 {
            return Some(0);
        }
        let tail = self.after - u32::from(self.head);
        let first_kept = self.before - tail;
        (turn >= first_kept && turn < self.before).then(|| turn - (self.before - self.after))
    }

    /// 「从第 `turn` 轮起」这种起点在压缩之后落在哪：被摘要吃掉的话挪到第一条
    /// 留下来的轮次上；还没发生的轮次（`turn >= before`）照样平移。
    pub fn floor(&self, turn: u32) -> u32 {
        if self.is_identity() || (self.head && turn == 0) {
            return turn;
        }
        let tail = self.after - u32::from(self.head);
        let first_kept = self.before - tail;
        turn.max(first_kept) - (self.before - self.after)
    }
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

    /// 没有对应调用的结果直接丢掉。
    ///
    /// 这是线上报错「Messages with role 'tool' must be a response to a preceding
    /// message with 'tool_calls'」最直接的成因：两条 runner 交错写同一个会话文件，
    /// 结果落到了下一条助手消息后面，前面的调用就被判成「前面没人调用」。
    /// 修历史不管这个的话，之后**每一条**消息都发不出去。
    #[test]
    fn a_result_without_any_call_is_dropped() {
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
            WireMessage::new(Role::User, vec![tool_result("call_1")]),
            // 孤零零的一条结果：它的调用不在这段历史里。
            WireMessage::new(Role::User, vec![tool_result("ghost")]),
            WireMessage::user_text("还在吗"),
        ]);
        repair_dangling_tool_uses(&mut t);
        assert_eq!(t.messages.len(), 4, "空的孤儿结果整条丢掉");
        assert!(
            !t.messages.iter().any(|m| m.content.iter().any(|b| matches!(
                b,
                Block::ToolResult { tool_use_id, .. } if tool_use_id == "ghost"
            ))),
            "孤儿结果必须消失：{:#?}",
            t.messages
        );
        assert_eq!(t.messages[3].concat_text(), "还在吗");
    }

    /// 同一个调用被回答了两次：第二次是多余的，要丢掉。
    ///
    /// 交错写入下很常见 —— 两个 runner 各自把同一个结果追加了一遍。留着它同样
    /// 是一条「前面没有对应 tool_calls」的 tool 消息。
    #[test]
    fn a_duplicate_result_for_the_same_call_is_dropped() {
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
            WireMessage::new(Role::User, vec![tool_result("call_1")]),
            WireMessage::new(Role::User, vec![tool_result("call_1")]),
            WireMessage::user_text("还在吗"),
        ]);
        repair_dangling_tool_uses(&mut t);
        assert_eq!(t.messages.len(), 4);
        let results: Vec<_> = t
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, Block::ToolResult { .. }))
            .collect();
        assert_eq!(results.len(), 1, "同一个调用只能有一个结果：{results:#?}");
    }

    /// 结果来晚了（中间夹了一条事件消息）：**把真结果搬到调用后面**，不要再合成。
    ///
    /// 合成那句「工具没有执行」会把真的执行结果盖掉 —— 模型以为命令没跑，
    /// 于是一遍遍重跑已经做过的动作。
    #[test]
    fn a_late_real_result_is_moved_up_instead_of_faked() {
        let mut real = tool_result("call_1");
        if let Block::ToolResult { content, .. } = &mut real {
            *content = ToolResultContent::text("真结果：服务已经起来了");
        }
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
            // 事件消息插在中间，把调用与结果分开了。
            WireMessage::user_text("【新事件】来源「钉钉」有新消息"),
            WireMessage::new(Role::User, vec![real]),
        ]);
        let repaired = repair_dangling_tool_uses(&mut t);
        assert!(repaired.is_empty(), "有真结果可用，不该合成");

        assert_eq!(t.messages.len(), 4);
        assert_eq!(t.messages[1].role, Role::Assistant);
        assert_eq!(t.messages[2].role, Role::User);
        match &t.messages[2].content[0] {
            Block::ToolResult {
                content, is_error, ..
            } => {
                assert!(content.as_text().contains("真结果"), "搬上来的必须是真结果");
                assert!(!*is_error, "真结果不是错误");
            }
            other => panic!("调用后面必须紧跟结果，得到 {other:?}"),
        }
        assert_eq!(
            t.messages[3].concat_text(),
            "【新事件】来源「钉钉」有新消息"
        );
    }

    /// 一条用户消息里既有正文又有结果：拆成两条，结果必须在正文**前面**。
    ///
    /// Chat 协议把结果编成独立的 `role:"tool"` 消息，夹在正文后面就不再紧跟
    /// 那次调用了 —— 上游直接 400。
    #[test]
    fn text_and_results_in_one_message_are_split_with_results_first() {
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
            WireMessage::new(
                Role::User,
                vec![Block::text("顺便说说"), tool_result("call_1")],
            ),
        ]);
        repair_dangling_tool_uses(&mut t);
        assert_eq!(t.messages.len(), 4, "结果一条、正文一条");
        assert!(matches!(t.messages[2].content[0], Block::ToolResult { .. }));
        assert_eq!(t.messages[3].concat_text(), "顺便说说");
    }

    /// 修复必须是幂等的：已经自洽的历史再过一遍，逐条不变。
    ///
    /// 会话文件只追加，`settle_history` 在一轮前后各调一次；第二次要是还在改，
    /// 同一条消息就会反复被搬来搬去。
    #[test]
    fn repairing_twice_changes_nothing_the_second_time() {
        let mut t = thread(vec![
            WireMessage::user_text("跑一下"),
            WireMessage::new(Role::Assistant, vec![tool_use("a", "read_file")]),
            WireMessage::new(Role::User, vec![tool_result("a")]),
            WireMessage::user_text("【新事件】来了"),
            WireMessage::new(Role::Assistant, vec![tool_use("b", "grep")]),
            WireMessage::new(Role::User, vec![tool_result("b"), tool_result("ghost")]),
        ]);
        repair_dangling_tool_uses(&mut t);
        let once = t.messages.clone();
        let repaired = repair_dangling_tool_uses(&mut t);
        assert!(repaired.is_empty(), "第二次不该再补任何东西");
        assert_eq!(t.messages, once, "第二次必须逐条不变");
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

    /// 就地重试：提问与前面的工具往返都留着，只切掉这一轮最后那条回答及之后的轮次。
    #[test]
    fn rewinding_a_reply_keeps_the_prompt_and_earlier_rounds() {
        let mut t = thread(vec![
            WireMessage::user_text("第一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("第一答")]),
            WireMessage::user_text("第二问"),
            WireMessage::new(Role::Assistant, vec![tool_use("call_1", "bash")]),
            WireMessage::new(Role::User, vec![tool_result("call_1")]),
            WireMessage::new(Role::Assistant, vec![Block::text("第二答")]),
            WireMessage::user_text("第三问"),
            WireMessage::new(Role::Assistant, vec![Block::text("第三答")]),
        ]);
        rewind_reply(&mut t, 1).unwrap();
        assert_eq!(t.messages.len(), 5, "停在工具结果之后");
        assert!(matches!(t.messages[4].content[0], Block::ToolResult { .. }));
        assert_eq!(t.user_turns, 1);
    }

    /// 这一轮只有一条回答：退到提问之后，提问本身留着。
    #[test]
    fn rewinding_a_single_reply_stops_right_after_the_prompt() {
        let mut t = thread(vec![
            WireMessage::user_text("只有一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("答")]),
        ]);
        rewind_reply(&mut t, 0).unwrap();
        assert_eq!(t.messages.len(), 1);
        assert_eq!(t.messages[0].concat_text(), "只有一问");
        assert!(rewind_reply(&mut t, 3).is_err());
    }

    /// 分叉截断：`turn + 1` = 留住这一轮的提问与回答。
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
        truncate_before_turn(&mut t, 2).unwrap();
        assert_eq!(t.messages.len(), 4);
        assert_eq!(t.messages[3].concat_text(), "第二答");
        // 新的轮次数要接上：从这条会话继续提问时不能与已有的轮次撞号。
        assert_eq!(t.user_turns, 2);
    }

    /// 「改写这句话再问」：切在这一轮之前，这句话本身要换掉。
    #[test]
    fn truncating_before_a_turn_drops_that_turn() {
        let mut t = thread(vec![
            WireMessage::user_text("第一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("第一答")]),
            WireMessage::user_text("第二问"),
            WireMessage::new(Role::Assistant, vec![Block::text("第二答")]),
        ]);
        truncate_before_turn(&mut t, 1).unwrap();
        assert_eq!(t.messages.len(), 2);
        assert_eq!(t.messages[0].concat_text(), "第一问");
        assert_eq!(t.user_turns, 1);
        // 第 0 轮之前是空历史 —— 合法，不是错误。
        truncate_before_turn(&mut t, 0).unwrap();
        assert!(t.messages.is_empty());
        assert_eq!(t.user_turns, 0);
    }

    /// 已经是最后 (或更短) 的一轮：什么都不切，也不报错。
    #[test]
    fn truncating_past_the_last_turn_is_a_noop() {
        let mut t = thread(vec![
            WireMessage::user_text("只有一问"),
            WireMessage::new(Role::Assistant, vec![Block::text("答")]),
        ]);
        truncate_before_turn(&mut t, 6).unwrap();
        assert_eq!(t.messages.len(), 2);
    }

    fn user(text: &str) -> WireMessage {
        WireMessage::user_text(text)
    }

    fn assistant(text: &str) -> WireMessage {
        WireMessage::new(Role::Assistant, vec![Block::text(text)])
    }

    /// 10 轮压成「首轮 + 摘要 + 最后 3 轮」：首轮还是 0，最后 3 轮挪到 1..=3，中间的没了。
    #[test]
    fn compaction_keeps_the_first_turn_and_shifts_the_tail() {
        let after = vec![
            user("原始任务"),
            assistant("【历史摘要·L3】…"),
            user("第 8 轮"),
            assistant("a"),
            user("第 9 轮"),
            assistant("b"),
            user("第 10 轮"),
            assistant("c"),
        ];
        let m = TurnRemap::new(10, &after);
        assert_eq!(m.after(), 4);
        assert_eq!(m.map(0), Some(0));
        assert_eq!(m.map(1), None);
        assert_eq!(m.map(6), None);
        assert_eq!(m.map(7), Some(1));
        assert_eq!(m.map(9), Some(3));
        assert_eq!(m.map(10), None, "越界的轮次不该映射出来");
        // 起点：被吃掉的挪到第一条留下的轮次，留下的照常平移，下一轮也平移。
        assert_eq!(m.floor(0), 0);
        assert_eq!(m.floor(3), 1);
        assert_eq!(m.floor(8), 2);
        assert_eq!(m.floor(10), 4);
    }

    /// 保留窗口从一轮的中间切开（开头是工具结果）：那半截不算新的一轮，映射照样对。
    #[test]
    fn a_tail_that_starts_mid_turn_does_not_count_as_a_turn() {
        let after = vec![
            user("原始任务"),
            assistant("【历史摘要·L2】…"),
            WireMessage::new(Role::User, vec![tool_result("call_1")]),
            assistant("a"),
            user("最后一轮"),
            assistant("b"),
        ];
        let m = TurnRemap::new(5, &after);
        assert_eq!(m.after(), 2);
        assert_eq!(m.map(4), Some(1));
        assert_eq!(m.map(3), None);
    }

    /// 只折叠了工具输出、没有摘要：轮次一个不少，映射是恒等的。
    #[test]
    fn an_l1_only_compaction_is_the_identity() {
        let after = vec![user("a"), assistant("x"), user("b"), assistant("y")];
        let m = TurnRemap::new(2, &after);
        assert!(m.is_identity());
        assert_eq!(m.map(1), Some(1));
    }
}
