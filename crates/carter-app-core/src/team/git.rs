//! git 通道：远端、快进同步、轮末提交。
//!
//! 团队数据分两类，各走一条通道：
//!
//! * **状态**（目标、任务、成员、看板）—— `.carter/` 下的文件，跟着代码分支走，
//!   由这里的 [`commit_turn`] 在每轮对话结束时收成一条提交，[`sync`] 快进同步。
//! * **事件**（评论、@、广播、指派、认领）—— 不在代码分支上，走独立的
//!   `refs/carter/events`，见 [`super::events`] 与 `docs/team-protocol.md`。
//!
//! v0 曾经把事件也打成当前分支上的空提交。那些旧事件仍然能被读出来（`events::read`
//! 会顺带扫当前分支），但不再往分支上写。
//!
//! ## 与观察源的关系
//!
//! 同步本身（`fetch` + 快进）会在初始化时自动建成一个观察源（见 `init.rs`），
//! 由卡特助理定时跑。那个命令**带副作用**：跑一次就把远端的东西拉下来。观察源
//! 的设计恰好容得下这一点 —— 它的语义是「定时跑一条命令，输出里新出现的行算事件」，
//! 而不是「跑一条只读命令」。

use std::path::Path;
use std::process::{Command, Stdio};

use super::model::Identity;

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
    remote_url(root).is_some()
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
/// 实时问 git「远端是什么」，而不是读 `team.json` 里那份。
///
/// `team.json` 的 `remote` 是**初始化那一刻**的快照。之后用户配了远端不会回填它 ——
/// 于是界面上会显示「没有远端」，而 git 那边明明配好了。以 git 为准：那份配置才是
/// `fetch` / `push` 真正看的东西。
///
/// **不假定远端叫 `origin`。** 上一版硬编码 `remote get-url origin`，远端叫
/// `upstream`、`team`、或者干脆是别的名字时，就会被判成「没有远端」——
/// 而 git 明明能用。团队仓库是用户自己的仓库，叫什么名字都合法。
///
/// 优先级：`origin` → 当前分支的上游 → 第一个远端。
pub fn remote_name(root: &Path) -> Option<String> {
    if git(root, &["remote", "get-url", "origin"]).is_ok() {
        return Some("origin".into());
    }
    // 当前分支已经跟踪的那个远端（`remotes/upstream/main` 取 `upstream`）。
    if let Ok(up) = git(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
    ) {
        if let Some((name, _)) = up.trim().split_once('/') {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    git(root, &["remote"])
        .ok()?
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// 远端地址。给界面显示用。
pub fn remote_url(root: &Path) -> Option<String> {
    let name = remote_name(root)?;
    git(root, &["remote", "get-url", &name])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn push(root: &Path) -> Result<String, String> {
    let Some(remote) = remote_name(root) else {
        return Err("还没有配远端，只在本地记录".into());
    };
    if git(root, &["push"]).is_ok() {
        return Ok(String::new());
    }
    let branch = current_branch(root)?;
    git(root, &["push", "-u", &remote, &branch])
}

/// 配（或改）远端。界面上那一个输入框走这里；不要求用户去终端。
///
/// 已经有一个远端叫 `origin` 时改它的地址，否则新建（`origin` 这个默认名优先，
/// 被占用就用 `origin` 之外的名字反而没必要 —— 直接覆盖更符合「我要改成这个」）。
pub fn set_remote(root: &Path, url: &str) -> Result<String, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("远端地址不能为空".into());
    }
    let name = remote_name(root).unwrap_or_else(|| "origin".into());
    if git(root, &["remote", "get-url", &name]).is_ok() {
        git(root, &["remote", "set-url", &name, url])?;
        Ok(format!("远端 {name} 已改为 {url}"))
    } else {
        git(root, &["remote", "add", &name, url])?;
        Ok(format!("已添加远端 {name} → {url}"))
    }
}

/// 把当前分支推上去并建立跟踪。
///
/// 首次推送之后，`fetch` / `merge` 才有东西可比 —— 所以「配好远端」之后下一步
/// 就该是它。没有远端时报错，而不是默默什么都不做。
pub fn publish(root: &Path) -> Result<String, String> {
    let Some(remote) = remote_name(root) else {
        return Err("还没有配远端".into());
    };
    let branch = current_branch(root)?;
    git(root, &["push", "-u", &remote, &branch])?;
    // 事件链在分支之外，`push <branch>` 不会带上它。
    let _ = super::events::push(root);
    Ok(format!("已推送 {branch} → {remote}"))
}

// ---------------------------------------------------------------------------
// 同步
// ---------------------------------------------------------------------------

/// 同步一次：先收发消息，再 `fetch` + 快进合并分支。
///
/// ## 为什么消息先做、而且不受工作区影响
///
/// 消息（事件链）不碰工作区，拉取与推送任何时候都安全。分支同步要求工作区干净 ——
/// 而用户写代码的时候工作区几乎总是脏的。两者绑在一起的话，「写代码期间收不到
/// 任何 @」，那这套消息就没法用了。
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
    let Some(remote) = remote_name(root) else {
        return Err("还没有配远端".into());
    };
    let messages = sync_events(root);
    let branch = sync_branch(root, &remote);
    match branch {
        Ok(b) => Ok(format!("{messages}；{b}")),
        // 分支没同步成，但消息照样收发了 —— 说清楚，免得用户以为什么都没发生。
        Err(e) => Err(format!("{messages}；分支没同步：{e}")),
    }
}

/// 收发消息，返回一句给人看的话。失败只说原因，不中断分支同步。
fn sync_events(root: &Path) -> String {
    let got = match super::events::fetch(root) {
        Ok(0) => "没有新消息".to_string(),
        Ok(n) => format!("收到 {n} 条新消息"),
        Err(e) => format!("拉取消息失败（{e}）"),
    };
    match super::events::push(root) {
        Ok(_) => got,
        Err(e) => format!("{got}，推送消息失败（{e}）"),
    }
}

fn sync_branch(root: &Path, remote: &str) -> Result<String, String> {
    let remote = remote.to_string();
    let branch = current_branch(root)?;

    // 有未提交的改动就不同步。`.carter/` 自己刚写完还没提交的那份也算。
    let dirty = status_porcelain(root);
    if !dirty.trim().is_empty() {
        return Err(format!(
            "工作区有未提交的改动，不同步（{} 个文件）",
            dirty.lines().count()
        ));
    }

    git(root, &["fetch", "--prune", &remote])?;
    let upstream = format!("{remote}/{branch}");
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

#[cfg(test)]
mod remote_tests {
    use super::*;

    fn repo(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("carter-remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "t"],
            vec!["config", "user.email", "t@example.com"],
        ] {
            std::process::Command::new("git")
                .current_dir(&dir)
                .args(&args)
                .output()
                .unwrap();
        }
        std::fs::write(dir.join("a"), "x").unwrap();
        std::process::Command::new("git")
            .current_dir(&dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(&dir)
            .args(["commit", "-qm", "init"])
            .output()
            .unwrap();
        dir
    }

    /// 这正是用户报的那次：远端不叫 origin，旧实现直接判成「没有远端」。
    #[test]
    fn a_remote_not_named_origin_is_still_found() {
        let dir = repo("named");
        assert!(remote_name(&dir).is_none());
        git(&dir, &["remote", "add", "upstream", "https://example.com/r.git"]).unwrap();

        assert_eq!(remote_name(&dir).as_deref(), Some("upstream"));
        assert_eq!(
            remote_url(&dir).as_deref(),
            Some("https://example.com/r.git"),
            "地址也要拿得到，界面要显示它"
        );
        assert!(has_remote(&dir), "不能因为名字不叫 origin 就说没有远端");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// origin 存在时优先用它 —— 这是约定俗成的名字，也多半是用户真正想推的那个。
    #[test]
    fn origin_wins_when_present() {
        let dir = repo("prefer");
        git(&dir, &["remote", "add", "upstream", "https://example.com/up.git"]).unwrap();
        git(&dir, &["remote", "add", "origin", "https://example.com/origin.git"]).unwrap();
        assert_eq!(remote_name(&dir).as_deref(), Some("origin"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_remote_adds_then_updates() {
        let dir = repo("set");
        let msg = set_remote(&dir, "https://example.com/a.git").unwrap();
        assert!(msg.contains("添加"), "{msg}");
        assert_eq!(remote_url(&dir).as_deref(), Some("https://example.com/a.git"));

        // 再配一次是改地址，不是加出第二个远端。
        let msg = set_remote(&dir, "https://example.com/b.git").unwrap();
        assert!(msg.contains("已改为"), "{msg}");
        assert_eq!(remote_url(&dir).as_deref(), Some("https://example.com/b.git"));
        assert_eq!(
            git(&dir, &["remote"]).unwrap().lines().count(),
            1,
            "不能多出第二个远端"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_url_is_rejected() {
        let dir = repo("empty");
        assert!(set_remote(&dir, "   ").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
