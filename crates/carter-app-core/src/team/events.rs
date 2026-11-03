//! 事件通道 v1：评论、@、广播、指派、认领，存在独立的 `refs/carter/events` 上。
//!
//! 协议全文见 `docs/team-protocol.md`。这里只说实现上的几个取舍。
//!
//! ## 为什么不再打在代码分支上
//!
//! v0 的事件是当前分支上的空提交（`git commit --allow-empty`）。用来写评论还凑合，
//! 当消息用就不成立了：你在 `feature/x` 上 @ 张三，张三在 `main` 上永远看不到，
//! 直到合并；PR 里还会多出一串 `carter/comment`，squash 一下讨论就没了。
//!
//! 现在每条事件是一个**空树提交**，用 `commit-tree` 生成、`update-ref` 写入一条独立
//! 的 ref。它不碰工作区、不碰索引、不碰任何分支 —— 用户正在半路的 rebase、暂存了
//! 一半的文件，都与它无关。
//!
//! ## 为什么推送冲突可以自动解决
//!
//! 事件只追加、彼此独立。两个人各自往本地那条链上加了几条，推送时后到的那个被拒，
//! 取回远端的链，把自己的新事件按原样（作者、时间、提交信息不变，只换父提交）
//! 重新接到远端链尾即可。这在代码分支上是 rebase，会冲突；在这里不会，因为每条
//! 提交的树都是同一棵空树。
//!
//! SHA 因此会变，所以事件之间靠 `eid` 引用，不靠 SHA。
//!
//! ## 读的时候并起来，不在拉的时候合并
//!
//! 拉取只是把远端的链放到 `refs/carter/remotes/<remote>/events`。读的时候把本地、
//! 各远端、以及当前分支上的 v0 旧事件一起 `git log`，按 SHA 去重。这样「拉取」
//! 是纯粹的只读动作，观察员在后台跑多少次都不会改坏任何东西。

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::git::{self, git};
use super::model::Identity;

/// 本地事件链。
pub const EVENTS_REF: &str = "refs/carter/events";
/// 远端事件链放在 `refs/carter/remotes/<remote>/events`。
const REMOTES_PREFIX: &str = "refs/carter/remotes";
/// git 的空树。每条事件的 tree 都是它。
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// 当前写出的协议版本。
pub const PROTOCOL_VERSION: u32 = 1;
/// 推送被拒后重试几次。
const PUSH_RETRIES: usize = 3;

// ---------------------------------------------------------------------------
// 数据
// ---------------------------------------------------------------------------

/// 事件种类。
///
/// 序列化成字符串而不是枚举：**不认识的种类也要能读出来、显示出来**（协议 2.4）。
/// 新版客户端写了一种旧版不认识的消息，旧版应该把它当普通消息显示，而不是吞掉 ——
/// 吞掉的话发消息的人以为对方看到了，对方其实什么都没看到。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventKind(pub String);

impl EventKind {
    pub const COMMENT: &'static str = "comment";
    pub const MENTION: &'static str = "mention";
    pub const BROADCAST: &'static str = "broadcast";
    pub const REQUEST: &'static str = "request";
    pub const ASSIGN: &'static str = "assign";
    pub const HANDOFF: &'static str = "handoff";
    pub const CLAIM: &'static str = "claim";
    pub const RELEASE: &'static str = "release";
    pub const JOIN: &'static str = "join";
    pub const NOTE: &'static str = "note";
    pub const SYNC: &'static str = "sync";

    /// 本版认识的全部种类。工具与界面的选项都从这里来。
    pub const KNOWN: &'static [&'static str] = &[
        Self::COMMENT,
        Self::MENTION,
        Self::BROADCAST,
        Self::REQUEST,
        Self::ASSIGN,
        Self::HANDOFF,
        Self::CLAIM,
        Self::RELEASE,
        Self::JOIN,
        Self::NOTE,
        Self::SYNC,
    ];

    pub fn new(s: &str) -> Self {
        Self(s.trim().to_ascii_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is(&self, s: &str) -> bool {
        self.0 == s
    }

    pub fn known(&self) -> bool {
        Self::KNOWN.contains(&self.0.as_str())
    }

    /// 给人看的动词。不认识的原样给出去。
    pub fn label(&self) -> &str {
        match self.0.as_str() {
            Self::COMMENT => "评论",
            Self::MENTION => "提到",
            Self::BROADCAST => "广播",
            Self::REQUEST => "请求",
            Self::ASSIGN => "指派",
            Self::HANDOFF => "转交",
            Self::CLAIM => "认领",
            Self::RELEASE => "释放",
            Self::JOIN => "加入",
            Self::NOTE => "进展",
            Self::SYNC => "同步",
            other => other,
        }
    }
}

/// 收件人里的「全员」。
pub const EVERYONE: &str = "*";

/// 一条事件。读出来的、要写下去的，都是它。
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    /// 提交的短 SHA。只用来显示与去重；引用请用 `eid`。
    pub sha: String,
    /// 协议版本。v0 的旧事件是 0。
    pub v: u32,
    pub kind: EventKind,
    /// 稳定的事件 id。v0 的旧事件没有，用 SHA 代替。
    pub eid: String,
    /// 对象：任务 / 目标 / 看板 id。
    pub target: Option<String>,
    pub goal_id: Option<String>,
    /// 发出者的成员 id。v0 没有，由读者按作者邮箱去匹配。
    pub from: Option<String>,
    /// 收件人成员 id。`*` = 全员。
    pub to: Vec<String>,
    /// `Some("carter")` = 由 agent 代发。
    pub via: Option<String>,
    /// 回复 / 引用的另一条事件的 `eid`。
    pub reply_to: Option<String>,
    pub due: Option<u64>,
    pub body: String,
    pub subject: String,
    pub author: Option<Identity>,
    pub at: u64,
    /// 来自 v0 的分支事件（只读）。
    pub legacy: bool,
}

impl Event {
    /// 这条消息是不是发给某个成员的（`to` 里有他，或者是全员）。
    pub fn addressed_to(&self, member: &str) -> bool {
        self.to.iter().any(|t| t == member || t == EVERYONE)
    }
}

/// 要写的一条事件。由调用方填好，[`append`] 负责补 `eid`、拼提交信息、落到 ref 上。
#[derive(Debug, Clone, Default)]
pub struct Draft {
    pub kind: String,
    pub target: Option<String>,
    pub goal_id: Option<String>,
    pub from: Option<String>,
    pub to: Vec<String>,
    pub via: Option<String>,
    pub reply_to: Option<String>,
    pub due: Option<u64>,
    /// 抬头。空 = 按种类与对象自动拼。
    pub subject: String,
    pub body: String,
}

/// 新事件 id：`e-<base36 毫秒>-<4 位随机>`，与任务 id 同一套生成规则。
pub fn new_eid() -> String {
    let t = super::model::new_id(super::model::Kind::Task);
    format!("e-{}", t.trim_start_matches("t-"))
}

// ---------------------------------------------------------------------------
// 提交信息：拼与解
// ---------------------------------------------------------------------------

/// trailer 的值里不能有空格（字段靠空格分隔）。有空格的值是调用方的 bug，
/// 这里直接拒绝，而不是悄悄替换 —— 替换过的 id 指向的就不是原来那个东西了。
fn check_value(key: &str, v: &str) -> Result<(), String> {
    if v.is_empty() || v.chars().any(|c| c.is_whitespace() || c == ',' && key != "to") {
        return Err(format!("事件字段 {key} 的值 `{v}` 不合法（不能为空、不能有空格）"));
    }
    Ok(())
}

/// 把一条草稿拼成提交信息。返回 `(message, eid)`。
pub fn render(d: &Draft) -> Result<(String, String), String> {
    let kind = EventKind::new(&d.kind);
    if kind.as_str().is_empty() {
        return Err("事件缺少 kind".into());
    }
    check_value("kind", kind.as_str())?;
    let eid = new_eid();
    let mut trailer = format!("Carter-Team: v={PROTOCOL_VERSION} kind={}", kind.as_str());
    let mut field = |k: &str, v: &str| -> Result<(), String> {
        check_value(k, v)?;
        trailer.push_str(&format!(" {k}={v}"));
        Ok(())
    };
    if let Some(t) = d.target.as_deref().filter(|s| !s.is_empty()) {
        field("id", t)?;
    }
    if let Some(g) = d.goal_id.as_deref().filter(|s| !s.is_empty()) {
        field("goal", g)?;
    }
    if let Some(f) = d.from.as_deref().filter(|s| !s.is_empty()) {
        field("from", f)?;
    }
    let to: Vec<&str> = d
        .to
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if !to.is_empty() {
        for t in &to {
            check_value("to", t)?;
        }
        field("to", &to.join(","))?;
    }
    if let Some(v) = d.via.as_deref().filter(|s| !s.is_empty()) {
        field("via", v)?;
    }
    if let Some(r) = d.reply_to.as_deref().filter(|s| !s.is_empty()) {
        field("ref", r)?;
    }
    if let Some(due) = d.due {
        field("due", &due.to_string())?;
    }
    field("eid", &eid)?;

    let subject = if d.subject.trim().is_empty() {
        let mut s = format!("carter/{}", kind.as_str());
        if let Some(t) = d.target.as_deref() {
            s.push(' ');
            s.push_str(t);
        }
        s
    } else {
        // 抬头只能是一行：多行的话 `%s` 只取第一行，后面的会混进正文。
        d.subject.lines().next().unwrap_or("").trim().to_string()
    };
    let body = d.body.trim();
    let message = if body.is_empty() {
        format!("{subject}\n\n{trailer}\n")
    } else {
        format!("{subject}\n\n{body}\n\n{trailer}\n")
    };
    Ok((message, eid))
}

/// `git log` 用的格式：字段之间 `\x1f`、提交之间 `\x1e`（正文里可能有换行）。
const LOG_FORMAT: &str = "--format=%h%x1f%H%x1f%an%x1f%ae%x1f%ct%x1f%s%x1f%b%x1e";

/// 解析 `git log --format=LOG_FORMAT` 的输出。`legacy` 标记这批是不是来自代码分支。
fn parse_log(raw: &str, legacy: bool) -> Vec<(String, Event)> {
    let mut out = Vec::new();
    for chunk in raw.split('\u{1e}') {
        let chunk = chunk.trim_start_matches('\n');
        if chunk.trim().is_empty() {
            continue;
        }
        let mut parts = chunk.split('\u{1f}');
        let sha = parts.next().unwrap_or("").trim().to_string();
        let full = parts.next().unwrap_or("").trim().to_string();
        let an = parts.next().unwrap_or("").trim();
        let ae = parts.next().unwrap_or("").trim();
        let ct = parts.next().unwrap_or("").trim();
        let subject = parts.next().unwrap_or("").trim().to_string();
        let body = parts.next().unwrap_or("");
        let author = if ae.is_empty() {
            None
        } else {
            Identity::parse(&format!("{an} <{ae}>"))
        };
        let at = ct.parse::<u64>().unwrap_or(0).saturating_mul(1000);
        if let Some(mut e) = parse_message(&subject, body, author, at) {
            if e.eid.is_empty() {
                e.eid = sha.clone();
            }
            e.sha = sha;
            e.legacy = legacy;
            out.push((full, e));
        }
    }
    out
}

/// 从抬头与正文里解出一条事件。没有 `Carter-Team:` trailer 的提交返回 `None`。
pub fn parse_message(
    subject: &str,
    body: &str,
    author: Option<Identity>,
    at: u64,
) -> Option<Event> {
    let trailer = body
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("Carter-Team:"))
        .map(|l| l.trim_start().trim_start_matches("Carter-Team:").trim())?;

    let mut e = Event {
        sha: String::new(),
        v: 0,
        kind: EventKind::new(""),
        eid: String::new(),
        target: None,
        goal_id: None,
        from: None,
        to: Vec::new(),
        via: None,
        reply_to: None,
        due: None,
        body: String::new(),
        subject: subject.to_string(),
        author,
        at,
        legacy: false,
    };
    for field in trailer.split_whitespace() {
        let Some((k, v)) = field.split_once('=') else {
            continue;
        };
        match k {
            "v" => e.v = v.parse().unwrap_or(0),
            "kind" => e.kind = EventKind::new(v),
            "eid" => e.eid = v.to_string(),
            "id" => e.target = Some(v.to_string()),
            "goal" => e.goal_id = Some(v.to_string()),
            "from" => e.from = Some(v.to_string()),
            "to" => {
                e.to = v
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            "via" => e.via = Some(v.to_string()),
            "ref" => e.reply_to = Some(v.to_string()),
            "due" => e.due = v.parse().ok(),
            // 不认识的字段忽略（协议 2.4）。
            _ => {}
        }
    }
    // 缺 kind 的不是事件。`kind=commit` 是轮末那条「本轮改了哪些文件」的状态提交，
    // 也不是消息，不进事件流。
    if e.kind.as_str().is_empty() || e.kind.is("commit") {
        return None;
    }
    e.body = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("Carter-Team:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    Some(e)
}

// ---------------------------------------------------------------------------
// ref 操作
// ---------------------------------------------------------------------------

fn rev(root: &Path, r: &str) -> Option<String> {
    git(root, &["rev-parse", "--verify", "--quiet", &format!("{r}^{{commit}}")])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 本地事件链的链尾。还没有任何事件时是 `None`。
pub fn local_tip(root: &Path) -> Option<String> {
    rev(root, EVENTS_REF)
}

fn remote_ref(remote: &str) -> String {
    format!("{REMOTES_PREFIX}/{remote}/events")
}

/// 往本地事件链上追加一条。返回 `(短 SHA, eid)`。
///
/// 用 `update-ref <ref> <new> <old>` 做比较并交换：本机上同时有两个地方在写
/// （界面点了一下、模型同时调了工具）时，后写的那个会失败并重试一次，而不是
/// 把先写的那条从链上挤掉。
pub fn append(root: &Path, d: &Draft) -> Result<(String, String), String> {
    if !git::git_available() {
        return Err("找不到 git，事件无处可写".into());
    }
    if !git::in_repo(root) {
        return Err("这个目录不是 git 仓库，事件无处可写".into());
    }
    let (message, eid) = render(d)?;
    for _ in 0..3 {
        let parent = local_tip(root);
        let mut args = vec!["commit-tree", EMPTY_TREE];
        if let Some(p) = parent.as_deref() {
            args.push("-p");
            args.push(p);
        }
        args.push("-m");
        args.push(&message);
        let new = git(root, &args)?;
        let new = new.trim();
        // 期望旧值：没有链时是空串（= 必须不存在），这样「两个人同时写第一条」也是安全的。
        let old = parent.unwrap_or_default();
        if git(root, &["update-ref", EVENTS_REF, new, &old]).is_ok() {
            let short = git(root, &["rev-parse", "--short", new]).unwrap_or_else(|_| new.into());
            return Ok((short, eid));
        }
    }
    Err("写事件时本地事件链一直在变，稍后再试".into())
}

/// 拉取所有远端的事件链。没有远端时什么都不做（返回 `Ok(0)`）。
///
/// 用通配 refspec：远端还没有任何事件时它安静地什么都不拉，而指名道姓地拉
/// `refs/carter/events` 会报「找不到远端 ref」。
pub fn fetch(root: &Path) -> Result<usize, String> {
    let Some(remote) = git::remote_name(root) else {
        return Ok(0);
    };
    let spec = format!("+refs/carter/*:{REMOTES_PREFIX}/{remote}/*");
    let before = rev(root, &remote_ref(&remote));
    git(root, &["fetch", "--quiet", "--no-tags", &remote, &spec])?;
    let after = rev(root, &remote_ref(&remote));
    if before == after {
        return Ok(0);
    }
    let range = match before {
        Some(b) => format!("{b}..{}", after.unwrap_or_default()),
        None => after.unwrap_or_default(),
    };
    Ok(git(root, &["rev-list", "--count", &range])
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0))
}

/// 推本地事件链。被拒时把自己的新事件接到远端链尾后面再推（见模块头）。
///
/// 没有远端时返回 `Ok(false)`：只在本地记着，下次配好远端再推。
pub fn push(root: &Path) -> Result<bool, String> {
    let Some(remote) = git::remote_name(root) else {
        return Ok(false);
    };
    let Some(_) = local_tip(root) else {
        return Ok(true);
    };
    let spec = format!("{EVENTS_REF}:{EVENTS_REF}");
    let mut last = String::new();
    for _ in 0..PUSH_RETRIES {
        match git(root, &["push", "--quiet", &remote, &spec]) {
            Ok(_) => {
                // 推上去之后远端就是本地这份，顺手记下来，省一次 fetch。
                if let Some(tip) = local_tip(root) {
                    let _ = git(root, &["update-ref", &remote_ref(&remote), &tip]);
                }
                return Ok(true);
            }
            Err(e) => last = e,
        }
        fetch(root)?;
        rebase_onto_remote(root, &remote)?;
    }
    Err(format!("推送团队消息失败：{last}"))
}

/// 把「本地有、远端没有」的事件逐条重新接到远端链尾，然后把本地 ref 指过去。
///
/// 每条用原来的作者与作者时间重建（`GIT_AUTHOR_*`），提交信息原样保留 ——
/// 只有父提交变了，所以语义不变。
fn rebase_onto_remote(root: &Path, remote: &str) -> Result<(), String> {
    let Some(theirs) = rev(root, &remote_ref(remote)) else {
        return Ok(());
    };
    let Some(ours) = local_tip(root) else {
        return Ok(());
    };
    // 远端已经包含了本地（只是 push 那一下网络抖了）：直接快进。
    if git(root, &["merge-base", "--is-ancestor", &ours, &theirs]).is_ok() {
        return git(root, &["update-ref", EVENTS_REF, &theirs, &ours]).map(|_| ());
    }
    let mine = git(
        root,
        &["rev-list", "--reverse", &format!("{theirs}..{ours}")],
    )?;
    let mut parent = theirs.clone();
    for sha in mine.lines().map(str::trim).filter(|s| !s.is_empty()) {
        let meta = git(root, &["log", "-1", "--format=%an%x1f%ae%x1f%ad%x1f%B", "--date=raw", sha])?;
        let mut it = meta.splitn(4, '\u{1f}');
        let (an, ae, ad, msg) = (
            it.next().unwrap_or(""),
            it.next().unwrap_or(""),
            it.next().unwrap_or(""),
            it.next().unwrap_or(""),
        );
        let out = commit_tree_as(root, &parent, msg, an, ae, ad)?;
        parent = out;
    }
    git(root, &["update-ref", EVENTS_REF, &parent, &ours]).map(|_| ())
}

fn commit_tree_as(
    root: &Path,
    parent: &str,
    message: &str,
    name: &str,
    email: &str,
    date: &str,
) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(root)
        .args(["commit-tree", EMPTY_TREE, "-p", parent, "-m", message])
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_AUTHOR_DATE", date)
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    crate::platform::hide_console(&mut cmd);
    let out = cmd.output().map_err(|e| format!("起 git 失败：{e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// 读
// ---------------------------------------------------------------------------

/// 读最近 `limit` 条事件：本地链 + 各远端链 + 当前分支上的 v0 旧事件，按 SHA 去重，
/// 新的在前。
pub fn read(root: &Path, limit: usize) -> Vec<Event> {
    if !git::git_available() || !git::in_repo(root) {
        return Vec::new();
    }
    let count = format!("-n{limit}");
    let mut all: Vec<(String, Event)> = Vec::new();

    // 事件链：`--glob=refs/carter` 一次覆盖本地与所有远端。没有任何事件时
    // git 什么都不输出，不报错。
    if let Ok(raw) = git(root, &["log", &count, "--glob=refs/carter", LOG_FORMAT]) {
        all.extend(parse_log(&raw, false));
    }
    // v0：当前分支上带 trailer 的空提交。还没有任何提交（空仓库）时 `log HEAD` 会报错，
    // 那就是没有。
    if let Ok(raw) = git(
        root,
        &[
            "log",
            &count,
            "--grep=Carter-Team:",
            "--fixed-strings",
            "HEAD",
            LOG_FORMAT,
        ],
    ) {
        all.extend(parse_log(&raw, true));
    }

    let mut seen = HashSet::new();
    all.retain(|(full, _)| seen.insert(full.clone()));
    let mut out: Vec<Event> = all.into_iter().map(|(_, e)| e).collect();
    // 提交时间只到秒：同一秒里连写两条（界面点一下、模型紧跟着又发一条）时间相同。
    // 同秒的不按 SHA 排（那是随机的），保留 `git log` 给的顺序 —— 它沿着父子链走，
    // 后写的在前。`sort_by` 是稳定排序，所以只按时间排就够了。
    out.sort_by(|a, b| b.at.cmp(&a.at));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo(tag: &str, name: &str, email: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("carter-ev-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["config", "user.name", name]);
        sh(&dir, &["config", "user.email", email]);
        std::fs::write(dir.join("a"), "x").unwrap();
        sh(&dir, &["add", "-A"]);
        sh(&dir, &["commit", "-qm", "init"]);
        dir
    }

    fn draft(kind: &str, body: &str) -> Draft {
        Draft {
            kind: kind.into(),
            body: body.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_rendered_event_parses_back_with_every_field() {
        let d = Draft {
            kind: "mention".into(),
            target: Some("t-1".into()),
            goal_id: Some("g-9".into()),
            from: Some("m-a".into()),
            to: vec!["m-b".into(), "m-c".into()],
            via: Some("carter".into()),
            reply_to: Some("e-0".into()),
            due: Some(42),
            subject: String::new(),
            body: "接口定好了\n可以联调".into(),
        };
        let (msg, eid) = render(&d).unwrap();
        let (subject, rest) = msg.split_once("\n\n").unwrap();
        let e = parse_message(subject, rest, None, 0).unwrap();
        assert_eq!(e.v, PROTOCOL_VERSION);
        assert!(e.kind.is("mention"));
        assert_eq!(e.eid, eid);
        assert_eq!(e.target.as_deref(), Some("t-1"));
        assert_eq!(e.goal_id.as_deref(), Some("g-9"));
        assert_eq!(e.from.as_deref(), Some("m-a"));
        assert_eq!(e.to, vec!["m-b", "m-c"]);
        assert_eq!(e.via.as_deref(), Some("carter"));
        assert_eq!(e.reply_to.as_deref(), Some("e-0"));
        assert_eq!(e.due, Some(42));
        assert_eq!(e.body, "接口定好了\n可以联调", "trailer 不进正文，多行正文原样保留");
        assert!(e.addressed_to("m-c"));
        assert!(!e.addressed_to("m-a"));
    }

    #[test]
    fn values_with_spaces_are_rejected_not_mangled() {
        let mut d = draft("comment", "x");
        d.target = Some("t 1".into());
        assert!(render(&d).is_err());
    }

    /// 协议 2.4：不认识的种类要读得出来。
    #[test]
    fn unknown_kinds_are_kept() {
        let e = parse_message("x", "正文\n\nCarter-Team: v=7 kind=poll eid=e-1", None, 0).unwrap();
        assert_eq!(e.kind.as_str(), "poll");
        assert!(!e.kind.known());
        assert_eq!(e.kind.label(), "poll");
        assert_eq!(e.v, 7);
    }

    /// 轮末的状态提交（`kind=commit`）不是消息。
    #[test]
    fn turn_commits_are_not_events() {
        assert!(parse_message("chore", "Carter-Team: kind=commit turn=3", None, 0).is_none());
    }

    #[test]
    fn appending_never_touches_the_branch_or_worktree() {
        let dir = repo("isolated", "张三", "z@x");
        let head = sh(&dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("b"), "staged").unwrap();
        sh(&dir, &["add", "b"]);

        append(&dir, &draft("comment", "第一条")).unwrap();
        append(&dir, &draft("broadcast", "第二条")).unwrap();

        assert_eq!(sh(&dir, &["rev-parse", "HEAD"]), head, "分支不能动");
        assert_eq!(
            sh(&dir, &["diff", "--cached", "--name-only"]),
            "b",
            "用户暂存的文件原样留着"
        );
        let evs = read(&dir, 10);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].body, "第二条", "新的在前");
        assert_eq!(evs[0].author.as_ref().unwrap().name, "张三");
        assert!(!evs[0].legacy);
    }

    #[test]
    fn legacy_branch_events_are_still_read() {
        let dir = repo("legacy", "a", "a@x");
        sh(
            &dir,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "carter/comment t-1",
                "-m",
                "旧评论",
                "-m",
                "Carter-Team: kind=comment id=t-1 goal=g-1",
            ],
        );
        append(&dir, &draft("comment", "新评论")).unwrap();
        let evs = read(&dir, 10);
        assert_eq!(evs.len(), 2, "{evs:?}");
        let old = evs.iter().find(|e| e.legacy).expect("要读到 v0 的那条");
        assert_eq!(old.v, 0);
        assert_eq!(old.body, "旧评论");
        assert_eq!(old.eid, old.sha, "v0 没有 eid，用 SHA 顶上");
    }

    /// 核心场景：两个人各自写了事件，后推的那个被拒，自动接到对方后面。
    #[test]
    fn concurrent_writers_converge_without_conflict() {
        let base = std::env::temp_dir().join(format!(
            "carter-ev-race-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let remote = base.join("r.git");
        std::fs::create_dir_all(&remote).unwrap();
        sh(&remote, &["init", "--bare", "-q"]);

        let a = repo("race-a", "张三", "z@x");
        let b = repo("race-b", "李四", "l@x");
        for d in [&a, &b] {
            sh(d, &["remote", "add", "origin", remote.to_str().unwrap()]);
        }

        // 第一次推之前远端什么都没有，拉取不能报错。
        assert_eq!(fetch(&b).unwrap(), 0);

        append(&a, &draft("comment", "张三 1")).unwrap();
        append(&b, &draft("comment", "李四 1")).unwrap();
        append(&b, &draft("broadcast", "李四 2")).unwrap();
        assert!(push(&a).unwrap());
        // 李四的推送会被拒一次，然后自动接到张三后面。
        assert!(push(&b).unwrap());

        fetch(&a).unwrap();
        let bodies: Vec<String> = read(&a, 10).into_iter().map(|e| e.body).collect();
        for want in ["张三 1", "李四 1", "李四 2"] {
            assert!(bodies.iter().any(|b| b == want), "张三那边缺「{want}」：{bodies:?}");
        }
        assert_eq!(bodies.len(), 3, "不能重复：{bodies:?}");

        // 重接之后作者不能变成推送的人。
        let li = read(&b, 10).into_iter().find(|e| e.body == "李四 2").unwrap();
        assert_eq!(li.author.unwrap().name, "李四");

        // 两边的链尾一致。
        fetch(&b).unwrap();
        assert_eq!(local_tip(&b), rev(&b, &remote_ref("origin")));
        for d in [&base, &a, &b] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
