//! 助理对话：一条固定 id 的真实会话，用户在里面用话配置观察源与规则，助理也在里面汇报。
//!
//! 原型的 Main Session 是一条人也能打开的会话（面板顶栏有入口），但日常对话发生在飞书
//! 线程、GitHub 评论里，助理回到原线程。桌面应用没有那些线程，这条对话同时充当 Main
//! Session 和它们的替身，所以它得像线程一样**一直在**：
//!
//! - 应用启动就开着（[`ensure`]），不等用户点进来。汇报随时能送进去，不会因为「对话
//!   没开」就只剩一条转瞬即逝的系统通知。
//! - 界面切走时不关它，切回来复用同一个任务（[`open`]）。关掉重开会打断正在说的话。
//!
//! 配置工具、工单工具与当前状态段在 `session.rs` / `tools/mod.rs` 里按 `CHAT_SESSION_ID` 挂上。

use std::path::PathBuf;
use std::sync::Arc;

use crate::protocol::Command;
use crate::session::{SessionOptions, StartedSession, start_session};
use crate::shell::AppShell;

use super::{CHAT_SESSION_ID, Decision, REPORT_MARK, Ticket};

/// 打开或续接助理对话的参数。盘上有就续接，没有就用固定 id 新建。
pub fn options(model: Option<String>) -> SessionOptions {
    let cwd = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    SessionOptions {
        cwd,
        model,
        resume_latest: false,
        resume_id: Some(CHAT_SESSION_ID.into()),
        new_id: Some(CHAT_SESSION_ID.into()),
    }
}

/// 确保助理对话在后台跑着。已经在跑（或正在收尾）就什么都不做。
pub fn ensure(shell: &Arc<AppShell>) -> Result<(), String> {
    if shell.session(CHAT_SESSION_ID).is_some() || shell.is_closing(CHAT_SESSION_ID) {
        return Ok(());
    }
    start_session(shell.clone(), options(None))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 界面要显示助理对话：在跑就接上那一个，不在就开一个。
///
/// 接上时历史取自句柄上的快照（每轮开始与结束时刷新）—— 不能关掉重开去读盘，
/// 那会打断它正在说的话、丢掉排着队的汇报。
pub fn open(shell: &Arc<AppShell>, model: Option<String>) -> Result<StartedSession, String> {
    if let Some(started) = crate::session::attach_session(shell, CHAT_SESSION_ID) {
        return Ok(started);
    }
    start_session(shell.clone(), options(model)).map_err(|e| e.to_string())
}

/// 这一轮是不是汇报推起来的（最后一条用户输入带汇报标记）。
///
/// 汇报里有外部事件的文字。原型的规矩是配置「never on observed content」，提示词拦不
/// 住的事不能只靠提示词，所以这一轮只给一个只读工具（见 `session.rs::turn_tools`）。
pub fn is_report_turn(messages: &[llm_adapt_core::WireMessage]) -> bool {
    messages
        .iter()
        .rev()
        .find(|m| crate::view::is_turn_start(m))
        .is_some_and(|m| {
            m.content.iter().any(|b| match b {
                llm_adapt_core::Block::Text { text, .. } => text.starts_with(REPORT_MARK),
                _ => false,
            })
        })
}

/// 一张工单要人处理时，推进对话的那段话。
pub fn report_text(t: &Ticket) -> String {
    let body = t
        .question
        .clone()
        .or_else(|| t.summary.clone())
        .unwrap_or_default();
    let mut s = format!(
        "{REPORT_MARK}{} · {}\n{}",
        t.status.label(),
        t.label(),
        body.trim()
    );
    if !t.options.is_empty() {
        s.push_str("\n候选：");
        s.push_str(&t.options.join(" / "));
    }
    s
}

/// 用户直接（面板、汇报卡片上）结束了一张工单 —— 记进助理对话的那段话。
pub fn closed_text(label: &str) -> String {
    format!("{REPORT_MARK}已结束 · {label}\n用户看过了，结束了这张工单，不用再跟进。")
}

/// 用户直接（面板、汇报卡片上）回答了一张工单 —— 记进助理对话的那段话。
pub fn answered_text(label: &str, text: &str) -> String {
    format!("{REPORT_MARK}已答复 · {label}\n用户直接回复了这张工单（已送进去，不用再转达）：{text}")
}

/// 观察模式下「本来会接」的那件事，推进对话时的那段话：请用户拍板。
pub fn proposal_text(d: &Decision) -> String {
    let mut s = format!(
        "{REPORT_MARK}本来会接 · {}\n",
        d.title.as_deref().unwrap_or("未命名")
    );
    if let Some(rule) = &d.rule {
        s.push_str(&format!("依据规则「{rule}」，来源「{}」。", d.source_name));
    }
    if !d.reason.trim().is_empty() {
        s.push_str(&format!("{}\n", d.reason.trim()));
    }
    s.push_str(&format!("（提议编号 {}）", d.id));
    s
}

/// 往助理对话里记一条汇报。对话没在跑就先开起来；开不起来返回 false，
/// 调用方还有系统通知兜底。
///
/// 只记、不跑模型（`Command::Record`）：用户不在场时那一轮没人看，汇报却会把上下文
/// 越撑越大。用户下次开口时，模型连同这期间的汇报一起看到。
pub(crate) fn record(shell: &Arc<AppShell>, text: String) -> bool {
    if let Err(e) = ensure(shell) {
        tracing::warn!(error = %e, "助理：开不出助理对话，汇报只走系统通知");
        return false;
    }
    shell.send(CHAT_SESSION_ID, Command::Record { text })
}

pub fn push_report(shell: &Arc<AppShell>, t: &Ticket) -> bool {
    record(shell, report_text(t))
}

pub fn push_proposal(shell: &Arc<AppShell>, d: &Decision) -> bool {
    record(shell, proposal_text(d))
}

/// 面板上的「开新一期」：之前的对话不再进上下文，只留纪要与记忆（见 `Command::NewEpisode`）。
/// 对话没在跑就先开起来 —— 盘上的本期历史照样要收进存档。
pub fn new_episode(shell: &Arc<AppShell>) -> Result<(), String> {
    ensure(shell)?;
    if shell.send(CHAT_SESSION_ID, Command::NewEpisode) {
        Ok(())
    } else {
        Err("助理对话没在运行".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chat_uses_a_fixed_id_for_both_new_and_resume() {
        let o = options(None);
        assert_eq!(o.resume_id.as_deref(), Some(CHAT_SESSION_ID));
        assert_eq!(o.new_id.as_deref(), Some(CHAT_SESSION_ID));
        assert!(o.cwd.is_dir());
    }

    #[test]
    fn a_report_carries_the_mark_and_prefers_the_question() {
        let mut s = super::super::State::default();
        let id = s.open_ticket(
            &super::super::tests::source(),
            "修 CI",
            "修主干 CI",
            "x",
            vec![],
        );
        let t = s.ticket_mut(&id).unwrap();
        t.summary = Some("改好了".into());
        t.question = Some("要推送吗？".into());
        let text = report_text(&t.clone());
        assert!(text.starts_with(REPORT_MARK));
        assert!(text.contains("要推送吗？"));
        assert!(!text.contains("改好了"));
    }

    /// 界面（`ChatView.tsx` 的 `reportTicket`）靠标题里的 ` · #编号 ` 找到工单、画回答框，
    /// 靠末行 `候选：` 去掉重复；已答复的标题以「已答复」开头，不给回答框。
    #[test]
    fn report_titles_carry_the_ticket_number_and_options_go_last() {
        let mut s = super::super::State::default();
        let id = s.assign_ticket("/tmp", "修登录", "x");
        let t = s.ticket_mut(&id).unwrap();
        t.status = super::super::TicketStatus::NeedsUser;
        t.question = Some("用哪个方案？".into());
        t.options = vec!["A：改接口".into(), "B：加适配层".into()];
        let text = report_text(&t.clone());
        let first = text.lines().next().unwrap();
        assert_eq!(first, format!("{REPORT_MARK}等你拍板 · #1 修登录"));
        assert!(text.ends_with("\n候选：A：改接口 / B：加适配层"), "{text}");
        assert!(
            answered_text("#1 修登录", "用 A")
                .starts_with(&format!("{REPORT_MARK}已答复 · #1 修登录\n"))
        );
        assert!(
            closed_text("#1 修登录").starts_with(&format!("{REPORT_MARK}已结束 · #1 修登录\n"))
        );
    }

    #[test]
    fn only_a_turn_started_by_a_report_counts_as_one() {
        use llm_adapt_core::WireMessage;
        let report = WireMessage::user_text(format!("{REPORT_MARK}待查看 · #1 修 CI\n改好了"));
        let person = WireMessage::user_text("开工");
        assert!(is_report_turn(&[person.clone(), report.clone()]));
        assert!(
            !is_report_turn(&[report, person]),
            "用户接着说话，工具就回来了"
        );
        assert!(!is_report_turn(&[]));
    }

    #[test]
    fn a_proposal_names_its_id_so_the_model_can_approve_it() {
        let d = Decision {
            id: "dec-1".into(),
            at: 0,
            source_id: "s".into(),
            source_name: "主干 CI".into(),
            events: vec![],
            verdict: super::super::Verdict::WouldAccept,
            rule: Some("修 CI".into()),
            reason: "主干 build 挂了".into(),
            title: Some("修主干 build".into()),
            task: None,
            ticket_id: None,
            dismissed: false,
        };
        let text = proposal_text(&d);
        assert!(text.starts_with(REPORT_MARK));
        assert!(text.contains("本来会接 · 修主干 build"));
        assert!(text.contains("dec-1"));
    }
}
