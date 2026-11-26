//! 决策台账：所有跨角色的消息与动作，追加写、可追溯。
//!
//! ## 为什么单独一个文件
//!
//! `assistant.json` 是状态，每次改都要整份重写；台账是流水，只追加。混在一起会让
//! 状态文件随运行时间无限增长，而且每次写盘都复制一遍历史。这里用 JSONL，一行一条，
//! 写坏一行不影响其余行；读的时候坏行跳过并记日志。
//!
//! ## 它不是事实源
//!
//! 工单状态以 `State` 为准。台账回答的是「谁在什么时候授权了什么、结果如何」，
//! 不参与状态推导。这样即使台账被截断，运行也不受影响；反过来则不行。
//!
//! ## 关联
//!
//! 每条记录有 `id`。汇报用 `in_reply_to` 指回指令，指令用 `auth` 指回授权来源
//! （用户答复 / 授权书 / 上一条指令）。界面据此画出一条完整的因果链。

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::protocol::now_ms;

/// 一条记录是哪一类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerKind {
    /// 工单 → 值守：汇报、决策请求。
    Report,
    /// 验证层对 `ready` 的核对。
    Verify,
    /// 值守 → 工单：指令。
    Directive,
    /// 值守 → 人：请示。
    Escalation,
    /// 人 → 值守：答复。
    Answer,
    /// 工单执行了一个外部动作（命令闸门记录）。
    Action,
    /// 决策的最终结果（由验证或后续汇报回填）。
    Outcome,
    /// 用户直接介入工单、叫停、预算耗尽等。
    Interrupt,
}

impl LedgerKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Verify => "verify",
            Self::Directive => "directive",
            Self::Escalation => "escalation",
            Self::Answer => "answer",
            Self::Action => "action",
            Self::Outcome => "outcome",
            Self::Interrupt => "interrupt",
        }
    }
}

/// 这条记录是谁做的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Worker,
    Supervisor,
    User,
    System,
}

/// 授权来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum AuthRef {
    /// 用户回答的某条请示。
    UserAnswer(String),
    /// 某份授权书（记录当时的 id；需要复盘时按台账里的快照对）。
    Charter(String),
    /// 上一条指令。
    Directive(String),
    /// 用户直接在工单里说的话。
    UserDirective,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: String,
    pub at: u64,
    pub kind: LedgerKind,
    pub actor: Actor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthRef>,
    #[serde(default)]
    pub data: serde_json::Value,
}

impl LedgerEntry {
    pub fn new(kind: LedgerKind, actor: Actor) -> Self {
        Self {
            id: new_id(),
            at: now_ms(),
            kind,
            actor,
            ticket: None,
            session: None,
            in_reply_to: None,
            auth: None,
            data: serde_json::Value::Null,
        }
    }

    pub fn ticket(mut self, id: impl Into<String>) -> Self {
        self.ticket = Some(id.into());
        self
    }

    pub fn session(mut self, id: impl Into<String>) -> Self {
        self.session = Some(id.into());
        self
    }

    pub fn reply_to(mut self, id: impl Into<String>) -> Self {
        self.in_reply_to = Some(id.into());
        self
    }

    pub fn auth(mut self, a: AuthRef) -> Self {
        self.auth = Some(a);
        self
    }

    pub fn data(mut self, v: impl Serialize) -> Self {
        self.data = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
        self
    }
}

fn new_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("lg_{}_{}", now_ms(), n)
}

/// 单个文件超过这个大小就轮转。台账只存结构化的摘要，不存全文，5MB 能放很久。
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// 轮转保留几份。
pub const KEEP: usize = 3;

/// 台账文件。多线程共享一份；写用一把锁串行化。
pub struct Ledger {
    path: PathBuf,
    write: Mutex<()>,
}

impl Ledger {
    pub fn open(path: PathBuf) -> Arc<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Arc::new(Self {
            path,
            write: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加一条。失败只记日志，不影响主流程 —— 台账是审计，不是运行依赖。
    pub fn append(&self, e: &LedgerEntry) {
        let _guard = self.write.lock().unwrap();
        self.rotate_if_needed();
        let line = match serde_json::to_string(e) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, "台账：序列化失败");
                return;
            }
        };
        let res = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(err) = res {
            tracing::warn!(path = %self.path.display(), error = %err, "台账：写入失败");
        }
    }

    fn rotate_if_needed(&self) {
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size < MAX_BYTES {
            return;
        }
        // ledger.jsonl -> ledger.jsonl.1 -> .2 -> .3（最老的删掉）
        for i in (1..KEEP).rev() {
            let from = self.path.with_extension(format!("jsonl.{i}"));
            let to = self.path.with_extension(format!("jsonl.{}", i + 1));
            let _ = std::fs::rename(from, to);
        }
        let _ = std::fs::rename(&self.path, self.path.with_extension("jsonl.1"));
    }

    /// 最近 `limit` 条（新的在后）。读不出来、坏行都跳过。
    pub fn tail(&self, limit: usize) -> Vec<LedgerEntry> {
        let mut out = read_file(&self.path);
        if out.len() < limit {
            for i in 1..=KEEP {
                let older = self.path.with_extension(format!("jsonl.{i}"));
                let mut more = read_file(&older);
                more.extend(out);
                out = more;
                if out.len() >= limit {
                    break;
                }
            }
        }
        let start = out.len().saturating_sub(limit);
        out.split_off(start)
    }

    /// 某张工单的记录（新的在后）。
    pub fn for_ticket(&self, ticket: &str, limit: usize) -> Vec<LedgerEntry> {
        let mut v: Vec<LedgerEntry> = self
            .tail(limit.saturating_mul(20).max(200))
            .into_iter()
            .filter(|e| e.ticket.as_deref() == Some(ticket))
            .collect();
        let start = v.len().saturating_sub(limit);
        v.split_off(start)
    }

    /// 某张工单在 `since` 之后有没有任何记录 —— 值守判断「这条事件处理过了没有」。
    pub fn touched_since(&self, ticket: &str, since: u64) -> bool {
        self.tail(500)
            .iter()
            .any(|e| e.ticket.as_deref() == Some(ticket) && e.at >= since)
    }
}

fn read_file(path: &Path) -> Vec<LedgerEntry> {
    let Ok(f) = File::open(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in BufReader::new(f).lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<LedgerEntry>(&line) {
            Ok(e) => out.push(e),
            Err(err) => tracing::debug!(path = %path.display(), error = %err, "台账：跳过坏行"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "carter-ledger-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ledger.jsonl")
    }

    #[test]
    fn entries_round_trip_and_filter_by_ticket() {
        let l = Ledger::open(tmp("round"));
        l.append(&LedgerEntry::new(LedgerKind::Report, Actor::Worker).ticket("t1"));
        l.append(&LedgerEntry::new(LedgerKind::Directive, Actor::Supervisor).ticket("t1"));
        l.append(&LedgerEntry::new(LedgerKind::Report, Actor::Worker).ticket("t2"));
        let all = l.tail(10);
        assert_eq!(all.len(), 3);
        let t1 = l.for_ticket("t1", 10);
        assert_eq!(t1.len(), 2);
        assert_eq!(t1[0].kind, LedgerKind::Report);
        assert_eq!(t1[1].kind, LedgerKind::Directive);
    }

    #[test]
    fn a_bad_line_does_not_poison_the_rest() {
        let path = tmp("bad");
        let l = Ledger::open(path.clone());
        l.append(&LedgerEntry::new(LedgerKind::Report, Actor::Worker));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{ broken\n")
            .unwrap();
        l.append(&LedgerEntry::new(LedgerKind::Outcome, Actor::System));
        assert_eq!(l.tail(10).len(), 2);
    }

    #[test]
    fn ids_are_unique_across_fast_writes() {
        let l = Ledger::open(tmp("ids"));
        let mut ids = std::collections::HashSet::new();
        for _ in 0..50 {
            let e = LedgerEntry::new(LedgerKind::Action, Actor::Worker);
            ids.insert(e.id.clone());
            l.append(&e);
        }
        assert_eq!(ids.len(), 50);
    }

    #[test]
    fn touched_since_sees_only_newer_entries() {
        let path = tmp("since");
        let l = Ledger::open(path);
        let mut old = LedgerEntry::new(LedgerKind::Report, Actor::Worker).ticket("t");
        old.at = 1;
        l.append(&old);
        assert!(!l.touched_since("t", 5));
        let mut new = LedgerEntry::new(LedgerKind::Directive, Actor::Supervisor).ticket("t");
        new.at = 10;
        l.append(&new);
        assert!(l.touched_since("t", 5));
    }
}
