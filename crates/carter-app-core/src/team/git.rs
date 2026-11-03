//! git 通道：空提交当消息总线，加上同步。
//!
//! ## 为什么用空提交
//!
//! 团队协作里有两类事实，它们的**合并语义**完全不同：
//!
//! * **状态**（任务现在是什么状态、归谁）—— 这一类需要「最后写的赢」，而且必须
//!   有一个能串行化的裁决者。让两个人同时对同一个任务点「认领」，光靠改文件
//!   不可能判定谁赢：两个人都写到自己的磁盘上，谁也看不见谁，直到 flush 那一刻。
//!   改成空提交之后，`git push` 是那个裁决者 —— 别人先进去了你就被拒，重试时
//!   能看到「已经被人占了」。这是 git 唯一真正原子化的地方。
//! * **事件**（评论、握手、认领记录）—— 这一类**永不冲突**：追加就是追加。
//!   塞进文件里反而会让每次评论都变成一处潜在的 merge 冲突点。
//!
//! 所以：状态落文件、事件走空提交。两套通道各管各的，职责不重叠。
//!
//! ## 提交信息格式
//!
//! 一行机器可读的抬头，加一个 trailer：
//!
//! ```text
//! carter/claim t-xyz 张三
//!
//! Carter-Team: kind=claim id=t-xyz goal=g-abc at=1790000000000
//! ```
//!
//! 抬头给人看（`git log --oneline` 里一眼能读），trailer 给程序解析。
//! 用 trailer 而不是往抬头里塞更多字段：抬头一长，`git log` 就折行，人反而看不清。
//! trailer 还有个好处 —— `git log --grep` 与 `git log -z --format` 都能直接取到
//! 作者与时间，等于免费得到了「谁、什么时候」。
//!
//! ## 与观察源的关系
//!
//! 同步本身（`fetch` + 快进）会在初始化时自动建成一个观察源（见 `init.rs`），
//! 由卡特助理定时跑。那个命令**带副作用**：跑一次就把远端的东西拉下来。观察源
//! 的设计恰好容得下这一点 —— 它的语义是「定时跑一条命令，输出里新出现的行算事件」，
//! 而不是「跑一条只读命令」。

use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use super::model::Identity;

/// 空提交能承载的事件种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// 认领一个任务。
    Claim,
    /// 释放认领。
    Release,
    /// 一条评论。`body` 里是正文。
    Comment,
    /// 一次同步握手（观察源跑通时打一条）。
    Sync,
    /// 进度汇报：做了一点、卡住了。正文里是说明。
    Note,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Claim => "claim",
            EventKind::Release => "release",
            EventKind::Comment => "comment",
            EventKind::Sync => "sync",
            EventKind::Note => "note",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "claim" => EventKind::Claim,
            "release" => EventKind::Release,
            "comment" => EventKind::Comment,
            "sync" => EventKind::Sync,
            "note" => EventKind::Note,
            _ => return None,
        })
    }
}

/// 从提交历史里解出来的一条事件。
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    /// 提交的短 SHA。
    pub sha: String,
    pub kind: EventKind,
    /// 事件的对象（任务 / 卡片 id）。`sync` 这类没有对象。
    pub target: Option<String>,
    pub goal_id: Option<String>,
    pub body: String,
    /// 提交信息抬头那一行，界面上当标题用。
    pub subject: String,
    pub author: Option<Identity>,
    pub at: u64,
}

// ---------------------------------------------------------------------------
// 命令执行
// ---------------------------------------------------------------------------

pub(crate) fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 装好的 release 包没有控制台；不带这个 flag 在 Windows 上会连闪黑框。
    #[cfg(windows)]
    crate::platform::hide_console(&mut cmd);
    let out = cmd.output().map_err(|e| format!("起 git 失败：{e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.trim().lines().rev().take(3).collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" / ");
        Err(if tail.is_empty() {
            format!("git {} 退出码 {:?}", args.first().unwrap_or(&""), out.status.code())
        } else {
            tail
        })
    }
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    git(cwd, args).is_ok()
}

pub fn git_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| crate::platform::which("git").is_some())
}

/// `dir` 是不是一个 git 仓库的**根或子目录**。
pub fn in_repo(dir: &Path) -> bool {
    dir.ancestors().any(|d| d.join(".git").exists())
}

/// `dir` 所在仓库的顶层目录。
pub fn repo_root(dir: &Path) -> Result<std::path::PathBuf, String> {
    let out = git(dir, &["rev-parse", "--show-toplevel"])?;
    if out.is_empty() {
        return Err("不在 git 仓库里".into());
    }
    Ok(std::path::PathBuf::from(out))
}

/// 本机的 git 身份。没有配置时**报错**而不是编一个 —— 编出来的身份会进提交历史，
/// 之后全团队都会看到一个不存在的人。
pub fn local_identity(dir: &Path) -> Result<Identity, String> {
    let name = git(dir, &["config", "user.name"]).unwrap_or_default();
    let email = git(dir, &["config", "user.email"]).unwrap_or_default();
    if name.trim().is_empty() || email.trim().is_empty() {
        return Err(
            "git 还没有配置身份。先跑：\n  git config user.name \"你的名字\"\n  git config user.email \"你的邮箱\""
                .into(),
        );
    }
    Ok(Identity {
        name: name.trim().to_string(),
        email: email.trim().to_string(),
    })
}

// ---------------------------------------------------------------------------
// 历史
// ---------------------------------------------------------------------------

/// 读最近 `limit` 条空提交（`.carter/` 相关的事件）。
///
/// 只认带 trailer 的提交：仓库里正常的工作提交不该混进事件流。用 `--grep` 在
/// 提交正文里找 `Carter-Team:`，比 `--allow-empty` 过滤更准 —— 后者会把所有空提交
/// 都捞出来，包括人在别处用空提交触发的 CI。
pub fn events(root: &Path, limit: usize) -> Vec<Event> {
    if !git_available() {
        return Vec::new();
    }
    // 字段之间用 \x1f 分隔、提交之间用 \x1e —— 正文里可能有换行，用普通分隔符会串行。
    let format = "--format=%h%x1f%H%x1f%an%x1f%ae%x1f%ct%x1f%s%x1f%b%x1e";
    let count = format!("-n{limit}");
    let Ok(raw) = git(
        root,
        &["log", count.as_str(), "--grep=Carter-Team:", "--fixed-strings", format],
    ) else {
        return Vec::new();
    };
    parse_events(&raw)
}

fn parse_events(raw: &str) -> Vec<Event> {
    let mut out = Vec::new();
    for chunk in raw.split('\u{1e}') {
        let chunk = chunk.trim_start_matches('\n');
        if chunk.trim().is_empty() {
            continue;
        }
        let mut parts = chunk.split('\u{1f}');
        let sha = parts.next().unwrap_or("").trim().to_string();
        let _full = parts.next();
        let an = parts.next().unwrap_or("").trim();
        let ae = parts.next().unwrap_or("").trim();
        let ct = parts.next().unwrap_or("").trim();
        let subject = parts.next().unwrap_or("").trim().to_string();
        let body = parts.next().unwrap_or("").to_string();

        let Some(trailer) = body
            .lines()
            .find(|l| l.trim_start().starts_with("Carter-Team:"))
            .map(|l| l.trim_start().trim_start_matches("Carter-Team:").trim())
        else {
            continue;
        };
        let mut kind = None;
        let mut target = None;
        let mut goal_id = None;
        for field in trailer.split_whitespace() {
            let Some((k, v)) = field.split_once('=') else {
                continue;
            };
            match k {
                "kind" => kind = EventKind::parse(v),
                "id" => target = Some(v.to_string()),
                "goal" => goal_id = Some(v.to_string()),
                _ => {}
            }
        }
        let Some(kind) = kind else { continue };

        // 正文去掉 trailer 那一行与抬头，剩下的才是评论正文。
        let text = body
            .lines()
            .filter(|l| !l.trim_start().starts_with("Carter-Team:"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();

        out.push(Event {
            sha,
            kind,
            target,
            goal_id,
            body: text,
            subject,
            author: if ae.is_empty() {
                None
            } else {
                Identity::parse(&format!("{an} <{ae}>"))
            },
            at: ct.parse::<u64>().unwrap_or(0).saturating_mul(1000),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// 写：空提交
// ---------------------------------------------------------------------------

/// 打一条空提交。**不 push** —— 推送由 [`push`] 单独决定，认领那条路径要
/// 「push 失败 = 认领失败」的语义，而评论可以攒着一起推。
pub fn commit_event(
    root: &Path,
    kind: EventKind,
    target: Option<&str>,
    goal_id: Option<&str>,
    subject: &str,
    body: &str,
) -> Result<String, String> {
    let mut trailer = format!("Carter-Team: kind={}", kind.as_str());
    if let Some(t) = target {
        trailer.push_str(&format!(" id={t}"));
    }
    if let Some(g) = goal_id {
        trailer.push_str(&format!(" goal={g}"));
    }
    let subject = subject.trim();
    let subject = if subject.is_empty() {
        format!("carter/{}", kind.as_str())
    } else {
        subject.to_string()
    };
    let message = if body.trim().is_empty() {
        format!("{subject}\n\n{trailer}\n")
    } else {
        format!("{subject}\n\n{}\n\n{trailer}\n", body.trim())
    };

    git(root, &["commit", "--allow-empty", "-m", &message])?;
    // 取刚打的那条的短 SHA（HEAD 就是它）。
    Ok(git(root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default())
}

/// 撤掉刚打的那条提交，但**保留工作区**（`--soft`）。
///
/// 用在「提交成功了、push 却被拒」的场合：本地必须先退回去，否则磁盘上写着
/// 「我认领了」、远端写着「别人认领了」，两个人各自看自己那份都是对的。
pub fn undo_last_commit(root: &Path) -> Result<(), String> {
    git(root, &["reset", "--soft", "HEAD~1"]).map(|_| ())
}

/// 把某个路径的改动从暂存区撤下来（`git restore --staged`）。
///
/// 与 [`undo_last_commit`] 一起用：退了提交之后，暂存区里还留着刚写进去的那份，
/// 得再把它撤掉，索引才真正回到提交之前。
pub fn unstage(root: &Path, pathspec: &str) -> Result<(), String> {
    git(root, &["restore", "--staged", "--", pathspec]).map(|_| ())
}

/// 提交 `.carter/` 里攒下的改动，并给出一条能读的提交信息。
///
/// 调用点在 `session.rs` 的轮末（`idle`）。**为什么不在每次工具调用时提交**：
/// 一轮里模型可能建三个任务、改两个状态，那是「一件事」；一条提交一条，`git log`
/// 会被切得读不出意图。一条提交，正文里列出这一轮动了什么。
///
/// 没有改动时安静返回 `None` —— 绝大多数轮次都不碰团队数据，那不是错误。
pub fn commit_turn(root: &Path, team_rel: &str, turn: u32) -> Result<Option<String>, String> {
    stage_team(root, team_rel)?;
    let changed = git(root, &["diff", "--cached", "--name-status", "--", team_rel])?;
    if changed.trim().is_empty() {
        return Ok(None);
    }

    // 从 name-status 里数出「新建 / 修改 / 删除」各几个，写成正文。
    let mut added: usize = 0;
    let mut modified: usize = 0;
    let mut deleted: usize = 0;
    let mut files = Vec::new();
    for line in changed.lines() {
        let mut it = line.split_whitespace();
        let status = it.next().unwrap_or("");
        let path = it.next().unwrap_or("");
        match status.chars().next() {
            Some('A') => added += 1,
            Some('D') => deleted += 1,
            _ => modified += 1,
        }
        // 只列最多 6 个，再多就「等 n 个」—— 提交正文不是文件清单。
        if files.len() < 6 {
            files.push(path.trim_start_matches(team_rel).trim_start_matches('/').to_string());
        }
    }
    let more = (added + modified + deleted).saturating_sub(files.len());
    let summary = format!(
        "更新 {} 个（新建 {added} / 修改 {modified} / 删除 {deleted}）",
        added + modified + deleted
    );
    let mut body = files.join("\n");
    if more > 0 {
        body.push_str(&format!("\n…等 {} 个", files.len() + more));
    }

    let subject = format!("chore(carter): 第 {turn} 轮 · {summary}");
    let message = format!("{subject}\n\n{body}\n\nCarter-Team: kind=commit turn={turn}");
    git(root, &["commit", "-m", &message, "--", team_rel])?;
    Ok(Some(git(root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default()))
}

/// 从会话轮末调用：找得到团队目录就提交，找不到就算了。
///
/// 刻意吞掉错误只记日志：提交失败不该让这一轮的对话失败 —— 用户看到的是回答，
/// 而团队数据仍然完整地躺在工作区里，下次一轮还会再试。
pub fn commit_turn_if_team(cwd: &Path, turn: u32) {
    let Some(store) = super::store::Store::discover(cwd) else {
        return;
    };
    let root = store.project_root();
    if !in_repo(root) {
        return;
    }
    let Ok(rel) = store
        .root()
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
    else {
        return;
    };
    match commit_turn(root, &rel, turn) {
        Ok(Some(sha)) => tracing::debug!("团队数据已提交 {sha}"),
        Ok(None) => {}
        Err(e) => tracing::warn!("提交团队数据失败：{e}"),
    }
}

/// 有没有远端可推。
pub fn has_remote(root: &Path) -> bool {
    git_ok(root, &["remote", "get-url", "origin"])
}

pub fn current_branch(root: &Path) -> Result<String, String> {
    let out = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if out.trim() == "HEAD" {
        return Err("当前处于游离 HEAD（detached），先切到一个分支上".into());
    }
    Ok(out.trim().to_string())
}

pub fn status_porcelain(root: &Path) -> String {
    git(root, &["status", "--porcelain"]).unwrap_or_default()
}

/// 把 `.carter/` 的改动放进暂存区。只看这一个目录 —— 用户工作区里其它文件不该
/// 因为「改了团队数据」被顺手提交。
pub fn stage_team(root: &Path, team_rel: &str) -> Result<(), String> {
    // `-A` 是必须的：删除的文件（删任务）也要进暂存区，否则删掉的任务会一直躺着。
    git(root, &["add", "-A", "--", team_rel]).map(|_| ())
}

/// 提交 `.carter/` 里已经暂存的改动。没有改动就返回 `None`（不是错误 ——
/// 状态没变的时候「什么都不用提交」是正常结局）。
pub fn commit_team(root: &Path, team_rel: &str, message: &str) -> Result<Option<String>, String> {
    let staged = git(root, &["diff", "--cached", "--name-only", "--", team_rel])?;
    if staged.trim().is_empty() {
        return Ok(None);
    }
    git(root, &["commit", "-m", message, "--", team_rel])?;
    Ok(Some(git(root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default()))
}

/// 推当前分支。
///
/// 首次推一个还没有 upstream 的分支时要 `-u`，否则用户下次还得自己设。这里先试
/// `push`，失败再试 `push -u origin <branch>`：前者是常态，后者是首次。
pub fn push(root: &Path) -> Result<String, String> {
    if !has_remote(root) {
        return Err("没有配置远端（origin），只在本地记录".into());
    }
    match git(root, &["push"]) {
        Ok(out) => Ok(out),
        Err(_) => {
            let branch = current_branch(root)?;
            git(root, &["push", "-u", "origin", &branch])
        }
    }
}

// ---------------------------------------------------------------------------
// 同步
// ---------------------------------------------------------------------------

/// 同步一次：`fetch` + 快进合并。
///
/// ## 为什么只做快进
///
/// 这是**后台自动跑**的命令，由观察源定时触发，没有人在旁边看着。自动做一次真正的
/// merge 意味着它可能产出冲突、留下一个半合并的工作区 —— 而那时用户可能正在跑别的
/// 东西。所以这里只在「能干净快进」时才动，其余情况一律原样不动、把原因报出来，
/// 让人自己处理。自动化的边界是：**只做不可怕的事**。
///
/// ## 有本地改动时不动
///
/// 工作区脏的时候快进可能把用户的改动弄丢。这里直接拒绝，理由同上。
pub fn sync(root: &Path) -> Result<String, String> {
    if !has_remote(root) {
        return Err("没有配置远端，跳过".into());
    }
    let branch = current_branch(root)?;

    // 有未提交的改动就不同步。`.carter/` 自己刚写完还没提交的那份也算。
    let dirty = status_porcelain(root);
    if !dirty.trim().is_empty() {
        return Err(format!(
            "工作区有未提交的改动，不同步（{} 个文件）",
            dirty.lines().count()
        ));
    }

    git(root, &["fetch", "--prune", "origin"])?;
    let upstream = format!("origin/{branch}");
    if !git_ok(root, &["rev-parse", "--verify", &upstream]) {
        // 远端还没有这个分支：首次 push 由调用方决定，这里不算失败。
        return Ok("远端还没有这个分支，无需同步".into());
    }

    let behind = git(
        root,
        &["rev-list", "--count", &format!("HEAD..{upstream}")],
    )?
    .trim()
    .parse::<u32>()
    .unwrap_or(0);
    let ahead = git(
        root,
        &["rev-list", "--count", &format!("{upstream}..HEAD")],
    )?
    .trim()
    .parse::<u32>()
    .unwrap_or(0);

    if behind == 0 {
        return Ok(if ahead > 0 {
            format!("本地领先 {ahead} 条，待推送")
        } else {
            "已经是最新的".into()
        });
    }
    if ahead > 0 {
        return Err(format!(
            "本地与远端分叉（本地 {ahead} 条、远端 {behind} 条），需要人工合并"
        ));
    }

    // 只允许快进：`--ff-only` 拒绝任何需要真正合并的情况。
    git(root, &["merge", "--ff-only", &upstream])?;
    Ok(format!("快进 {behind} 条"))
}

/// 初始化团队时用：确保目录是个 git 仓库，没有就 `git init`。
pub fn ensure_repo(dir: &Path) -> Result<String, String> {
    if in_repo(dir) {
        return Ok(format!("已在 git 仓库里（{}）", repo_root(dir)?.display()));
    }
    git(dir, &["init"])?;
    Ok("已初始化 git 仓库".into())
}

/// 当前 HEAD 的短 SHA；还没有任何提交时返回 `None`。
pub fn head(root: &Path) -> Option<String> {
    git(root, &["rev-parse", "--short", "HEAD"])
        .ok()
        .filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailers_are_parsed_back_out() {
        // `%b` 是抬头之后的那一段，不含抬头本身 —— 夹具要照这个形状写。
        let raw = "abc123\u{1f}abc123full\u{1f}张三\u{1f}z@example.com\u{1f}1790000000\u{1f}carter/claim t-1 张三\u{1f}开始做这个\n\nCarter-Team: kind=claim id=t-1 goal=g-9\u{1e}";
        let evs = parse_events(raw);
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.kind, EventKind::Claim);
        assert_eq!(e.target.as_deref(), Some("t-1"));
        assert_eq!(e.goal_id.as_deref(), Some("g-9"));
        assert_eq!(e.author.as_ref().unwrap().name, "张三");
        assert_eq!(e.at, 1_790_000_000_000);
        assert_eq!(e.body, "开始做这个", "trailer 与抬头都不进正文");
    }

    #[test]
    fn commits_without_a_trailer_are_ignored() {
        let raw = "abc\u{1f}abc\u{1f}a\u{1f}a@x\u{1f}1\u{1f}普通提交\u{1f}正文\u{1e}";
        assert!(parse_events(raw).is_empty());
    }

    #[test]
    fn a_turn_of_team_edits_becomes_one_commit() {
        let n = std::process::id();
        let dir = std::env::temp_dir().join(format!("carter-team-turn-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "t"],
            vec!["config", "user.email", "t@example.com"],
        ] {
            std::process::Command::new("git")
                .current_dir(&dir)
                .args(&args)
                .output()
                .unwrap();
        }
        super::super::init::initialize(&dir, Some("t")).unwrap();

        // 模拟一轮里改了三处团队数据。
        let store = super::super::store::Store::open(dir.join(super::super::store::DIR));
        let me = Identity::parse("t <t@example.com>").unwrap();
        for i in 0..3 {
            store
                .save_goal(&super::super::model::Goal::new(format!("g{i}"), &me))
                .unwrap();
        }

        let sha = commit_turn(&dir, ".carter", 7).unwrap();
        assert!(sha.is_some(), "一轮的改动要收成一条提交");

        // 三次改动 → **一条**提交（不是三条）。
        let count = git(&dir, &["rev-list", "--count", "HEAD"]).unwrap();
        assert_eq!(count.trim(), "2", "初始化一条 + 本轮一条");

        // 再提交一次：没有改动就该安静返回 None。
        assert!(commit_turn(&dir, ".carter", 8).unwrap().is_none());
        assert_eq!(git(&dir, &["rev-list", "--count", "HEAD"]).unwrap().trim(), "2");

        // 提交信息要能读出这一轮干了什么，并且带 trailer。
        let msg = git(&dir, &["log", "-1", "--format=%s%n%b"]).unwrap();
        assert!(msg.contains("第 7 轮"), "{msg}");
        assert!(msg.contains("新建 3"), "{msg}");
        assert!(msg.contains("Carter-Team: kind=commit"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn linked_worktree_git_file_counts_as_a_repo() {
        // `.git` 是文件（worktree / submodule）时同样算仓库 —— 用 is_dir 判断会漏。
        let dir = std::env::temp_dir().join(format!("carter-team-wt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join(".git"), "gitdir: ../x\n").unwrap();
        assert!(in_repo(&dir.join("sub")));
        assert!(in_repo(&dir.join("sub").join("deeper")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
