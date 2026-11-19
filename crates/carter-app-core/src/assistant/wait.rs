//! 「等回复」：工单回复过对方之后，直接盯着观察源等下文，等到就唤醒 —— 不经过评估。
//!
//! ## 为什么要有这一档
//!
//! 群里 @ 我、私聊我的消息命中规则开了工单，工单回复了对方，对方再回时走的是原来那条
//! 路：观察源到点轮询 → 评估模型判成 attach → 送进工单 → 排队 → 续接。中间那次评估
//! 调用和排队都是多余的：工单自己最清楚它在等谁的什么话。所以让它用
//! `assistant_report(status = waiting)` 把「等什么」留下来，运行时拿新行先对一遍等待
//! 名单，命中就直接送进去，剩下的才交给评估。
//!
//! ## 为什么不是在会话里挂一个后台命令盯着
//!
//! carter 的后台命令结束时会把输出当成一条消息送回会话，看起来正好能当监视器用。但
//! 那样工单得一直挂在「进行中」：占并发名额、会话常驻几小时、一汇报就被收尾连带杀掉
//! 命令，而且观察源那边照样会把同一条回复再评估、再送一遍。做成状态之后这些都没有：
//! 等的时候与「待查看」一样是停着的，续接走的是现成的排队那条路。
//!
//! ## 匹配
//!
//! 关键词是子串、不分大小写，任一命中即可；没给关键词就是「这个来源的任何新行」。
//! 一行只送给一张工单（等得最久的那张）—— 两张工单等同一个人时，先到先得，另一张
//! 到期自会醒来问一句。没有命中的行原样交给评估，与以前一样。

use super::{EVENT_MARK, Source, Ticket, TicketStatus, Wait};

/// 默认最多等多久（分钟）。
pub const DEFAULT_TIMEOUT_MINUTES: u64 = 24 * 60;
/// 最多等多久（分钟）。再久就不该是「等回复」，该是一条新的观察规则。
pub const MAX_TIMEOUT_MINUTES: u64 = 7 * 24 * 60;

pub fn matches(w: &Wait, line: &str) -> bool {
    if w.keywords.is_empty() {
        return true;
    }
    let hay = line.to_lowercase();
    w.keywords
        .iter()
        .map(|k| k.trim().to_lowercase())
        .filter(|k| !k.is_empty())
        .any(|k| hay.contains(&k))
}

/// 一张工单认领到的行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub ticket_id: String,
    pub lines: Vec<String>,
    /// 命中的那张工单的 `wait`，写决定记录的理由用。
    pub wait: Wait,
}

/// 把一批新行分给在等它们的工单。返回（各工单认领到的行，没人认领的行）。
///
/// 只看 `waiting` 且盯着这个来源的工单；一行最多给一张（先按 `updated_at` 排，等得久的先挑）。
pub fn claim(
    tickets: &[Ticket],
    source_id: &str,
    events: Vec<String>,
) -> (Vec<Claim>, Vec<String>) {
    let mut waiters: Vec<&Ticket> = tickets
        .iter()
        .filter(|t| t.status == TicketStatus::Waiting)
        .filter(|t| t.wait.as_ref().is_some_and(|w| w.source_id == source_id))
        .collect();
    waiters.sort_by_key(|t| t.updated_at);
    if waiters.is_empty() {
        return (Vec::new(), events);
    }

    let mut claims: Vec<Claim> = Vec::new();
    let mut rest = Vec::new();
    for line in events {
        let hit = waiters
            .iter()
            .find(|t| t.wait.as_ref().is_some_and(|w| matches(w, &line)));
        match hit {
            Some(t) => match claims.iter_mut().find(|c| c.ticket_id == t.id) {
                Some(c) => c.lines.push(line),
                None => claims.push(Claim {
                    ticket_id: t.id.clone(),
                    lines: vec![line],
                    wait: t.wait.clone().expect("筛过了"),
                }),
            },
            None => rest.push(line),
        }
    }
    (claims, rest)
}

/// 等到了：送进工单的那段话。与评估 attach 的写法同一种格式（`EVENT_MARK` 开头），
/// 模型认得。
pub fn arrived_text(source: &Source, w: &Wait, lines: &[String]) -> String {
    let mut s = format!(
        "{EVENT_MARK}来源「{}」 · 等到的回复（你在等：{}）\n",
        source.name,
        w.note.trim()
    );
    for l in lines {
        s.push_str(&format!("- {l}\n"));
    }
    s.push_str("\n接着处理。处理完照常用 assistant_report 汇报；还要等下文就再报一次 waiting。");
    s
}

/// 到期没等到：送进工单的那段话。
pub fn expired_text(w: &Wait, source_name: &str, waited_minutes: u64) -> String {
    format!(
        "（等了 {waited_minutes} 分钟，来源「{source_name}」里没有出现你在等的回复：{}。\
         自己决定是催一下、继续等（再报一次 waiting），还是汇报结局。）",
        w.note.trim()
    )
}

/// 决定记录里的理由：这条没有经过评估。
pub fn claim_reason(label: &str, w: &Wait) -> String {
    if w.keywords.is_empty() {
        format!("{label} 在等这个来源的回复，直接送进去，没有经过评估")
    } else {
        format!(
            "{label} 在等这条回复（命中「{}」），直接送进去，没有经过评估",
            w.keywords.join("」「")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::now_ms;

    fn waiting(id: &str, updated_at: u64, keywords: &[&str]) -> Ticket {
        let mut s = super::super::State::default();
        let tid = s.open_ticket(&super::super::tests::source(), "r", id, "x", vec![]);
        let mut t = s.ticket(&tid).cloned().unwrap();
        t.id = id.to_string();
        t.status = TicketStatus::Waiting;
        t.updated_at = updated_at;
        t.wait = Some(Wait {
            source_id: "src_1".into(),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            until: now_ms() + 60_000,
            note: "张三的答复".into(),
        });
        t
    }

    /// 关键词是子串、不分大小写、任一命中；没给关键词就全收。
    #[test]
    fn keywords_are_case_insensitive_substrings() {
        let w = Wait {
            source_id: "s".into(),
            keywords: vec!["张三".into(), " cid=42 ".into()],
            until: 0,
            note: String::new(),
        };
        assert!(matches(&w, "[群] 张三：好的"));
        assert!(matches(&w, "private CID=42 reply"));
        assert!(!matches(&w, "[群] 李四：好的"));
        let any = Wait {
            keywords: vec![],
            ..w.clone()
        };
        assert!(matches(&any, "随便什么"));
    }

    /// 命中的行归工单，没命中的原样留给评估；一行只给一张工单，等得久的先挑。
    #[test]
    fn lines_go_to_the_longest_waiting_match_and_the_rest_to_eval() {
        let newer = waiting("new", 200, &["张三"]);
        let older = waiting("old", 100, &["张三", "李四"]);
        let events = vec![
            "张三：收到".to_string(),
            "王五：无关".to_string(),
            "李四：也收到".to_string(),
        ];
        let (claims, rest) = claim(&[newer, older], "src_1", events);
        assert_eq!(claims.len(), 1, "两张都等张三，只有等得久的那张拿到");
        assert_eq!(claims[0].ticket_id, "old");
        assert_eq!(claims[0].lines, vec!["张三：收到", "李四：也收到"]);
        assert_eq!(rest, vec!["王五：无关"]);
    }

    /// 只认盯着这个来源的、而且正在等的工单。
    #[test]
    fn only_waiting_tickets_on_this_source_claim() {
        let mut other_source = waiting("a", 1, &[]);
        other_source.wait.as_mut().unwrap().source_id = "src_2".into();
        let mut not_waiting = waiting("b", 2, &[]);
        not_waiting.status = TicketStatus::Ready;
        let (claims, rest) = claim(&[other_source, not_waiting], "src_1", vec!["x".into()]);
        assert!(claims.is_empty());
        assert_eq!(rest, vec!["x"]);
    }
}
