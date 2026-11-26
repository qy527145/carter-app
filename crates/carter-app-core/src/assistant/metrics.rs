//! 值守的指标：用数字回答「它是不是真的少打扰了、判断得准不准」。
//!
//! ## 为什么必须有
//!
//! 没有这组数字，改造是否成功只能靠感觉。指标从台账聚合，不改状态、不影响运行；
//! 读的是最近一段流水，按本地日期过滤，口径写在这里而不是散在界面里。
//!
//! 刻意保持少而诚实：只统计**能直接从台账数出来**的东西。像「改判率」这种需要
//! 知道用户当时有没有选建议的，等台账里稳定带上选项再算，不在这里编一个近似值。

use serde::Serialize;

use super::ledger::{Ledger, LedgerEntry, LedgerKind};
use super::memory::day_of;
use super::{State, TicketStatus};

#[derive(Debug, Clone, Default, Serialize)]
pub struct Metrics {
    pub day: String,
    /// 今天创建的请示。
    pub escalations_today: u32,
    /// 今天答复的请示。
    pub answered_today: u32,
    /// 今天过期的请示。
    pub expired_today: u32,
    /// 今天值守真正下发的指令（不含观察模式的建议）。
    pub directives_today: u32,
    /// 观察模式下「我会这样定」的建议数。
    pub proposed_today: u32,
    /// 今天验证不通过的次数。
    pub verify_fail_today: u32,
    /// 现在还开着的请示。
    pub open_escalations: u32,
    /// 现在还没结束的工单。
    pub open_tickets: u32,
    /// 自决率：下发的指令 / （指令 + 请示）。今天没有事件时是 `None`。
    pub self_resolved: Option<f64>,
}

pub fn compute(state: &State, ledger: &Ledger, now: u64) -> Metrics {
    compute_from(&ledger.tail(2000), state, now)
}

pub fn compute_from(entries: &[LedgerEntry], state: &State, now: u64) -> Metrics {
    let day = day_of(now);
    let mut m = Metrics {
        day: day.clone(),
        open_escalations: state.escalations.iter().filter(|e| e.is_open()).count() as u32,
        open_tickets: state.tickets.iter().filter(|t| !t.status.ended()).count() as u32,
        ..Default::default()
    };
    for e in entries {
        if day_of(e.at) != day {
            continue;
        }
        match e.kind {
            LedgerKind::Escalation => m.escalations_today += 1,
            LedgerKind::Answer => m.answered_today += 1,
            LedgerKind::Interrupt => {
                if e.data.get("reason").and_then(|v| v.as_str()) == Some("escalation_expired") {
                    m.expired_today += 1;
                }
            }
            LedgerKind::Directive => {
                if e.data.get("proposed").and_then(|v| v.as_bool()) == Some(true) {
                    m.proposed_today += 1;
                } else {
                    m.directives_today += 1;
                }
            }
            LedgerKind::Verify if e.data.get("ok").and_then(|v| v.as_bool()) == Some(false) => {
                m.verify_fail_today += 1;
            }
            _ => {}
        }
    }
    let total = m.directives_today + m.escalations_today;
    m.self_resolved = (total > 0).then(|| m.directives_today as f64 / total as f64);
    m
}

/// 当前没有结束、且停在「要人处理」之外的工单数。界面用来说明值守正在消化多少事。
pub fn supervised(state: &State) -> u32 {
    state
        .tickets
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                TicketStatus::Queued
                    | TicketStatus::Working
                    | TicketStatus::Verifying
                    | TicketStatus::NeedsDecision
                    | TicketStatus::Rework
            )
        })
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::ledger::{Actor, AuthRef};
    use crate::assistant::tests::source;
    use serde_json::json;

    fn entry(kind: LedgerKind, at: u64, data: serde_json::Value) -> LedgerEntry {
        LedgerEntry {
            id: format!("lg_{at}"),
            at,
            kind,
            actor: Actor::Supervisor,
            ticket: Some("t".into()),
            session: None,
            in_reply_to: None,
            auth: Some(AuthRef::Charter("c".into())),
            data,
        }
    }

    #[test]
    fn counts_are_per_day_and_split_proposals_from_directives() {
        let mut state = State::default();
        state.ensure_defaults();
        let id = state.open_ticket(&source(), "r", "t", "x", vec![]);
        state.ticket_mut(&id).unwrap().status = TicketStatus::NeedsUser;
        state.push_escalation(crate::assistant::escalation::Escalation::open(
            "#1",
            Some(id.clone()),
            crate::assistant::escalation::Brief {
                situation: "s".into(),
                proposal: "p".into(),
                options: vec![],
                if_silent: "i".into(),
                refs: vec![],
            },
            crate::assistant::escalation::DEFAULT_TTL_MS,
        ));
        let now = 1_700_000_000_000u64;
        let entries = vec![
            entry(LedgerKind::Directive, now, json!({"proposed": false})),
            entry(LedgerKind::Directive, now - 1, json!({"proposed": true})),
            entry(LedgerKind::Escalation, now - 2, json!({})),
            entry(LedgerKind::Answer, now - 3, json!({})),
            entry(LedgerKind::Verify, now - 4, json!({"ok": false})),
            entry(
                LedgerKind::Interrupt,
                now - 5,
                json!({"reason": "escalation_expired"}),
            ),
        ];
        let m = compute_from(&entries, &state, now);
        assert_eq!(m.directives_today, 1);
        assert_eq!(m.proposed_today, 1);
        assert_eq!(m.escalations_today, 1);
        assert_eq!(m.answered_today, 1);
        assert_eq!(m.expired_today, 1);
        assert_eq!(m.verify_fail_today, 1);
        assert_eq!(m.open_escalations, 1);
        assert_eq!(m.open_tickets, 1);
        assert!((m.self_resolved.unwrap() - 0.5).abs() < 1e-9);
    }
}
