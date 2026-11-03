//! `team`：让模型直接读写 `.carter/` 里的目标、任务、看板。
//!
//! 看板是一整页 HTML（见 `model.rs` 的 [`Board`]）：工具只负责发号、补标题与关联、
//! 原子写盘，版式全由模型决定。
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
//! * 事件（`comment` / `mention` / `broadcast` / `assign` / `claim` …）例外：它们走
//!   独立的事件 ref（`refs/carter/events`，见 `events.rs`），写完立刻推送，不等轮末。
//!   `claim` 还要求推送成功 —— 推送成功才算认领成功。

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use super::events::{self, Draft, EventKind};
use super::git;
use super::members::{self, Roster};
use super::model::*;
use super::store::Store;

pub struct TeamTool {
    /// 会话的工作目录。团队目录从它往上找 —— 与技能扫描同一套规则。
    cwd: PathBuf,
    /// 写事件时的 `via`。模型调用是 `carter`（界面上标「经卡特」）；界面上人自己点的
    /// 是 `None`。两条入口走同一份逻辑，只差这一个标记 —— 分两份写，迟早会出现
    /// 「界面上指派会发通知、让卡特指派却不发」这种不一致。
    via: Option<&'static str>,
}

impl TeamTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd, via: Some("carter") }
    }

    /// 界面上人自己操作用的。事件不带 `via`。
    pub fn for_person(cwd: PathBuf) -> Self {
        Self { cwd, via: None }
    }

    /// 给界面用的入口：参数与工具调用完全一样。
    pub fn act(&self, args: &Value) -> Result<String, String> {
        self.run(args)
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

    /// 本机在名单里的成员 id。还没加入时用 git 身份串顶上 —— 不阻止模型干活，
    /// 但写出去的引用是旧格式，等用户在界面上加入之后再写的就是 id 了。
    fn me_ref(&self, roster: &Roster) -> Result<String, String> {
        let me = self.me()?;
        Ok(roster
            .me(&me)
            .map(|m| m.id.clone())
            .unwrap_or_else(|| me.as_key()))
    }

    /// 把一个「人」的写法解析成成员 id。认不出就报错，并把名单列给模型 ——
    /// 让它自己挑一个对的，而不是写一个谁都匹配不上的名字进任务文件。
    fn person(&self, roster: &Roster, raw: &str) -> Result<String, String> {
        if let Some(m) = roster.resolve(raw) {
            return Ok(m.id.clone());
        }
        // 还没加入的人（新同事、名单为空）可以用 `名字 <邮箱>`：读的地方都认，
        // 他加入之后按邮箱就能对上。
        if let Some(i) = Identity::parse(raw).filter(|i| !i.email.is_empty()) {
            return Ok(i.as_key());
        }
        let names = roster
            .members
            .iter()
            .map(|m| format!("{}（{}）", m.name, m.id))
            .collect::<Vec<_>>()
            .join("、");
        Err(if names.is_empty() {
            format!("认不出「{raw}」是谁。团队里还没有人加入，写成 `名字 <邮箱>` 的形式。")
        } else {
            format!(
                "认不出「{raw}」是谁（重名时也认不出）。团队成员有：{names}。\
                 写成员 id，或者还没加入的人写 `名字 <邮箱>`。"
            )
        })
    }

    /// 写一条事件并尽量推送。推送失败只提示，不算失败 —— 事件已经在本地链上，
    /// 下一次推送会带上它。（认领例外，见 `task_claim`。）
    fn emit(&self, store: &Store, mut d: Draft) -> Result<String, String> {
        let roster = members::load_all(&store);
        if d.from.is_none() {
            d.from = member_only(self.me_ref(&roster)?);
        }
        // 收件人同理：还没加入的人没有 id，`to=` 里写不了他。消息照样记下，
        // 他加入之后能在动态里看到，只是不会被单独提醒 —— 回灌里说清楚。
        let before = d.to.len();
        d.to.retain(|t| t == events::EVERYONE || members::valid_member_id(t));
        let dropped = before - d.to.len();
        // 模型调的工具一律标成「经卡特」：人发的和 agent 代发的要能一眼分开。
        if d.via.is_none() {
            d.via = self.via.map(str::to_string);
        }
        let root = store.project_root();
        let (sha, eid) = events::append(root, &d)?;
        let pushed = match events::push(root) {
            Ok(true) => "已推送".to_string(),
            Ok(false) => "还没有远端，只记在本地".to_string(),
            Err(e) => format!("推送失败（下次会再推）：{e}"),
        };
        let note = if dropped > 0 {
            format!(" · {dropped} 个收件人还没加入团队，收不到提醒")
        } else {
            String::new()
        };
        Ok(format!("{sha} · {eid} · {pushed}{note}"))
    }

    /// 指派 / 转交之后给新负责人发一条事件，返回附在回灌末尾的一句话。
    ///
    /// 失败不影响指派本身（任务文件已经写好了）：只是对方要等同步之后才在「我的」里
    /// 看到，不会收到提醒。把原因说出来，模型可以转告用户。
    fn notify(&self, store: &Store, task: &Task, to: &str, kind: &str, note: &str) -> String {
        let d = Draft {
            kind: kind.into(),
            target: Some(task.id.clone()),
            goal_id: Some(task.goal_id.clone()),
            to: vec![to.to_string()],
            body: note.to_string(),
            ..Default::default()
        };
        match self.emit(store, d) {
            Ok(out) => format!(" 已通知对方（{out}）。"),
            Err(e) => format!(" 但没能发出通知：{e}"),
        }
    }

    /// 新建 / 更新任务时顺手带上的指派通知。指派给自己不发 —— 自己不用提醒自己。
    fn notify_assign(&self, store: &Store, roster: &Roster, task: &Task, to: &str) -> String {
        match self.me_ref(roster) {
            Ok(me) if roster.same(&me, to) => String::new(),
            _ => self.notify(store, task, to, EventKind::ASSIGN, ""),
        }
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

/// 只有成员 id 才能进 trailer（`名字 <邮箱>` 里有空格）。还没加入的人不写 `from`，
/// 读的人按提交作者的邮箱去认（协议 3.4）。
fn member_only(r: String) -> Option<String> {
    members::valid_member_id(&r).then_some(r)
}

/// 这个任务上，排在**我最近那条认领之前**、最近的一条认领是不是别人的。是的话返回那人的名字。
///
/// 用来裁决「两个人同时认领」：推送之后链上的顺序就是全团队公认的顺序，排在我前面、
/// 中间又没有被释放的那条认领赢。排在我**后面**的认领是输家，与我无关。
///
/// 我还没认领过（认领前的预检）时，就看链上最近的一条：是别人的认领就说明已经被占了。
/// 顺序取链上的（[`events::chain`]），不按提交时间 —— 各人的本机时钟不可信。
fn claimed_by_other(root: &std::path::Path, task_id: &str, me: &str, roster: &Roster) -> Option<String> {
    let evs: Vec<_> = events::chain(root, 500)
        .into_iter()
        .filter(|e| e.target.as_deref() == Some(task_id))
        .collect();
    // 发出者：有成员 id 用 id；还没人加入时（没有 `from`）退回提交作者的 `名字 <邮箱>`，
    // 与 `me_ref` 在同样情况下给出的写法对得上。
    let from_of = |e: &events::Event| {
        super::inbox::sender(e, roster)
            .or_else(|| e.author.as_ref().map(|a| a.as_key()))
            .unwrap_or_default()
    };
    let is_mine = |e: &events::Event| {
        let f = from_of(e);
        f == me || roster.same(&f, me)
    };
    let start = evs
        .iter()
        .position(|e| e.kind.is(EventKind::CLAIM) && is_mine(e))
        .map(|i| i + 1)
        .unwrap_or(0);
    for e in &evs[start..] {
        if e.kind.is(EventKind::RELEASE) {
            return None;
        }
        if e.kind.is(EventKind::CLAIM) {
            return if is_mine(e) { None } else { Some(roster.display(&from_of(e))) };
        }
    }
    None
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
         或者要你把这件活记下来、更新进展、认领 / 指派任务、留评论、@ 某人、发广播。\n\
         \n\
         什么时候不该用：项目没有 `.carter/`（先让用户初始化）；只是聊天。\n\
         \n\
         规矩：\n\
         * id 一律由工具生成，不要自己编。\n\
         * 状态推进用 `task_status`，不要靠改文件。\n\
         * `claim` 会 push，被拒说明别人先占了 —— 换个任务，不要覆盖。\n\
         * 指派之前先 `members` 看有谁；拿不准派给谁就列出建议，让用户确认。\n\
         * 消息（comment / mention / broadcast / request）都会标成「经卡特」，\
           只在用户要你说、或者完成 / 受阻 / 改方案需要留个记录时才发。\n\
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
                        "assign", "members",
                        "comment", "mention", "broadcast", "request", "note", "inbox", "events",
                        "check", "sync",
                        "board_list", "board_new", "board_update"
                    ],
                    "description": "要做什么。先看 action 的分组：读用 overview / my_tasks / events；目标用 goal_*；任务用 task_*；看板用 board_*。"
                },
                "goal_id": { "type": "string", "description": "目标 id（g- 开头）。goal_* 与 task_new 必填。" },
                "task_id": { "type": "string", "description": "任务 id（t- 开头）。task_* 必填。" },
                "board_id": { "type": "string", "description": "看板 id（boards/ 下的文件名，不带 .html）。board_update 必填。" },
                "html": {
                    "type": "string",
                    "description": "看板的整页 HTML（board_new 必填；board_update 给了就整页替换）。自由排版，样式与脚本都内联；格式约定见 carter-board 技能。"
                },
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
                "assignee": { "type": "string", "description": "负责人：成员 id（m- 开头）或昵称，先用 members 看有谁。还没加入的人写 `名字 <邮箱>`。task_* 里是任务负责人，goal_update 里是目标负责人；传空串表示清掉。" },
                "to": {
                    "type": "array", "items": { "type": "string" },
                    "description": "收件人：成员 id 或昵称。mention / request 必填；comment 可选（顺带 @ 人）。broadcast 不用填（发给全员）。"
                },
                "due_at": { "type": "integer", "description": "期限（毫秒时间戳）。request 用。" },
                "reply_to": { "type": "string", "description": "回复哪条事件（事件的 eid，e- 开头）。" },
                "estimate_hours": { "type": "number", "description": "估时（小时）。" },
                "deliverable": { "type": "string", "description": "交付物：一句话说清交什么。" },
                "acceptance": {
                    "type": "array", "items": { "type": "string" },
                    "description": "验收标准，每条都要可验证（「cargo test 通过」而不是「质量好」）。"
                },
                "blocked_reason": { "type": "string", "description": "受阻原因。status = blocked 时填。" },
                "text": { "type": "string", "description": "消息正文（comment / mention / broadcast / request / note 必填）；assign 时是附言。卡片正文也用它（若有卡片）。" },
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

        // 不认识的参数直接报错，不静默忽略。
        //
        // 拼错一个字段（`prioirty`、`depend_on`）时，忽略它等于这次调用「成功了但没做
        // 那件事」—— 模型会以为优先级已经改了，而它根本没被读到。合法的名字取自
        // `parameters()` 那份 schema，加字段只改一处。
        if let Some(obj) = args.as_object() {
            let schema = self.parameters();
            let known = schema
                .get("properties")
                .and_then(Value::as_object)
                .ok_or("team 工具的参数表坏了")?;
            let unknown: Vec<&str> = obj
                .keys()
                .map(String::as_str)
                .filter(|k| !known.contains_key(*k))
                .collect();
            if !unknown.is_empty() {
                let mut legal: Vec<&str> = known.keys().map(String::as_str).collect();
                legal.sort_unstable();
                return Err(format!(
                    "不认识的参数：{}。这次调用没有执行。合法的参数有：{}",
                    unknown.join("、"),
                    legal.join(" / ")
                ));
            }
        }

        let s = args.get("status").and_then(Value::as_str);
        let title = args.get("title").and_then(Value::as_str);
        let desc = args.get("description").and_then(Value::as_str);
        let goal_id = args.get("goal_id").and_then(Value::as_str);
        let task_id = args.get("task_id").and_then(Value::as_str);
        let board_id = args.get("board_id").and_then(Value::as_str);
        let html = args.get("html").and_then(Value::as_str);
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

            "sync" => {
                // 同步做成一个工具动作，而不是只留着 `carter-team sync` 脚本：
                // 那个脚本是 POSIX sh + jq，Windows 上跑不起来。工具这条路
                // 三平台都一样，模型在任何系统上都能真的把远端拉下来。
                let store = self.store()?;
                let root = store.project_root();
                if !git::has_remote(root) {
                    return Ok(
                        "还没有配远端，团队数据目前只在本地。让用户在界面上点「配远端」填一个仓库地址。"
                            .into(),
                    );
                }
                git::sync(root)
            }

            "my_tasks" => {
                let store = self.store()?;
                let me = self.me()?;
                let roster = members::load_all(&store);
                let me_key = roster.me(&me).map(|m| m.id.clone()).unwrap_or_else(|| me.as_key());
                let mut out = Vec::new();
                for goal in store.goals().items {
                    if !goal.status.is_live() {
                        continue;
                    }
                    for t in store.tasks(&goal.id).items {
                        if t.assignee.as_deref().is_some_and(|a| roster.same(a, &me_key)) && t.status.is_open() {
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
                // 负责人：与任务的 assignee 同一个写法（成员 id / 昵称 / `名字 <邮箱>`），空串 = 清掉。
                if let Some(o) = args.get("assignee").and_then(Value::as_str) {
                    goal.owner = if o.trim().is_empty() {
                        None
                    } else {
                        Some(self.person(&members::load_all(&store), o)?)
                    };
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
                let roster = members::load_all(&store);
                if let Some(a) = args.get("assignee").and_then(Value::as_str).filter(|a| !a.trim().is_empty()) {
                    task.assignee = Some(self.person(&roster, a)?);
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

                // `id = …` 必须**单独一行**：技能让模型从回灌里取这一行拿到新 id，
                // 后面再跟半句说明就会一起被取走。
                let mut msg = format!("已新建任务「{}」。\nid = {}\n", task.title, task.id);
                if !task.depends_on.is_empty() {
                    msg.push_str(&format!(
                        "要等 {} 完成才能开始。\n",
                        task.depends_on.join("、")
                    ));
                }
                if let Some(a) = task.assignee.clone() {
                    msg.push_str(self.notify_assign(&store, &roster, &task, &a).trim_start());
                }
                Ok(msg.trim_end().to_string())
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
                let roster = members::load_all(&store);
                let before = task.assignee.clone();
                if let Some(a) = args.get("assignee").and_then(Value::as_str) {
                    task.assignee = if a.trim().is_empty() {
                        None
                    } else {
                        Some(self.person(&roster, a)?)
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
                let mut msg = format!("任务「{}」已更新。", task.title);
                if task.assignee != before {
                    if let Some(a) = task.assignee.clone() {
                        msg.push_str(&self.notify_assign(&store, &roster, &task, &a));
                    }
                }
                Ok(msg)
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
                let roster = members::load_all(&store);
                let me_ref = self.me_ref(&roster)?;
                let (_, mut task) = self.locate_task(&store, args)?;
                let claiming = action == "task_claim";
                let root = store.project_root();

                // 别人认领过就不抢：覆盖别人的认领在 git 里看不出来（那是「最后写入赢」），
                // 而后果是两个人同时在改同一件事。
                if claiming {
                    if let Some(a) = task.assignee.as_deref() {
                        if !a.trim().is_empty() && !roster.same(a, &me_ref) {
                            return Err(format!(
                                "「{}」已经被 {} 认领了。换个任务，或者先问一句（mention）。",
                                task.title,
                                roster.display(a)
                            ));
                        }
                    }
                }
                if !git::in_repo(root) {
                    return Err("这个目录不是 git 仓库，认领无处可记".into());
                }

                // 认领的裁决者是**事件链**的推送：两个人同时认领，链上排在前面的那条赢。
                // 先拉一次，有人刚抢先一步的话直接说，不必再写一条注定作废的认领。
                if claiming && git::has_remote(root) {
                    let _ = events::fetch(root);
                    if let Some(who) = claimed_by_other(root, &task.id, &me_ref, &roster) {
                        return Err(format!(
                            "「{}」已经被 {who} 认领了（远端事件里有）。换个任务，或者先问一句。",
                            task.title
                        ));
                    }
                }

                let original = task.clone();
                task.assignee = if claiming { Some(me_ref.clone()) } else { None };
                task.touch(&me);
                store.save_task(&task)?;
                self.stage(&store);

                let kind = if claiming { EventKind::CLAIM } else { EventKind::RELEASE };
                events::append(
                    root,
                    &Draft {
                        kind: kind.into(),
                        target: Some(task.id.clone()),
                        goal_id: Some(task.goal_id.clone()),
                        from: member_only(me_ref.clone()),
                        via: self.via.map(str::to_string),
                        ..Default::default()
                    },
                )?;

                if claiming && git::has_remote(root) {
                    // 推送成功、而且链上排在我前面的没有别人的认领，才算认领成功。
                    let lost = match events::push(root) {
                        Ok(_) => claimed_by_other(root, &task.id, &me_ref, &roster)
                            .map(|who| format!("{who} 先认领了")),
                        Err(e) => Some(format!("推送失败（{e}）")),
                    };
                    if let Some(why) = lost {
                        // **本地也要退回去。** 文件里写着自己、远端却是别人，两人各自看
                        // 自己那份都是对的，这种不一致没有任何地方会报出来。
                        let _ = store.save_task(&original);
                        self.stage(&store);
                        return Err(format!(
                            "认领没有生效：{why}「{}」。本地改动已撤回，换一个任务吧。",
                            task.title
                        ));
                    }
                } else {
                    let _ = events::push(root);
                }
                Ok(if claiming {
                    format!("已认领「{}」。做完记得 task_status 改成 done。", task.title)
                } else {
                    format!("已释放「{}」。", task.title)
                })
            }

            "assign" => {
                let store = self.store()?;
                let me = self.me()?;
                let roster = members::load_all(&store);
                let (_, mut task) = self.locate_task(&store, args)?;
                let who = args
                    .get("assignee")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("assign 需要 assignee（成员 id 或昵称）")?;
                let to = self.person(&roster, who)?;
                let previous = task.assignee.clone();
                task.assignee = Some(to.clone());
                task.touch(&me);
                self.check_task(&store, &task)?;
                store.save_task(&task)?;
                self.stage(&store);
                // 从别人手里转过来的是「转交」，空着派出去的是「指派」—— 收件人那边看到的话不一样。
                let handoff = previous
                    .as_deref()
                    .is_some_and(|p| !p.trim().is_empty() && !roster.same(p, &to));
                let kind = if handoff { EventKind::HANDOFF } else { EventKind::ASSIGN };
                let tail = self.notify(&store, &task, &to, kind, text.unwrap_or("").trim());
                Ok(format!(
                    "「{}」已{}给 {}。{tail}",
                    task.title,
                    if handoff { "转交" } else { "指派" },
                    roster.display(&to)
                ))
            }

            "comment" | "mention" | "broadcast" | "request" | "note" => {
                let store = self.store()?;
                let roster = members::load_all(&store);
                let body = text.ok_or(format!("{action} 需要 text"))?.trim();
                if body.is_empty() {
                    return Err("text 不能为空".into());
                }
                // 对象：任务优先（任务 id 能推出目标），其次目标。
                let (target, goal) = if let Some(tid) = task_id.filter(|t| !t.trim().is_empty()) {
                    let (gid, t) = self.locate_task(&store, &json!({ "task_id": tid }))?;
                    (Some(t.id), Some(gid))
                } else if let Some(gid) = goal_id.filter(|g| !g.trim().is_empty()) {
                    let g = store.goal(gid)?;
                    (Some(g.id.clone()), Some(g.id))
                } else {
                    (None, None)
                };
                if action == "comment" && target.is_none() {
                    return Err("comment 需要 task_id 或 goal_id；要对全员说话用 broadcast".into());
                }
                let mut to: Vec<String> = Vec::new();
                if action == "broadcast" {
                    to.push(events::EVERYONE.into());
                } else if let Some(list) = args.get("to").and_then(Value::as_array) {
                    for v in list.iter().filter_map(Value::as_str) {
                        to.push(self.person(&roster, v)?);
                    }
                }
                if matches!(action.as_str(), "mention" | "request") && to.is_empty() {
                    return Err(format!("{action} 需要 to（收件人的成员 id 或昵称列表）"));
                }
                let out = self.emit(
                    &store,
                    Draft {
                        kind: action.clone(),
                        target,
                        goal_id: goal,
                        to: to.clone(),
                        due: args.get("due_at").and_then(Value::as_u64),
                        reply_to: args
                            .get("reply_to")
                            .and_then(Value::as_str)
                            .filter(|s| !s.trim().is_empty())
                            .map(str::to_string),
                        body: body.to_string(),
                        ..Default::default()
                    },
                )?;
                let who = to
                    .iter()
                    .map(|t| if t == events::EVERYONE { "全员".to_string() } else { roster.display(t) })
                    .collect::<Vec<_>>()
                    .join("、");
                Ok(if who.is_empty() {
                    format!("已记录（{out}）。")
                } else {
                    format!("已发给 {who}（{out}）。")
                })
            }

            "members" => {
                let store = self.store()?;
                let roster = members::load_all(&store);
                let me_id = self
                    .me()
                    .ok()
                    .and_then(|m| roster.me(&m).map(|x| x.id.clone()));
                let mut out: Vec<String> = roster
                    .members
                    .iter()
                    .map(|m| {
                        let mark = if Some(&m.id) == me_id.as_ref() { "（我）" } else { "" };
                        let role = if m.role.trim().is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", m.role)
                        };
                        format!("- {}{mark}  {}{role}", m.name, m.id)
                    })
                    .collect();
                if out.is_empty() {
                    out.push("还没有人加入这个团队。".into());
                }
                if me_id.is_none() {
                    out.push("\n本机还不在名单里：让用户在团队页里点一下「加入团队」。".into());
                }
                let pending = members::git_candidates(store.project_root(), &roster);
                if !pending.is_empty() {
                    out.push(format!(
                        "\n提交过代码、但还没加入的人（要指派给他们就写 `名字 <邮箱>`）：{}",
                        pending
                            .iter()
                            .take(10)
                            .map(|i| i.as_key())
                            .collect::<Vec<_>>()
                            .join("、")
                    ));
                }
                Ok(out.join("\n"))
            }

            "inbox" => {
                let store = self.store()?;
                let me = self.me()?;
                let _ = events::fetch(store.project_root());
                let roster = members::load_all(&store);
                let titles = super::inbox::task_titles(&store);
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
                let items = super::inbox::read(&store, &me, limit);
                if items.is_empty() {
                    return Ok("没有跟你有关的消息。".into());
                }
                Ok(items
                    .iter()
                    .map(|i| format!("- {}", super::inbox::line(i, &roster, &titles)))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }

            "events" => {
                let store = self.store()?;
                let root = store.project_root();
                let _ = events::fetch(root);
                let roster = members::load_all(&store);
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(30) as usize;
                let evs = events::read(root, limit);
                if evs.is_empty() {
                    return Ok("还没有任何团队事件。".into());
                }
                let mut out = Vec::new();
                for e in evs {
                    let who = super::inbox::sender(&e, &roster)
                        .map(|id| roster.display(&id))
                        .or_else(|| e.author.as_ref().map(|a| a.short().to_string()))
                        .unwrap_or_default();
                    let target = e.target.as_deref().unwrap_or("-");
                    let to = if e.to.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " → {}",
                            e.to.iter()
                                .map(|t| if t == events::EVERYONE { "全员".into() } else { roster.display(t) })
                                .collect::<Vec<_>>()
                                .join("、")
                        )
                    };
                    let body = if e.body.trim().is_empty() {
                        String::new()
                    } else {
                        format!("：{}", e.body.replace('\n', " "))
                    };
                    out.push(format!(
                        "- {} {} {} ({who}){to}{body}  #{}",
                        e.sha,
                        e.kind.label(),
                        target,
                        e.eid
                    ));
                }
                Ok(out.join("\n"))
            }

            "board_list" => {
                let store = self.store()?;
                let boards = store.boards();
                let mut out: Vec<String> = boards
                    .items
                    .iter()
                    .map(|b| {
                        let goal = b.goal_id.as_deref().map(|g| format!(" · 关联 {g}")).unwrap_or_default();
                        format!("- {}  ({}){goal}  boards/{}.html", b.title, b.id, b.id)
                    })
                    .collect();
                if out.is_empty() {
                    out.push("还没有看板。用 board_new 建一块。".into());
                }
                for (p, why) in &boards.issues {
                    out.push(format!("（{p} 读不出来：{why}）"));
                }
                Ok(out.join("\n"))
            }

            "board_new" => {
                let store = self.store()?;
                let title = title.map(str::trim).filter(|t| !t.is_empty()).ok_or("board_new 需要 title")?;
                let body = html.map(str::trim).filter(|h| !h.is_empty()).ok_or(
                    "board_new 需要 html：整页内容由你来写（格式见 carter-board 技能）",
                )?;
                let goal = goal_id.map(str::trim).filter(|g| !g.is_empty());
                if let Some(g) = goal {
                    store.goal(g)?;
                }
                let id = new_id(Kind::Board);
                store.save_board(&id, &stamp_board(body, Some(title), goal))?;
                self.stage(&store);
                Ok(format!("已新建看板「{title}」。\nid = {id}\n文件：.carter/boards/{id}.html"))
            }

            "board_update" => {
                let store = self.store()?;
                let bid = board_id.ok_or("board_update 需要 board_id")?.trim();
                let old = store.board(bid)?;
                let goal = goal_id.map(str::trim).filter(|g| !g.is_empty());
                if let Some(g) = goal {
                    store.goal(g)?;
                }
                let new_title = title.map(str::trim).filter(|t| !t.is_empty());
                if html.is_none() && new_title.is_none() && goal.is_none() {
                    return Err("board_update 至少要给 html / title / goal_id 之一".into());
                }
                let body = match html.map(str::trim) {
                    Some("") => return Err("html 不能是空的；要删看板请让用户直接删文件".into()),
                    Some(h) => h.to_string(),
                    None => old.html.clone(),
                };
                // 换了整页却漏写 <title> / 关联：沿用旧的，免得看板名字退化成 id、关联悄悄丢掉。
                // 新页面自己写了的，以新页面为准。
                let replaced = html.is_some();
                let keep_title = (replaced && html_title(&body).is_none()).then_some(old.title.as_str());
                let keep_goal = (replaced && html_meta(&body, "carter-goal").is_none())
                    .then_some(old.goal_id.as_deref())
                    .flatten();
                let goal = goal.or(keep_goal);
                store.save_board(&old.id, &stamp_board(&body, new_title.or(keep_title), goal))?;
                self.stage(&store);
                Ok(format!("看板「{}」已更新。", new_title.unwrap_or(&old.title)))
            }

            other => Err(format!(
                "不认识的 action `{other}`。可用的：overview / my_tasks / goal_new / goal_update / \
                 task_new / task_update / task_status / task_claim / task_release / assign / members / \
                 comment / mention / broadcast / request / note / inbox / events / check / sync / board_*"
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
        let roster = members::load_all(&store);
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
                        roster: &Roster,
                        out: &mut Vec<String>,
                    ) {
                        let Some(tv) = by_id.get(id) else { return };
                        let t = &tv.task;
                        let who = t
                            .assignee
                            .as_deref()
                            .map(|a| format!(" @{}", roster.display(a)))
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
                            walk(c, depth + 1, by_id, roster, out);
                        }
                    }
                    for root in &v.roots {
                        walk(root, 0, &by_id, &roster, &mut out);
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
        let evs = events::read(&dir, 10);
        assert!(
            evs.iter().any(|e| e.kind.is(EventKind::CLAIM) && e.target.as_deref() == Some(&tid)),
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
        let evs = events::read(&dir, 5);
        let c = evs.iter().find(|e| e.kind.is(EventKind::COMMENT)).expect("要有一条评论");
        assert_eq!(c.body, "第一阶段先不做界面");
        assert_eq!(c.author.as_ref().unwrap().email, "t@example.com");
    }

    #[test]
    fn a_board_is_one_html_page_that_can_be_rewritten() {
        let dir = project("board");
        let t = TeamTool::new(dir.clone());
        let gid = id_of(&call(&t, json!({"action": "goal_new", "title": "g"})));
        let out = call(
            &t,
            json!({
                "action": "board_new", "title": "十月回顾", "goal_id": gid,
                "html": "<!doctype html><html><head></head><body><h1>做得好的</h1></body></html>"
            }),
        );
        let bid = id_of(&out);

        let store = Store::open(dir.join(".carter"));
        let board = store.board(&bid).unwrap();
        assert_eq!(board.title, "十月回顾", "标题由工具补进 <title>");
        assert_eq!(board.goal_id.as_deref(), Some(gid.as_str()));
        assert!(board.html.contains("<h1>做得好的</h1>"));

        // 整页替换时漏写了 <title>：沿用旧标题与关联，不能退化成 id。
        call(&t, json!({"action": "board_update", "board_id": bid, "html": "<p>改过了</p>"}));
        let board = store.board(&bid).unwrap();
        assert_eq!(board.title, "十月回顾");
        assert_eq!(board.goal_id.as_deref(), Some(gid.as_str()));
        assert!(board.html.contains("改过了"));

        // 只改名不动正文。
        call(&t, json!({"action": "board_update", "board_id": bid, "title": "十月复盘"}));
        let board = store.board(&bid).unwrap();
        assert_eq!(board.title, "十月复盘");
        assert!(board.html.contains("改过了"));

        assert!(call(&t, json!({"action": "board_list"})).contains("十月复盘"));
        assert!(t.run(&json!({"action": "board_new", "title": "空"})).is_err(), "没有 html 要拒绝");
        assert!(t.run(&json!({"action": "board_update", "board_id": "../x", "html": "x"})).is_err());
    }

    /// 拼错参数名时整次调用不执行，并列出合法参数 —— 否则「改优先级」会静默成功、
    /// 实际什么都没改。
    #[test]
    fn a_misspelled_parameter_is_rejected_not_ignored() {
        let dir = project("typo");
        let t = TeamTool::new(dir.clone());
        let gid = id_of(&call(&t, json!({"action": "goal_new", "title": "g"})));
        let err = t
            .run(&json!({"action": "goal_update", "goal_id": gid, "prioirty": "P0"}))
            .unwrap_err();
        assert!(err.contains("prioirty"), "{err}");
        assert!(err.contains("priority"), "要列出合法的参数名：{err}");
        let store = Store::discover(&dir).unwrap();
        assert_eq!(store.goal(&gid).unwrap().priority, Priority::P2, "被拒的调用不能有副作用");

        // 单个依赖照样能写（脚本时代的那个 bug 不该在工具里出现）。
        let a = id_of(&call(&t, json!({"action": "task_new", "goal_id": gid, "title": "a"})));
        let b = id_of(&call(&t, json!({"action": "task_new", "goal_id": gid, "title": "b", "depends_on": [a]})));
        assert_eq!(store.task(&gid, &b).unwrap().depends_on, vec![a]);
    }

    #[test]
    fn goal_update_can_set_and_clear_the_owner() {
        let dir = project("owner");
        let t = TeamTool::new(dir.clone());
        let gid = id_of(&call(&t, json!({"action": "goal_new", "title": "g"})));
        call(&t, json!({"action": "goal_update", "goal_id": gid, "assignee": "张三 <z@example.com>"}));
        let store = Store::discover(&dir).unwrap();
        assert_eq!(store.goal(&gid).unwrap().owner.as_deref(), Some("张三 <z@example.com>"));
        call(&t, json!({"action": "goal_update", "goal_id": gid, "assignee": ""}));
        assert_eq!(store.goal(&gid).unwrap().owner, None);
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
        let evs = events::read(&w.bob, 20);
        let c = evs
            .iter()
            .find(|e| e.kind.is(EventKind::COMMENT))
            .unwrap_or_else(|| panic!("李四没看到张三的评论：{evs:?}"));
        assert_eq!(c.body, "数据层先落地");
        assert_eq!(c.author.as_ref().unwrap().name, "张三");
        let _ = std::fs::remove_dir_all(&w.base);
    }

    /// 有人先认领了：后来的那个在写之前拉一次事件就能看到，直接拒绝，本地什么都不改。
    #[test]
    fn a_claim_already_on_the_remote_is_refused_up_front() {
        let w = world("race");
        let (gid, tid) = shared_task(&w, "抢我");

        // 张三先认领 —— 认领事件立刻推到远端。
        TeamTool::new(w.alice.clone())
            .run(&json!({"action": "task_claim", "task_id": tid}))
            .unwrap();

        let bob = TeamTool::new(w.bob.clone());
        let err = bob
            .run(&json!({"action": "task_claim", "task_id": tid}))
            .unwrap_err();
        assert!(err.contains("张三"), "要说出是谁先认领的：{err}");

        let store_b = Store::discover(&w.bob).unwrap();
        assert!(
            store_b.task(&gid, &tid).unwrap().assignee.is_none(),
            "被拒之后本地不能留下「我认领了」"
        );
        assert!(
            git::status_porcelain(store_b.project_root()).trim().is_empty(),
            "工作区应该干净：{}",
            git::status_porcelain(store_b.project_root())
        );
        let _ = std::fs::remove_dir_all(&w.base);
    }

    /// 真正的并发：两边都在对方推送之前写下了自己的认领。事件链会把两条接成一条，
    /// 排在前面的赢；输的那一方本地退回原样。
    #[test]
    fn simultaneous_claims_have_exactly_one_winner() {
        let w = world("tie");
        let (gid, tid) = shared_task(&w, "抢我");
        let store_a = Store::discover(&w.alice).unwrap();
        let store_b = Store::discover(&w.bob).unwrap();

        // 张三的认领先上了远端链。
        let alice = TeamTool::new(w.alice.clone());
        alice.run(&json!({"action": "task_claim", "task_id": tid})).unwrap();

        // 李四在拉到之前就写下了自己的认领（模拟两边同时点）。
        events::append(
            &w.bob,
            &Draft {
                kind: EventKind::CLAIM.into(),
                target: Some(tid.clone()),
                goal_id: Some(gid.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        // 推送时被拒、自动接到张三后面 —— 链上张三在前。
        events::push(&w.bob).unwrap();
        let me_b = git::local_identity(&w.bob).unwrap().as_key();
        let roster = members::load_all(&store_b);
        assert_eq!(
            claimed_by_other(&w.bob, &tid, &me_b, &roster).as_deref(),
            Some("张三"),
            "链上排在前面的那条认领赢"
        );
        let me_a = git::local_identity(&w.alice).unwrap().as_key();
        events::fetch(&w.alice).unwrap();
        assert!(
            claimed_by_other(&w.alice, &tid, &me_a, &members::load_all(&store_a)).is_none(),
            "赢的那一方不该被判输"
        );
        let _ = std::fs::remove_dir_all(&w.base);
    }

    /// @ 与指派跨机器送达：张三发，李四拉一次就能在收件箱里看到。
    #[test]
    fn mentions_and_assignments_reach_the_other_member() {
        let w = world("inbox");
        let (_, tid) = shared_task(&w, "接口联调");
        // 两个人都加入团队，名单随状态提交同步。
        let zs = members::join(
            &w.alice.join(".carter"),
            &git::local_identity(&w.alice).unwrap(),
            members::Join::New { name: "张三".into() },
        )
        .unwrap()
        .0;
        let ls = members::join(
            &w.alice.join(".carter"),
            &Identity::parse("李四 <li@example.com>").unwrap(),
            members::Join::New { name: "李四".into() },
        )
        .unwrap()
        .0;
        let root_a = Store::discover(&w.alice).unwrap();
        git::stage_team(root_a.project_root(), ".carter").unwrap();
        git::commit_team(root_a.project_root(), ".carter", "chore: 成员").unwrap();
        git::push(root_a.project_root()).unwrap();
        git::sync(&w.bob).unwrap();

        let alice = TeamTool::new(w.alice.clone());
        alice
            .run(&json!({"action": "mention", "to": ["李四"], "task_id": tid, "text": "接口定好了"}))
            .unwrap();
        let out = alice
            .run(&json!({"action": "assign", "task_id": tid, "assignee": "李四"}))
            .unwrap();
        assert!(out.contains("指派给 李四"), "{out}");
        // 任务文件里存的是成员 id，不是名字。
        let t = root_a.task(&shared_goal(&w), &tid).unwrap();
        assert_eq!(t.assignee.as_deref(), Some(ls.id.as_str()));

        // 李四这边：只拉事件、不合并分支，就能收到。
        let bob = TeamTool::new(w.bob.clone());
        let inbox = bob.run(&json!({"action": "inbox"})).unwrap();
        assert!(inbox.contains("[@我] 张三（经卡特） 提到"), "{inbox}");
        assert!(inbox.contains("[派给我]"), "{inbox}");
        assert!(inbox.contains("接口联调"), "消息里要带任务标题：{inbox}");
        // 张三自己发的不进自己的收件箱。
        let own = alice.run(&json!({"action": "inbox"})).unwrap();
        assert!(!own.contains("接口定好了"), "{own}");
        let _ = zs;
        let _ = std::fs::remove_dir_all(&w.base);
    }

    /// 张三建一个目标 + 任务，提交并推上去，李四同步下来。返回 `(goal_id, task_id)`。
    fn shared_task(w: &World, title: &str) -> (String, String) {
        let alice = TeamTool::new(w.alice.clone());
        let gid = id_of(&alice.run(&json!({"action": "goal_new", "title": "g"})).unwrap());
        let tid = id_of(
            &alice
                .run(&json!({"action": "task_new", "goal_id": gid, "title": title}))
                .unwrap(),
        );
        let store_a = Store::discover(&w.alice).unwrap();
        git::stage_team(store_a.project_root(), ".carter").unwrap();
        git::commit_team(store_a.project_root(), ".carter", "feat: 建任务").unwrap();
        git::push(store_a.project_root()).unwrap();
        git::sync(&w.bob).unwrap();
        (gid, tid)
    }

    fn shared_goal(w: &World) -> String {
        Store::discover(&w.alice).unwrap().goals().items[0].id.clone()
    }
}
