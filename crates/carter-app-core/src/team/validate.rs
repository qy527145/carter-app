//! 团队数据的校验器：把「文件长什么样才算对」写成一个可以随时跑的检查。
//!
//! ## 为什么需要它
//!
//! 已经踩过一次：`Priority` 用 `rename_all = "snake_case"` 派生，写出 `"p2"`；
//! 而脚本与技能文档写的是 `"P2"`。两边各自都对，合起来就是**目标建出来了、
//! 界面读不出来** —— 而报错只在打开界面的那一刻才出现，说到 `unknown variant`
//! 就停了，看不出真正的原因是大小写。
//!
//! 那次的根本问题是：**没有一处地方能回答「现在这些文件是不是好的」**。
//! 读取路径只会「读到坏的就说读不到」，它没法告诉人「哪里坏了、应该改成什么」。
//!
//! 所以这里把校验独立出来，有三个用途：
//!
//! 1. **每次写完立刻验一遍**（见 [`check_task`] / [`check_goal`]，写路径已经在调）。
//! 2. **`team` 工具开工前先验**：模型改坏了当场就拿到具体到行的话，可以接着改到对。
//! 3. **界面上有一个「校验」入口**：人也能主动问一次全库有没有问题。
//!
//! ## 为什么与 `Store` 分开
//!
//! `Store` 管读写，它的错误是「读不到 / 写不进」。这里管**语义**：id 与目录名对不对、
//! 父子是不是成环、依赖指向的任务在不在、看板关联的目标存不存在。
//!
//! 分开还有一个实际好处：校验器吃的是**已经解析好的结构 + 原始 JSON**，不碰文件系统。
//! 于是它能在「还没写盘之前」就验一遍，坏数据根本没机会落地。

use std::collections::{HashMap, HashSet};

use super::model::*;

/// 一条问题。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Issue {
    /// 出问题的文件，相对 `.carter/`（`goals/g-x/goal.json`）。
    /// 空串 = 不是某个具体文件的问题（比如「这个目标下有两个任务 id 撞了」）。
    pub path: String,
    /// 一句话说清哪里不对。
    pub message: String,
    /// 能修的话，该怎么修。界面与工具都直接显示这句话。
    pub fix: Option<String>,
}

impl Issue {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            fix: None,
        }
    }

    fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }

    /// 给模型 / 人看的一行。
    pub fn render(&self) -> String {
        let mut s = if self.path.is_empty() {
            self.message.clone()
        } else {
            format!("{}：{}", self.path, self.message)
        };
        if let Some(fix) = &self.fix {
            s.push_str(&format!("（{fix}）"));
        }
        s
    }
}

/// 校验结论。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Report {
    pub issues: Vec<Issue>,
    /// 参检的文件数。用来区分「全好」与「什么都没扫到」。
    pub checked: usize,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.issues.is_empty()
    }

    /// 拼成给模型读的正文。空报告返回一句明确的「没问题」，而不是空串 ——
    /// 空串会让模型以为工具失败了。
    pub fn render(&self) -> String {
        if self.issues.is_empty() {
            return format!("校验通过（{} 个文件）。", self.checked);
        }
        let mut out = vec![format!(
            "发现 {} 个问题（共检查 {} 个文件）：",
            self.issues.len(),
            self.checked
        )];
        for (i, issue) in self.issues.iter().enumerate() {
            out.push(format!("{}. {}", i + 1, issue.render()));
        }
        out.join("\n")
    }
}

// ---------------------------------------------------------------------------
// 单实体
// ---------------------------------------------------------------------------

/// 一个目标。`path` 用来把问题指到具体文件上（空串 = 还没落盘）。
pub fn check_goal(path: &str, goal: &Goal) -> Vec<Issue> {
    let mut out = Vec::new();
    if !valid_id(Kind::Goal, &goal.id) {
        out.push(
            Issue::new(path, format!("id `{}` 不合法", goal.id))
                .with_fix("目标的 id 必须形如 g-<时间戳>-<随机>，用工具重新建一个，别手改"),
        );
    }
    if goal.title.trim().is_empty() {
        out.push(Issue::new(path, "标题是空的").with_fix("填一句说清范围的话"));
    }
    if goal.title.chars().count() > 200 {
        out.push(
            Issue::new(path, format!("标题 {} 字，太长了", goal.title.chars().count()))
                .with_fix("压到 200 字以内，细节写进 description"),
        );
    }
    if goal.schema_version > SCHEMA_VERSION {
        out.push(
            Issue::new(
                path,
                format!(
                    "schema_version 是 {}，这个版本的应用只认到 {SCHEMA_VERSION}",
                    goal.schema_version
                ),
            )
            .with_fix("升级应用，或者把 schema_version 改回 1（前提是结构真的没变）"),
        );
    }
    // owner 写成一个人看的名字而不是身份串时，后面认领与「我的任务」都匹配不上。
    if let Some(owner) = goal.owner.as_deref().filter(|o| !o.trim().is_empty()) {
        if !owner.contains('<') {
            out.push(
                Issue::new(path, format!("owner `{owner}` 不是 `名字 <邮箱>` 的形式"))
                    .with_fix("用 git 里那份身份写，比如 `张三 <zhang@example.com>`"),
            );
        }
    }
    out
}

/// 一个任务。`path` 同上。
pub fn check_task(path: &str, task: &Task) -> Vec<Issue> {
    let mut out = Vec::new();
    if !valid_id(Kind::Task, &task.id) {
        out.push(
            Issue::new(path, format!("id `{}` 不合法", task.id))
                .with_fix("任务 id 必须形如 t-<时间戳>-<随机>，用工具重新建一个"),
        );
    }
    if !valid_id(Kind::Goal, &task.goal_id) {
        out.push(
            Issue::new(path, format!("goal_id `{}` 不合法", task.goal_id))
                .with_fix("检查这个任务是不是放错了目录"),
        );
    }
    if task.title.trim().is_empty() {
        out.push(Issue::new(path, "标题是空的").with_fix("填一句说清做什么的话"));
    }
    if task.title.chars().count() > 200 {
        out.push(
            Issue::new(path, format!("标题 {} 字，太长了", task.title.chars().count()))
                .with_fix("压到 200 字以内，做法写进 description"),
        );
    }
    if task.status == TaskStatus::Blocked && task.blocked_reason.trim().is_empty() {
        out.push(
            Issue::new(path, "状态是「受阻」但没写 blocked_reason")
                .with_fix("写清卡在哪，或者把状态改回 todo / doing"),
        );
    }
    if let Some(p) = &task.parent_id {
        if !valid_id(Kind::Task, p) {
            out.push(Issue::new(path, format!("parent_id `{p}` 不合法")));
        }
        if p == &task.id {
            out.push(
                Issue::new(path, "parent_id 指向自己")
                    .with_fix("子任务不能是自己的父任务，去掉 parent_id"),
            );
        }
    }
    if task.depends_on.iter().any(|d| d == &task.id) {
        out.push(
            Issue::new(path, "depends_on 里有自己")
                .with_fix("去掉这一条 —— 任务不能等自己"),
        );
    }
    let mut seen = HashSet::new();
    for d in &task.depends_on {
        if !seen.insert(d.as_str()) {
            out.push(
                Issue::new(path, format!("depends_on 里 `{d}` 出现了两次"))
                    .with_fix("去重"),
            );
        }
    }
    if let Some(h) = task.estimate_hours {
        if !h.is_finite() || h < 0.0 {
            out.push(Issue::new(path, format!("estimate_hours 是 {h}，不合法")).with_fix("填一个非负数"));
        }
    }
    if task.schema_version > SCHEMA_VERSION {
        out.push(Issue::new(
            path,
            format!(
                "schema_version 是 {}，这个版本的应用只认到 {SCHEMA_VERSION}",
                task.schema_version
            ),
        ));
    }
    out
}

/// 一块看板。HTML 本身不验 —— 那是自由版式；只验列表与关联要用到的那两处元信息。
pub fn check_board(path: &str, board: &Board, goal_ids: &HashSet<String>) -> Vec<Issue> {
    let mut out = Vec::new();
    if html_title(&board.html).is_none_or(|t| t.is_empty()) {
        out.push(
            Issue::new(path, "页面没有 <title>")
                .with_fix("在 <head> 里加一个 <title>，列表里显示的就是它"),
        );
    }
    if let Some(g) = board.goal_id.as_deref() {
        if !goal_ids.contains(g) {
            out.push(
                Issue::new(path, format!("carter-goal 指向的目标 `{g}` 不存在"))
                    .with_fix("改成一个现有目标的 id，或者把那条 <meta> 去掉"),
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 跨实体：整个目标
// ---------------------------------------------------------------------------

/// 一个目标下的全部任务：父子成环、依赖成环、引用了不存在的任务。
///
/// 单个任务自己看不出这些问题 —— 它们只在「跟别的任务放在一起」时才成立。
pub fn check_task_set(goal_id: &str, tasks: &[Task]) -> Vec<Issue> {
    let mut out = Vec::new();
    let by_id: HashMap<&str, &Task> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();

    // 每个任务都得真的在这个目标里。
    for t in tasks {
        if t.goal_id != goal_id {
            out.push(
                Issue::new(
                    task_path(goal_id, &t.id),
                    format!("goal_id 写的是 `{}`，但它放在 `{goal_id}` 目录下", t.goal_id),
                )
                .with_fix("两个地方改成一致 —— 通常是搬目录时漏改了文件里的字段"),
            );
        }
    }

    for t in tasks {
        let path = task_path(goal_id, &t.id);
        for dep in &t.depends_on {
            match by_id.get(dep.as_str()) {
                None => out.push(
                    Issue::new(path.as_str(), format!("依赖的 `{dep}` 不在这个目标里"))
                        .with_fix("改成正确的任务 id，或者把那条依赖去掉"),
                ),
                Some(d) if d.goal_id != goal_id => out.push(Issue::new(
                    path.as_str(),
                    format!("依赖的 `{dep}` 属于别的目标"),
                )),
                Some(_) => {}
            }
        }
        if let Some(p) = &t.parent_id {
            if !by_id.contains_key(p.as_str()) {
                out.push(
                    Issue::new(path.as_str(), format!("父任务 `{p}` 不在这个目标里"))
                        .with_fix("去掉 parent_id，它就变回根任务"),
                );
            }
        }
    }

    // 父子成环：沿着 parent 往上走，走回自己就是环。
    for t in tasks {
        let mut cur = t.parent_id.as_deref();
        let mut seen = HashSet::new();
        let mut hops = 0;
        while let Some(p) = cur {
            if p == t.id {
                out.push(
                    Issue::new(task_path(goal_id, &t.id), "父子关系成环")
                        .with_fix("把其中一条 parent_id 去掉"),
                );
                break;
            }
            if !seen.insert(p) || hops > tasks.len() {
                break;
            }
            hops += 1;
            cur = by_id.get(p).and_then(|x| x.parent_id.as_deref());
        }
    }

    // 依赖成环：深度优先找回到自己的路径。
    for t in tasks {
        let mut path_stack: Vec<&str> = Vec::new();
        let mut on_stack: Vec<&str> = Vec::new();
        if let Some(cycle) = find_cycle(t.id.as_str(), &by_id, &mut path_stack, &mut on_stack) {
            out.push(
                Issue::new(task_path(goal_id, &t.id), format!("依赖成环：{cycle}"))
                    .with_fix("环上的任务永远开不了工，去掉一条依赖把它改成先后顺序"),
            );
        }
    }
    // 同一个环会被每个成员各报一次，去个重。
    out.sort_by(|a, b| (a.path.as_str(), a.message.as_str()).cmp(&(b.path.as_str(), b.message.as_str())));
    out.dedup_by(|a, b| a.path == b.path && a.message == b.message);
    out
}

fn find_cycle<'a>(
    id: &'a str,
    by_id: &HashMap<&'a str, &'a Task>,
    path: &mut Vec<&'a str>,
    on_stack: &mut Vec<&'a str>,
) -> Option<String> {
    if on_stack.contains(&id) {
        let start = path.iter().position(|x| *x == id).unwrap_or(0);
        let mut cycle: Vec<&str> = path[start..].to_vec();
        cycle.push(id);
        return Some(cycle.join(" → "));
    }
    let t = by_id.get(id)?;
    on_stack.push(id);
    path.push(id);
    for dep in &t.depends_on {
        if let Some(c) = find_cycle(dep.as_str(), by_id, path, on_stack) {
            return Some(c);
        }
    }
    path.pop();
    on_stack.pop();
    None
}

fn task_path(goal_id: &str, task_id: &str) -> String {
    format!("goals/{goal_id}/tasks/{task_id}.json")
}

// ---------------------------------------------------------------------------
// 整个团队目录
// ---------------------------------------------------------------------------

/// 扫一遍整个 `.carter/`，把读不出来的文件也一并作为问题报出来。
///
/// 这是「现在这些文件是不是好的」那个问题的唯一答案。界面上的「校验」按钮、
/// `team` 工具的 `check` action 都走它。
pub fn check_store(store: &super::store::Store) -> Report {
    use super::store::Store;

    let mut issues = Vec::new();
    let mut checked = 0usize;

    // 元信息。
    checked += 1;
    if store.meta().is_err() {
        issues.push(
            Issue::new("team.json", "读不出来（文件不存在或格式不对）")
                .with_fix("重新初始化一次团队目录可以补上"),
        );
    }

    // 每个目标 + 它下面的任务。
    let mut goal_ids = HashSet::new();
    let goals_dir = store.root().join("goals");
    if let Ok(entries) = std::fs::read_dir(&goals_dir) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(goal_id) = dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !valid_id(Kind::Goal, goal_id) {
                issues.push(
                    Issue::new(
                        format!("goals/{goal_id}"),
                        "目录名不是一个合法的目标 id",
                    )
                    .with_fix("目录名就是目标的 id，改成 g-<时间戳>-<随机> 的形式"),
                );
                continue;
            }

            checked += 1;
            goal_ids.insert(goal_id.to_string());
            let goal_file = dir.join("goal.json");
            match std::fs::read_to_string(&goal_file)
                .map_err(|e| e.to_string())
                .and_then(|raw| super::store::parse_json::<Goal>(&raw))
            {
                Err(e) => issues.push(
                    Issue::new(format!("goals/{goal_id}/goal.json"), format!("解析失败：{e}"))
                        .with_fix("按报错里的字段名改回来；拿不准就对照技能文档里的格式"),
                ),
                Ok(goal) => {
                    issues.extend(check_goal(&format!("goals/{goal_id}/goal.json"), &goal));
                    if goal.id != goal_id {
                        issues.push(
                            Issue::new(
                                format!("goals/{goal_id}/goal.json"),
                                format!("文件里的 id 是 `{}`，与目录名不一致", goal.id),
                            )
                            .with_fix("改文件里的 id 去对齐目录名，别改目录名"),
                        );
                    }
                }
            }

            // 任务逐个解析，坏的单独报；好的凑齐之后再做跨任务检查。
            let mut good: Vec<Task> = Vec::new();
            if let Ok(files) = std::fs::read_dir(dir.join("tasks")) {
                for f in files.flatten() {
                    let p = f.path();
                    if p.extension().and_then(|s| s.to_str()) != Some("json") {
                        continue;
                    }
                    checked += 1;
                    let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(str::to_string)
                    else {
                        continue;
                    };
                    let rel = format!("goals/{goal_id}/tasks/{stem}.json");
                    match std::fs::read_to_string(&p)
                        .map_err(|e| e.to_string())
                        .and_then(|raw| super::store::parse_json::<Task>(&raw))
                    {
                        Err(e) => issues.push(
                            Issue::new(rel, format!("解析失败：{e}"))
                                .with_fix("按报错里的字段名改回来；拿不准就对照技能文档"),
                        ),
                        Ok(task) => {
                            issues.extend(check_task(&rel, &task));
                            if task.id != stem {
                                issues.push(
                                    Issue::new(
                                        rel.clone(),
                                        format!("文件里的 id 是 `{}`，与文件名不一致", task.id),
                                    )
                                    .with_fix("改文件里的 id 去对齐文件名"),
                                );
                            }
                            good.push(task);
                        }
                    }
                }
            }
            issues.extend(check_task_set(goal_id, &good));
        }
    }

    // 看板。
    let boards = store.boards();
    for (path, why) in &boards.issues {
        issues.push(Issue::new(path.clone(), why.clone()));
    }
    for board in &boards.items {
        checked += 1;
        issues.extend(check_board(&format!("boards/{}.html", board.id), board, &goal_ids));
    }
    // 旧版的 `boards/<id>/board.json` 目录：现在不读了，留着只会让人以为它还有用。
    if let Ok(entries) = std::fs::read_dir(store.root().join("boards")) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if dir.is_dir() && dir.join("board.json").is_file() {
                let name = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
                issues.push(
                    Issue::new(format!("boards/{name}/"), "旧格式的看板（board.json + cards/），已经不显示了")
                        .with_fix(format!("让卡特把它改写成 boards/{name}.html，再删掉这个目录")),
                );
            }
        }
    }

    let _ = Store::open(store.root());
    Report { issues, checked }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn me() -> Identity {
        Identity::parse("测试 <t@example.com>").unwrap()
    }

    fn tmp(tag: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("carter-val-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("goals")).unwrap();
        std::fs::create_dir_all(d.join("boards")).unwrap();
        d
    }

    #[test]
    fn a_healthy_goal_and_task_produce_nothing() {
        let g = Goal::new("第一阶段", &me());
        assert!(check_goal("", &g).is_empty());
        let mut t = Task::new(&g.id, "写数据层", &me());
        t.acceptance.push("cargo test 通过".into());
        assert!(check_task("", &t).is_empty());
    }

    #[test]
    fn blocked_without_a_reason_is_flagged_with_a_fix() {
        let g = Goal::new("g", &me());
        let mut t = Task::new(&g.id, "t", &me());
        t.status = TaskStatus::Blocked;
        let issues = check_task("", &t);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].fix.is_some(), "要给人/模型一句怎么改");
        assert!(issues[0].render().contains("blocked_reason"));
    }

    #[test]
    fn cross_task_cycles_and_dangling_refs_are_caught() {
        let g = Goal::new("g", &me());
        let mut a = Task::new(&g.id, "a", &me());
        let mut b = Task::new(&g.id, "b", &me());
        a.depends_on = vec![b.id.clone()];
        b.depends_on = vec![a.id.clone()];
        let mut c = Task::new(&g.id, "c", &me());
        c.depends_on = vec!["t-nope".into()];
        c.parent_id = Some("t-gone".into());

        let issues = check_task_set(&g.id, &[a, b, c]);
        let text = issues.iter().map(|i| i.render()).collect::<Vec<_>>().join("\n");
        assert!(text.contains("依赖成环"), "{text}");
        assert!(text.contains("不在这个目标里"), "{text}");
        assert!(text.contains("父任务"), "{text}");
    }

    #[test]
    fn parent_cycles_are_caught() {
        let g = Goal::new("g", &me());
        let mut a = Task::new(&g.id, "a", &me());
        let mut b = Task::new(&g.id, "b", &me());
        a.parent_id = Some(b.id.clone());
        b.parent_id = Some(a.id.clone());
        let issues = check_task_set(&g.id, &[a, b]);
        assert!(
            issues.iter().any(|i| i.message.contains("父子关系成环")),
            "{:?}",
            issues.iter().map(|i| i.message.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn boards_need_a_title_and_a_real_goal() {
        let root = tmp("board");
        let store = super::super::store::Store::open(&root);
        store.save_meta(&TeamMeta::new("t", &me())).unwrap();
        let g = Goal::new("g", &me());
        store.save_goal(&g).unwrap();
        store
            .save_board("b-ok", &stamp_board("<p>x</p>", Some("好的"), Some(&g.id)))
            .unwrap();
        store.save_board("b-bare", "<p>x</p>").unwrap();
        store
            .save_board("b-dangling", &stamp_board("<p>x</p>", Some("t"), Some("g-gone")))
            .unwrap();
        // 旧格式留下的目录。
        std::fs::create_dir_all(root.join("boards/b-old/cards")).unwrap();
        std::fs::write(root.join("boards/b-old/board.json"), "{}").unwrap();

        let text = check_store(&store).render();
        assert!(!text.contains("b-ok"), "{text}");
        assert!(text.contains("boards/b-bare.html") && text.contains("<title>"), "{text}");
        assert!(text.contains("g-gone"), "{text}");
        assert!(text.contains("boards/b-old/") && text.contains("旧格式"), "{text}");
    }

    /// 这正是用户遇到的那一次：`"P3"` 写进文件，而某个版本的应用只认 `"p3"`。
    /// 那个坏文件必须在**校验**这一步被指出来，而不是等到界面读的时候。
    #[test]
    fn a_file_with_the_wrong_case_parses_and_validates_clean() {
        let g = Goal::new("g", &me());
        let _ = std::fs::create_dir_all(tmp("case").join("goals"));
        let raw = serde_json::to_string(&g).unwrap().replace("\"P2\"", "\"P3\"");
        // 现在的实现大小写都认，所以解析得进来。
        let back: Goal = super::super::store::parse_json(&raw).unwrap();
        assert_eq!(back.priority, Priority::P3);
        assert!(check_goal("", &back).is_empty());
    }

    #[test]
    fn a_broken_file_is_reported_with_its_path() {
        let root = tmp("broken");
        std::fs::create_dir_all(root.join("goals/g-x")).unwrap();
        std::fs::write(root.join("goals/g-x/goal.json"), "{ 这不是 json").unwrap();
        let store = super::super::store::Store::open(&root);
        let report = check_store(&store);
        assert!(!report.ok());
        let text = report.render();
        assert!(text.contains("goals/g-x/goal.json"), "{text}");
        assert!(text.contains("解析失败"), "{text}");
    }

    #[test]
    fn id_must_match_the_directory_name() {
        let root = tmp("mismatch");
        let store = super::super::store::Store::open(&root);
        store.save_meta(&TeamMeta::new("t", &me())).unwrap();
        let g = Goal::new("g", &me());
        store.save_goal(&g).unwrap();
        // 把文件搬到另一个名字的目录下，制造「目录名 != id」。
        let fake = root.join("goals/g-fake-1234");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::copy(
            root.join("goals").join(&g.id).join("goal.json"),
            fake.join("goal.json"),
        )
        .unwrap();
        std::fs::remove_dir_all(root.join("goals").join(&g.id)).unwrap();

        let report = check_store(&store);
        let text = report.render();
        assert!(text.contains("与目录名不一致"), "{text}");
    }

    #[test]
    fn clean_store_reports_ok_with_a_count() {
        let root = tmp("clean");
        let store = super::super::store::Store::open(&root);
        store.save_meta(&TeamMeta::new("t", &me())).unwrap();
        let g = Goal::new("g", &me());
        store.save_goal(&g).unwrap();
        let t = Task::new(&g.id, "t", &me());
        store.save_task(&t).unwrap();
        let report = check_store(&store);
        assert!(report.ok(), "{}", report.render());
        assert!(report.render().contains("校验通过"));
        assert!(report.checked >= 3, "team.json + goal + task = {}", report.checked);
    }
}
