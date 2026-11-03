//! 读写 `.carter/` 目录：一实体一文件、原子写、派生视图。
//!
//! ## 布局
//!
//! ```text
//! <root>/.carter/
//!   team.json                       项目元信息
//!   .gitignore                      只有 .cache/
//!   goals/
//!     categories.json               分类字典
//!     <goal-id>/
//!       goal.json
//!       tasks/<task-id>.json
//!   boards/<board-id>/
//!     board.json
//!     cards/<card-id>.json
//!   skills/                         内置技能（carter 已经会扫这里）
//!   .cache/                         派生数据，不进 git
//!     index.json                    成员表 + 跨目标汇总
//! ```
//!
//! ## 为什么每个写操作都是「读整份 → 改 → 写整份 → 原子改名」
//!
//! 单个实体一个文件时，这个粒度的读改写在**同一台机器**上是安全的：写的是自己的
//! 文件，改的是自己刚读出来的那份。跨机器并发由 git 负责 —— 冲突只会在
//! 「两个人真的同时改了同一个任务」时发生，而那正是应该惊动人的情况。
//!
//! 真正要守住的是**不要留下半截文件**：进程崩溃、磁盘满、断电都可能发生在写的中途。
//! 所以一律先写 `<name>.tmp` 再 `rename`，rename 在同一个文件系统上是原子的。
//! 与 `goal.rs::save_to`、`changes.rs::write_stored` 同一套做法。
//!
//! ## 为什么不在读的时候顺手修东西
//!
//! 读到坏文件（手改坏了、merge 冲突标记留在里面）时**报错**，不猜、不覆盖。多人
//! 协作里最坏的事是「工具悄悄把别人的东西改掉了」：一方看到的是自己刚写的，
//! 另一方下次打开发现全变了，而两次事件之间没有任何提示。`list_*` 这类批量读
//! 会把坏文件单独挑出来放进 `issues`，界面能指出来是哪个文件。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::model::*;

/// 团队目录在这一层之下的名字。
pub const DIR: &str = ".carter";

/// 一次批量读的产物：能读出来的 + 读不出来的。
#[derive(Debug, Clone)]
pub struct Loaded<T> {
    pub items: Vec<T>,
    /// `(相对路径, 原因)`。有人手改坏了文件时，界面要能指出是哪一个。
    pub issues: Vec<(String, String)>,
}

// 手写而不是 derive：derive 会给 `T: Default` 加约束，而 `Goal` / `Task` 没有
// （它们都需要一个身份才能构造）。`Loaded<Goal>::default()` 本来就该是空的。
impl<T> Default for Loaded<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            issues: Vec::new(),
        }
    }
}

impl<T> Loaded<T> {
    fn push_issue(&mut self, path: &Path, root: &Path, err: impl std::fmt::Display) {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        self.issues.push((rel, err.to_string()));
    }
}

/// 一个已经初始化过的团队目录。
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// 包住一个**已经存在**的团队目录。初始化走 [`super::init::initialize`]。
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 从会话的 cwd 往上找最近的 `.carter/`（与技能扫描同一套规则）。
    pub fn discover(cwd: &Path) -> Option<Self> {
        for dir in cwd.ancestors() {
            let candidate = dir.join(DIR);
            if candidate.join("team.json").is_file() {
                return Some(Self::open(candidate));
            }
        }
        None
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 团队目录所在的**项目根**（`.carter/` 的上一级）。
    pub fn project_root(&self) -> &Path {
        self.root.parent().unwrap_or(&self.root)
    }

    fn goals_dir(&self) -> PathBuf {
        self.root.join("goals")
    }

    fn goal_dir(&self, goal_id: &str) -> PathBuf {
        self.goals_dir().join(goal_id)
    }

    fn tasks_dir(&self, goal_id: &str) -> PathBuf {
        self.goal_dir(goal_id).join("tasks")
    }

    fn boards_dir(&self) -> PathBuf {
        self.root.join("boards")
    }

    fn board_dir(&self, board_id: &str) -> PathBuf {
        self.boards_dir().join(board_id)
    }

    fn cards_dir(&self, board_id: &str) -> PathBuf {
        self.board_dir(board_id).join("cards")
    }

    // -----------------------------------------------------------------------
    // 元信息
    // -----------------------------------------------------------------------

    pub fn meta(&self) -> Result<TeamMeta, String> {
        read_json(&self.root.join("team.json"))
    }

    pub fn save_meta(&self, meta: &TeamMeta) -> Result<(), String> {
        write_json(&self.root.join("team.json"), meta)
    }

    // -----------------------------------------------------------------------
    // 目标
    // -----------------------------------------------------------------------

    pub fn goals(&self) -> Loaded<Goal> {
        let mut out = Loaded::default();
        let dir = self.goals_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let file = path.join("goal.json");
            if !file.is_file() {
                continue;
            }
            match read_json::<Goal>(&file) {
                Ok(goal) => out.items.push(goal),
                Err(e) => out.push_issue(&file, &self.root, e),
            }
        }
        out.items.sort_by(goal_order);
        out
    }

    pub fn goal(&self, goal_id: &str) -> Result<Goal, String> {
        if !valid_id(Kind::Goal, goal_id) {
            return Err(format!("目标 id `{goal_id}` 不合法"));
        }
        read_json(&self.goal_dir(goal_id).join("goal.json"))
            .map_err(|e| format!("读不到目标 {goal_id}：{e}"))
    }

    pub fn save_goal(&self, goal: &Goal) -> Result<(), String> {
        if !valid_id(Kind::Goal, &goal.id) {
            return Err(format!("目标 id `{}` 不合法", goal.id));
        }
        write_json(&self.goal_dir(&goal.id).join("goal.json"), goal)
    }

    /// 删目标：连同它下面的任务一起删掉。
    ///
    /// 返回删掉的目录路径，由调用方决定要不要 `git add -A`。**不删看板**：
    /// 看板可能跨目标，`goal_id` 只是它挂在哪，不是它的归属。
    pub fn delete_goal(&self, goal_id: &str) -> Result<(), String> {
        if !valid_id(Kind::Goal, goal_id) {
            return Err(format!("目标 id `{goal_id}` 不合法"));
        }
        let dir = self.goal_dir(goal_id);
        if !dir.exists() {
            return Err(format!("目标 {goal_id} 不存在"));
        }
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删 {} 失败：{e}", dir.display()))
    }

    // -----------------------------------------------------------------------
    // 任务
    // -----------------------------------------------------------------------

    pub fn tasks(&self, goal_id: &str) -> Loaded<Task> {
        let mut out = Loaded::default();
        if !valid_id(Kind::Goal, goal_id) {
            out.issues
                .push((goal_id.into(), "目标 id 不合法".into()));
            return out;
        }
        let dir = self.tasks_dir(goal_id);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            match read_json::<Task>(&path) {
                Ok(task) => out.items.push(task),
                Err(e) => out.push_issue(&path, &self.root, e),
            }
        }
        out.items.sort_by(task_order);
        out
    }

    pub fn task(&self, goal_id: &str, task_id: &str) -> Result<Task, String> {
        for (kind, id, what) in [
            (Kind::Goal, goal_id, "目标"),
            (Kind::Task, task_id, "任务"),
        ] {
            if !valid_id(kind, id) {
                return Err(format!("{what} id `{id}` 不合法"));
            }
        }
        read_json(&self.tasks_dir(goal_id).join(format!("{task_id}.json")))
            .map_err(|e| format!("读不到任务 {task_id}：{e}"))
    }

    pub fn save_task(&self, task: &Task) -> Result<(), String> {
        for (kind, id, what) in [
            (Kind::Goal, task.goal_id.as_str(), "目标"),
            (Kind::Task, task.id.as_str(), "任务"),
        ] {
            if !valid_id(kind, id) {
                return Err(format!("{what} id `{id}` 不合法"));
            }
        }
        write_json(
            &self
                .tasks_dir(&task.goal_id)
                .join(format!("{}.json", task.id)),
            task,
        )
    }

    pub fn delete_task(&self, goal_id: &str, task_id: &str) -> Result<(), String> {
        if !valid_id(Kind::Goal, goal_id) || !valid_id(Kind::Task, task_id) {
            return Err("id 不合法".into());
        }
        let path = self.tasks_dir(goal_id).join(format!("{task_id}.json"));
        std::fs::remove_file(&path).map_err(|e| format!("删 {} 失败：{e}", path.display()))
    }

    /// 校验一组依赖不会成环，且引用的任务都在同一个目标里。
    ///
    /// 在**写之前**验而不是写之后补救：环一旦落盘，`ready` 的计算会永远返回
    /// 「等着」，界面上表现为任务永远开不了工，而且看不出原因。
    pub fn validate_deps(&self, goal_id: &str, candidate: &Task) -> Result<(), String> {
        let loaded = self.tasks(goal_id);
        let mut by_id: HashMap<&str, &Task> =
            loaded.items.iter().map(|t| (t.id.as_str(), t)).collect();
        // 改用候选版本参与遍历：新增 / 改动的依赖要一起验。
        by_id.insert(candidate.id.as_str(), candidate);

        for dep in &candidate.depends_on {
            if dep == &candidate.id {
                return Err("任务不能依赖自己".into());
            }
            match by_id.get(dep.as_str()) {
                None => return Err(format!("依赖的任务 {dep} 不在这个目标里")),
                Some(t) if t.goal_id != goal_id => {
                    return Err(format!("依赖的任务 {dep} 属于别的目标"))
                }
                _ => {}
            }
        }

        // 深度优先找环。从候选出发沿着 depends_on 走，走回自己就是环。
        let mut stack: Vec<&str> = vec![candidate.id.as_str()];
        let mut path: Vec<&str> = Vec::new();
        let mut on_stack: Vec<&str> = Vec::new();
        fn walk<'a>(
            id: &'a str,
            by_id: &HashMap<&'a str, &'a Task>,
            path: &mut Vec<&'a str>,
            on_stack: &mut Vec<&'a str>,
        ) -> Result<(), String> {
            if on_stack.contains(&id) {
                let cycle = path
                    .iter()
                    .skip_while(|x| **x != id)
                    .copied()
                    .chain(std::iter::once(id))
                    .collect::<Vec<_>>()
                    .join(" → ");
                return Err(format!("依赖成环：{cycle}"));
            }
            let Some(t) = by_id.get(id) else {
                return Ok(());
            };
            on_stack.push(id);
            path.push(id);
            for dep in &t.depends_on {
                walk(dep.as_str(), by_id, path, on_stack)?;
            }
            path.pop();
            on_stack.pop();
            Ok(())
        }
        while let Some(start) = stack.pop() {
            walk(start, &by_id, &mut path, &mut on_stack)?;
        }
        Ok(())
    }

    /// 一个目标的派生视图：任务树、进度、能不能开工。
    ///
    /// 所有「算出来的」东西都在这里算，不落盘 —— 见 `model.rs` 模块头第 3 条。
    pub fn goal_view(&self, goal_id: &str) -> Result<GoalView, String> {
        let goal = self.goal(goal_id)?;
        let loaded = self.tasks(goal_id);
        let mut tasks: BTreeMap<&str, &Task> = loaded.items.iter().map(|t| (t.id.as_str(), t)).collect();

        let mut counts = Counts::default();
        for t in &loaded.items {
            counts.add(t.status);
        }

        // 子任务表：一次遍历建好，后面算进度与 `children` 都用它。
        let mut kids: HashMap<&str, Vec<&Task>> = HashMap::new();
        for t in &loaded.items {
            if let Some(parent) = t.parent_id.as_deref() {
                if tasks.contains_key(parent) {
                    kids.entry(parent).or_default().push(t);
                }
            }
        }
        for v in kids.values_mut() {
            v.sort_by(|a, b| task_order(a, b));
        }

        let done: HashMap<&str, bool> = loaded
            .items
            .iter()
            .map(|t| (t.id.as_str(), matches!(t.status, TaskStatus::Done | TaskStatus::Dropped)))
            .collect();

        let mut views = Vec::with_capacity(loaded.items.len());
        for t in &loaded.items {
            let children: Vec<String> = kids
                .get(t.id.as_str())
                .map(|v| v.iter().map(|c| c.id.clone()).collect())
                .unwrap_or_default();

            let progress = if children.is_empty() {
                match t.status {
                    TaskStatus::Done | TaskStatus::Dropped => 100,
                    TaskStatus::Doing => 50,
                    _ => 0,
                }
            } else {
                // 子任务口径：完成数 / 总数。子任务自己也可以有子任务，
                // 但这里**不递归** —— 子任务自己的 status 由它自己的完成情况决定，
                // 而它的 status 是谁改的（人还是模型）这里不关心。
                let total = children.len() as f64;
                let finished = children
                    .iter()
                    .filter(|id| done.get(id.as_str()).copied().unwrap_or(false))
                    .count() as f64;
                ((finished / total) * 100.0).round() as u32
            };

            let waiting_on: Vec<String> = t
                .depends_on
                .iter()
                .filter(|id| !done.get(id.as_str()).copied().unwrap_or(true))
                .cloned()
                .collect();

            let ready = matches!(t.status, TaskStatus::Todo)
                && waiting_on.is_empty()
                && (t.assignee.is_none() || t.assignee.as_deref() == Some(goal.owner.as_deref().unwrap_or("")));

            views.push(TaskView {
                task: (*t).clone(),
                children,
                progress,
                waiting_on,
                ready,
            });
        }

        let roots: Vec<String> = loaded
            .items
            .iter()
            .filter(|t| match t.parent_id.as_deref() {
                None => true,
                Some(p) => !tasks.contains_key(p),
            })
            .map(|t| t.id.clone())
            .collect();
        tasks.clear();

        Ok(GoalView {
            goal,
            tasks: views,
            roots,
            counts,
        })
    }

    // -----------------------------------------------------------------------
    // 看板
    // -----------------------------------------------------------------------

    pub fn boards(&self) -> Loaded<Board> {
        let mut out = Loaded::default();
        let Ok(entries) = std::fs::read_dir(self.boards_dir()) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let file = path.join("board.json");
            if !file.is_file() {
                continue;
            }
            match read_json::<Board>(&file) {
                Ok(b) => out.items.push(b),
                Err(e) => out.push_issue(&file, &self.root, e),
            }
        }
        out.items.sort_by(|a, b| {
            a.title
                .cmp(&b.title)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        out
    }

    pub fn board(&self, board_id: &str) -> Result<Board, String> {
        if !valid_id(Kind::Board, board_id) {
            return Err(format!("看板 id `{board_id}` 不合法"));
        }
        read_json(&self.board_dir(board_id).join("board.json"))
            .map_err(|e| format!("读不到看板 {board_id}：{e}"))
    }

    pub fn save_board(&self, board: &Board) -> Result<(), String> {
        if !valid_id(Kind::Board, &board.id) {
            return Err(format!("看板 id `{}` 不合法", board.id));
        }
        write_json(&self.board_dir(&board.id).join("board.json"), board)
    }

    pub fn delete_board(&self, board_id: &str) -> Result<(), String> {
        if !valid_id(Kind::Board, board_id) {
            return Err(format!("看板 id `{board_id}` 不合法"));
        }
        let dir = self.board_dir(board_id);
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删 {} 失败：{e}", dir.display()))
    }

    pub fn cards(&self, board_id: &str) -> Loaded<Card> {
        let mut out = Loaded::default();
        if !valid_id(Kind::Board, board_id) {
            out.issues.push((board_id.into(), "看板 id 不合法".into()));
            return out;
        }
        let Ok(entries) = std::fs::read_dir(self.cards_dir(board_id)) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            match read_json::<Card>(&path) {
                Ok(c) => out.items.push(c),
                Err(e) => out.push_issue(&path, &self.root, e),
            }
        }
        // 列内按 order，列间按看板里声明的列顺序 —— 界面直接照着渲染。
        out.items.sort_by(|a, b| {
            a.column_id
                .cmp(&b.column_id)
                .then_with(|| a.order.partial_cmp(&b.order).unwrap_or(std::cmp::Ordering::Equal))
        });
        out
    }

    pub fn save_card(&self, card: &Card) -> Result<(), String> {
        if !valid_id(Kind::Board, &card.board_id) || !valid_id(Kind::Card, &card.id) {
            return Err("id 不合法".into());
        }
        write_json(
            &self.cards_dir(&card.board_id).join(format!("{}.json", card.id)),
            card,
        )
    }

    pub fn delete_card(&self, board_id: &str, card_id: &str) -> Result<(), String> {
        if !valid_id(Kind::Board, board_id) || !valid_id(Kind::Card, card_id) {
            return Err("id 不合法".into());
        }
        let path = self.cards_dir(board_id).join(format!("{card_id}.json"));
        std::fs::remove_file(&path).map_err(|e| format!("删 {} 失败：{e}", path.display()))
    }

    // -----------------------------------------------------------------------
    // 分类
    // -----------------------------------------------------------------------

    pub fn categories(&self) -> CategoryFile {
        read_json(&self.goals_dir().join("categories.json")).unwrap_or_default()
    }

    pub fn save_categories(&self, file: &CategoryFile) -> Result<(), String> {
        write_json(&self.goals_dir().join("categories.json"), file)
    }
}

// ---------------------------------------------------------------------------
// 排序
// ---------------------------------------------------------------------------

fn goal_order(a: &Goal, b: &Goal) -> std::cmp::Ordering {
    a.order
        .partial_cmp(&b.order)
        .unwrap_or(std::cmp::Ordering::Equal)
        // 顺序值撞了（人手改过）时按 id 定序，至少每次读出来都一样。
        .then_with(|| a.id.cmp(&b.id))
}

fn task_order(a: &Task, b: &Task) -> std::cmp::Ordering {
    a.order
        .partial_cmp(&b.order)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.id.cmp(&b.id))
}

// ---------------------------------------------------------------------------
// JSON 读写
// ---------------------------------------------------------------------------

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_json(&raw)
}

/// 解析一段 JSON，冲突标记单独给一句人话。
///
/// merge 冲突没解决时文件里会有 `<<<<<<<` —— 这时 serde 报的是「expected value
/// at line 1 column 1」，没人能从中看出是冲突没解决。
pub(crate) fn parse_json<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, String> {
    if raw.contains("<<<<<<<") || raw.contains(">>>>>>>") {
        return Err("文件里有未解决的 git 冲突标记（<<<<<<<），先解决冲突再继续".into());
    }
    serde_json::from_str(raw).map_err(|e| e.to_string())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
    }
    // 两空格缩进 + 结尾换行：这份文件主要是给人看、给 git diff 看的，
    // 压缩成一行的 JSON 在 review 时等于不可读。
    let mut json = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("改名到 {} 失败：{e}", path.display()))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn tmp(tag: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("carter-team-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(DIR)).unwrap();
        d.join(DIR)
    }

    fn me() -> Identity {
        Identity::parse("Me <me@example.com>").unwrap()
    }

    fn seed(root: &Path) -> Store {
        let s = Store::open(root);
        s.save_meta(&TeamMeta::new("demo", &me())).unwrap();
        s
    }

    #[test]
    fn goals_and_tasks_round_trip() {
        let s = seed(&tmp("rt"));
        let g = Goal::new("第一阶段", &me());
        s.save_goal(&g).unwrap();
        assert_eq!(s.goal(&g.id).unwrap().title, "第一阶段");

        let mut t = Task::new(&g.id, "写数据层", &me());
        t.status = TaskStatus::Doing;
        s.save_task(&t).unwrap();

        let loaded = s.tasks(&g.id);
        assert_eq!(loaded.items.len(), 1);
        assert!(loaded.issues.is_empty());
        assert_eq!(loaded.items[0].status, TaskStatus::Doing);
    }

    #[test]
    fn writes_are_atomic_and_leave_no_tmp() {
        let s = seed(&tmp("atomic"));
        let g = Goal::new("g", &me());
        s.save_goal(&g).unwrap();
        let dir = s.goal_dir(&g.id);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "临时文件要清掉");
    }

    #[test]
    fn conflict_markers_give_a_readable_error() {
        let root = tmp("conflict");
        let s = seed(&root);
        let g = Goal::new("g", &me());
        s.save_goal(&g).unwrap();
        std::fs::write(
            s.goal_dir(&g.id).join("goal.json"),
            "<<<<<<< HEAD\n{}\n=======\n{}\n>>>>>>> x\n",
        )
        .unwrap();
        let loaded = s.goals();
        assert_eq!(loaded.items.len(), 0);
        assert!(loaded.issues[0].1.contains("冲突标记"), "{:?}", loaded.issues);
    }

    #[test]
    fn deps_reject_cycles_and_strangers() {
        let s = seed(&tmp("deps"));
        let g = Goal::new("g", &me());
        s.save_goal(&g).unwrap();

        let mut a = Task::new(&g.id, "a", &me());
        s.save_task(&a).unwrap();
        let mut b = Task::new(&g.id, "b", &me());
        b.depends_on = vec![a.id.clone()];
        s.save_task(&b).unwrap();

        // 让 a 依赖 b，和 b→a 一起成环。
        a.depends_on = vec![b.id.clone()];
        assert!(s.validate_deps(&g.id, &a).unwrap_err().contains("成环"));

        // 引用一个不存在的任务。
        let mut c = Task::new(&g.id, "c", &me());
        c.depends_on = vec!["t-nope".into()];
        assert!(s.validate_deps(&g.id, &c).unwrap_err().contains("不在这个目标里"));

        // 自己依赖自己。
        let mut d = Task::new(&g.id, "d", &me());
        d.depends_on = vec![d.id.clone()];
        assert!(s.validate_deps(&g.id, &d).unwrap_err().contains("依赖自己"));
    }

    #[test]
    fn goal_view_computes_progress_and_readiness() {
        let s = seed(&tmp("view"));
        let g = Goal::new("g", &me());
        s.save_goal(&g).unwrap();

        let parent = Task::new(&g.id, "父", &me());
        s.save_task(&parent).unwrap();

        let mut child1 = Task::new(&g.id, "子一", &me());
        child1.parent_id = Some(parent.id.clone());
        child1.status = TaskStatus::Done;
        s.save_task(&child1).unwrap();

        let mut child2 = Task::new(&g.id, "子二", &me());
        child2.parent_id = Some(parent.id.clone());
        child2.depends_on = vec![child1.id.clone()];
        s.save_task(&child2).unwrap();

        let v = s.goal_view(&g.id).unwrap();
        assert_eq!(v.roots, vec![parent.id.clone()], "有父的都不算根");
        assert_eq!(v.counts.total, 3);
        assert_eq!(v.counts.done, 1);

        let parent_view = v.tasks.iter().find(|t| t.task.id == parent.id).unwrap();
        assert_eq!(parent_view.children.len(), 2);
        assert_eq!(parent_view.progress, 50, "两个子任务完成一个");

        let c2 = v.tasks.iter().find(|t| t.task.id == child2.id).unwrap();
        assert!(c2.waiting_on.is_empty(), "前置已完成，不再等");
        assert!(c2.ready, "待办 + 前置完成 = 可以开工");
    }

    #[test]
    fn boards_store_cards_per_column_in_order() {
        let s = seed(&tmp("board"));
        let b = Board::new("回顾", &me());
        s.save_board(&b).unwrap();

        let first = b.columns[0].id.clone();
        let mut c1 = Card::new(&b.id, &first, "一", &me());
        c1.order = 2.0;
        let mut c2 = Card::new(&b.id, &first, "二", &me());
        c2.order = 1.0;
        s.save_card(&c1).unwrap();
        s.save_card(&c2).unwrap();

        let loaded = s.cards(&b.id);
        assert_eq!(loaded.items.len(), 2);
        assert_eq!(loaded.items[0].text, "二", "按 order 排，不按写入先后");
    }

    #[test]
    fn invalid_ids_never_touch_the_filesystem() {
        let s = seed(&tmp("id"));
        assert!(s.goal("../../etc/passwd").is_err());
        assert!(s.task("g-1", "../x").is_err());
        assert!(s.board("b-x/../../y").is_err());
    }

    #[test]
    fn discover_walks_up_to_the_project_root() {
        let root = tmp("discover");
        let s = seed(&root);
        let deep = s.project_root().join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        let found = Store::discover(&deep).expect("从深层目录要能找到");
        assert_eq!(found.root(), s.root());
        // 没有 .carter/ 的地方返回 None，而不是瞎猜一个。
        assert!(Store::discover(&std::env::temp_dir().join("no-such-team-xyz")).is_none());
    }
}
