//! `team`：让模型直接读写 `.carter/` 里的目标、任务、看板。
//!
//! ## 为什么要有这个工具
//!
//! 技能里教模型「用 `.carter/bin/carter-team` 命令」，但那条路在三种情况下会断：
//! 用户机器上没有 `jq`、用的是 Windows 没有 `sh`、或者模型在描述里漏了参数。
//! 每次失败都是一次完整的模型往返。这里把同一批操作做成**原生工具**，跨平台、
//! 参数是结构化校验的，失败信息直接回灌给模型自己改。
//!
//! 脚本仍然保留：它是给人用的（`git log` 里看到团队数据怎么改的），也让
//! 「不进本应用、只 clone 了仓库」的人能操作。两条路改的是同一批文件。
//!
//! ## 为什么一个工具十几个 action，而不是十几个工具
//!
//! carter 的工具表是**每轮全量下发**的。多一个工具就多一段 description 占着
//! 系统提示的固定篇幅，而「新建任务」和「改任务状态」对模型来说本来就是同一个
//! 心智模型里的事。一个 `action` 枚举 + 按需给出的参数分支，比十几个互相引用的
//! 工具更好学，也更好在界面上归成一组卡片。
//!
//! ## 副作用边界
//!
//! * 所有写操作都**落盘并 `git add`**，但**不 commit** —— 提交由外层统一做
//!   （见 `session.rs` 的轮末钩子），否则一轮里改十个任务就是十条提交。
//! * `claim` / `release` / `comment` 例外：它们是事件，各自成一条空提交。
//!   `claim` 还要 push —— push 成功才算认领成功。

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use super::git::{self, EventKind};
use super::model::*;
use super::store::Store;

pub struct TeamTool {
    /// 会话的工作目录。团队目录从它往上找 —— 与技能扫描同一套规则。
    cwd: PathBuf,
}

impl TeamTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn store(&self) -> Result<Store, String> {
        Store::discover(&self.cwd).ok_or_else(|| {
            "这个项目还没有初始化团队目录。让用户在界面上点「初始化团队」，\
             或者在项目根上跑一次初始化。"
                .to_string()
        })
    }

    fn me(&self) -> Result<Identity, String> {
        let store = self.store()?;
        git::local_identity(store.project_root())
    }

    /// 写完之后把 `.carter/` 放进暂存区，但不提交。
    ///
    /// 不提交的理由：一轮对话里模型可能建三个任务、改两个状态，那是**一件事**，
    /// 应该是一条提交。条数由会话层按轮收口，不由每次工具调用决定。
    fn stage(&self, store: &Store) {
        let root = store.project_root();
        if let Ok(rel) = team_rel(root, store.root()) {
            let _ = git::stage_team(root, &rel);
        }
    }

    fn goal_of(&self, store: &Store, id: &str) -> Result<Goal, String> {
        store.goal(id)
    }
}

fn team_rel(repo_root: &std::path::Path, team_root: &std::path::Path) -> Result<String, String> {
    // 与 `init::relative_to` 同一个理由：git 给的仓库根是解析过符号链接的路径，
    // 团队目录是从 cwd 往上找到的，两边形态不一致，必须先各自解析再比。
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (repo_root, team_root) = (canon(repo_root), canon(team_root));
    let rel = team_root
        .strip_prefix(&repo_root)
        .map_err(|_| "团队目录不在仓库里".to_string())?;
    Ok(rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/"))
}

#[async_trait::async_trait]
impl Tool for TeamTool {
    fn name(&self) -> &str {
        "team"
    }

    fn description(&self) -> &str {
        "读写本项目的团队目录（`.carter/`）：目标、任务、看板。\n\
         \n\
         什么时候该用：用户问「有什么目标 / 我手上有什么任务 / 下一步做什么」，\
         或者要你把这件活记下来、更新进展、认领任务、留一条评论。\n\
         \n\
         什么时候不该用：项目没有 `.carter/`（先让用户初始化）；只是聊天。\n\
         \n\
         规矩：\n\
         * id 一律由工具生成，不要自己编。\n\
         * 状态推进用 `task_status`，不要靠改文件。\n\
         * `claim` 会 push，被拒说明别人先占了 —— 换个任务，不要覆盖。\n\
         * 依赖不能成环；工具会验，验不过就换种拆法。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "overview", "my_tasks",
                        "goal_new", "goal_update",
                        "task_new", "task_update", "task_status", "task_claim", "task_release",
                        "comment", "events", "check",
                        "board_new", "card_new", "card_move"
                    ],
                    "description": "要做什么。先看 action 的分组：读用 overview / my_tasks / events；目标用 goal_*；任务用 task_*；看板用 board_* / card_*。"
                },
                "goal_id": { "type": "string", "description": "目标 id（g- 开头）。goal_* 与 task_new 必填。" },
                "task_id": { "type": "string", "description": "任务 id（t- 开头）。task_* 必填。" },
                "board_id": { "type": "string", "description": "看板 id（b- 开头）。card_* 必填。" },
                "card_id": { "type": "string", "description": "卡片 id（k- 开头）。" },
                "title": { "type": "string", "description": "标题。goal_new / task_new 必填；*_update 用它改名。" },
                "description": { "type": "string", "description": "说明。目标写「为什么做、做到什么算完」，任务写做法。" },
                "status": {
                    "type": "string",
                    "description": "状态。task_status 用：todo / doing / blocked / done / dropped；goal_update 用：active / paused / done / archived。"
                },
                "priority": { "type": "string", "enum": ["P0", "P1", "P2", "P3"], "description": "优先级。" },
                "parent_id": { "type": "string", "description": "父任务 id。task_new 用它做成子任务。" },
                "depends_on": {
                    "type": "array", "items": { "type": "string" },
                    "description": "前置任务 id 列表（要等它们先完成）。与 parent_id 是两件事，别混。"
                },
                "assignee": { "type": "string", "description": "负责人，写成 `名字 <邮箱>`，与 git 提交身份一致。传空串表示取消认领。" },
                "estimate_hours": { "type": "number", "description": "估时（小时）。" },
                "deliverable": { "type": "string", "description": "交付物：一句话说清交什么。" },
                "acceptance": {
                    "type": "array", "items": { "type": "string" },
                    "description": "验收标准，每条都要可验证（「cargo test 通过」而不是「质量好」）。"
                },
                "blocked_reason": { "type": "string", "description": "受阻原因。status = blocked 时填。" },
                "text": { "type": "string", "description": "卡片正文（card_new 必填）或评论正文（comment 必填）。" },
                "column_id": { "type": "string", "description": "列 id。card_new / card_move 用；不给就落在第一列。" },
                "limit": { "type": "integer", "description": "events 取多少条，默认 30。" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match self.run(&args) {
            Ok(msg) => ToolResult::ok(msg),
            Err(e) => ToolResult::err(e),
        }
    }
}

impl TeamTool {
    fn run(&self, args: &Value) -> Result<String, String> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or("缺少 action")?
            .trim()
            .to_string();

        let s = args.get("status").and_then(Value::as_str);
        let title = args.get("title").and_then(Value::as_str);
        let desc = args.get("description").and_then(Value::as_str);
        let goal_id = args.get("goal_id").and_then(Value::as_str);
        let task_id = args.get("task_id").and_then(Value::as_str);
        let board_id = args.get("board_id").and_then(Value::as_str);
        let card_id = args.get("card_id").and_then(Value::as_str);
        let text = args.get("text").and_then(Value::as_str);

        match action.as_str() {
            "overview" => {
                let store = self.store()?;
                Ok(self.overview(&store))
            }

            "check" => {
                let store = self.store()?;
                Ok(super::validate::check_store(&store).render())
            }

            "my_tasks" => {
                let store = self.store()?;
                let me = self.me()?;
                let mut out = Vec::new();
                for goal in store.goals().items {
                    if !goal.status.is_live() {
                        continue;
                    }
                    for t in store.tasks(&goal.id).items {
                        if t.assignee.as_deref() == Some(me.as_key().as_str()) && t.status.is_open() {
                            out.push(format!(
                                "- [{}] {}  ({})  目标：{}",
                                t.status.label(),
                                t.title,
                                t.id,
                                goal.title
                            ));
                        }
                    }
                }
                Ok(if out.is_empty() {
                    "你名下没有未完成的任务。".to_string()
                } else {
                    format!("你名下 {} 件：\n{}", out.len(), out.join("\n"))
                })
            }

            "goal_new" => {
                let store = self.store()?;
                let me = self.me()?;
                let title = title.ok_or("goal_new 需要 title")?;
                if title.trim().is_empty() {
                    return Err("title 不能为空".into());
                }
                let mut goal = Goal::new(title.trim(), &me);
                goal.description = desc.unwrap_or("").trim().to_string();
                if let Some(p) = args.get("priority").and_then(Value::as_str) {
                    goal.priority = Priority::parse(p).ok_or(format!("认不出优先级 `{p}`"))?;
                }
                // 排在现有目标之后。
                goal.order = store
                    .goals()
                    .items
                    .iter()
                    .map(|g| g.order)
                    .fold(0.0f64, f64::max)
                    + 1.0;
                self.check_goal(&store, &goal)?;
                store.save_goal(&goal)?;
                self.stage(&store);
                Ok(format!(
                    "已新建目标「{}」。\nid = {}\n接下来用 task_new 往这个目标下面拆任务。",
                    goal.title, goal.id
                ))
            }

            "goal_update" => {
                let store = self.store()?;
                let me = self.me()?;
                let id = goal_id.ok_or("goal_update 需要 goal_id")?;
                let mut goal = self.goal_of(&store, id)?;
                if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
                    goal.title = t.trim().to_string();
                }
                if let Some(d) = desc {
                    goal.description = d.trim().to_string();
                }
                if let Some(p) = args.get("priority").and_then(Value::as_str) {
                    goal.priority = Priority::parse(p).ok_or(format!("认不出优先级 `{p}`"))?;
                }
                if let Some(s) = s {
                    goal.status = GoalStatus::parse(s).ok_or(format!(
                        "状态只能是 active / paused / done / archived，收到 `{s}`"
                    ))?;
                }
                goal.touch(&me);
                self.check_goal(&store, &goal)?;
                store.save_goal(&goal)?;
                self.stage(&store);
                Ok(format!("目标「{}」已更新（{} / {}）。", goal.title, goal.status.label(), goal.priority.as_str()))
            }

            "task_new" => {
                let store = self.store()?;
                let me = self.me()?;
                let gid = goal_id.ok_or("task_new 需要 goal_id")?;
                let title = title.ok_or("task_new 需要 title")?;
                if title.trim().is_empty() {
                    return Err("title 不能为空".into());
                }
                // 先确认目标在，否则任务会变成一个挂在不存在目录上的孤儿。
                store.goal(gid)?;

                let mut task = Task::new(gid, title.trim(), &me);
                task.description = desc.unwrap_or("").trim().to_string();
                if let Some(p) = args.get("priority").and_then(Value::as_str) {
                    task.priority = Priority::parse(p).ok_or(format!("认不出优先级 `{p}`"))?;
                }
                if let Some(parent) = args.get("parent_id").and_then(Value::as_str) {
                    let parent = parent.trim();
                    if !parent.is_empty() {
                        let p = store.task(gid, parent)?;
                        if p.goal_id != gid {
                            return Err("parent_id 指向的任务属于别的目标".into());
                        }
                        task.parent_id = Some(p.id.clone());
                    }
                }
                if let Some(deps) = args.get("depends_on").and_then(Value::as_array) {
                    task.depends_on = deps
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                if let Some(a) = args.get("assignee").and_then(Value::as_str) {
                    task.assignee = Identity::parse(a).map(|i| i.as_key());
                }
                if let Some(h) = args.get("estimate_hours").and_then(Value::as_f64) {
                    task.estimate_hours = Some(h);
                }
                if let Some(d) = args.get("deliverable").and_then(Value::as_str) {
                    task.deliverable = d.trim().to_string();
                }
                if let Some(a) = args.get("acceptance").and_then(Value::as_array) {
                    task.acceptance = a
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                if let Some(r) = args.get("blocked_reason").and_then(Value::as_str) {
                    task.blocked_reason = r.trim().to_string();
                }
                // 排在兄弟节点之后：同一个父任务下的新任务排最后。
                task.order = store
                    .tasks(gid)
                    .items
                    .iter()
                    .filter(|t| t.parent_id == task.parent_id)
                    .map(|t| t.order)
                    .fold(0.0f64, f64::max)
                    + 1.0;

                self.check_task(&store, &task)?;
                store.save_task(&task)?;
                self.stage(&store);

                let mut msg = format!("已新建任务「{}」。\nid = {}", task.title, task.id);
                if !task.depends_on.is_empty() {
                    msg.push_str(&format!(" 等 {} 之后才能开始。", task.depends_on.join("、")));
                }
                Ok(msg)
            }

            "task_update" => {
                let store = self.store()?;
                let me = self.me()?;
                let (gid, mut task) = self.locate_task(&store, args)?;
                if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
                    task.title = t.trim().to_string();
                }
                if let Some(d) = desc {
                    task.description = d.trim().to_string();
                }
                if let Some(p) = args.get("priority").and_then(Value::as_str) {
                    task.priority = Priority::parse(p).ok_or(format!("认不出优先级 `{p}`"))?;
                }
                if let Some(a) = args.get("assignee").and_then(Value::as_str) {
                    task.assignee = if a.trim().is_empty() {
                        None
                    } else {
                        Identity::parse(a).map(|i| i.as_key())
                    };
                }
                if let Some(h) = args.get("estimate_hours").and_then(Value::as_f64) {
                    task.estimate_hours = Some(h);
                }
                if let Some(d) = args.get("deliverable").and_then(Value::as_str) {
                    task.deliverable = d.trim().to_string();
                }
                if let Some(a) = args.get("acceptance").and_then(Value::as_array) {
                    task.acceptance = a
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                if let Some(deps) = args.get("depends_on").and_then(Value::as_array) {
                    task.depends_on = deps
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                if let Some(parent) = args.get("parent_id").and_then(Value::as_str) {
                    task.parent_id = if parent.trim().is_empty() {
                        None
                    } else {
                        Some(parent.trim().to_string())
                    };
                }
                if let Some(p) = task.parent_id.as_deref() {
                    // 不能让任务成为自己的祖先：否则算进度时无限递归。
                    if p == task.id {
                        return Err("任务不能以自己的父任务".into());
                    }
                    store.task(&gid, p)?;
                }
                task.touch(&me);
                self.check_task(&store, &task)?;
                store.save_task(&task)?;
                self.stage(&store);
                Ok(format!("任务「{}」已更新。", task.title))
            }

            "task_status" => {
                let store = self.store()?;
                let me = self.me()?;
                let s = s.ok_or("task_status 需要 status")?;
                let new = TaskStatus::parse(s)
                    .ok_or(format!("状态只能是 todo / doing / blocked / done / dropped，收到 `{s}`"))?;
                let (_, mut task) = self.locate_task(&store, args)?;
                task.status = new;
                if let Some(r) = args.get("blocked_reason").and_then(Value::as_str) {
                    task.blocked_reason = r.trim().to_string();
                }
                if new == TaskStatus::Blocked && task.blocked_reason.trim().is_empty() {
                    return Err("status = blocked 时要用 blocked_reason 说清卡在哪".into());
                }
                task.touch(&me);
                store.save_task(&task)?;
                self.stage(&store);

                // 完成时顺手报一下「谁在等它」—— 模型下一步就知道该去推什么。
                let mut msg = format!("任务「{}」现在是「{}」。", task.title, new.label());
                if new == TaskStatus::Done {
                    let v = store.goal_view(&task.goal_id)?;
                    let unblocked: Vec<_> = v
                        .tasks
                        .iter()
                        .filter(|t| t.waiting_on.is_empty() && t.ready)
                        .map(|t| format!("{}（{}）", t.task.title, t.task.id))
                        .collect();
                    if !unblocked.is_empty() {
                        msg.push_str(&format!("\n现在可以开工：{}", unblocked.join("、")));
                    }
                }
                Ok(msg)
            }

            "task_claim" | "task_release" => {
                let store = self.store()?;
                let me = self.me()?;
                let (_, mut task) = self.locate_task(&store, args)?;
                let claiming = action == "task_claim";
                let root = store.project_root();

                // 别人认领过就不抢：覆盖别人的认领在 git 里看不出来（那是「最后写入赢」），
                // 而后果是两个人同时在改同一件事。
                if claiming {
                    if let Some(a) = task.assignee.as_deref() {
                        if a != me.as_key() && !a.trim().is_empty() {
                            return Err(format!(
                                "「{}」已经被 {} 认领了。换个任务，或者先问一句。",
                                task.title, a
                            ));
                        }
                    }
                }

                // 记下原样：push 被拒时要原路退回去。
                let original = task.clone();
                task.assignee = if claiming { Some(me.as_key()) } else { None };
                task.touch(&me);
                store.save_task(&task)?;

                let kind = if claiming { EventKind::Claim } else { EventKind::Release };
                let subject = format!(
                    "carter/{} {} {}",
                    kind.as_str(),
                    task.id,
                    me.short()
                );
                if git::in_repo(root) {
                    let rel = team_rel(root, store.root())?;
                    let _ = git::stage_team(root, &rel);
                    git::commit_event(
                        root,
                        kind,
                        Some(&task.id),
                        Some(&task.goal_id),
                        &subject,
                        "",
                    )?;
                    // push 成功才算认领成功 —— 这是整条通道里唯一原子化的地方。
                    if git::has_remote(root) {
                        if let Err(e) = git::push(root) {
                            // **本地也要退回去。** 只把错误报给模型是不够的：文件里
                            // 已经写上了自己的名字，而远端是别人的 —— 两人各自看自己
                            // 那份都是对的，这种不一致没有任何地方会报出来。
                            let _ = git::undo_last_commit(root);
                            let _ = store.save_task(&original);
                            let _ = git::unstage(root, &rel);
                            return Err(format!(
                                "认领没有生效：push 被拒（{e}）。\
                                 本地改动已撤回。多半是别人先占了同一个任务，\
                                 先 team sync 再看一遍现状，或者换一个任务。"
                            ));
                        }
                    }
                }
                Ok(if claiming {
                    format!("已认领「{}」。做完记得 task_status 改成 done。", task.title)
                } else {
                    format!("已释放「{}」。", task.title)
                })
            }

            "comment" => {
                let store = self.store()?;
                let me = self.me()?;
                let body = text.ok_or("comment 需要 text")?.trim();
                if body.is_empty() {
                    return Err("text 不能为空".into());
                }
                let root = store.project_root();
                let target = goal_id
                    .map(str::to_string)
                    .or_else(|| task_id.map(str::to_string))
                    .ok_or("comment 需要 goal_id 或 task_id")?;
                let (goal, subject) = if let Some(gid) = goal_id {
                    let g = store.goal(gid)?;
                    (g.id.clone(), format!("carter/comment {} {}", g.id, me.short()))
                } else {
                    let (gid, t) = self.locate_task(&store, args)?;
                    (gid, format!("carter/comment {} {}", t.id, me.short()))
                };
                if !git::in_repo(root) {
                    return Err("这个目录不是 git 仓库，评论无处可写".into());
                }
                git::commit_event(
                    root,
                    EventKind::Comment,
                    Some(&target),
                    Some(&goal),
                    &subject,
                    body,
                )?;
                Ok("评论已记录（走空提交，永不冲突）。".into())
            }

            "events" => {
                let store = self.store()?;
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(30) as usize;
                let evs = git::events(store.project_root(), limit);
                if evs.is_empty() {
                    return Ok("还没有任何团队事件。".into());
                }
                let mut out = Vec::new();
                for e in evs {
                    let who = e.author.as_ref().map(|a| a.short().to_string()).unwrap_or_default();
                    let target = e.target.as_deref().unwrap_or("-");
                    if e.body.trim().is_empty() {
                        out.push(format!("- {} {} {} ({})", e.sha, e.kind.as_str(), target, who));
                    } else {
                        out.push(format!(
                            "- {} {} {} ({})：{}",
                            e.sha,
                            e.kind.as_str(),
                            target,
                            who,
                            e.body.replace('\n', " ")
                        ));
                    }
                }
                Ok(out.join("\n"))
            }

            "board_new" => {
                let store = self.store()?;
                let me = self.me()?;
                let title = title.ok_or("board_new 需要 title")?;
                let mut board = Board::new(title.trim(), &me);
                board.description = desc.unwrap_or("").trim().to_string();
                if let Some(g) = goal_id.filter(|g| !g.trim().is_empty()) {
                    store.goal(g)?;
                    board.goal_id = Some(g.to_string());
                }
                // 回顾看板的列和别人不一样，这里按标题判一下，省得模型还要多调一次。
                if title.contains("回顾") || title.to_ascii_lowercase().contains("retro") {
                    board.columns = ["做得好的", "要改的", "待跟进"]
                        .into_iter()
                        .enumerate()
                        .map(|(i, name)| BoardColumn {
                            id: format!("col-{}", i + 1),
                            name: name.into(),
                            order: i as f64 + 1.0,
                            wip_limit: None,
                        })
                        .collect();
                }
                store.save_board(&board)?;
                self.stage(&store);
                Ok(format!(
                    "已新建看板「{}」。\nid = {}\n列：{}",
                    board.title,
                    board.id,
                    board
                        .columns
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                ))
            }

            "card_new" => {
                let store = self.store()?;
                let me = self.me()?;
                let bid = board_id.ok_or("card_new 需要 board_id")?;
                let body = text.ok_or("card_new 需要 text")?.trim();
                if body.is_empty() {
                    return Err("text 不能为空".into());
                }
                let board = store.board(bid)?;
                let col = match args.get("column_id").and_then(Value::as_str) {
                    Some(c) if !c.trim().is_empty() => board
                        .columns
                        .iter()
                        .find(|x| x.id == c.trim())
                        .ok_or(format!("看板里没有列 `{c}`"))?
                        .id
                        .clone(),
                    _ => board
                        .columns
                        .iter()
                        .min_by(|a, b| a.order.partial_cmp(&b.order).unwrap_or(std::cmp::Ordering::Equal))
                        .map(|c| c.id.clone())
                        .ok_or("这块看板一列都没有")?,
                };
                let mut card = Card::new(&board.id, &col, body, &me);
                card.order = store
                    .cards(&board.id)
                    .items
                    .iter()
                    .filter(|c| c.column_id == card.column_id)
                    .map(|c| c.order)
                    .fold(0.0f64, f64::max)
                    + 1.0;
                if let Some(t) = task_id.filter(|t| !t.trim().is_empty()) {
                    self.locate_task(&store, &json!({"task_id": t}))?;
                    card.task_id = Some(t.to_string());
                }
                store.save_card(&card)?;
                self.stage(&store);
                Ok(format!("已加卡片。\nid = {}", card.id))
            }

            "card_move" => {
                let store = self.store()?;
                let me = self.me()?;
                let bid = board_id.ok_or("card_move 需要 board_id")?;
                let cid = card_id.ok_or("card_move 需要 card_id")?;
                let board = store.board(bid)?;
                let target = args
                    .get("column_id")
                    .and_then(Value::as_str)
                    .ok_or("card_move 需要 column_id")?;
                if !board.columns.iter().any(|c| c.id == target) {
                    return Err(format!("看板里没有列 `{target}`"));
                }
                let mut card = store
                    .cards(bid)
                    .items
                    .into_iter()
                    .find(|c| c.id == cid)
                    .ok_or(format!("找不到卡片 {cid}"))?;
                card.column_id = target.to_string();
                card.order = store
                    .cards(bid)
                    .items
                    .iter()
                    .filter(|c| c.column_id == target)
                    .map(|c| c.order)
                    .fold(0.0f64, f64::max)
                    + 1.0;
                card.touch(&me);
                store.save_card(&card)?;
                self.stage(&store);
                Ok("卡片已移动。".into())
            }

            other => Err(format!(
                "不认识的 action `{other}`。可用的：overview / my_tasks / goal_new / goal_update / \
                 task_new / task_update / task_status / task_claim / task_release / comment / events / \
                 board_new / card_new / card_move"
            )),
        }
    }

    /// 按 `task_id` 找到任务，返回 `(goal_id, task)`。
    ///
    /// 不要求调用方同时给 `goal_id`：任务 id 全局唯一，而让模型多填一个字段就多一次
    /// 填错的机会。给 `goal_id` 也能用 —— 那样会在目标里找，找不到再全局找。
    fn locate_task(&self, store: &Store, args: &Value) -> Result<(String, Task), String> {
        let id = args
            .get("task_id")
            .and_then(Value::as_str)
            .ok_or("需要 task_id")?
            .trim();
        if id.is_empty() {
            return Err("task_id 不能为空".into());
        }
        if let Some(g) = args.get("goal_id").and_then(Value::as_str) {
            if let Ok(t) = store.task(g.trim(), id) {
                return Ok((t.goal_id.clone(), t));
            }
        }
        for goal in store.goals().items {
            if let Ok(t) = store.task(&goal.id, id) {
                return Ok((t.goal_id.clone(), t));
            }
        }
        Err(format!("找不到任务 {id}"))
    }

    /// 写之前的统一闸门：把校验器的话原样回灌给模型。
    ///
    /// 回灌的不是「失败了」，而是**具体哪里不对、该怎么改**（见 `validate::Issue`）。
    /// 差别很实际：只说「失败」时模型会原样重试一次，再失败一次，第三次开始瞎猜；
    /// 说了「owner 得是 `名字 <邮箱>` 的形式」它一次就能改对。
    ///
    /// 错误里带上原来的参数形状，模型不需要回头翻自己上一条调用。
    fn gate(&self, issues: Vec<super::validate::Issue>) -> Result<(), String> {
        if issues.is_empty() {
            return Ok(());
        }
        let mut out = String::from("这次改动没有写进文件，先按下面的说明改：");
        for (i, issue) in issues.iter().enumerate() {
            out.push_str(&format!("\n{}. {}", i + 1, issue.render()));
        }
        Err(out)
    }

    fn check_goal(&self, _store: &Store, goal: &Goal) -> Result<(), String> {
        self.gate(super::validate::check_goal("", goal))
    }

    /// 任务：单体征 + 跨任务（依赖成环、指向不存在的任务）。
    ///
    /// 跨任务那部分必须在**写之前**做：成环一旦落盘，`ready` 会永远算成「等着」，
    /// 界面上表现为任务永远开不了工，而且看不出原因。
    fn check_task(&self, store: &Store, task: &Task) -> Result<(), String> {
        let mut issues = super::validate::check_task("", task);
        // 候选版本一起参与跨任务检查：新增 / 改动的依赖要和不写下去的现状一起验。
        let mut peers: Vec<Task> = store
            .tasks(&task.goal_id)
            .items
            .into_iter()
            .filter(|t| t.id != task.id)
            .collect();
        peers.push(task.clone());
        issues.extend(super::validate::check_task_set(&task.goal_id, &peers));
        // 没有文件路径可指的那些（`path` 为空）对模型没用，统一挂在当前这次改动上。
        self.gate(issues)
    }

    /// 全景：目标 + 每个目标的任务树 + 进度。
    ///
    /// 输出是**给模型读的**，比 JSON 省掉大量 token，也比 JSON 好判断「下一步该做什么」。
    fn overview(&self, store: &Store) -> String {
        let goals = store.goals();
        let mut out = Vec::new();
        if goals.items.is_empty() {
            let mut s = "还没有任何目标。用 goal_new 建一个，再用 task_new 往下拆。".to_string();
            if !goals.issues.is_empty() {
                s.push_str(&format!("\n（有 {} 个目标文件读不出来）", goals.issues.len()));
            }
            return s;
        }
        for goal in &goals.items {
            out.push(format!(
                "## {}  [{}] {}  ({})",
                goal.title,
                goal.status.label(),
                goal.priority.as_str(),
                goal.id
            ));
            if !goal.description.trim().is_empty() {
                out.push(format!("   {}", goal.description.trim()));
            }
            match store.goal_view(&goal.id) {
                Err(e) => out.push(format!("   （读不出来：{e}）")),
                Ok(v) => {
                    let c = v.counts;
                    out.push(format!(
                        "   完成 {}/{}（{}%），进行中 {}，受阻 {}",
                        c.done,
                        c.total.saturating_sub(c.dropped),
                        c.percent(),
                        c.doing,
                        c.blocked
                    ));
                    // 按父子排成缩进树。
                    let by_id: std::collections::HashMap<&str, &TaskView> =
                        v.tasks.iter().map(|t| (t.task.id.as_str(), t)).collect();
                    fn walk(
                        id: &str,
                        depth: usize,
                        by_id: &std::collections::HashMap<&str, &TaskView>,
                        out: &mut Vec<String>,
                    ) {
                        let Some(tv) = by_id.get(id) else { return };
                        let t = &tv.task;
                        let who = t
                            .assignee
                            .as_deref()
                            .and_then(|a| a.split('<').next())
                            .map(|n| format!(" @{}", n.trim()))
                            .unwrap_or_default();
                        let waiting = if tv.waiting_on.is_empty() {
                            String::new()
                        } else {
                            format!(" 等：{}", tv.waiting_on.join(","))
                        };
                        let reason = if t.blocked_reason.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" 原因：{}", t.blocked_reason.trim())
                        };
                        out.push(format!(
                            "{}{}- [{}] {}  ({}){}{}{}",
                            "   ".repeat(depth + 1),
                            if tv.ready { "▶ " } else { "" },
                            t.status.label(),
                            t.title,
                            t.id,
                            who,
                            waiting,
                            reason
                        ));
                        for c in &tv.children {
                            walk(c, depth + 1, by_id, out);
                        }
                    }
                    for root in &v.roots {
                        walk(root, 0, &by_id, &mut out);
                    }
                }
            }
        }
        if !goals.issues.is_empty() {
            out.push(format!(
                "\n有 {} 个目标文件读不出来：{}",
                goals.issues.len(),
                goals
                    .issues
                    .iter()
                    .map(|(p, _)| p.as_str())
                    .collect::<Vec<_>>()
                    .join("、")
            ));
        }
        out.join("\n")
    }
}

/// 装团队工具。
///
/// **恒常挂上，不预先探测有没有 `.carter/`。** 两个理由：
///
/// 1. 工具表一变，请求前缀就变（与 `goal.rs` 模块头关于 cache 的说明同一个约束）。
///    如果有没有团队目录决定挂不挂，那么用户点一次「初始化团队」，同一条对话的
///    工具表就变了 —— 缓存全废，而且模型会重新打量整个工具列表。
/// 2. 更实际的问题：工具池在会话起的时候装配一次。初始化完团队目录之后，
///    当前这条对话里根本拿不到这个工具，用户得重开对话才生效 —— 没人会预期到。
///
/// 所以恒常挂上，由工具自己在没有团队目录时给出「先去初始化」的明确错误。
///
/// 只挂**主对话**：侧边聊天（`/btw`）不挂 —— 它是一个不落盘的临时抽屉，在里面
/// 改团队数据等于「问个问题顺手改了账本」。子 agent 也不挂 —— 它们拿的是主对话
/// 工具表的快照，而快照是在这一处之前取的（见 `tools/mod.rs`）。
pub fn attach(cwd: &std::path::Path) -> Arc<dyn Tool> {
    Arc::new(TeamTool::new(cwd.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn project(tag: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("carter-ttool-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "测试"],
            vec!["config", "user.email", "t@example.com"],
        ] {
            std::process::Command::new("git")
                .current_dir(&dir)
                .args(&args)
                .output()
                .unwrap();
        }
        super::super::init::initialize(&dir, Some("测试团队")).unwrap();
        dir
    }

    fn call(tool: &TeamTool, args: Value) -> String {
        match tool.run(&args) {
            Ok(s) => s,
            Err(e) => panic!("调用失败：{e}"),
        }
    }

    /// 从回灌文本里取 `id = ...` 那一行。
    fn id_of(out: &str) -> String {
        out.lines()
            .find_map(|l| l.trim().strip_prefix("id = "))
            .unwrap_or_else(|| panic!("回灌里没有 id：{out}"))
            .trim()
            .to_string()
    }

    #[test]
    fn create_goal_then_tasks_and_report_overview() {
        let dir = project("flow");
        let t = TeamTool::new(dir.clone());

        let out = call(&t, json!({"action": "goal_new", "title": "第一阶段", "description": "把数据层做完"}));
        let gid = id_of(&out);

        let out = call(&t, json!({"action": "task_new", "goal_id": gid, "title": "写模型"}));
        let tid = id_of(&out);
        call(&t, json!({"action": "task_new", "goal_id": gid, "title": "写存储", "depends_on": [tid]}));

        let overview = call(&t, json!({"action": "overview"}));
        assert!(overview.contains("第一阶段"));
        assert!(overview.contains("写模型"));
        assert!(overview.contains("等："), "依赖要显示出来：{overview}");

        // 完成前置之后，后面的任务应该变成「可以开工」。
        call(&t, json!({"action": "task_status", "task_id": tid, "status": "done"}));
        let overview = call(&t, json!({"action": "overview"}));
        assert!(overview.contains("▶ "), "前置完成后要标出可开工：{overview}");
    }

    #[test]
    fn blocked_requires_a_reason() {
        let dir = project("blocked");
        let t = TeamTool::new(dir);
        let out = call(&t, json!({"action": "goal_new", "title": "g"}));
        let gid = id_of(&out);
        let out = call(&t, json!({"action": "task_new", "goal_id": gid, "title": "t"}));
        let tid = id_of(&out);

        let err = t
            .run(&json!({"action": "task_status", "task_id": tid, "status": "blocked"}))
            .unwrap_err();
        assert!(err.contains("blocked_reason"), "{err}");
    }

    #[test]
    fn claiming_writes_an_event_and_sets_the_assignee() {
        let dir = project("claim");
        let t = TeamTool::new(dir.clone());
        let out = call(&t, json!({"action": "goal_new", "title": "g"}));
        let gid = id_of(&out);
        let out = call(&t, json!({"action": "task_new", "goal_id": gid, "title": "t"}));
        let tid = id_of(&out);

        call(&t, json!({"action": "task_claim", "task_id": tid}));
        let store = Store::discover(&dir).unwrap();
        let task = store.task(&gid, &tid).unwrap();
        assert_eq!(task.assignee.as_deref(), Some("测试 <t@example.com>"));

        // 事件流里应该有一条 claim。
        let evs = git::events(&dir, 10);
        assert!(
            evs.iter().any(|e| e.kind == EventKind::Claim && e.target.as_deref() == Some(&tid)),
            "{evs:?}"
        );

        // 名下的任务看得到。
        let mine = call(&t, json!({"action": "my_tasks"}));
        assert!(mine.contains("t"), "{mine}");
    }

    #[test]
    fn a_second_person_cannot_quietly_steal_a_claim() {
        let dir = project("steal");
        let t = TeamTool::new(dir.clone());
        let out = call(&t, json!({"action": "goal_new", "title": "g"}));
        let gid = id_of(&out);
        let out = call(&t, json!({"action": "task_new", "goal_id": gid, "title": "t"}));
        let tid = id_of(&out);

        // 手工造一个「别人已经认领」的状态。
        let store = Store::open(dir.join(".carter"));
        let mut task = store.task(&gid, &tid).unwrap();
        task.assignee = Some("别人 <other@example.com>".into());
        store.save_task(&task).unwrap();

        let err = t
            .run(&json!({"action": "task_claim", "task_id": tid}))
            .unwrap_err();
        assert!(err.contains("已经被"), "{err}");
    }

    #[test]
    fn comments_become_events_with_the_author_and_body() {
        let dir = project("comment");
        let t = TeamTool::new(dir.clone());
        let out = call(&t, json!({"action": "goal_new", "title": "g"}));
        let gid = id_of(&out);

        call(&t, json!({"action": "comment", "goal_id": gid, "text": "第一阶段先不做界面"}));
        let evs = git::events(&dir, 5);
        let c = evs.iter().find(|e| e.kind == EventKind::Comment).expect("要有一条评论");
        assert_eq!(c.body, "第一阶段先不做界面");
        assert_eq!(c.author.as_ref().unwrap().email, "t@example.com");
    }

    #[test]
    fn a_board_can_be_created_and_cards_moved() {
        let dir = project("board");
        let t = TeamTool::new(dir.clone());
        let out = call(&t, json!({"action": "board_new", "title": "十月回顾"}));
        let bid = id_of(&out);

        let store = Store::open(dir.join(".carter"));
        let board = store.board(&bid).unwrap();
        assert_eq!(board.columns.len(), 3);
        assert_eq!(board.columns[0].name, "做得好的", "回顾看板要用回顾的列");

        let out = call(&t, json!({"action": "card_new", "board_id": bid, "text": "拆成一实体一文件很值"}));
        let kid = id_of(&out);
        call(&t, json!({"action": "card_move", "board_id": bid, "card_id": kid, "column_id": "col-3"}));

        let cards = store.cards(&bid).items;
        assert_eq!(cards[0].column_id, "col-3");
    }

    #[test]
    fn missing_team_dir_says_how_to_fix_it() {
        let dir = std::env::temp_dir().join(format!("carter-ttool-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let t = TeamTool::new(dir);
        let err = t.run(&json!({"action": "overview"})).unwrap_err();
        assert!(err.contains("初始化"), "{err}");
    }
}

#[cfg(test)]
mod e2e {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 造一个「裸远端 + 两个成员」的真实场景。
    ///
    /// 这不是模拟：两个独立的工作副本各自 `git config` 成不同的人，认领走的就是
    /// 真实的 `commit` + `push`。空提交当作裁决者这件事，只有在真 push 被拒的时候
    /// 才看得出来对不对。
    struct World {
        base: PathBuf,
        alice: PathBuf,
        bob: PathBuf,
    }

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} 起不来：{e}"));
        assert!(
            out.status.success(),
            "git {args:?} 失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn world(tag: &str) -> World {
        let n = std::process::id();
        let base = std::env::temp_dir().join(format!("carter-e2e-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&base);
        let remote = base.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git_in(&remote, &["init", "--bare", "-q", "-b", "main"]);

        let seed = base.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git_in(&seed, &["init", "-q", "-b", "main"]);
        git_in(&seed, &["config", "user.name", "seed"]);
        git_in(&seed, &["config", "user.email", "seed@example.com"]);
        std::fs::write(seed.join("README.md"), "demo\n").unwrap();
        git_in(&seed, &["add", "-A"]);
        git_in(&seed, &["commit", "-qm", "init"]);
        git_in(&seed, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git_in(&seed, &["push", "-q", "-u", "origin", "main"]);

        super::super::init::initialize(&seed, Some("联调团队")).unwrap();
        git_in(&seed, &["push", "-q"]);

        // 闭包在这里借 `base` 会与最后的 move 冲突，写成带参数的局部函数。
        fn clone_into(base: &Path, remote: &Path, who: &str, name: &str, email: &str) -> PathBuf {
            let dir = base.join(who);
            git_in(
                base,
                &[
                    "clone",
                    "-q",
                    remote.to_str().unwrap(),
                    dir.to_str().unwrap(),
                ],
            );
            git_in(&dir, &["config", "user.name", name]);
            git_in(&dir, &["config", "user.email", email]);
            dir
        }
        let alice = clone_into(&base, &remote, "alice", "张三", "zhang@example.com");
        let bob = clone_into(&base, &remote, "bob", "李四", "li@example.com");
        World { base, alice, bob }
    }

    fn id_of(out: &str) -> String {
        out.lines()
            .find_map(|l| l.trim().strip_prefix("id = "))
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn two_members_share_goals_through_the_remote() {
        let w = world("share");
        let alice = TeamTool::new(w.alice.clone());
        let gid = id_of(&alice.run(&json!({"action": "goal_new", "title": "第一阶段"})).unwrap());
        let tid = id_of(
            &alice
                .run(&json!({"action": "task_new", "goal_id": gid, "title": "写数据层"}))
                .unwrap(),
        );

        // 李四还没拉到任何东西。
        let bob = TeamTool::new(w.bob.clone());
        bob.run(&json!({"action": "task_new", "goal_id": gid, "title": "x"}))
            .unwrap_err();

        // 张三再留一条评论 —— 那是走空提交的事件，与状态文件是两条不同的通道。
        alice
            .run(&json!({"action": "comment", "goal_id": gid, "text": "数据层先落地"}))
            .unwrap();

        // 张三把改动推到远端之后，李四就能看见了。
        let store = Store::discover(&w.alice).unwrap();
        let rel = ".carter";
        git::stage_team(store.project_root(), rel).unwrap();
        git::commit_team(store.project_root(), rel, "feat(carter): 第一阶段").unwrap();
        git::push(store.project_root()).unwrap();

        assert!(git::sync(&w.bob).is_ok());
        let overview = bob.run(&json!({"action": "overview"})).unwrap();
        assert!(overview.contains("第一阶段"), "{overview}");
        assert!(overview.contains("写数据层"), "{overview}");
        assert!(overview.contains(&tid));
        // 事件流也同步过来了，而且带着作者与正文。
        let evs = git::events(&w.bob, 20);
        let c = evs
            .iter()
            .find(|e| e.kind == EventKind::Comment)
            .unwrap_or_else(|| panic!("李四没看到张三的评论：{evs:?}"));
        assert_eq!(c.body, "数据层先落地");
        assert_eq!(c.author.as_ref().unwrap().name, "张三");
        let _ = std::fs::remove_dir_all(&w.base);
    }

    #[test]
    fn a_rejected_claim_is_rolled_back_locally() {
        let w = world("race");
        let alice = TeamTool::new(w.alice.clone());
        let gid = id_of(&alice.run(&json!({"action": "goal_new", "title": "g"})).unwrap());
        let tid = id_of(
            &alice.run(&json!({"action": "task_new", "goal_id": gid, "title": "抢我"})).unwrap(),
        );
        let store_a = Store::discover(&w.alice).unwrap();
        git::stage_team(store_a.project_root(), ".carter").unwrap();
        git::commit_team(store_a.project_root(), ".carter", "feat: 建任务").unwrap();
        git::push(store_a.project_root()).unwrap();
        git::sync(&w.bob).unwrap();

        // 张三先认领并推上去。
        alice.run(&json!({"action": "task_claim", "task_id": tid})).unwrap();

        // 李四还拿着旧的远端视图去抢 —— 结果本地的推送被远端拒绝。
        let bob = TeamTool::new(w.bob.clone());
        let err = bob
            .run(&json!({"action": "task_claim", "task_id": tid}))
            .unwrap_err();
        assert!(err.contains("push 被拒"), "{err}");

        // 关键：本地不能留下「我认领了」的假象。
        let store_b = Store::discover(&w.bob).unwrap();
        let task = store_b.task(&gid, &tid).unwrap();
        assert!(
            task.assignee.is_none(),
            "推送被拒之后本地必须退回原样，实际是 {:?}",
            task.assignee
        );
        // 工作区要干净：不能留一条未推送的认领提交或一堆暂存改动。
        assert!(
            git::status_porcelain(store_b.project_root()).trim().is_empty(),
            "工作区应该干净：{}",
            git::status_porcelain(store_b.project_root())
        );

        // 同步之后本地应该看到张三的认领。
        git::sync(store_b.project_root()).unwrap();
        let task = store_b.task(&gid, &tid).unwrap();
        assert_eq!(task.assignee.as_deref(), Some("张三 <zhang@example.com>"));
        let _ = std::fs::remove_dir_all(&w.base);
    }
}
