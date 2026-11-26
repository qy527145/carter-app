//! 验证：把「工单说做完了」变成「有证据表明做完了」。
//!
//! ## 缺口在哪
//!
//! 工单的 `ready` 是模型对模型的说法。没有这一层，值守层只能选择相信或者问人 ——
//! 前者会把幻觉一路传给用户，后者就是我们要消灭的打扰。所以 `ready` 先进入
//! `Verifying`，由这里收集证据：
//!
//! 1. 汇报里声明的检查（命令、是否成功、输出尾巴）；
//! 2. 工作目录的 git 状态与改动摘要（由运行时实际执行，不是模型转述）；
//! 3. 授权书里配置的验证命令，按 `VerifyLevel` 决定要不要重跑。
//!
//! 证据不足就降成 `Rework`，把缺什么具体退回去。这样「假完成」在一个回合内被抓住。
//!
//! ## 为什么不看会话全文
//!
//! 会话里有几千行工具调用，读它既贵又容易被文字误导。这里只认**结构化事实**：
//! 命令退出码、改动文件列表。需要语义判断时交给值守模型，不混进验证层。

use serde::{Deserialize, Serialize};

use crate::protocol::now_ms;

use super::source;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckRun {
    pub cmd: String,
    pub ok: bool,
    /// 成功时是 0；失败时尽力从错误里取出退出码。
    pub exit: Option<i32>,
    /// 输出的最后几行，最多 200 字。
    pub tail: String,
    pub at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default)]
    pub checks: Vec<CheckRun>,
    /// 这一轮改过的文件（git status 里的路径）。
    #[serde(default)]
    pub changes: Vec<String>,
    /// 改动摘要的指纹：用来判断「汇报之后又改过没有」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_hash: Option<String>,
    /// 工单自己写的验证说明（查清楚类任务没有命令可跑时用）。
    #[serde(default)]
    pub note: String,
    /// 验证层跑过之后置位。
    #[serde(default)]
    pub verified: bool,
}

impl Evidence {
    /// 有没有站得住脚的证据。规则故意宽松但不为空：
    /// 跑过一条成功的检查、或确实改过文件、或给出了可核对的说明，三者有其一。
    pub fn sufficient(&self) -> bool {
        self.checks.iter().any(|c| c.ok) || !self.changes.is_empty() || !self.note.trim().is_empty()
    }

    /// 一句人话的摘要，进台账与返工说明。
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let ok = self.checks.iter().filter(|c| c.ok).count();
        if ok > 0 {
            parts.push(format!("{ok} 条检查通过"));
        }
        let bad: Vec<&CheckRun> = self.checks.iter().filter(|c| !c.ok).collect();
        if !bad.is_empty() {
            parts.push(format!(
                "{} 条检查失败（{}）",
                bad.len(),
                bad.iter()
                    .map(|c| c.cmd.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }
        if !self.changes.is_empty() {
            parts.push(format!("改了 {} 个文件", self.changes.len()));
        }
        if !self.note.trim().is_empty() {
            parts.push(self.note.trim().to_string());
        }
        if parts.is_empty() {
            "没有任何证据".into()
        } else {
            parts.join("；")
        }
    }
}

/// 验证级别。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// 收下汇报里声明的检查，再用 git 状态补改动列表。
    #[default]
    Recorded,
    /// 在 Recorded 之上，把授权书里的命令重跑一遍。
    Rerun,
    /// 不验证（不推荐；只给「查清楚」类工单）。
    Off,
}

/// 收集证据。`declared` 是汇报里带来的检查结果。
pub async fn collect(cwd: &str, declared: Vec<CheckRun>, note: String, level: Level) -> Evidence {
    let mut ev = Evidence {
        checks: declared,
        note,
        ..Default::default()
    };
    if level == Level::Off {
        ev.verified = true;
        return ev;
    }
    let (changes, hash) = git_state(cwd).await;
    ev.changes = changes;
    ev.diff_hash = hash;
    ev.verified = true;
    ev
}

/// 按授权书重跑检查。返回的结果追加进证据。
pub async fn rerun(cwd: &str, checks: &[String]) -> Vec<CheckRun> {
    let mut out = Vec::new();
    for cmd in checks.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let started = now_ms();
        match source::run_command(cmd, std::path::Path::new(cwd)).await {
            Ok(text) => out.push(CheckRun {
                cmd: cmd.to_string(),
                ok: true,
                exit: Some(0),
                tail: tail(&text),
                at: started,
            }),
            Err(e) => out.push(CheckRun {
                cmd: cmd.to_string(),
                ok: false,
                exit: exit_of(&e),
                tail: tail(&e),
                at: started,
            }),
        }
    }
    out
}

/// 工作目录的 git 状态。不是仓库、没有 git 都返回空，不当作错误。
pub async fn git_state(cwd: &str) -> (Vec<String>, Option<String>) {
    let dir = std::path::Path::new(cwd);
    if !dir.is_dir() {
        return (Vec::new(), None);
    }
    let Ok(status) = source::run_command("git status --porcelain=v1", dir).await else {
        return (Vec::new(), None);
    };
    let changes: Vec<String> = status
        .lines()
        .filter_map(|l| l.get(3..).map(str::trim))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .take(100)
        .collect();
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        status.hash(&mut h);
        Some(format!("{:016x}", h.finish()))
    };
    (changes, hash)
}

/// 证据不足时退回工单的那段话。
pub fn rework_text(missing: &str) -> String {
    format!(
        "【验证未通过】汇报说完成了，但没有可核对的证据：{missing}。\n\
         请补上：实际跑过的检查命令与结果（或改动的文件列表），然后用 assistant_report 重新汇报。\
         只有一句话的结论不算完成。"
    )
}

fn tail(text: &str) -> String {
    let s = text.trim();
    let mut lines: Vec<&str> = s.lines().rev().take(6).collect();
    lines.reverse();
    let mut out = lines.join("\n");
    if out.chars().count() > 400 {
        out = out.chars().take(400).collect();
        out.push('…');
    }
    out
}

fn exit_of(err: &str) -> Option<i32> {
    let idx = err.find("退出码 ")?;
    err[idx + "退出码 ".len()..]
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()
        .and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_needs_at_least_one_fact() {
        assert!(!Evidence::default().sufficient());
        assert!(
            Evidence {
                checks: vec![CheckRun {
                    cmd: "cargo test".into(),
                    ok: true,
                    exit: Some(0),
                    tail: "ok".into(),
                    at: 0,
                }],
                ..Default::default()
            }
            .sufficient()
        );
        assert!(
            Evidence {
                changes: vec!["src/a.rs".into()],
                ..Default::default()
            }
            .sufficient()
        );
        assert!(
            Evidence {
                note: "查了日志，是上游 502".into(),
                ..Default::default()
            }
            .sufficient()
        );
    }

    #[test]
    fn a_failed_check_alone_is_not_enough() {
        let ev = Evidence {
            checks: vec![CheckRun {
                cmd: "cargo test".into(),
                ok: false,
                exit: Some(1),
                tail: "2 failed".into(),
                at: 0,
            }],
            ..Default::default()
        };
        assert!(!ev.sufficient());
        assert!(ev.summary().contains("失败"));
    }

    #[test]
    fn exit_codes_are_recovered_from_the_error_text() {
        assert_eq!(exit_of("命令退出码 2：x"), Some(2));
        assert_eq!(exit_of("命令被信号终止"), None);
    }

    #[tokio::test]
    async fn a_non_git_directory_yields_empty_git_state() {
        let dir = std::env::temp_dir().join(format!("carter-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (changes, _) = git_state(&dir.to_string_lossy()).await;
        assert!(changes.is_empty());
    }

    #[tokio::test]
    async fn rerun_records_success_and_failure() {
        let dir = std::env::temp_dir().join(format!("carter-rerun-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = rerun(
            &dir.to_string_lossy(),
            &["true".to_string(), "false".to_string()],
        )
        .await;
        assert_eq!(out.len(), 2);
        assert!(out[0].ok);
        assert!(!out[1].ok);
    }
}
