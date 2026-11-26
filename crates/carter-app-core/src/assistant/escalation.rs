//! 请示：值守层拿不准、必须由人拍板的一件事。
//!
//! ## 为什么是结构化对象，不是一段话
//!
//! 一段话里问「要不要用方案 B」，模型（和用户）都无法可靠地知道答复对应哪个问题、
//! 是否还有效、选的是哪个选项。所以请示有 id、有期限、有默认动作、有选项 id；
//! 答复必须带着这个 id 回来，过期或被取代的请示一律不再接受答复。
//!
//! ## 四段式
//!
//! 一条合格的请示必须回答四件事：发生了什么、我建议怎么做、还有哪些走法、
//! 你不管的话我会怎么办。缺任何一段，持守层在 `supervise_escalate` 里会被拒绝 ——
//! 「只会转达的秘书没有价值」。

use serde::{Deserialize, Serialize};

use crate::protocol::now_ms;

/// 请示的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Open,
    Answered,
    /// 到了期限，按 `on_expiry` 处理过了。
    Expired,
    /// 值守自己要回了（发现了新情况，问题不再成立）。
    Withdrawn,
    /// 同一张工单后来的汇报覆盖了这个问题。
    Superseded,
}

impl Status {
    pub fn open(self) -> bool {
        self == Self::Open
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "等你拍板",
            Self::Answered => "已答复",
            Self::Expired => "已过期",
            Self::Withdrawn => "已撤回",
            Self::Superseded => "已失效",
        }
    }
}

/// 到期后怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expiry {
    /// 搁置，等用户回来。不可逆事项只能这样。
    #[default]
    Hold,
    /// 按建议执行一次（只允许可逆动作；写入前再校验一次）。
    ExecuteDefault,
}

/// 一个候选走法。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// 稳定 id（`a` / `b` / `c`），答复带它回来。
    pub id: String,
    /// 一句话说明走这条会怎样。
    pub label: String,
    /// 选它的后果 / 代价。
    #[serde(default)]
    pub consequence: String,
    /// 有没有不可逆动作。有就不能当默认。
    #[serde(default)]
    pub risky: bool,
    /// 选这条授权哪些动作（用户答复后写进工单的逐次授权）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<super::charter::ActionClass>,
}

/// 请示正文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    /// 发生了什么（含关键证据）。
    pub situation: String,
    /// 我建议怎么做、为什么。
    pub proposal: String,
    #[serde(default)]
    pub options: Vec<Choice>,
    /// 你不管的话我会怎么办。
    pub if_silent: String,
    /// 关联引用：`#12`、会话 id、diff hash、台账 id。
    #[serde(default)]
    pub refs: Vec<String>,
}

impl Brief {
    pub fn validate(&self) -> Result<(), String> {
        if self.situation.trim().is_empty() {
            return Err("请示要写清发生了什么（situation）".into());
        }
        if self.proposal.trim().is_empty() {
            return Err("请示要带建议（proposal）—— 只会转达的秘书没有价值".into());
        }
        if self.if_silent.trim().is_empty() {
            return Err("请示要写清「你不管的话我会怎么办」（if_silent）".into());
        }
        if self.options.len() > 4 {
            return Err("选项最多 4 个".into());
        }
        let mut ids = std::collections::HashSet::new();
        for o in &self.options {
            if o.id.trim().is_empty() || o.label.trim().is_empty() {
                return Err("每个选项都要有 id 和一句话说明".into());
            }
            if !ids.insert(o.id.clone()) {
                return Err(format!("选项 id 重复：{}", o.id));
            }
        }
        Ok(())
    }
}

/// 用户的答复。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    /// 选了哪个选项；空 = 自由回答。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    /// 用户原话。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub at: u64,
}

impl Answer {
    pub fn render(&self, e: &Escalation) -> String {
        let mut s = String::new();
        if let Some(c) = &self.choice {
            match e.brief.options.iter().find(|o| o.id == *c) {
                Some(o) => s.push_str(&format!("{}：{}", o.id, o.label)),
                None => s.push_str(c),
            }
        }
        if let Some(t) = self
            .text
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(t);
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Escalation {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// 工单编号的快照（工单被清理后仍看得懂）。
    #[serde(default)]
    pub ticket_label: String,
    pub created_at: u64,
    pub expires_at: u64,
    #[serde(default)]
    pub on_expiry: Expiry,
    pub brief: Brief,
    #[serde(default)]
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// 触发这次请示的问题键（同一话题在短时间内去重）。
    #[serde(default)]
    pub topic: String,
}

/// 默认请示期限：30 分钟。太短会逼用户立刻做别的事，太长等于搁置。
pub const DEFAULT_TTL_MS: u64 = 30 * 60 * 1000;
pub const MIN_TTL_MS: u64 = 5 * 60 * 1000;
pub const MAX_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

impl Escalation {
    pub fn open(
        ticket_label: impl Into<String>,
        ticket: Option<String>,
        brief: Brief,
        ttl_ms: u64,
    ) -> Self {
        let now = now_ms();
        Self {
            id: format!("esc_{}_{}", now, rand_suffix()),
            ticket,
            ticket_label: ticket_label.into(),
            created_at: now,
            expires_at: now + ttl_ms.clamp(MIN_TTL_MS, MAX_TTL_MS),
            on_expiry: Expiry::Hold,
            brief,
            status: Status::Open,
            answer: None,
            topic: String::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.status.open()
    }

    pub fn expired(&self, now: u64) -> bool {
        self.is_open() && self.expires_at <= now
    }

    /// 答复。只有还开着的请示能答复；重复答复、过期答复一律拒绝。
    pub fn answer(&mut self, a: Answer) -> Result<(), String> {
        if !self.is_open() {
            return Err(format!("这条请示{}，不再接受答复", self.status.label()));
        }
        if self.expires_at <= a.at {
            self.status = Status::Expired;
            return Err("这条请示已经过期，不再接受答复".into());
        }
        if let Some(c) = &a.choice
            && !self.brief.options.iter().any(|o| o.id == *c)
        {
            return Err(format!(
                "没有这个选项：{c}。现有：{}",
                self.brief
                    .options
                    .iter()
                    .map(|o| o.id.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
        }
        self.answer = Some(a);
        self.status = Status::Answered;
        Ok(())
    }

    /// 到期的请示按策略处理。返回这次是不是真的从「开着」变成了「过期」——
    /// 调用方据此决定要不要通知用户。`ExecuteDefault` 的执行在运行时里做，
    /// 而且只对可逆动作开放（见 `runtime::expire_escalations_now`）。
    pub fn expire(&mut self, now: u64) -> bool {
        if !self.expired(now) {
            return false;
        }
        self.status = Status::Expired;
        true
    }

    /// 生成给人看的纯文本（系统通知、简报用）。
    pub fn text(&self) -> String {
        let mut s = format!(
            "{}\n\n发生了什么：{}\n\n我的建议：{}\n",
            self.ticket_label, self.brief.situation, self.brief.proposal
        );
        if !self.brief.options.is_empty() {
            s.push_str("\n可选：\n");
            for o in &self.brief.options {
                s.push_str(&format!("- {}：{}", o.id, o.label));
                if !o.consequence.trim().is_empty() {
                    s.push_str(&format!("（{}）", o.consequence));
                }
                s.push('\n');
            }
        }
        s.push_str(&format!("\n你不管的话：{}", self.brief.if_silent));
        s
    }
}

fn rand_suffix() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    now_ms().hash(&mut h);
    std::process::id().hash(&mut h);
    format!("{:04x}", h.finish() & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief() -> Brief {
        Brief {
            situation: "迁移脚本会改 200 行数据".into(),
            proposal: "用 A 方案，可回滚".into(),
            options: vec![
                Choice {
                    id: "a".into(),
                    label: "一次性脚本".into(),
                    consequence: "快，回滚靠备份".into(),
                    risky: false,
                    actions: Vec::new(),
                },
                Choice {
                    id: "b".into(),
                    label: "兼容层".into(),
                    consequence: "慢，改动大".into(),
                    risky: false,
                    actions: Vec::new(),
                },
            ],
            if_silent: "半小时后按 A 执行".into(),
            refs: vec!["#12".into()],
        }
    }

    #[test]
    fn a_complete_brief_passes_and_an_incomplete_one_does_not() {
        brief().validate().unwrap();
        let mut b = brief();
        b.proposal = "  ".into();
        assert!(b.validate().unwrap_err().contains("建议"));
        let mut b = brief();
        b.if_silent.clear();
        assert!(b.validate().unwrap_err().contains("不管"));
    }

    #[test]
    fn answering_needs_a_live_question_and_a_known_choice() {
        let mut e = Escalation::open("#12", Some("t".into()), brief(), DEFAULT_TTL_MS);
        let bad = Answer {
            choice: Some("z".into()),
            text: None,
            at: now_ms(),
        };
        assert!(e.answer(bad).is_err());
        let ok = Answer {
            choice: Some("a".into()),
            text: Some("按你说的".into()),
            at: now_ms(),
        };
        e.answer(ok.clone()).unwrap();
        assert_eq!(e.status, Status::Answered);
        assert!(e.answer(ok).is_err(), "不能重复答复");
        assert!(
            e.answer(Answer {
                at: now_ms(),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn an_expired_question_rejects_answers() {
        let mut e = Escalation::open("#12", None, brief(), DEFAULT_TTL_MS);
        e.expires_at = 10;
        let a = Answer {
            choice: None,
            text: Some("a".into()),
            at: 20,
        };
        assert!(e.answer(a).is_err());
        assert_eq!(e.status, Status::Expired);
    }

    #[test]
    fn expiry_only_applies_to_open_questions() {
        let mut e = Escalation::open("#12", None, brief(), DEFAULT_TTL_MS);
        e.expires_at = 10;
        assert!(e.expire(20));
        assert_eq!(e.status, Status::Expired);
        assert!(!e.expire(30));
    }

    #[test]
    fn the_rendered_text_carries_all_four_sections() {
        let e = Escalation::open("#12 修迁移", None, brief(), DEFAULT_TTL_MS);
        let t = e.text();
        assert!(t.contains("发生了什么"));
        assert!(t.contains("我的建议"));
        assert!(t.contains("可选"));
        assert!(t.contains("你不管的话"));
    }
}
