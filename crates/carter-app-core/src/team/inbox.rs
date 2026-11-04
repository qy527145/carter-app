//! 收件箱：从事件流里挑出「跟我有关」的那些。
//!
//! 界面上的「消息」、观察员的送达，用的都是同一套判断（协议第 4 节）。放在一处，
//! 免得出现「界面上有红点、观察员却没提醒」或者反过来的情况。
//!
//! 跟我有关 =
//! * `to` 里有我，或者是全员（`*`）；
//! * 我负责的任务上的评论 / 指派 / 转交；
//! * 而且**不是我自己发的**。
//!
//! 已读状态不在这里、也不在 git 里：已读回执要是也打成事件，消息流会被淹没。
//! 它存在本机（界面的 localStorage / 观察源的「见过的行」）。

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use super::events::{self, Event, EventKind};
use super::members::Roster;
use super::model::Identity;
use super::store::Store;

/// 收件箱里的一条，带上「为什么在这里」。
#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    #[serde(flatten)]
    pub event: Event,
    /// `mention` | `broadcast` | `assigned` | `my_task`
    pub reason: &'static str,
}

/// 发出者的成员 id。v1 直接看 `from`；v0 没有，按作者邮箱去名单里找。
pub fn sender(e: &Event, roster: &Roster) -> Option<String> {
    if let Some(f) = e.from.as_deref().filter(|s| !s.is_empty()) {
        return Some(f.to_string());
    }
    e.author
        .as_ref()
        .and_then(|a| roster.by_email(&a.email))
        .map(|m| m.id.clone())
}

/// 这条事件是不是本机这个人发的。名单里还没有自己时按 git 邮箱比。
fn from_me(e: &Event, me_id: Option<&str>, me: &Identity, roster: &Roster) -> bool {
    if let (Some(id), Some(s)) = (me_id, sender(e, roster))
        && id == s
    {
        return true;
    }
    e.author
        .as_ref()
        .is_some_and(|a| !me.email.is_empty() && a.email.eq_ignore_ascii_case(&me.email))
}

/// 从一批事件里挑出跟 `me` 有关的。`my_tasks` 是我负责的任务 id。
///
/// 纯函数：界面、观察员、测试共用。
pub fn filter(
    events: Vec<Event>,
    me: &Identity,
    roster: &Roster,
    my_tasks: &std::collections::HashSet<String>,
) -> Vec<InboxItem> {
    let me_id = roster.me(me).map(|m| m.id.clone());
    let mut out = Vec::new();
    for e in events {
        if from_me(&e, me_id.as_deref(), me, roster) {
            continue;
        }
        let to_me = me_id
            .as_deref()
            .is_some_and(|id| e.to.iter().any(|t| t == id));
        let reason = if to_me && (e.kind.is(EventKind::ASSIGN) || e.kind.is(EventKind::HANDOFF)) {
            "assigned"
        } else if to_me {
            "mention"
        } else if e.to.iter().any(|t| t == events::EVERYONE) {
            "broadcast"
        } else if e.target.as_deref().is_some_and(|t| my_tasks.contains(t))
            && (e.kind.is(EventKind::COMMENT)
                || e.kind.is(EventKind::NOTE)
                || e.kind.is(EventKind::HANDOFF)
                || e.kind.is(EventKind::ASSIGN))
        {
            "my_task"
        } else {
            continue;
        };
        out.push(InboxItem { event: e, reason });
    }
    out
}

/// 我负责的、还没完成的任务 id。
pub fn my_task_ids(
    store: &Store,
    roster: &Roster,
    me: &Identity,
) -> std::collections::HashSet<String> {
    let me_key = roster
        .me(me)
        .map(|m| m.id.clone())
        .unwrap_or_else(|| me.as_key());
    let mut out = std::collections::HashSet::new();
    for goal in store.goals().items {
        for t in store.tasks(&goal.id).items {
            if let Some(a) = t.assignee.as_deref()
                && roster.same(a, &me_key)
            {
                out.insert(t.id.clone());
            }
        }
    }
    out
}

/// 读这个项目里跟本机有关的最近 `limit` 条。
pub fn read(store: &Store, me: &Identity, limit: usize) -> Vec<InboxItem> {
    let roster = super::members::load_all(store);
    let mine = my_task_ids(store, &roster, me);
    // 多读一些再筛：大部分事件跟我无关。
    let evs = events::read(store.project_root(), limit.saturating_mul(5).max(100));
    let mut out = filter(evs, me, &roster, &mine);
    out.truncate(limit);
    out
}

/// 观察源用的一行：稳定（同一条事件每次输出完全一样，观察源靠它去重）、
/// 一眼能读、带上 `eid` 方便卡特助理回头定位。
pub fn line(item: &InboxItem, roster: &Roster, titles: &HashMap<String, String>) -> String {
    let e = &item.event;
    let who = sender(e, roster)
        .and_then(|id| roster.by_id(&id).map(|m| m.name.clone()))
        .or_else(|| e.author.as_ref().map(|a| a.short().to_string()))
        .unwrap_or_else(|| "有人".into());
    let via = if e.via.as_deref() == Some("carter") {
        "（经卡特）"
    } else {
        ""
    };
    let target = e
        .target
        .as_deref()
        .map(
            |t| match titles.get(t).cloned().or_else(|| subject_title(&e.subject)) {
                Some(title) => format!(" 「{title}」({t})"),
                None => format!(" {t}"),
            },
        )
        .unwrap_or_default();
    let body: String = e.body.replace('\n', " ").chars().take(200).collect();
    let body = if body.is_empty() {
        String::new()
    } else {
        format!("：{body}")
    };
    // 期限写原始毫秒值：这一行要**稳定**（观察源靠整行去重），按本地时区格式化的话
    // 换个时区就成了「新事件」。给模型读，毫秒值它看得懂。
    let due = e.due.map(|d| format!(" 期限 {d}")).unwrap_or_default();
    format!(
        "[{}] {who}{via} {}{target}{body}{due} #{}",
        reason_label(item.reason),
        e.kind.label(),
        e.eid
    )
}

/// 从抬头 `carter/assign t-… 「标题」` 里取出标题。收件人还没同步到任务文件时用它。
pub fn subject_title(subject: &str) -> Option<String> {
    let start = subject.find('「')? + '「'.len_utf8();
    let end = subject.rfind('」')?;
    (end > start).then(|| subject[start..end].to_string())
}

pub fn reason_label(reason: &str) -> &'static str {
    match reason {
        "assigned" => "派给我",
        "mention" => "@我",
        "broadcast" => "广播",
        "my_task" => "我的任务",
        _ => "消息",
    }
}

/// 观察员入口：拉取、筛选、每条一行。拉取失败不算失败 —— 离线时照样把本地已有的
/// 列出来，观察源不该因为一次网络抖动就报「来源出问题」。
pub fn observe(cwd: &Path, limit: usize) -> Result<String, String> {
    let store = Store::discover(cwd).ok_or("这个目录没有团队目录")?;
    let me = super::git::local_identity(store.project_root())?;
    let _ = events::fetch(store.project_root());
    let roster = super::members::load_all(&store);
    let titles = task_titles(&store);
    let items = read(&store, &me, limit);
    // 旧的在前：观察源按行比对，新事件追加在末尾读起来更自然。
    Ok(items
        .iter()
        .rev()
        .map(|i| line(i, &roster, &titles))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// 任务 id → 标题。消息里显示「评论了 t-1a1a…」没人看得懂。
pub fn task_titles(store: &Store) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for goal in store.goals().items {
        out.insert(goal.id.clone(), goal.title.clone());
        for t in store.tasks(&goal.id).items {
            out.insert(t.id.clone(), t.title.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::members::{Join, join, load};
    use std::collections::HashSet;

    fn ev(kind: &str, author: &str, to: &[&str], target: Option<&str>) -> Event {
        Event {
            sha: "abc".into(),
            v: 1,
            kind: EventKind::new(kind),
            eid: format!("e-{kind}"),
            target: target.map(str::to_string),
            goal_id: None,
            from: None,
            to: to.iter().map(|s| s.to_string()).collect(),
            via: None,
            reply_to: None,
            due: None,
            body: "正文".into(),
            subject: String::new(),
            author: Identity::parse(author),
            at: 0,
            legacy: false,
        }
    }

    #[test]
    fn only_what_concerns_me_gets_through() {
        let root = std::env::temp_dir().join(format!("carter-inbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let me = Identity::parse("我 <me@x>").unwrap();
        let (m, _) = join(&root, &me, Join::New { name: "我".into() }).unwrap();
        let (other, _) = join(
            &root,
            &Identity::parse("张三 <z@x>").unwrap(),
            Join::New {
                name: "张三".into(),
            },
        )
        .unwrap();
        let roster = load(&root);
        let mine: HashSet<String> = ["t-mine".to_string()].into();

        let items = filter(
            vec![
                ev("mention", "张三 <z@x>", &[&m.id], None),
                ev("broadcast", "张三 <z@x>", &["*"], None),
                ev("assign", "张三 <z@x>", &[&m.id], Some("t-new")),
                ev("comment", "张三 <z@x>", &[], Some("t-mine")),
                ev("comment", "张三 <z@x>", &[], Some("t-other")),
                ev("mention", "张三 <z@x>", &[&other.id], None),
                // 自己发的全员广播不该提醒自己。
                ev("broadcast", "我 <ME@x>", &["*"], None),
            ],
            &me,
            &roster,
            &mine,
        );
        let reasons: Vec<&str> = items.iter().map(|i| i.reason).collect();
        assert_eq!(reasons, vec!["mention", "broadcast", "assigned", "my_task"]);

        let line = line(&items[0], &roster, &HashMap::new());
        assert!(line.starts_with("[@我] 张三 提到"), "{line}");
        assert!(
            line.ends_with("#e-mention"),
            "带 eid 才能去重与回头定位：{line}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
