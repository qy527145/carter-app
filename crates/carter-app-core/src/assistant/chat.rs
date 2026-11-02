//! 助理对话：一条固定 id 的真实会话，用户在里面用话配置观察源与规则，助理也在里面汇报。
//!
//! 对应原型的 Main Session。它不属于任何工单，也不是后台 worker，所以不会被自动关掉；
//! 配置工具与当前状态段在 `session.rs` 里按 `CHAT_SESSION_ID` 挂上。

use std::path::PathBuf;

use crate::protocol::Command;
use crate::session::SessionOptions;
use crate::shell::AppShell;

use super::{CHAT_SESSION_ID, REPORT_MARK, Ticket};

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

/// 一张工单要人处理时，推进对话的那段话。
pub fn report_text(t: &Ticket) -> String {
    let body = t
        .question
        .clone()
        .or_else(|| t.summary.clone())
        .unwrap_or_default();
    format!("{REPORT_MARK}{} · {}\n{}", t.status.label(), t.label(), body.trim())
}

/// 往正在运行的助理对话里送一条汇报。对话没开着返回 false，调用方回落到系统通知。
pub fn push_report(shell: &AppShell, t: &Ticket) -> bool {
    shell.send(
        CHAT_SESSION_ID,
        Command::SendPrompt {
            text: report_text(t),
            id: None,
            interject: false,
        },
    )
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
        let id = s.open_ticket(&super::super::tests::source(), "修 CI", "修主干 CI", "x", vec![]);
        let t = s.ticket_mut(&id).unwrap();
        t.summary = Some("改好了".into());
        t.question = Some("要推送吗？".into());
        let text = report_text(&t.clone());
        assert!(text.starts_with(REPORT_MARK));
        assert!(text.contains("要推送吗？"));
        assert!(!text.contains("改好了"));
    }
}
