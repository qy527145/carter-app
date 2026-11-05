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
    // 第一次开会话（盘上还没有）才用到 `cwd`；续接时以会话文件里记的为准。
    // 家目录会被侧栏当成一个「项目」自动展开，所以这里能躲就躲（见 [`resume_cwd`]）。
    let cwd = home();
    SessionOptions {
        cwd,
        model,
        resume_latest: false,
        resume_id: Some(CHAT_SESSION_ID.into()),
        new_id: Some(CHAT_SESSION_ID.into()),
    }
}

/// 家目录（会话的兜底目录）。
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

/// 助理对话上次在哪里干活：最近一张工单的执行目录，那个目录还在就用它。
///
/// 助理对话本身不碰项目文件，它只是**派工单时**顺手定一个起点（见
/// `assistant_ticket.rs` 的 `cwd` 参数）。固定在 `~` 的话，侧栏里家目录永远被
/// 自动展开 —— 而用户真正在看的是工单所在的项目。跟着最近一张工单走，展开的就是
/// 那个项目；一张工单都没有（全新安装）时才回家目录。
///
/// 用 shell 里那份现成的状态，不另开一个 store：另开一份读的是同一个文件，但会
/// 多一次全量解析。
pub fn resume_cwd(shell: &crate::shell::AppShell) -> Option<PathBuf> {
    shell.assistant.read(|s| {
        s.tickets
            .iter()
            .rev()
            .map(|t| PathBuf::from(&t.cwd))
            .find(|p| p.is_dir())
    })
}

/// 确保助理对话在后台跑着。已经在跑（或正在收尾）就什么都不做。
pub fn ensure(shell: &Arc<AppShell>) -> Result<(), String> {
    if shell.session(CHAT_SESSION_ID).is_some() || shell.is_closing(CHAT_SESSION_ID) {
        return Ok(());
    }
    start_with(shell, None)
}

/// 与 [`ensure`] 同一条路，只是能指定模型（界面上第一次打开时带的那个）。
fn start_with(shell: &Arc<AppShell>, model: Option<String>) -> Result<(), String> {
    let mut opts = options(model);
    // 新起一条（盘上没有）时的工作目录跟着最近的工单走，见 [`resume_cwd`]。
    if let Some(dir) = resume_cwd(shell) {
        opts.cwd = dir;
    }
    start_session(shell.clone(), opts)
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
    start_with(shell, model)?;
    Ok(
        crate::session::attach_session(shell, CHAT_SESSION_ID)
            .expect("刚起来的会话必须在 shell 里"),
    )
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

/// 助理对话里**这张工单最近一条汇报**在第几轮 —— 卡片上的「去工单对话」据此跳过去。
///
/// 「最近」是为了与界面一致：只有一张工单最后那条汇报给回答框（见 `ChatView` 的
/// `latestReports`），跳转也只该落在那一轮。数轮次用的是 `view.rs::is_turn_start`
/// 的同一判据：工具结果不带正文，不算一轮。
pub fn turn_of_report(history: &[llm_adapt_core::WireMessage], number: u32) -> Option<u32> {
    // 轮次号与界面**看到的那份**对齐（`ChatView` 的 `messagesToTurns`）：历史里带正文的
    // user 消息从 0 数起，但真正的第 0 轮——用户说的第一句话——界面会把它与它引出来的
    // 那条回复合成一个桶画出来，所以这里的第 N 个桶对应界面上的第 N-1 轮。
    //
    // 边数边记，最后留下的就是这张工单**最近**那条汇报所在的桶 —— 卡片显示的就是它。
    let mut turn: u32 = 0;
    let mut first = true;
    let mut found = None;
    for m in history {
        let text: String = m
            .content
            .iter()
            .filter_map(|b| match b {
                llm_adapt_core::Block::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if m.role != llm_adapt_core::Role::User || text.trim().is_empty() {
            continue;
        }
        if first {
            first = false;
        } else {
            turn += 1;
        }
        let head = text
            .strip_prefix(REPORT_MARK)
            .map(|rest| rest.lines().next().unwrap_or(""))
            .unwrap_or("");
        if report_number(head) == Some(number) {
            found = Some(turn);
        }
    }
    found.map(|n| n.saturating_sub(1))
}

/// 汇报标题里的工单编号：`等你拍板 · #3 标题` → 3。与界面 `reportTicket` 同一条规则
/// —— 「已答复 / 已结束 / 本来会接 / 已忽略」这类不算（它们不是那张工单的汇报）。
fn report_number(head: &str) -> Option<u32> {
    if ["已答复", "已结束", "本来会接", "已忽略"]
        .iter()
        .any(|p| head.starts_with(p))
    {
        return None;
    }
    let at = head.find(" · #")?;
    let rest = &head[at + " · #".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// 用户在助理对话里对这张工单做了一件事（回了一句、划掉了汇报）—— 记进对话的那段话。
///
/// 不结束工单（面板上还在），只是让助理知道「这条我处理过了」，下次开口别再当成
/// 还等着它、也别再问一遍。
pub fn noted_text(what: &str, label: &str, detail: &str) -> String {
    format!("{REPORT_MARK}{what} · {label}\n{detail}")
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
    use llm_adapt_core::WireMessage;

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

    /// 划掉一条汇报的那句话：标题以「已忽略」开头（界面据此不再给回答框），正文说清
    /// 不是结束 —— 面板上那张工单还在。
    #[test]
    fn an_ignored_note_says_the_ticket_is_not_closed() {
        let text = noted_text("已忽略", "#3 修登录", "知道了，不用再催。");
        assert!(text.starts_with(&format!("{REPORT_MARK}已忽略 · #3 修登录\n")));
        assert!(text.contains("知道了，不用再催。"));
    }

    /// 卡片上的「去工单对话」要跳到写着它的那一轮：助手轮的回答归到同一轮，工具
    /// 结果不带正文不算一轮，已答复 / 已忽略那些也不算这张工单的上报。
    #[test]
    fn a_report_is_located_in_the_turn_that_carried_it() {
        let msgs = vec![
            WireMessage::user_text("第一轮"),
            WireMessage::user_text(format!("{REPORT_MARK}待查看 · #1 修 CI\n好")),
            WireMessage::user_text("第二轮"),
            WireMessage::user_text(format!("{REPORT_MARK}已答复 · #1 修 CI\n答过了")),
            WireMessage::user_text(format!("{REPORT_MARK}等你拍板 · #1 修 CI\n再来一次")),
        ];
        // 界面上第 0 轮是第一句话引出来的那条；「等你拍板」落在第 3 轮。已答复那条
        // 不算这张工单的上报。
        assert_eq!(turn_of_report(&msgs, 1), Some(3));
        assert_eq!(turn_of_report(&msgs, 2), None);
    }

    #[test]
    fn only_a_turn_started_by_a_report_counts_as_one() {
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
