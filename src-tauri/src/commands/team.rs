use crate::prelude::*;

// ---------------------------------------------------------------------------
// 团队目录（目标 / 任务 / 看板）
// ---------------------------------------------------------------------------

/// 这个目录（或它上面某一层）已经初始化过团队目录了吗。
#[tauri::command]
pub(crate) fn team_status(cwd: String) -> Option<TeamStatus> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))?;
    let meta = store.meta().ok()?;
    let goals = store.goals();
    Some(TeamStatus {
        root: store.root().to_string_lossy().to_string(),
        project_root: store.project_root().to_string_lossy().to_string(),
        name: meta.name,
        remote: meta.remote,
        initialized: true,
        goals: goals.items.len(),
        issues: goals.issues.len(),
    })
}

#[derive(Serialize)]
pub(crate) struct TeamStatus {
    root: String,
    project_root: String,
    name: String,
    remote: Option<String>,
    initialized: bool,
    goals: usize,
    issues: usize,
}

/// 初始化团队目录。建骨架 + 首次提交 + 自动建同步观察源。
///
/// 观察源建在这里而不是 `team::init` 里：那个模块刻意不依赖 `AppShell`（它要能在
/// 没有 webview 的情况下测），而建源需要整个外壳。
#[tauri::command]
pub(crate) fn team_initialize(
    shell: State<'_, Arc<AppShell>>,
    project_root: String,
    name: Option<String>,
) -> Result<carter_app_core::InitOutcome, String> {
    let mut outcome = carter_app_core::team::initialize(Path::new(&project_root), name.as_deref())?;
    match carter_app_core::team::init::create_sync_source(
        shell.inner(),
        Path::new(&project_root),
        outcome
            .project_root
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("项目"),
    ) {
        Ok(created) => {
            outcome.sync_source = created;
            if outcome.remote.is_none() {
                // 源已经建好了，但它连不上任何远端。等用户配好地址，`team_set_remote`
                // 会再补齐一次，不需要重跑初始化。
                outcome.warnings.push(
                    "还没有远端，同步暂时空转。点右上角「配远端」填一个仓库地址，后台就会开始自动同步。"
                        .into(),
                );
            }
        }
        Err(e) => outcome.warnings.push(format!("同步观察源没建成：{e}")),
    }
    Ok(outcome)
}

/// 目标全景：目标 + 任务树 + 进度 + 看板。界面一次拉完。
#[tauri::command]
pub(crate) fn team_overview(cwd: String) -> Result<TeamOverview, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;

    let mut warnings = Vec::new();
    let goals_loaded = store.goals();
    for (path, why) in &goals_loaded.issues {
        warnings.push(format!("{path}：{why}"));
    }

    let mut goals = Vec::new();
    for goal in goals_loaded.items {
        let view = store.goal_view(&goal.id);
        let tasks = match &view {
            Ok(v) => v
                .tasks
                .iter()
                .map(|t| TaskRow {
                    id: t.task.id.clone(),
                    title: t.task.title.clone(),
                    status: t.task.status.as_str().to_string(),
                    status_label: t.task.status.label().to_string(),
                    priority: t.task.priority.as_str().to_string(),
                    assignee: t.task.assignee.clone(),
                    parent_id: t.task.parent_id.clone(),
                    depends_on: t.task.depends_on.clone(),
                    children: t.children.clone(),
                    waiting_on: t.waiting_on.clone(),
                    progress: t.progress,
                    ready: t.ready,
                    blocked_reason: t.task.blocked_reason.clone(),
                    estimate_hours: t.task.estimate_hours,
                    due_at: t.task.due_at,
                })
                .collect(),
            Err(e) => {
                warnings.push(format!("{}：{e}", goal.id));
                Vec::new()
            }
        };
        goals.push(GoalRow {
            id: goal.id.clone(),
            title: goal.title.clone(),
            description: goal.description.clone(),
            status: goal.status.as_str().to_string(),
            status_label: goal.status.label().to_string(),
            priority: goal.priority.as_str().to_string(),
            owner: goal.owner.clone(),
            due_at: goal.due_at,
            percent: view.as_ref().map(|v| v.counts.percent()).unwrap_or(0),
            counts: view.as_ref().map(|v| v.counts).unwrap_or_default(),
            roots: view.as_ref().map(|v| v.roots.clone()).unwrap_or_default(),
            tasks,
        });
    }

    let boards_loaded = store.boards();
    for (path, why) in &boards_loaded.issues {
        warnings.push(format!("{path}：{why}"));
    }
    let boards = boards_loaded
        .items
        .into_iter()
        .map(|b| BoardRow {
            id: b.id,
            title: b.title,
            goal_id: b.goal_id,
        })
        .collect();

    let meta = store.meta().ok();
    let identity = carter_app_core::team::git::local_identity(store.project_root()).ok();

    Ok(TeamOverview {
        root: store.root().to_string_lossy().to_string(),
        name: meta.as_ref().map(|m| m.name.clone()).unwrap_or_default(),
        // 实时问 git，不读 `team.json` 里那份初始化时的快照 ——
        // 用户之后 `git remote add` 不会回填那个字段。
        remote: carter_app_core::team::git::remote_url(store.project_root())
            .or_else(|| meta.and_then(|m| m.remote)),
        identity: identity.map(|i| i.as_key()),
        goals,
        boards,
        events: carter_app_core::team::events::read(store.project_root(), 60),
        warnings,
    })
}

#[derive(Serialize)]
pub(crate) struct TeamOverview {
    root: String,
    name: String,
    remote: Option<String>,
    identity: Option<String>,
    goals: Vec<GoalRow>,
    boards: Vec<BoardRow>,
    events: Vec<carter_app_core::team::events::Event>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct GoalRow {
    id: String,
    title: String,
    description: String,
    status: String,
    status_label: String,
    priority: String,
    owner: Option<String>,
    due_at: Option<u64>,
    percent: u32,
    counts: carter_app_core::team::Counts,
    roots: Vec<String>,
    tasks: Vec<TaskRow>,
}

#[derive(Serialize)]
pub(crate) struct TaskRow {
    id: String,
    title: String,
    status: String,
    status_label: String,
    priority: String,
    assignee: Option<String>,
    parent_id: Option<String>,
    depends_on: Vec<String>,
    children: Vec<String>,
    waiting_on: Vec<String>,
    progress: u32,
    ready: bool,
    blocked_reason: String,
    estimate_hours: Option<f64>,
    due_at: Option<u64>,
}

/// 全景里只带看板的元信息：正文是一整页 HTML，切到看板页、选中那一块时再取
/// （见 [`team_board_html`]），不必每次刷新全景都把所有页面搬一遍。
#[derive(Serialize)]
pub(crate) struct BoardRow {
    id: String,
    title: String,
    goal_id: Option<String>,
}

/// 一块看板的整页 HTML。
#[tauri::command]
pub(crate) fn team_board_html(cwd: String, board_id: String) -> Result<String, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;
    store.board(&board_id).map(|b| b.html)
}

/// 我名下的未完成任务。消息框下方的待办条用它。
///
/// 单独一条命令而不是让前端从 `team_overview` 里筛：待办条要跟着**每轮结束**刷新
/// （模型可能刚改过状态），而全景包含所有目标的任务，两边的刷新频率不一样。
#[tauri::command]
pub(crate) fn team_my_tasks(cwd: String) -> Vec<TeamTodo> {
    let Some(store) = carter_app_core::team::Store::discover(Path::new(&cwd)) else {
        return Vec::new();
    };
    let Ok(me) = carter_app_core::team::git::local_identity(store.project_root()) else {
        return Vec::new();
    };
    // 认人走名单：任务里存的可能是成员 id（v1）也可能是 `名字 <邮箱>`（v0），
    // 一个人还可能挂着好几台机器的邮箱。
    let roster = carter_app_core::team::members::load_all(&store);
    let key = roster
        .me(&me)
        .map(|m| m.id.clone())
        .unwrap_or_else(|| me.as_key());
    let mut out = Vec::new();
    for goal in store.goals().items {
        if !goal.status.is_live() {
            continue;
        }
        let Ok(view) = store.goal_view(&goal.id) else {
            continue;
        };
        for t in view.tasks {
            if !t
                .task
                .assignee
                .as_deref()
                .is_some_and(|a| roster.same(a, &key))
                || !t.task.status.is_open()
            {
                continue;
            }
            out.push(TeamTodo {
                id: t.task.id.clone(),
                title: t.task.title.clone(),
                status: t.task.status.as_str().to_string(),
                status_label: t.task.status.label().to_string(),
                priority: t.task.priority.as_str().to_string(),
                goal_id: goal.id.clone(),
                goal_title: goal.title.clone(),
                blocked_reason: t.task.blocked_reason.clone(),
                waiting_on: t.waiting_on.len() as u32,
                ready: t.ready,
            });
        }
    }
    // 能开工的排前面，然后按优先级。
    out.sort_by(|a, b| {
        b.ready
            .cmp(&a.ready)
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.title.cmp(&b.title))
    });
    out
}

#[derive(Serialize)]
pub(crate) struct TeamTodo {
    id: String,
    title: String,
    status: String,
    status_label: String,
    priority: String,
    goal_id: String,
    goal_title: String,
    blocked_reason: String,
    waiting_on: u32,
    ready: bool,
}

/// 把团队数据提交成一条 git 提交。
///
/// 单独一条命令而不是每次改都提交：一轮对话里模型可能建三个任务、改两个状态，
/// 那是**一件事**，应该是一条提交。界面在轮末调用它。
#[tauri::command]
pub(crate) fn team_commit(cwd: String, message: String) -> Result<Option<String>, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;
    let root = store.project_root();
    let rel = store
        .root()
        .strip_prefix(root)
        .map_err(|_| "团队目录不在仓库里".to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    carter_app_core::team::git::stage_team(root, &rel)?;
    carter_app_core::team::git::commit_team(root, &rel, message.trim())
}

/// 立刻同步一次（拉远端、快进）。观察源也会定时跑，这里是手动入口。
#[tauri::command]
pub(crate) fn team_sync(cwd: String) -> Result<String, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;
    let root = store.project_root();
    // 「没有远端」不是错误，是一个还没配好的状态。用红色报错去说这件事，
    // 会让人觉得同步坏了；实际是团队仓库本来就只在本地。
    if !carter_app_core::team::git::has_remote(root) {
        return Ok(
            "还没有配远端，团队仓库目前只在本地。点右上角「配远端」填一个仓库地址即可。".into(),
        );
    }
    carter_app_core::team::git::sync(root)
}

// ---------------------------------------------------------------------------
// 团队成员与消息（协议见 docs/team-protocol.md）
// ---------------------------------------------------------------------------

/// 团队目录相对仓库根的路径（`/` 分隔）。两边先各自解析符号链接再比：macOS 上
/// `/var` 与 `/private/var` 直接比会对不上。
pub(crate) fn team_rel_of(store: &carter_app_core::team::Store) -> String {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(store.root())
        .strip_prefix(canon(store.project_root()))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".carter".into())
}

pub(crate) fn team_store(cwd: &str) -> Result<carter_app_core::team::Store, String> {
    carter_app_core::team::Store::discover(Path::new(cwd))
        .ok_or("这个项目还没有初始化团队目录".into())
}

/// 团队里有谁、本机是谁、还有谁提交过代码却没加入。
///
/// 「本机是谁」为空时界面弹「加入团队」—— 第一次在这个团队里需要身份时才问，
/// 不在初始化时问（初始化的人未必是要在这里干活的人）。
#[tauri::command]
pub(crate) fn team_people(cwd: String) -> Result<TeamPeople, String> {
    use carter_app_core::team::{git, members};
    let store = team_store(&cwd)?;
    let roster = members::load_all(&store);
    let identity = git::local_identity(store.project_root()).ok();
    let me = identity
        .as_ref()
        .and_then(|i| roster.me(i))
        .map(|m| m.id.clone());
    let pending = members::git_candidates(store.project_root(), &roster)
        .into_iter()
        .take(20)
        .map(|i| i.as_key())
        .collect();
    Ok(TeamPeople {
        members: roster.members,
        me,
        identity: identity.map(|i| i.as_key()),
        pending,
    })
}

#[derive(Serialize)]
pub(crate) struct TeamPeople {
    members: Vec<carter_app_core::team::members::Member>,
    /// 本机的成员 id。`None` = 还没加入。
    me: Option<String>,
    /// 本机的 git 身份（`名字 <邮箱>`）。没配 git 时为空 —— 那种情况下没法加入。
    identity: Option<String>,
    /// 提交过代码、还没加入的人。指派时可以先写他们的 `名字 <邮箱>`。
    pending: Vec<String>,
}

/// 加入团队。`member_id` 给了 = 「我是这个人，这是我的另一台机器」；否则用 `name` 新建。
///
/// 加入之后顺手补上「团队消息」观察源：在这台机器上加入了，才需要在这台机器上收
/// 消息。旧版初始化的团队（以及 clone 下来、没在本机初始化过的）原本没有这条源。
#[tauri::command]
pub(crate) fn team_join(
    shell: State<'_, Arc<AppShell>>,
    cwd: String,
    name: Option<String>,
    member_id: Option<String>,
) -> Result<TeamJoined, String> {
    use carter_app_core::team::members::{Join, join_and_announce};
    let store = team_store(&cwd)?;
    let how = match member_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => Join::Existing { member_id: id },
        None => Join::New {
            name: name.unwrap_or_default(),
        },
    };
    let (m, _) = join_and_announce(&store, how, None)?;
    let label = store
        .meta()
        .map(|meta| meta.name)
        .ok()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| {
            store
                .project_root()
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "项目".into())
        });
    // 收不到提醒不该让加入失败：团队页里照样能看到消息。把原因交给界面说一句。
    let inbox_source = carter_app_core::team::init::create_inbox_source(
        shell.inner(),
        store.project_root(),
        &label,
    )
    .err();
    Ok(TeamJoined {
        member: m,
        inbox_source,
    })
}

#[derive(Serialize)]
pub(crate) struct TeamJoined {
    member: carter_app_core::team::members::Member,
    /// 消息观察源没建成的原因。`None` = 建好了（卡特助理会定时送达）。
    inbox_source: Option<String>,
}

/// 改自己在这个团队里的称呼。只改本地文件，随下一次提交走。
#[tauri::command]
pub(crate) fn team_rename_me(
    cwd: String,
    name: String,
) -> Result<carter_app_core::team::members::Member, String> {
    use carter_app_core::team::{git, members};
    let store = team_store(&cwd)?;
    let me = git::local_identity(store.project_root())?;
    let roster = members::load(store.root());
    let id = roster
        .me(&me)
        .map(|m| m.id.clone())
        .ok_or("你还没加入这个团队")?;
    let m = members::rename(store.root(), &id, &name)?;
    let root = store.project_root();
    let rel = format!("{}/{}", team_rel_of(&store), members::MEMBERS_DIR);
    let _ = git::stage_team(root, &rel);
    let _ = git::commit_team(root, &rel, &format!("chore(carter): 改称呼为 {}", m.name));
    Ok(m)
}

/// 人在界面上操作团队数据：指派、认领、评论、@、广播……
///
/// 参数与 `team` 工具完全一样，走的也是同一份实现（`TeamTool::for_person`），只是
/// 事件不带 `via=carter`。两条入口共用一份逻辑，才不会出现「界面上指派会通知对方、
/// 让卡特指派却不通知」这种不一致。
///
/// 改了任务文件的动作（指派、认领）在这里**立刻提交**：界面上没有「轮末」，不提交
/// 的话这次改动要等下一轮对话才进 git，别人同步不到。
#[tauri::command]
pub(crate) fn team_act(cwd: String, args: serde_json::Value) -> Result<String, String> {
    use carter_app_core::team::{TeamTool, git};
    let store = team_store(&cwd)?;
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let out = TeamTool::for_person(PathBuf::from(&cwd)).act(&args)?;
    if matches!(
        action.as_str(),
        "assign" | "task_claim" | "task_release" | "task_status" | "task_update"
    ) {
        let root = store.project_root();
        let rel = team_rel_of(&store);
        let _ = git::stage_team(root, &rel);
        let _ = git::commit_team(root, &rel, &format!("chore(carter): {action}"));
    }
    Ok(out)
}

/// 跟本机有关的消息。`fetch = true` 时先拉一次远端事件。
#[tauri::command]
pub(crate) async fn team_inbox(
    cwd: String,
    fetch: bool,
) -> Result<Vec<carter_app_core::team::inbox::InboxItem>, String> {
    // git 调用是阻塞的；拉远端可能要好几秒，别占着 Tauri 的命令线程。
    tauri::async_runtime::spawn_blocking(move || {
        use carter_app_core::team::{events, git, inbox};
        let store = team_store(&cwd)?;
        if fetch {
            let _ = events::fetch(store.project_root());
        }
        let me = git::local_identity(store.project_root())?;
        Ok(inbox::read(&store, &me, 100))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 配远端。界面上那一个输入框走这里 —— 不该让用户为了同步去开终端。
#[tauri::command]
pub(crate) fn team_set_remote(
    shell: State<'_, Arc<AppShell>>,
    cwd: String,
    url: String,
) -> Result<String, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;
    let msg = carter_app_core::team::git::set_remote(store.project_root(), &url)?;
    // 配完顺手推一次：不推的话 `fetch` / `merge` 没东西可比，同步仍然是空转，
    // 用户会以为「配了没用」。
    let pushed = carter_app_core::team::git::publish(store.project_root());
    // 初始化时还没远端的话，同步源要么没建、要么一直空转。这里补齐，免得用户
    // 还得回去再点一次「初始化」。
    let synced = carter_app_core::team::init::create_sync_source(
        shell.inner(),
        store.project_root(),
        &store
            .meta()
            .map(|m| m.name)
            .ok()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| {
                store
                    .project_root()
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "项目".into())
            }),
    );
    let tail = match synced {
        Ok(_) => String::new(),
        Err(e) => format!("；后台同步源没建成：{e}"),
    };
    Ok(match pushed {
        Ok(p) => format!("{msg}；{p}{tail}"),
        Err(e) => format!("{msg}；但首次推送没成功：{e}"),
    })
}
