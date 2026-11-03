//! 给已有项目做团队初始化：`git init`（必要时）+ 建 `.carter/` 标准结构 + 首次提交。
//!
//! ## 为什么初始化要写进 git，而不是只建几个目录
//!
//! 团队协作的全部同步、认领裁决、评论历史都建立在「这些文件在 git 里」之上。
//! 建了目录却不提交，界面看起来是好的，第二个人 clone 下来却什么都没有 —— 而这类
//! 失败在第一个人的机器上完全不可见。所以初始化**必须**走到「有一次提交」为止，
//! 并且把结果明确报出来。
//!
//! ## 已经初始化过的不重来
//!
//! 第二次点「初始化」不该覆盖任何东西。这里只补齐缺的骨架（有人漏了某个目录、
//! 手动删过一次），已有的 `team.json` 与数据一律不动。
//!
//! ## `.carter/.gitignore` 只忽略 `.cache/`
//!
//! 派生数据（成员表、跨目标汇总）每次都能从 git 历史与实体文件重建，进版本库只会
//! 制造噪音与无谓冲突。反过来，`.carter/` 里其余的东西**都必须**进 git —— 所以
//! 不用「整个 `.carter/` 加白名单」那种写法，只在里面放一条针对 `.cache/` 的规则。

use std::path::Path;

use super::git;
use super::model::{Identity, TeamMeta};
use super::store::{DIR, Store};
use super::skills;

/// 初始化的结果。界面拿它显示「做了什么」。
#[derive(Debug, Clone, serde::Serialize)]
pub struct InitOutcome {
    /// 团队目录的绝对路径。
    pub root: String,
    /// 项目根（`.carter/` 的上一级）的绝对路径。
    pub project_root: String,
    /// 这次实际动了什么，按顺序。
    pub steps: Vec<String>,
    /// 首次提交的短 SHA。`None` = 没有可提交的东西（已经初始化过）。
    pub commit: Option<String>,
    /// 本机的 git 身份 —— 界面上要显示「你以谁的身份参与」。
    pub identity: Identity,
    /// 远端地址（有的话）。
    pub remote: Option<String>,
    /// 同步观察源是否已经建好。
    pub sync_source: bool,
    /// 不致命、但用户该知道的（比如没有远端、之后要手动 push）。
    pub warnings: Vec<String>,
}

/// 初始化一个项目。
///
/// `project_root` 是项目根目录（不是 `.carter/` 本身）。
/// `team_name` 为空时用目录名。
///
/// 这个函数**只做文件与 git 的事**，不碰助理状态 —— 建观察源在
/// [`create_sync_source`] 里，因为那需要 `AppShell`，而这一层刻意不依赖它。
pub fn initialize(project_root: &Path, team_name: Option<&str>) -> Result<InitOutcome, String> {
    if !git::git_available() {
        return Err("本机没有 git。团队同步依赖 git，先装一个再初始化。".into());
    }
    if !project_root.is_dir() {
        return Err(format!("{} 不是一个存在的目录", project_root.display()));
    }

    let mut steps = Vec::new();
    let mut warnings = Vec::new();

    steps.push(git::ensure_repo(project_root)?);
    let repo_root = git::repo_root(project_root)?;
    if repo_root != project_root {
        warnings.push(format!(
            "{} 在 git 仓库 {} 里，团队数据会落在这个仓库里",
            project_root.display(),
            repo_root.display()
        ));
    }

    let identity = git::local_identity(&repo_root)?;

    let root = project_root.join(DIR);
    let existed = root.join("team.json").is_file();
    create_skeleton(&root, project_root, team_name, &identity, &mut steps)?;

    // 本机的 `~/.carter` 与会话 cwd 之间，项目级技能要靠这个目录被发现；
    // 顺带它就是团队目录本身，不需要额外做什么。
    let store = Store::open(&root);
    steps.push("已就绪：目标 / 任务 / 看板".into());
    let _ = store;

    // 首次提交：把所有骨架与数据一起提进去。用 `--` 限定路径，别把用户工作区里
    // 跟团队无关的改动捎带提交。
    let team_rel = relative_to(&repo_root, &root)?;
    git::stage_team(&repo_root, &team_rel)?;
    let commit = if existed {
        None
    } else {
        git::commit_team(
            &repo_root,
            &team_rel,
            "chore(carter): 初始化团队目标 / 任务 / 看板目录",
        )?
    };
    if commit.is_some() {
        steps.push("已提交 .carter/ 标准结构".into());
    } else if !existed {
        steps.push("没有任何东西需要提交（可能已经提交过）".into());
    }

    let remote = git::has_remote(&repo_root).then(|| {
        git::git(&repo_root, &["remote", "get-url", "origin"]).unwrap_or_default()
    });
    if remote.as_deref().unwrap_or("").trim().is_empty() {
        warnings.push("还没有配置远端（origin）。团队同步需要各人指向同一个 git 仓库。".into());
    }

    Ok(InitOutcome {
        root: root.to_string_lossy().into_owned(),
        project_root: project_root.to_string_lossy().into_owned(),
        steps,
        commit,
        identity,
        remote: remote.filter(|r| !r.trim().is_empty()),
        sync_source: false,
        warnings,
    })
}

/// 建目录骨架与那些「只补缺、不覆盖」的文件。
fn create_skeleton(
    root: &Path,
    project_root: &Path,
    team_name: Option<&str>,
    identity: &Identity,
    steps: &mut Vec<String>,
) -> Result<(), String> {
    let dirs = [
        root.to_path_buf(),
        root.join("goals"),
        root.join("boards"),
    ];
    let mut made = false;
    for d in &dirs {
        if !d.is_dir() {
            std::fs::create_dir_all(d).map_err(|e| format!("建 {} 失败：{e}", d.display()))?;
            made = true;
        }
    }
    if made {
        steps.push(format!("建好 {}", root.display()));
    }

    // team.json：只在没有的时候写。
    let meta_path = root.join("team.json");
    if !meta_path.is_file() {
        let name = team_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                project_root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "team".into());
        let mut meta = TeamMeta::new(name, identity);
        meta.remote = git::git(project_root, &["remote", "get-url", "origin"])
            .ok()
            .filter(|s| !s.trim().is_empty());
        Store::open(root).save_meta(&meta)?;
        steps.push("写入 team.json".into());
    }

    // .gitignore：只忽略 .cache/。已经存在就不动（用户可能加过别的规则）。
    let ignore = root.join(".gitignore");
    if !ignore.is_file() {
        std::fs::write(
            &ignore,
            "# 派生数据：成员表、跨目标汇总。都能从实体文件与 git 历史重建。\n\
             .cache/\n\
             # 原子写的临时文件（进程被强杀时可能残留）。\n\
             *.tmp\n",
        )
        .map_err(|e| format!("写 {} 失败：{e}", ignore.display()))?;
        steps.push("写入 .gitignore（忽略 .cache/）".into());
    }

    // 不再写 `.gitattributes`：它当初只为把 `bin/carter-team` 钉成 LF（CRLF 会弄坏
    // shebang），脚本已经不装了。已有仓库里的那份不删 —— 它无害，用户也可能往里
    // 加过自己的规则。
    // 技能：只补缺。用户改过的 SKILL.md 不会被覆盖。
    let skills_dir = root.join("skills");
    if !skills_dir.is_dir() {
        std::fs::create_dir_all(&skills_dir).map_err(|e| format!("建技能目录失败：{e}"))?;
    }
    let written = skills::install_builtin(&skills_dir)?;
    if !written.is_empty() {
        steps.push(format!("内置技能：{}", written.join("、")));
    }
    // 旧版初始化装过的命令行脚本：现在一切走 `team` 工具，留着它只会让 agent
    // 多一条走不通（Windows）或行为不一致的路。
    if let Some(note) = skills::retire_script(project_root)? {
        steps.push(note);
    }

    Ok(())
}

/// `.carter/` 相对仓库根的路径，永远用 `/` 分隔。
///
/// git 在 Windows 上也认 `/`，混用 `\` 会让 `git add -- <path>` 的 pathspec
/// 匹配不上 —— 那种失败表现为「提交成功但只提交了部分文件」。
fn relative_to(repo_root: &Path, target: &Path) -> Result<String, String> {
    // 两边都要先解析成真实路径再比。`git rev-parse --show-toplevel` 给的是**解析过
    // 符号链接**的路径（macOS 上 `/var` → `/private/var`，Linux 上 `/tmp` 也常是链接），
    // 而调用方手里那个是用户给的、未被解析的路径 —— 直接 `strip_prefix` 必然对不上，
    // 表现为「初始化到一半说团队目录不在仓库里」。
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (repo_root, target) = (canon(repo_root), canon(target));
    let rel = target
        .strip_prefix(&repo_root)
        .map_err(|_| format!("{} 不在仓库 {} 里", target.display(), repo_root.display()))?;
    let s = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    // 团队目录就在仓库根上时 `file_name()` 是 ".carter"，不会是空串。
    Ok(if s.is_empty() { DIR.to_string() } else { s })
}

/// 自动建（或补齐）同步观察源。
///
/// ## 为什么不再调 `carter-team` 脚本
///
/// 上一版这条源跑的是 `.carter/bin/carter-team sync`。那是 `#!/bin/sh` + `jq`，
/// 在 Linux / macOS 上没问题，**在 Windows 上根本起不来**：没有 `sh`、也没有
/// `jq`，而且 git 检出时 `core.autocrlf` 会把脚本转成 CRLF，shebang 变成
/// `#!/bin/sh\r` —— 连 Git Bash 都会报 "bad interpreter"。
///
/// 同步本身只是「拉一下」，一条 `git pull --ff-only` 就够了，而 git 是三平台
/// 都有的。把这条通道从脚本上摘下来，Windows 用户的自动同步才真正成立。
///
/// 推送不在这里做：本地改动由轮末提交时顺带推（见 `git::commit_turn_if_team`）。
/// 分开的理由是观察源每 10 分钟跑一次，而推送应该跟着「真的改了东西」走。
///
/// 幂等：已经有同名的源就更新命令与目录，不重复添加。
pub fn create_sync_source(
    shell: &std::sync::Arc<crate::shell::AppShell>,
    project_root: &Path,
    team_label: &str,
) -> Result<bool, String> {
    if !git::has_remote(project_root) {
        // 没有远端时建一个只会一直报错的源，比不建更糟。
        return Ok(false);
    }
    // `--ff-only`：只在能干净快进时才动。后台自动跑的东西不该产出冲突。
    // 命令里不写路径 —— 观察源自带 cwd，而拼路径要处理三平台的引号规则。
    let command = "git pull --ff-only".to_string();
    let name = format!("{team_label} · 团队同步");

    let existing = shell
        .assistant
        .snapshot()
        .sources
        .into_iter()
        .find(|s| s.name == name);

    let mut source = existing.unwrap_or_else(|| crate::assistant::Source {
        id: String::new(),
        name: name.clone(),
        command: String::new(),
        cwd: String::new(),
        every_minutes: 10,
        enabled: true,
        state: Default::default(),
    });
    source.command = command;
    source.cwd = project_root.to_string_lossy().into_owned();
    source.every_minutes = 10;
    source.enabled = true;

    crate::assistant::runtime::save_source(shell, source)?;
    create_inbox_source(shell, project_root, team_label)?;
    Ok(true)
}

/// 建（或补齐）团队消息的观察源：定时拉事件链、筛出跟本机有关的（@我 / 广播 /
/// 派给我 / 我的任务上的评论），每条一行交给卡特助理。
///
/// 与同步源分开：同步每 10 分钟拉一次分支就够了；消息要快一些，而且它**不碰工作区**，
/// 写代码期间照样能收 —— 绑在同步源上的话，工作区一脏就什么都收不到。
///
/// 命令是内置的 `carter:team-inbox`（见 `assistant::source`），不走 shell：要对照成员
/// 名单、读 git 事件链，用 shell 拼得依赖 `jq`，Windows 上没有。
pub fn create_inbox_source(
    shell: &std::sync::Arc<crate::shell::AppShell>,
    project_root: &Path,
    team_label: &str,
) -> Result<(), String> {
    let name = format!("{team_label} · 团队消息");
    let existing = shell
        .assistant
        .snapshot()
        .sources
        .into_iter()
        .find(|s| s.name == name);
    let mut source = existing.unwrap_or_else(|| crate::assistant::Source {
        id: String::new(),
        name: name.clone(),
        command: String::new(),
        cwd: String::new(),
        every_minutes: 3,
        enabled: true,
        state: Default::default(),
    });
    source.command = crate::assistant::source::TEAM_INBOX.to_string();
    source.cwd = project_root.to_string_lossy().into_owned();
    crate::assistant::runtime::save_source(shell, source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    /// 造一个干净的临时 git 仓库，配好身份（不配身份时 git 会拒绝提交）。
    fn repo(tag: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("carter-init-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "测试"],
            vec!["config", "user.email", "t@example.com"],
        ] {
            let ok = std::process::Command::new("git")
                .current_dir(&dir)
                .args(&args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?} 失败");
        }
        dir
    }

    #[test]
    fn initialize_creates_the_skeleton_and_commits_it() {
        let dir = repo("fresh");
        let out = initialize(&dir, Some("演示团队")).unwrap();
        assert!(out.commit.is_some(), "首次初始化必须留下一次提交");

        let root = dir.join(DIR);
        for p in [
            root.join("team.json"),
            root.join(".gitignore"),
            root.join("goals"),
            root.join("boards"),
            root.join("skills").join("carter-team").join("SKILL.md"),
        ] {
            assert!(p.exists(), "{} 没建出来", p.display());
        }
        let meta = Store::open(&root).meta().unwrap();
        assert_eq!(meta.name, "演示团队");
        assert_eq!(out.root, root.to_string_lossy());

        // 真的进了版本库。
        let tracked = std::process::Command::new("git")
            .current_dir(&dir)
            .args(["ls-files", "--", DIR])
            .output()
            .unwrap();
        let tracked = String::from_utf8_lossy(&tracked.stdout);
        assert!(tracked.contains("team.json"), "team.json 要在 git 里");
        assert!(tracked.contains(".carter/skills"), "技能要在 git 里");
    }

    /// 用旧版应用初始化过的仓库：重新初始化要删掉我们装的脚本、把教 agent 调脚本的
    /// 旧技能换成新版，而用户改过的技能原样保留。
    #[test]
    fn reinit_retires_the_script_and_upgrades_script_era_skills() {
        let dir = repo("legacy");
        initialize(&dir, Some("t")).unwrap();
        let root = dir.join(DIR);
        // 造一个旧仓库的样子：v2 脚本 + v1 的 carter-task + 用户改过的 carter-goal。
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(
            root.join("bin/carter-team"),
            "#!/bin/sh\n# carter-team-version: 2\nset -eu\n",
        )
        .unwrap();
        std::fs::write(
            root.join("skills/carter-task/SKILL.md"),
            "---\nname: carter-task\ndescription: 需要拆解任务、更新任务状态、认领任务、或者回答……\n---\n.carter/bin/carter-team task new …\n",
        )
        .unwrap();
        std::fs::write(
            root.join("skills/carter-goal/SKILL.md"),
            "---\nname: carter-goal\ndescription: 我们组自己的目标约定\n---\n",
        )
        .unwrap();

        let out = initialize(&dir, None).unwrap();
        assert!(!root.join("bin").exists(), "旧脚本与空的 bin/ 都该没了：{:?}", out.steps);
        let task = std::fs::read_to_string(root.join("skills/carter-task/SKILL.md")).unwrap();
        assert!(task.contains("\"action\": \"task_new\""), "carter-task 要换成教工具的新版");
        assert!(!task.contains("bin/carter-team"));
        let goal = std::fs::read_to_string(root.join("skills/carter-goal/SKILL.md")).unwrap();
        assert!(goal.contains("我们组自己的"), "用户改过的技能不能动");
        assert!(out.steps.iter().any(|s| s.contains("carter-task（v1")), "{:?}", out.steps);
    }

    #[test]
    fn initialize_is_idempotent_and_never_clobbers() {
        let dir = repo("idem");
        let first = initialize(&dir, Some("一次")).unwrap();
        assert!(first.commit.is_some());

        // 人工改一份数据 + 改一处 team.json 字段。
        let root = dir.join(DIR);
        let store = Store::open(&root);
        let mut meta = store.meta().unwrap();
        meta.description = "我手改的".into();
        store.save_meta(&meta).unwrap();

        let second = initialize(&dir, Some("二次")).unwrap();
        assert_eq!(second.commit, None, "第二次没有新东西可提交");
        let meta = Store::open(&root).meta().unwrap();
        assert_eq!(meta.name, "一次", "已有的 team.json 不能被覆盖");
        assert_eq!(meta.description, "我手改的");
    }

    #[test]
    fn no_remote_is_a_warning_not_a_failure() {
        let dir = repo("noremote");
        let out = initialize(&dir, None).unwrap();
        assert!(out.remote.is_none());
        assert!(out.warnings.iter().any(|w| w.contains("远端")), "{:?}", out.warnings);
    }

    #[test]
    fn works_when_the_project_is_a_subdirectory_of_a_repo() {
        let dir = repo("sub");
        let nested = dir.join("packages").join("app");
        std::fs::create_dir_all(&nested).unwrap();
        let out = initialize(&nested, Some("子项目")).unwrap();
        assert!(out.warnings.iter().any(|w| w.contains("在 git 仓库")));
        assert!(nested.join(DIR).join("team.json").is_file());
    }

    #[test]
    fn cache_is_ignored_but_data_is_not() {
        let dir = repo("ignore");
        initialize(&dir, None).unwrap();
        let ok = |path: &str| {
            std::process::Command::new("git")
                .current_dir(&dir)
                .args(["check-ignore", "-q", path])
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        assert!(ok(".carter/.cache/index.json"), ".cache 要被忽略");
        assert!(!ok(".carter/team.json"), "数据文件不能被忽略");
        assert!(!ok(".carter/goals/g-1/goal.json"));
    }
}
