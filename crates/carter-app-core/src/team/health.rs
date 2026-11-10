//! 团队就绪状态：这个项目的团队协作到底有没有在工作。
//!
//! ## 为什么需要这一层
//!
//! 初始化是**一次性**的：`git init`、建 `.carter/` 骨架、首次提交，再顺手在助理里
//! 建两条观察源。这三件事建完之后没有任何地方能看见它们还在不在 —— 而它们分属
//! 三个互不相干的存储：
//!
//! * git 仓库在项目目录里；
//! * `.carter/` 是项目里的一堆文件；
//! * 观察源在 `~/.carter-app/assistant.json`，**不在 git 里**，删了就真没了。
//!
//! 任何一处失效的表现都是「团队同步静默停了」「消息一直不进来」，从界面上看不出
//! 是哪一处、也看不出该怎么修。这个模块把三处都探一遍，把「哪一处坏了」变成
//! 一句可以照着做的话。
//!
//! ## 三条判据
//!
//! 1. **正常的状态不该报警。** 没配远端是警告；还没加入团队时「团队消息」这条源
//!    本来就不该存在，那是「不适用」而不是「坏掉」。把关不严的话，用户每次打开都
//!    看见一片红，很快就不再看了。
//! 2. **每一项只说一句话。** 现在的实际情况是什么，一句话讲清（`detail`）；要动
//!    手时给一个动作（`action`），不给一串选项。
//! 3. **探测是只读的。** 这个模块不建目录、不写文件、不建观察源 —— 光看一眼状态
//!    不该产生副作用。所有写操作都在 [`super::init`] 里，由界面显式触发。
//!
//! ## 为什么不依赖 `AppShell`
//!
//! 观察源作为参数传进来（`&[Source]`，这个类型本来就在 core 里）。这样整个模块
//! 能脱离 webview 测，也让「探测用的是哪份源」在签名上一目了然。

use std::path::Path;

use serde::Serialize;

use super::store::DIR;
use super::{git, members, skills, store::Store};
use crate::assistant::Source;

/// 一项检查的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// 好着呢。
    Ok,
    /// 能用，但有该做的事（没配远端、有没提交的改动）。不拦人。
    Warn,
    /// 这一块没在工作，需要修。
    Broken,
    /// 这个项目里这一项本来就不成立（比如还没加入团队时的「团队消息」）。
    NotApplicable,
}

/// 一项检查。
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// 稳定标识，界面按它决定按钮调哪个动作。
    pub id: String,
    pub title: String,
    pub state: CheckState,
    /// 现在的实际情况，一句话。界面上原样显示。
    pub detail: String,
    /// 按钮文字。`None` = 这一项没有可点的动作。
    pub action: Option<String>,
    /// 动作的种类：`repo` / `files` / `sources` / `commit` / `remote`。
    /// 具体怎么做由调用方（Tauri 层）分发。
    pub action_kind: Option<String>,
    /// 仅 `files` 用：会写回哪些文件。界面拿它做确认弹窗。
    pub paths: Vec<String>,
}

impl Check {
    fn new(id: &str, title: &str, state: CheckState, detail: impl Into<String>) -> Self {
        Self {
            id: id.to_string(),
            title: title.to_string(),
            state,
            detail: detail.into(),
            action: None,
            action_kind: None,
            paths: Vec::new(),
        }
    }

    fn action(mut self, label: &str, kind: &str) -> Self {
        self.action = Some(label.to_string());
        self.action_kind = Some(kind.to_string());
        self
    }

    fn paths(mut self, paths: Vec<String>) -> Self {
        self.paths = paths;
        self
    }
}

/// 整个项目的就绪状态。
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    pub project_root: String,
    /// 团队目录的绝对路径。`.carter/` 不在时是 `None`。
    pub team_root: Option<String>,
    /// 本机有没有加入这个团队。界面据此决定「团队消息」那一行怎么解释。
    pub joined: bool,
    pub checks: Vec<Check>,
}

impl Health {
    /// 需要动手的项数（坏掉的 + 该修的），给 header 上的角标用。
    pub fn attention(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| matches!(c.state, CheckState::Broken | CheckState::Warn))
            .count()
    }
}

/// 这个项目根下有没有团队目录，有就打开。
fn open_team(project_root: &Path) -> Option<Store> {
    let root = project_root.join(DIR);
    root.is_dir().then(|| Store::open(root))
}

/// 探一遍。只读。
///
/// `sources` 传这个项目相关的全部观察源 —— 由调用方按 `cwd` 过滤，这里不猜。
pub fn inspect(project_root: &Path, sources: &[Source]) -> Health {
    let store = open_team(project_root);
    let joined = store
        .as_ref()
        .map(|s| {
            git::local_identity(project_root)
                .ok()
                .is_some_and(|i| members::load(s.root()).me(&i).is_some())
        })
        .unwrap_or(false);

    let mut checks = Vec::new();
    checks.push(repo_check(project_root));
    if let Some(c) = uncommitted_check(project_root, store.is_some()) {
        checks.push(c);
    }
    checks.push(remote_check(project_root));
    checks.push(skeleton_check(project_root, store.as_ref()));
    checks.push(joined_check(joined, store.is_some()));
    checks.push(sync_source_check(project_root, sources));
    checks.push(inbox_source_check(project_root, sources, joined));

    Health {
        project_root: project_root.to_string_lossy().into_owned(),
        team_root: store.map(|s| s.root().to_string_lossy().into_owned()),
        joined,
        checks,
    }
}

// ---------------------------------------------------------------------------
// 单项
// ---------------------------------------------------------------------------

fn repo_check(project_root: &Path) -> Check {
    const ID: &str = "git";
    const TITLE: &str = "git 仓库";

    if !git::in_repo(project_root) {
        return Check::new(
            ID,
            TITLE,
            CheckState::Broken,
            "这个目录不在 git 仓库里。团队数据靠 git 同步，没有仓库就只能是本机的。",
        )
        .action("初始化 git 仓库", "repo");
    }
    let branch = git::current_branch(project_root).ok();
    match (git::head(project_root), branch) {
        (None, _) => Check::new(
            ID,
            TITLE,
            CheckState::Warn,
            "仓库还是空的，一次提交都还没有。初始化团队会建出第一条。",
        ),
        (Some(sha), Some(branch)) => Check::new(
            ID,
            TITLE,
            CheckState::Ok,
            format!("在 {branch} 上，最新提交 {sha}"),
        ),
        // 游离 HEAD 是有意为之的用法（看某个历史版本），不是坏掉。
        (Some(sha), None) => Check::new(
            ID,
            TITLE,
            CheckState::Warn,
            format!("处在游离 HEAD 上（{sha}）。同步需要在一个分支上。"),
        ),
    }
}

fn uncommitted_check(project_root: &Path, has_team: bool) -> Option<Check> {
    if !has_team {
        return None;
    }
    const ID: &str = "dirty";
    const TITLE: &str = "未提交的团队改动";

    let paths = super::init::uncommitted(project_root);
    if paths.is_empty() {
        return Some(Check::new(
            ID,
            TITLE,
            CheckState::Ok,
            "团队目录里没有未提交的改动。",
        ));
    }
    let shown = paths.iter().take(3).cloned().collect::<Vec<_>>().join("、");
    let more = paths.len().saturating_sub(3);
    let tail = if more > 0 {
        format!(" 等 {more} 个")
    } else {
        String::new()
    };
    Some(
        Check::new(
            ID,
            TITLE,
            CheckState::Warn,
            format!(
                "{} 处改动还只在本地：{shown}{tail}。别人拉不到。",
                paths.len()
            ),
        )
        .action("提交一次", "commit"),
    )
}

fn remote_check(project_root: &Path) -> Check {
    const ID: &str = "remote";
    const TITLE: &str = "远端仓库";

    if !git::in_repo(project_root) {
        return Check::new(ID, TITLE, CheckState::NotApplicable, "还没有 git 仓库。");
    }
    match git::remote_url(project_root) {
        Some(url) => Check::new(ID, TITLE, CheckState::Ok, url),
        // 警告而不是错误：一个人的项目、或者暂时只在本地做，都是合理的。
        None => Check::new(
            ID,
            TITLE,
            CheckState::Warn,
            "还没有远端。团队数据目前只在这台机器上，别人拿不到。",
        )
        .action("配远端", "remote"),
    }
}

fn skeleton_check(project_root: &Path, store: Option<&Store>) -> Check {
    const ID: &str = "files";
    const TITLE: &str = "团队目录 .carter/";

    let root = project_root.join(DIR);
    if store.is_none() {
        let restore = super::init::restore_preview(project_root).unwrap_or_default();
        if restore.is_empty() {
            return Check::new(
                ID,
                TITLE,
                CheckState::Broken,
                "还没有团队目录。初始化之后才有目标、任务与看板。",
            )
            .action("初始化", "files");
        }
        return Check::new(
            ID,
            TITLE,
            CheckState::Broken,
            format!(
                "团队目录不见了，但 git 历史里还留着 {} 个文件 —— 能原样找回来。",
                restore.len()
            ),
        )
        .action("从 git 历史找回", "files")
        .paths(restore);
    }

    // 目录在，看缺了什么。
    let mut missing = Vec::new();
    if !root.join("team.json").is_file() {
        missing.push("team.json");
    }
    for dir in ["goals", "boards"] {
        if !root.join(dir).is_dir() {
            missing.push(dir);
        }
    }
    let pending_skills = skills::pending_builtin(&root.join("skills"));
    if !pending_skills.is_empty() {
        missing.push("内置技能");
    }

    if missing.is_empty() {
        return Check::new(ID, TITLE, CheckState::Ok, "结构与内置技能都齐。");
    }
    Check::new(
        ID,
        TITLE,
        CheckState::Broken,
        format!("缺了：{}。补齐不会覆盖已有的任何文件。", missing.join("、")),
    )
    .action("补齐", "files")
}

fn joined_check(joined: bool, has_team: bool) -> Check {
    const ID: &str = "joined";
    const TITLE: &str = "本机身份";

    if !has_team {
        return Check::new(ID, TITLE, CheckState::NotApplicable, "还没有团队目录。");
    }
    if joined {
        Check::new(ID, TITLE, CheckState::Ok, "本机已经以团队里的身份加入。")
    } else {
        Check::new(
            ID,
            TITLE,
            CheckState::NotApplicable,
            "本机还没有加入团队。加入之后才会收团队消息、才能被指派。",
        )
    }
}

/// 按命令找这个项目的源。认命令不认名字 —— 名字里带团队标签，用户改过就找不到了。
fn find_source<'a>(
    project_root: &Path,
    sources: &'a [Source],
    command: &str,
) -> Option<&'a Source> {
    sources
        .iter()
        .find(|s| s.command.trim() == command && same_dir(&s.cwd, project_root))
}

/// 观察源里存的是用户配置那一刻的字符串，和手上这个路径未必字面相同（末尾斜杠、
/// 符号链接、`~` 展开）。比的是「是不是同一个目录」，不是字符串。
fn same_dir(raw: &str, target: &Path) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return false;
    }
    let candidate = Path::new(raw);
    if candidate == target {
        return true;
    }
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    resolve(candidate) == resolve(target)
}

fn sync_source_check(project_root: &Path, sources: &[Source]) -> Check {
    const ID: &str = "sync_source";
    const TITLE: &str = "同步观察源";

    let cmd = crate::assistant::source::TEAM_SYNC;
    match find_source(project_root, sources, cmd) {
        None => Check::new(
            ID,
            TITLE,
            CheckState::Broken,
            "定时同步的那条观察源不见了。团队数据不会自动拉取与推送。",
        )
        .action("重新创建", "sources"),
        Some(s) if !s.enabled => Check::new(
            ID,
            TITLE,
            CheckState::Broken,
            "同步源被停用了。团队数据不会自动拉取与推送。",
        )
        .action("重新启用", "sources"),
        Some(s) => Check::new(
            ID,
            TITLE,
            CheckState::Ok,
            format!("每 {} 分钟一次{}。", s.every_minutes, last_run(s)),
        ),
    }
}

fn inbox_source_check(project_root: &Path, sources: &[Source], joined: bool) -> Check {
    const ID: &str = "inbox_source";
    const TITLE: &str = "消息观察源";

    // 没加入团队时这条源本来就不该有 —— 报成「坏掉」会让人去修一件不该修的事。
    if !joined {
        return Check::new(
            ID,
            TITLE,
            CheckState::NotApplicable,
            "加入团队之后才会有这条源。",
        );
    }
    let cmd = crate::assistant::source::TEAM_INBOX;
    match find_source(project_root, sources, cmd) {
        None => Check::new(
            ID,
            TITLE,
            CheckState::Broken,
            "收团队消息的那条观察源不见了。@ 你、派给你的任务都不会提醒。",
        )
        .action("重新创建", "sources"),
        Some(s) if !s.enabled => Check::new(
            ID,
            TITLE,
            CheckState::Broken,
            "消息源被停用了。@ 你、派给你的任务都不会提醒。",
        )
        .action("重新启用", "sources"),
        Some(s) => Check::new(
            ID,
            TITLE,
            CheckState::Ok,
            format!("每 {} 分钟一次{}。", s.every_minutes, last_run(s)),
        ),
    }
}

/// 「，上次检查 3 分钟前」/「，还没检查过」/「，上次失败：…」。
fn last_run(s: &Source) -> String {
    match (s.state.last_polled_at, s.state.last_error.as_deref()) {
        (_, Some(err)) if s.state.failures > 0 => format!("，上次失败：{err}"),
        (Some(at), _) => {
            let mins = crate::protocol::now_ms().saturating_sub(at) / 60_000;
            if mins == 0 {
                "，刚刚检查过".to_string()
            } else {
                format!("，{mins} 分钟前检查过")
            }
        }
        (None, _) => "，还没检查过".to_string(),
    }
}

#[cfg(test)]
mod tests;
