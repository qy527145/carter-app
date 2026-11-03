//! 团队协作的数据模型：目标 / 任务 / 看板。
//!
//! ## 与 Bibrain 的差别，以及每处差别的理由
//!
//! 这套结构参考了 BiBrain 的 `goal.json` + `task-graph.json`，但把几件在多人并发下
//! 迟早会疼的事提前改掉了：
//!
//! 1. **一个实体一个文件。** Bibrain 把整张任务图（节点数组 + 独立的 `edges` 数组）
//!    放在一个 `task-graph.json` 里。两个人各改一个任务，改动落在同一个文件的相邻
//!    行上，git 的三路合并只能靠「规范序列化」去赌；追加一条依赖更是必然撞同一处。
//!    这里拆成 `tasks/<id>.json`，冲突域收敛到「同一个人同一个任务同时改」——
//!    那本来就该串行。依赖关系也收进任务自身（`depends_on`），边不再是第二个冲突热点。
//!
//! 2. **只存当前态，历史交给 git。** BiBrain 一个节点上挂着 `created_at` /
//!    `created_by` / `started_at` / `finished_at` / `updated_at` / `updated_by` /
//!    `blocked_from` / `handed_from` 十来个时间与来源字段。那些字段与 git 里的提交
//!    历史是**同一份事实的两次记录**，一旦不一致就没人说得清哪个对。这里只留
//!    `created_*` / `updated_*`（用来排序与显示「谁最后动的」），「什么时候开始的」
//!    「上一棒是谁」这类问题去 `git log` 里现算。
//!
//! 3. **派生量不落盘。** 父任务的进度由子任务算，不存 `progress` 在父节点上；成员表
//!    由 git 提交记录派生，不维护一份 `members.json`；跨目标的统计进 `.cache/`，
//!    随删随建。BiBrain 里那套 `baseline_*` 投影表是同一个思路，这里连数据库都不用。
//!
//! 4. **顺序用浮点数插值。** 在 A 与 B 之间插入一件东西，取两者中点即可，不用把
//!    兄弟节点的 `sort_order` 全部重排一遍 —— 整树重排会让「插入一个任务」变成
//!    「改掉所有兄弟节点的文件」，等于每次都制造全体冲突。
//!
//! 5. **id 自带单调性。** `t-<base36 毫秒>-<4 位随机>`：多人同时新建不撞车，字典序
//!    即创建序。BiBrain 的 `display_no` 要靠网关发号，这里省掉一个中心服务。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 文件格式版本。读到更高的版本要明确报错，而不是按旧结构硬解。
pub const SCHEMA_VERSION: u32 = 1;

/// 给「字符串当枚举」的类型实现 `Serialize` / `Deserialize`：写时用 `as_str()`
/// 的规范形式，读时走 `parse()`（它本来就大小写不敏感）。
///
/// 这个宏存在的原因是踩过一次坑：`Priority` 用 `#[serde(rename_all = "snake_case")]`
/// 派生，写出 `"p2"`、也只认 `"p2"`；而脚本与技能文档写的是 `"P2"`。两边各自都对，
/// 合起来就是「数据建出来了、界面读不出来」，报错还只说到 `unknown variant`。
///
/// 派生宏生成的是**精确匹配**，而 `parse()` 是有意宽容的 —— 用同一个入口，
/// 就不会出现「校验说没问题、serde 说不认识」这种两套规则并存的局面。
macro_rules! string_enum {
    ($ty:ty) => {
        impl Serialize for $ty {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }
        impl<'de> ::serde::Deserialize<'de> for $ty {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = <String as ::serde::Deserialize>::deserialize(d)?;
                Self::parse(&raw)
                    .ok_or_else(|| ::serde::de::Error::custom($crate::team::model::bad_value(&raw, &<$ty>::allowed())))
            }
        }
    };
}

/// 认不出的值时给的一句话：既说收到了什么，也说合法的有哪些。
pub fn bad_value(raw: &str, allowed: &str) -> String {
    format!("认不出 `{raw}`，只能是 {allowed}")
}

// ---------------------------------------------------------------------------
// id
// ---------------------------------------------------------------------------

/// 实体种类。id 的前缀由它决定，界面上也靠它区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Goal,
    Task,
    Board,
    Card,
    Category,
}

impl Kind {
    pub fn prefix(self) -> &'static str {
        match self {
            Kind::Goal => "g",
            Kind::Task => "t",
            Kind::Board => "b",
            Kind::Card => "k",
            Kind::Category => "cat",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Goal => "目标",
            Kind::Task => "任务",
            Kind::Board => "看板",
            Kind::Card => "卡片",
            Kind::Category => "分类",
        }
    }
}

/// 生成一个新 id：`<前缀>-<base36 毫秒>-<随机>`。
///
/// 随机段不是装饰：同一毫秒里两次「新建任务」（并发派生几个子 agent 时会这样）
/// 光靠时间戳会撞成同一个 id，而 id 在这里就是文件名 —— 撞了等于静默覆盖别人的
/// 任务。4 位 base36 是 168 万种取值，够用了。
pub fn new_id(kind: Kind) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let ms = crate::protocol::now_ms();
    let n = (ms as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left(17)
        ^ SEQ.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x2545_F491_4F6C_DD1D)
        ^ (std::process::id() as u64).wrapping_shl(24);
    let rand = to_base36(n % (36 * 36 * 36 * 36));
    format!("{}-{}-{:0>4}", kind.prefix(), to_base36(ms), rand)
}

fn to_base36(mut v: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if v == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while v > 0 {
        out.push(DIGITS[(v % 36) as usize]);
        v /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// 这个 id 是不是给定种类的一个合法 id。
///
/// 校验在这里做而不是在写文件时顺手拼路径：id 会直接变成文件名，`../../x` 这种
/// 输入在 Windows 与 unix 上行为还不一样，放进来就是一次路径穿越。
pub fn valid_id(kind: Kind, id: &str) -> bool {
    let Some(rest) = id.strip_prefix(kind.prefix()) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 64
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---------------------------------------------------------------------------
// 成员身份
// ---------------------------------------------------------------------------

/// 一个团队成员。身份就是 git 提交里的 `Name <email>`。
///
/// 不另建成员表：git 的 `user.name` / `user.email` 已经是每个人在提交里留下的
/// 唯一标识，再维护一份名单只会多出一份「谁是谁」的映射需要同步。显示名与邮箱
/// 都在这一条里，界面要展示头像色就从邮箱哈希算。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    /// `Name <email>` —— 与 git 自己的写法一致，方便直接写进提交 trailer。
    pub fn as_key(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }

    /// 从 `Name <email>` 解析回来。解析不出来就当整串是名字。
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if let (Some(open), Some(close)) = (raw.find('<'), raw.rfind('>')) {
            if close > open {
                let name = raw[..open].trim();
                let email = raw[open + 1..close].trim();
                if !email.is_empty() {
                    return Some(Self {
                        name: if name.is_empty() {
                            email.split('@').next().unwrap_or(email).to_string()
                        } else {
                            name.to_string()
                        },
                        email: email.to_string(),
                    });
                }
            }
        }
        Some(Self {
            name: raw.to_string(),
            email: String::new(),
        })
    }

    /// 界面上那个短名字。
    pub fn short(&self) -> &str {
        if self.name.is_empty() { &self.email } else { &self.name }
    }
}

impl fmt::Display for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.email.is_empty() {
            f.write_str(&self.name)
        } else {
            write!(f, "{} <{}>", self.name, self.email)
        }
    }
}

// ---------------------------------------------------------------------------
// 目标
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalStatus {
    /// 正在推进。
    Active,
    /// 暂时搁置（人不在、等上游）。与 `Active` 的区别只影响界面排序与「我的任务」。
    Paused,
    /// 达成。任务图不再自动带进「待办」。
    Done,
    /// 归档：留在仓库里，界面默认折叠。
    Archived,
}

string_enum!(GoalStatus);

impl GoalStatus {
    /// 合法的写法。与 `parse` 同源，界面与错误信息都从这里取。
    pub fn allowed() -> String {
        "active / paused / done / archived".to_string()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Paused => "paused",
            GoalStatus::Done => "done",
            GoalStatus::Archived => "archived",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GoalStatus::Active => "进行中",
            GoalStatus::Paused => "搁置",
            GoalStatus::Done => "已完成",
            GoalStatus::Archived => "已归档",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "active" | "doing" => GoalStatus::Active,
            "paused" => GoalStatus::Paused,
            "done" | "completed" | "finished" => GoalStatus::Done,
            "archived" => GoalStatus::Archived,
            _ => return None,
        })
    }

    /// 这一档还会不会被界面当作「当前在做的事」。
    pub fn is_live(self) -> bool {
        matches!(self, GoalStatus::Active | GoalStatus::Paused)
    }
}

/// 优先级。
///
/// **序列化一律大写 `P0`..`P3`，反序列化大小写都认。**
///
/// 这条不对齐过一次：这个枚举原先用 `rename_all = "snake_case"`，于是 Rust 写出
/// `"p2"`、也只认 `"p2"`；而 `carter-team` 脚本与技能文档写的都是人习惯的大写
/// `"P2"`。两边各自都对，合到一起就是「目标建出来了、界面读不出来」——
/// 报错还只说到 `unknown variant`，看不出真正的原因是大小写。
///
/// 现在往**大写**这一边收：它是人写、脚本写、issue 里写的形式。旧文件里的
/// `"p2"` 照样读得进来，下次写回时自动变成 `"P2"`，不需要迁移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    P0,
    P1,
    P2,
    P3,
}

string_enum!(Priority);

impl Default for Priority {
    fn default() -> Self {
        Priority::P2
    }
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::P0 => "P0",
            Priority::P1 => "P1",
            Priority::P2 => "P2",
            Priority::P3 => "P3",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_uppercase().as_str() {
            "P0" | "URGENT" => Priority::P0,
            "P1" | "HIGH" => Priority::P1,
            "P2" | "NORMAL" => Priority::P2,
            "P3" | "LOW" => Priority::P3,
            _ => return None,
        })
    }

    /// 合法的写法，拼进错误信息里。与 `parse` 认的那几种同源。
    pub fn allowed() -> String {
        "P0 / P1 / P2 / P3".to_string()
    }

    /// 排序用：数越小越靠前。
    pub fn rank(self) -> u8 {
        match self {
            Priority::P0 => 0,
            Priority::P1 => 1,
            Priority::P2 => 2,
            Priority::P3 => 3,
        }
    }
}

/// 一个目标：阶段、模块、一次迭代，都行。
///
/// 目标下面是任务树，所以「目标」与 BiBrain 里那个目标实体同构 —— 区别在于
/// 目标不再挂 `deliverable` / `acceptance` 这类执行细节（那些属于任务），
/// 目标是**划分范围**用的，越薄越好。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default = "kind_goal")]
    pub kind: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_goal_status")]
    pub status: GoalStatus,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default)]
    pub category_id: Option<String>,
    /// 负责人。`None` = 还没人接。
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    /// 目标排序。浮点插值，见模块头第 4 条。
    #[serde(default)]
    pub order: f64,
    #[serde(default)]
    pub start_at: Option<u64>,
    #[serde(default)]
    pub due_at: Option<u64>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub updated_at: Option<u64>,
    #[serde(default)]
    pub updated_by: Option<String>,
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
}
fn kind_goal() -> String {
    "goal".into()
}
fn default_goal_status() -> GoalStatus {
    GoalStatus::Active
}

impl Goal {
    pub fn new(title: impl Into<String>, by: &Identity) -> Self {
        let now = crate::protocol::now_ms();
        Self {
            schema_version: SCHEMA_VERSION,
            kind: "goal".into(),
            id: new_id(Kind::Goal),
            title: title.into(),
            description: String::new(),
            status: GoalStatus::Active,
            priority: Priority::default(),
            category_id: None,
            owner: None,
            labels: Vec::new(),
            order: now as f64,
            start_at: None,
            due_at: None,
            created_at: now,
            created_by: by.as_key(),
            updated_at: None,
            updated_by: None,
        }
    }

    /// 写回之前的统一收尾：谁改的、什么时候。
    pub fn touch(&mut self, by: &Identity) {
        self.updated_at = Some(crate::protocol::now_ms());
        self.updated_by = Some(by.as_key());
    }
}

// ---------------------------------------------------------------------------
// 任务
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// 还没开始。
    Todo,
    /// 有人在做了。
    Doing,
    /// 卡住了 —— 与 `Doing` 的区别是「要不要人来看」，界面上会单独标出来。
    Blocked,
    /// 完成。
    Done,
    /// 不做了（保留记录，不删文件）。
    Dropped,
}

string_enum!(TaskStatus);

impl TaskStatus {
    /// 合法的写法。与 `parse` 同源。
    pub fn allowed() -> String {
        "todo / doing / blocked / done / dropped".to_string()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::Doing => "doing",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Done => "done",
            TaskStatus::Dropped => "dropped",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskStatus::Todo => "待办",
            TaskStatus::Doing => "进行中",
            TaskStatus::Blocked => "受阻",
            TaskStatus::Done => "已完成",
            TaskStatus::Dropped => "已放弃",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "todo" | "pending" | "open" => TaskStatus::Todo,
            "doing" | "in_progress" | "progress" => TaskStatus::Doing,
            "blocked" => TaskStatus::Blocked,
            "done" | "completed" | "finished" => TaskStatus::Done,
            "dropped" | "cancelled" | "canceled" => TaskStatus::Dropped,
            _ => return None,
        })
    }

    /// 还占着「待办」名额的状态。`done` / `dropped` 不算。
    pub fn is_open(self) -> bool {
        matches!(self, TaskStatus::Todo | TaskStatus::Doing | TaskStatus::Blocked)
    }
}

/// 一个任务。任务自己成文件（见模块头第 1 条）。
///
/// 父子关系用 `parent_id` 表达，依赖关系用 `depends_on` 表达，两者**刻意分开**：
/// 父子是「拆解」，依赖是「先后」。BiBrain 把两者都塞进 `edges` 数组，于是
/// 「这个任务属于谁」和「这个任务在等谁」在数据上长得一样，界面上只能靠 kind
/// 字段区分 —— 而画图时两者用的是完全不同的布局约束。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default = "kind_task")]
    pub kind: String,
    pub id: String,
    /// 所属目标。冗余在文件里是**有意的**：任务文件名在目标目录下，但把任务
    /// 整体挪到另一个目标时，文件里的这个字段能立刻看出有没有搬干净。
    pub goal_id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_task_status")]
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default)]
    pub assignee: Option<String>,
    /// 必须先完成的任务 id。**有环就是错**，写入前要验（见 [`Store::validate_deps`]）。
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub order: f64,
    #[serde(default)]
    pub estimate_hours: Option<f64>,
    #[serde(default)]
    pub deliverable: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// 受阻原因。只在 `status = blocked` 时有意义，但不清空 —— 恢复之后这段话
    /// 仍然是「上次为什么卡住」的记录。
    #[serde(default)]
    pub blocked_reason: String,
    #[serde(default)]
    pub due_at: Option<u64>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub updated_at: Option<u64>,
    #[serde(default)]
    pub updated_by: Option<String>,
}

fn kind_task() -> String {
    "task".into()
}
fn default_task_status() -> TaskStatus {
    TaskStatus::Todo
}

impl Task {
    pub fn new(goal_id: &str, title: impl Into<String>, by: &Identity) -> Self {
        let now = crate::protocol::now_ms();
        Self {
            schema_version: SCHEMA_VERSION,
            kind: "task".into(),
            id: new_id(Kind::Task),
            goal_id: goal_id.to_string(),
            parent_id: None,
            title: title.into(),
            description: String::new(),
            status: TaskStatus::Todo,
            priority: Priority::default(),
            assignee: None,
            depends_on: Vec::new(),
            order: now as f64,
            estimate_hours: None,
            deliverable: String::new(),
            acceptance: Vec::new(),
            blocked_reason: String::new(),
            due_at: None,
            created_at: now,
            created_by: by.as_key(),
            updated_at: None,
            updated_by: None,
        }
    }

    pub fn touch(&mut self, by: &Identity) {
        self.updated_at = Some(crate::protocol::now_ms());
        self.updated_by = Some(by.as_key());
    }
}

// ---------------------------------------------------------------------------
// 看板
// ---------------------------------------------------------------------------

/// 看板的一列。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardColumn {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub order: f64,
    /// 这一列最多放几张卡。`None` = 不限。
    ///
    /// 只有回顾类看板真的用得着（「待改进」一列堆到 20 张就说明这次回顾没收敛），
    /// 但存成通用字段比留一个 `is_retro` 特判更好 —— 特判会让看板多出一种只对
    /// 某些看板成立的行为，而数据本身不需要知道自己是回顾还是别的。
    #[serde(default)]
    pub wip_limit: Option<u32>,
}

/// 看板里的一个主题分组（回顾里的「做得好的 / 要改的 / 待跟进」也能这么放）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub order: f64,
    #[serde(default)]
    pub color: Option<String>,
}

/// 一块看板：回顾、风险板、待决事项，都行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Board {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default = "kind_board")]
    pub kind: String,
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// 关联的目标。`None` = 跨目标的团队级看板（回顾通常是这样）。
    #[serde(default)]
    pub goal_id: Option<String>,
    #[serde(default = "default_columns")]
    pub columns: Vec<BoardColumn>,
    #[serde(default)]
    pub groups: Vec<BoardGroup>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub updated_at: Option<u64>,
    #[serde(default)]
    pub updated_by: Option<String>,
}

fn kind_board() -> String {
    "board".into()
}

/// 新建看板时的默认三列：起手就能用，用户不必先想清楚列怎么分。
fn default_columns() -> Vec<BoardColumn> {
    [
        ("col-todo", "待办"),
        ("col-doing", "进行中"),
        ("col-done", "完成"),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (id, name))| BoardColumn {
        id: id.into(),
        name: name.into(),
        order: i as f64 + 1.0,
        wip_limit: None,
    })
    .collect()
}

impl Board {
    pub fn new(title: impl Into<String>, by: &Identity) -> Self {
        let now = crate::protocol::now_ms();
        Self {
            schema_version: SCHEMA_VERSION,
            kind: "board".into(),
            id: new_id(Kind::Board),
            title: title.into(),
            description: String::new(),
            goal_id: None,
            columns: default_columns(),
            groups: Vec::new(),
            created_at: now,
            created_by: by.as_key(),
            updated_at: None,
            updated_by: None,
        }
    }

    pub fn touch(&mut self, by: &Identity) {
        self.updated_at = Some(crate::protocol::now_ms());
        self.updated_by = Some(by.as_key());
    }
}

/// 看板上的一张卡。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default = "kind_card")]
    pub kind: String,
    pub id: String,
    pub board_id: String,
    pub column_id: String,
    #[serde(default)]
    pub group_id: Option<String>,
    pub text: String,
    /// 关联的任务。可选 —— 回顾里大多数卡片是「话」而不是「活」。
    #[serde(default)]
    pub task_id: Option<String>,
    /// 点赞数。回顾里用它投票挑出最该跟进的那几条。
    #[serde(default)]
    pub votes: i64,
    #[serde(default)]
    pub order: f64,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub updated_at: Option<u64>,
    #[serde(default)]
    pub updated_by: Option<String>,
}

fn kind_card() -> String {
    "card".into()
}

impl Card {
    pub fn new(board_id: &str, column_id: &str, text: impl Into<String>, by: &Identity) -> Self {
        let now = crate::protocol::now_ms();
        Self {
            schema_version: SCHEMA_VERSION,
            kind: "card".into(),
            id: new_id(Kind::Card),
            board_id: board_id.to_string(),
            column_id: column_id.to_string(),
            group_id: None,
            text: text.into(),
            task_id: None,
            votes: 0,
            order: now as f64,
            created_at: now,
            created_by: by.as_key(),
            updated_at: None,
            updated_by: None,
        }
    }

    pub fn touch(&mut self, by: &Identity) {
        self.updated_at = Some(crate::protocol::now_ms());
        self.updated_by = Some(by.as_key());
    }
}

/// 目标分类字典：阶段、模块这类「横向」的划分。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub order: f64,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryFile {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default = "kind_categories")]
    pub kind: String,
    #[serde(default)]
    pub categories: Vec<Category>,
}

fn kind_categories() -> String {
    "goal_categories".into()
}

impl Default for CategoryFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind: "goal_categories".into(),
            categories: Vec::new(),
        }
    }
}

/// 项目的元信息：`.carter/team.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMeta {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default = "kind_team")]
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// 团队用来同步的远端（`origin` 之类）。`None` = 只在本地用。
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub created_by: String,
}

fn kind_team() -> String {
    "team".into()
}

impl TeamMeta {
    pub fn new(name: impl Into<String>, by: &Identity) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind: "team".into(),
            name: name.into(),
            description: String::new(),
            remote: None,
            created_at: crate::protocol::now_ms(),
            created_by: by.as_key(),
        }
    }
}

// ---------------------------------------------------------------------------
// 顺序插值
// ---------------------------------------------------------------------------

/// 在 `before` 与 `after` 之间算一个新顺序值。
///
/// 三种情况：插在最前（`after` 为空）、插在最后（`before` 为空）、插在中间。
/// 中间的落点取中点；两端各留 1.0 的余量，让「往最前面插」永远有空间。
///
/// 浮点精度用完时会退化成 0，此时调用方应该做一次重排 —— 但那要改一整批文件，
/// 几十万次插入才轮得到一次，为它写一套后台维护不值得。这里用 `f64` 的中值，
/// 在 1.0 附近连续对半砍 50 次才到浮点精度极限，正常使用到不了。
pub fn between(before: Option<f64>, after: Option<f64>) -> f64 {
    match (before, after) {
        (Some(a), Some(b)) if b > a => (a + b) / 2.0,
        // 两个给定的值不构成区间（数据被人手改坏了）：退到「比后者大一点」，
        // 至少保持唯一，不会把两件东西排到同一个位置。
        (Some(a), Some(b)) => b.max(a) + 1.0,
        (None, Some(b)) => b - 1.0,
        (Some(a), None) => a + 1.0,
        (None, None) => crate::protocol::now_ms() as f64,
    }
}

// ---------------------------------------------------------------------------
// 派生视图
// ---------------------------------------------------------------------------

/// 一个任务在派生视图里的样子：原样字段 + 算出来的东西。
#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub task: Task,
    /// 子任务 id（按 `order`）。
    pub children: Vec<String>,
    /// 父任务的进度（0~100）：有子任务就按子任务算，否则按状态给个粗值。
    pub progress: u32,
    /// 还没完成的前置任务。
    pub waiting_on: Vec<String>,
    /// 可以直接开工：状态是 todo、前置全完成、没有认领人或就是自己。
    pub ready: bool,
}

/// 一个目标的派生视图。
#[derive(Debug, Clone, Serialize)]
pub struct GoalView {
    pub goal: Goal,
    pub tasks: Vec<TaskView>,
    /// 根任务 id（`parent_id` 为空），按 `order`。
    pub roots: Vec<String>,
    pub counts: Counts,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Counts {
    pub total: u32,
    pub todo: u32,
    pub doing: u32,
    pub blocked: u32,
    pub done: u32,
    pub dropped: u32,
}

impl Counts {
    pub fn add(&mut self, s: TaskStatus) {
        self.total += 1;
        match s {
            TaskStatus::Todo => self.todo += 1,
            TaskStatus::Doing => self.doing += 1,
            TaskStatus::Blocked => self.blocked += 1,
            TaskStatus::Done => self.done += 1,
            TaskStatus::Dropped => self.dropped += 1,
        }
    }

    /// 完成度（0~100）。放弃了的不算分母 —— 否则删掉一件事反而让进度倒退。
    pub fn percent(&self) -> u32 {
        let denom = self.total.saturating_sub(self.dropped);
        if denom == 0 {
            return if self.done > 0 { 100 } else { 0 };
        }
        ((self.done as f64 / denom as f64) * 100.0).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_sortable_by_time() {
        let a = new_id(Kind::Task);
        let b = new_id(Kind::Task);
        assert_ne!(a, b, "同一毫秒连续生成也不能撞");
        assert!(valid_id(Kind::Task, &a));
        // 前缀不对的一律不认。
        assert!(!valid_id(Kind::Goal, &a));
        assert!(!valid_id(Kind::Task, "t-"));
        assert!(!valid_id(Kind::Task, "t-../../etc/passwd"));
    }

    #[test]
    fn identity_round_trips() {
        let i = Identity::parse("Ada Lovelace <ada@example.com>").unwrap();
        assert_eq!(i.name, "Ada Lovelace");
        assert_eq!(i.email, "ada@example.com");
        assert_eq!(i.as_key(), "Ada Lovelace <ada@example.com>");
        assert_eq!(Identity::parse(&i.as_key()).unwrap(), i);
        // 只有一个邮箱时用 @ 前面的部分当名字。
        let bare = Identity::parse("<bob@example.com>").unwrap();
        assert_eq!(bare.name, "bob");
    }

    #[test]
    fn order_values_fit_between_neighbours() {
        assert!(between(Some(1.0), Some(2.0)) > 1.0);
        assert!(between(Some(1.0), Some(2.0)) < 2.0);
        assert!(between(None, Some(5.0)) < 5.0);
        assert!(between(Some(5.0), None) > 5.0);
        // 连插三次仍严格递增（这是「往两件东西中间反复插」的实际用法）。
        let mut lo = 1.0;
        let hi = 2.0;
        for _ in 0..20 {
            let m = between(Some(lo), Some(hi));
            assert!(m > lo && m < hi);
            lo = m;
        }
    }

    #[test]
    fn percent_ignores_dropped() {
        let mut c = Counts::default();
        for s in [TaskStatus::Done, TaskStatus::Todo, TaskStatus::Dropped] {
            c.add(s);
        }
        assert_eq!(c.percent(), 50, "放弃的不进分母");
        let empty = Counts::default();
        assert_eq!(empty.percent(), 0);
    }
}

#[cfg(test)]
mod enum_tests {
    use super::*;

    /// 这正是用户遇到的那个 bug：脚本写 `"P3"`，而旧实现只认 `"p3"`。
    #[test]
    fn upper_case_priority_is_accepted() {
        let p: Priority = serde_json::from_str("\"P3\"").unwrap();
        assert_eq!(p, Priority::P3);
    }

    /// 旧文件里的 `"p2"` 必须继续读得进来，否则这次修法会让已有数据全废。
    #[test]
    fn lower_case_priority_still_reads() {
        let p: Priority = serde_json::from_str("\"p2\"").unwrap();
        assert_eq!(p, Priority::P2);
        // 写回去统一成大写，下次保存就自动归一了。
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"P2\"");
    }

    #[test]
    fn bad_priority_names_the_legal_values() {
        let e = serde_json::from_str::<Priority>("\"急\"").unwrap_err().to_string();
        assert!(e.contains("P0 / P1 / P2 / P3"), "{e}");
    }

    #[test]
    fn statuses_go_the_same_way() {
        assert_eq!(serde_json::from_str::<TaskStatus>("\"Done\"").unwrap(), TaskStatus::Done);
        assert_eq!(serde_json::from_str::<TaskStatus>("\"doing\"").unwrap(), TaskStatus::Doing);
        assert_eq!(serde_json::to_string(&TaskStatus::Blocked).unwrap(), "\"blocked\"");
        assert_eq!(serde_json::from_str::<GoalStatus>("\"ARCHIVED\"").unwrap(), GoalStatus::Archived);
        assert_eq!(serde_json::to_string(&GoalStatus::Done).unwrap(), "\"done\"");
    }
}
