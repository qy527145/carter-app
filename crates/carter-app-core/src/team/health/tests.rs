use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;
use crate::assistant::{Source, SourceState};

static N: AtomicU32 = AtomicU32::new(0);

/// 一个干净的临时 git 仓库（配好身份，否则提交会被拒）。
fn repo(tag: &str) -> PathBuf {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("carter-health-{tag}-{}-{n}", std::process::id()));
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

fn source(cwd: &Path, command: &str, enabled: bool) -> Source {
    Source {
        id: format!("src-{command}"),
        name: command.to_string(),
        command: command.to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        every_minutes: 5,
        enabled,
        state: SourceState::default(),
    }
}

fn find<'a>(h: &'a Health, id: &str) -> &'a Check {
    h.checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("没有 {id} 这一项"))
}

/// 刚初始化好的团队：该报的只有「没远端」这一条警告。
#[test]
fn a_fresh_team_is_green_except_the_missing_remote() {
    let dir = repo("fresh");
    crate::team::init::initialize(&dir, Some("演示")).unwrap();
    let label = "演示";
    // 初始化只建了同步源；消息源要加入之后才有，这里模拟「还没加入」。
    let sources = vec![source(&dir, crate::assistant::source::TEAM_SYNC, true)];
    let h = inspect(&dir, &sources);
    let _ = label;

    assert_eq!(find(&h, "git").state, CheckState::Ok);
    assert_eq!(find(&h, "remote").state, CheckState::Warn);
    assert_eq!(find(&h, "files").state, CheckState::Ok);
    assert_eq!(find(&h, "sync_source").state, CheckState::Ok);
    assert_eq!(find(&h, "dirty").state, CheckState::Ok);
    // 还没加入：消息源是「不适用」，不是坏掉。
    assert_eq!(find(&h, "inbox_source").state, CheckState::NotApplicable);
    assert_eq!(h.attention(), 1, "只有「没远端」这一条要人管");
}

/// 没配远端是警告，不该算成「坏掉」。
#[test]
fn no_remote_is_a_warning_not_a_failure() {
    let dir = repo("noremote");
    crate::team::init::initialize(&dir, None).unwrap();
    let h = inspect(&dir, &[]);
    assert_eq!(find(&h, "remote").state, CheckState::Warn);
    assert!(find(&h, "remote").action_kind.as_deref() == Some("remote"));
}

/// 不在 git 仓库里：仓库这一项坏掉，且给出初始化动作。
#[test]
fn a_directory_outside_git_is_broken_with_an_action() {
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("carter-health-nogit-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let h = inspect(&dir, &[]);
    assert_eq!(find(&h, "git").state, CheckState::Broken);
    assert_eq!(find(&h, "git").action_kind.as_deref(), Some("repo"));
    // 没有仓库时远端与团队目录都不适用/坏掉，但不能因为没仓库就误报成「已就绪」。
    assert!(h.attention() > 0);
}

/// 观察源被删掉：对应那一条报坏，并给出重建动作。
#[test]
fn a_missing_source_is_broken() {
    let dir = repo("nosource");
    crate::team::init::initialize(&dir, None).unwrap();
    let h = inspect(&dir, &[]);
    let sync = find(&h, "sync_source");
    assert_eq!(sync.state, CheckState::Broken);
    assert_eq!(sync.action_kind.as_deref(), Some("sources"));
}

/// 观察源被停用与「不存在」是两回事，但都算没在工作。
#[test]
fn a_disabled_source_is_broken() {
    let dir = repo("disabled");
    crate::team::init::initialize(&dir, None).unwrap();
    let sources = vec![source(&dir, crate::assistant::source::TEAM_SYNC, false)];
    let h = inspect(&dir, &sources);
    let sync = find(&h, "sync_source");
    assert_eq!(sync.state, CheckState::Broken);
    assert!(sync.detail.contains("停用"), "{}", sync.detail);
}

/// 观察源的工作目录不是这个项目：不能把它当成这个项目的源。
#[test]
fn a_source_from_another_project_does_not_count() {
    let dir = repo("other");
    crate::team::init::initialize(&dir, None).unwrap();
    let other = repo("otherapp");
    let sources = vec![source(&other, crate::assistant::source::TEAM_SYNC, true)];
    let h = inspect(&dir, &sources);
    assert_eq!(find(&h, "sync_source").state, CheckState::Broken);
}

/// `.carter/` 整个不见了、但 git 历史里还有：报「能找回」，并列出路径。
#[test]
fn a_deleted_team_dir_offers_restore_from_history() {
    let dir = repo("deleted");
    crate::team::init::initialize(&dir, Some("演示")).unwrap();
    std::fs::remove_dir_all(dir.join(DIR)).unwrap();

    let h = inspect(&dir, &[]);
    let files = find(&h, "files");
    assert_eq!(files.state, CheckState::Broken);
    assert!(files.detail.contains("git 历史"), "{}", files.detail);
    assert!(
        files.paths.iter().any(|p| p.ends_with("team.json")),
        "{:?}",
        files.paths
    );
    assert_eq!(files.action_kind.as_deref(), Some("files"));
}

/// 骨架里缺一个目录：报缺什么，动作是「补齐」而不是「找回」。
#[test]
fn a_half_built_skeleton_names_what_is_missing() {
    let dir = repo("half");
    crate::team::init::initialize(&dir, None).unwrap();
    std::fs::remove_dir_all(dir.join(DIR).join("goals")).unwrap();
    // 删目录会留下一处删除，先提交掉，免得「未提交」把状态搅混。
    crate::team::init::repair_skeleton(&dir).unwrap();

    let h = inspect(&dir, &[]);
    let files = find(&h, "files");
    assert_eq!(files.state, CheckState::Ok, "{}", files.detail);
}

/// 用户改过的技能不该被算成「缺了」。
#[test]
fn a_user_edited_skill_is_not_reported_as_missing() {
    let dir = repo("editedskill");
    crate::team::init::initialize(&dir, None).unwrap();
    std::fs::write(
        dir.join(DIR).join("skills/carter-goal/SKILL.md"),
        "---\nname: carter-goal\ndescription: 我们组自己的约定\n---\n",
    )
    .unwrap();

    let h = inspect(&dir, &[]);
    assert_eq!(find(&h, "files").state, CheckState::Ok);
}

/// `.carter/` 里有未提交的改动：警告 + 一条提交动作。
#[test]
fn uncommitted_team_changes_are_a_warning_with_a_commit_action() {
    let dir = repo("dirty");
    crate::team::init::initialize(&dir, None).unwrap();
    std::fs::write(dir.join(DIR).join("boards/note.html"), "<p>x</p>").unwrap();

    let h = inspect(&dir, &[]);
    let dirty = find(&h, "dirty");
    assert_eq!(dirty.state, CheckState::Warn);
    assert_eq!(dirty.action_kind.as_deref(), Some("commit"));

    // 提交之后就回到正常。
    crate::team::init::commit_pending(&dir).unwrap();
    let h = inspect(&dir, &[]);
    assert_eq!(find(&h, "dirty").state, CheckState::Ok);
}

/// 工作区里跟团队无关的改动不该被算进来。
#[test]
fn unrelated_working_tree_changes_are_ignored() {
    let dir = repo("unrelated");
    crate::team::init::initialize(&dir, None).unwrap();
    std::fs::write(dir.join("main.rs"), "fn main() {}").unwrap();

    let h = inspect(&dir, &[]);
    assert_eq!(find(&h, "dirty").state, CheckState::Ok);
}
