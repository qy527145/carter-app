//! 长任务目标（`/goal`）：状态机、持久化、以及**每轮结束后该怎么走**的决策。
//!
//! ## 为什么状态不放进会话文件
//!
//! carter 的 `RecordKind` 是**封闭枚举**，会话文件里出现一个它不认识的记录类型，
//! `fold_file` 会解析失败 —— 整条对话都读不出来了（不是「丢掉这一条」，是「这个
//! 会话打不开」）。所以目标状态存在 `~/.carter-app/goals.json`，按会话 id 索引。
//!
//! ## 为什么决策是一个纯函数
//!
//! 「模型停下了，要不要再推它一把」是这块最容易出错、也最需要被守住的逻辑：
//! 判错一边是模型自己空转烧钱，判错另一边是长任务干到一半就静静停住了。
//! [`decide_after_turn`] 把这件事收成一个（目标状态 + 本轮结果）→ 决策的纯映射，
//! 每条终止规则都能单独测，runner 那边只剩「照着做」。
//!
//! ## 系统提示里绝不能出现会变的东西
//!
//! [`system_segment`] 拼出来的是系统提示的**最后一段**。今天 carter 0.1.4 还没打开
//! cache 断点（`agent/turn.rs` 生成的 system 块 `cache` 恒为 `None`，全仓没有一处
//! `CacheMarker`），所以现在写什么都还不影响命中率。但断点打在「system 末段 / 工具表 /
//! 最新一条 user 消息」上是既定方向，而**三段前缀里任何一段被改动，整个会话的缓存
//! 就全废**（messages 与 tools 都排在 system 之后，system 变一个字节，后面的 token
//! 前缀全部错位）。所以这里现在就按那个约束写：目标正文与状态进提示（它们只在用户
//! 动 `/goal` 时才变），轮数这类每轮都变的东西只进界面。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/// 连续自动继续的上限。
///
/// 不是 token 预算（那要接 usage 记账，且用户明确说不需要），是一道**死循环的闸**：
/// 模型一直不表态、也不干活时，别让它把钱包烧穿。到了上限就转 `paused` 并告诉用户，
/// 用户点一下「继续」就能再来一轮上限。
pub const MAX_AUTO_CONTINUES: u32 = 20;

/// 连续多少轮「一个工具都没调」就判定为空转。
const IDLE_STREAK_LIMIT: u32 = 2;

/// 自动继续时追加的那条用户消息。
///
/// 写成一条**明确的指令**而不是「继续」两个字：模型看到的最后一条消息如果是
/// 用户自己的话，它可能以为用户换了话题；这里要让它知道这是系统在推着走，
/// 以及推不动时该怎么退出。
pub const CONTINUE_PROMPT: &str = "\
（自动继续）长任务目标还没有完成，接着推进。

如果已经完成：调用 goal_update，status = completed，并说明做了什么。
如果被外部条件卡住（缺权限、缺依赖、上游报错…）：调用 goal_update，status = blocked，说明卡在哪。
如果必须由用户拍板才能往下走（删文件、改协议、选方案…）：调用 goal_update，status = needs_user，把问题问清楚。

不要在没有进展的情况下重复同一件事。";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// 正在推进：模型停下而目标未完成时会自动继续。
    Active,
    /// 暂停：模型不再被自动推着走，但目标仍作为上下文（用户点「继续」恢复）。
    Paused,
    /// 模型报告完成。
    Completed,
    /// 模型报告被卡住。
    Blocked,
}

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Paused => "paused",
            GoalStatus::Completed => "completed",
            GoalStatus::Blocked => "blocked",
        }
    }

    /// 界面上的说法。状态是给人看的，别让前端再维护一份映射。
    pub fn label(self) -> &'static str {
        match self {
            GoalStatus::Active => "进行中",
            GoalStatus::Paused => "已暂停",
            GoalStatus::Completed => "已完成",
            GoalStatus::Blocked => "被卡住",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    /// 目标正文（用户写的那句话）。
    pub objective: String,
    pub status: GoalStatus,
    /// 已经自动继续了多少轮。**只进界面，不进系统提示**（见模块头）。
    #[serde(default)]
    pub auto_continues: u32,
    /// 含本轮的连续「没调任何工具」轮数。用来识别空转。
    #[serde(default)]
    pub idle_streak: u32,
    /// 模型在 `goal_update` 里写的说明（blocked / needs_user 时界面要显出来）。
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub updated_at: u64,
}

impl Goal {
    pub fn new(objective: impl Into<String>) -> Self {
        Self {
            objective: objective.into(),
            status: GoalStatus::Active,
            auto_continues: 0,
            idle_streak: 0,
            note: None,
            updated_at: crate::protocol::now_ms(),
        }
    }

    /// 换状态时的公共收尾：清掉/保留 note、刷新时间戳。
    fn with_status(mut self, status: GoalStatus, note: Option<String>) -> Self {
        self.status = status;
        self.note = note;
        self.updated_at = crate::protocol::now_ms();
        self
    }
}

// ---------------------------------------------------------------------------
// 给模型的完成信号
// ---------------------------------------------------------------------------

/// 模型通过 `goal_update` 报上来的结局。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Completed { note: Option<String> },
    Blocked { note: String },
    NeedsUser { note: String },
}

/// `goal_update` 工具与 runner 之间那一格共享状态。
///
/// 用共享格子而不是通道：工具是在 `run_turn` 里面被 await 的，而 runner 那会儿正
/// 阻塞在 select 上、没有机会收通道里的东西。一格共享状态最简单，也天然只有
/// 「最后一次调用」—— 模型一轮里调两次时，以后者为准。
///
/// `armed` 是给工具用的：`goal_update` **恒常注册**（工具表每轮都变会改变请求前缀，
/// 见模块头；而且突然多一个工具会让模型重新打量整个工具列表），所以它必须自己知道
/// 「现在这个会话到底有没有在推目标」，否则模型会在没有目标时也调它、还得到一个
/// 看起来成功的回复。
#[derive(Default)]
pub struct GoalSignal {
    armed: std::sync::atomic::AtomicBool,
    slot: std::sync::Mutex<Option<Signal>>,
}

impl GoalSignal {
    /// 目标是否正在推进（= 状态为 `active`）。由 runner 在每轮开始前设置。
    pub fn set_armed(&self, on: bool) {
        self.armed.store(on, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn armed(&self) -> bool {
        self.armed.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn set(&self, s: Signal) {
        *self.slot.lock().unwrap() = Some(s);
    }

    /// 取走（读一次就清空）。一轮结束后读，避免影响下一轮。
    pub fn take(&self) -> Option<Signal> {
        self.slot.lock().unwrap().take()
    }
}

// ---------------------------------------------------------------------------
// 每轮结束后的决策
// ---------------------------------------------------------------------------

/// 一轮跑完之后，runner 收集到的「这一轮发生了什么」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnOutcome {
    /// 用户在这一轮里点过「停止」。
    pub user_cancelled: bool,
    /// 这一轮以错误收场（上游 5xx、网络断了…）。
    pub failed: bool,
    /// 这一轮调用过工具（含 MCP）。
    pub used_tools: bool,
    /// **含本轮**的连续无工具轮数。
    pub idle_streak: u32,
    /// 模型通过 `goal_update` 报的结局。
    pub signal: Option<Signal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// 什么都不做（目标不活跃，或这一轮不需要继续）。
    None,
    /// 追加一条「继续」再跑一轮。
    Continue,
    /// 改状态，并且要告诉用户为什么 —— 静默停下是最坏的：用户以为它还在干。
    Set {
        status: GoalStatus,
        note: Option<String>,
        notice: String,
    },
}

/// 一轮结束后该怎么走。**纯函数**，六条终止规则各自可测。
///
/// 规则顺序即优先级，不能随意调换：
/// 1. 目标不活跃（暂停 / 完成 / 被卡住）→ 不动；
/// 2. 模型报了结局 → 照它说的落状态（这是唯一的「正常收束」路径）；
/// 3. **用户中断过 → 不继续**。用户按停止是有理由的，自动再推一轮会把「停止」
///    这个动作变成假的；状态留在 `active`，他下次发消息或点「继续」就恢复；
/// 4. 本轮出错 → 转 paused。对着一个正在失败的上游自动重试，是把一次故障
///    放大成 20 次；
/// 5. 连续若干轮一个工具都没调 → 转 paused（模型只是在说话，不是在干活）；
/// 6. 到自动继续上限 → 转 paused。
pub fn decide_after_turn(goal: &Goal, outcome: &TurnOutcome) -> Decision {
    if goal.status != GoalStatus::Active {
        return Decision::None;
    }

    if let Some(signal) = &outcome.signal {
        return match signal {
            Signal::Completed { note } => Decision::Set {
                status: GoalStatus::Completed,
                note: note.clone(),
                notice: "目标已完成".to_string(),
            },
            Signal::Blocked { note } => Decision::Set {
                status: GoalStatus::Blocked,
                note: Some(note.clone()),
                notice: format!("模型报告目标被卡住：{note}"),
            },
            Signal::NeedsUser { note } => Decision::Set {
                status: GoalStatus::Paused,
                note: Some(note.clone()),
                notice: format!("模型需要你拍板才能继续：{note}"),
            },
        };
    }

    if outcome.user_cancelled {
        return Decision::None;
    }

    if outcome.failed {
        return Decision::Set {
            status: GoalStatus::Paused,
            note: None,
            notice: format!(
                "本轮出错，已暂停目标模式（避免对着失败反复重试）。点目标条上的「继续」可以再来。"
            ),
        };
    }

    if !outcome.used_tools && outcome.idle_streak >= IDLE_STREAK_LIMIT {
        return Decision::Set {
            status: GoalStatus::Paused,
            note: None,
            notice: format!(
                "连续 {IDLE_STREAK_LIMIT} 轮没有调用任何工具，判断为原地打转，已暂停目标模式。"
            ),
        };
    }

    if goal.auto_continues >= MAX_AUTO_CONTINUES {
        return Decision::Set {
            status: GoalStatus::Paused,
            note: None,
            notice: format!(
                "已连续自动继续 {MAX_AUTO_CONTINUES} 轮，先停下来。点「继续」可以接着推。"
            ),
        };
    }

    Decision::Continue
}

// ---------------------------------------------------------------------------
// 系统提示
// ---------------------------------------------------------------------------

/// 目标在系统提示里那一段。`None` = 不该出现（没有目标，或已经收尾）。
///
/// **不能带轮数、时间戳、note 之外的任何变量** —— 见模块头。
pub fn system_segment(goal: &Goal) -> Option<String> {
    match goal.status {
        GoalStatus::Active => Some(format!(
            "## 长任务目标\n{}\n\n\
             这是用户在本次会话里设定的目标，未完成之前要持续推进。\n\
             - 完成之后调用 `goal_update`（status = \"completed\"）说明结果；\n\
             - 被外部条件卡住、或必须由用户拍板才能往下走时，也调用 `goal_update`\n  \
               （status = \"blocked\" / \"needs_user\"）说明卡在哪 —— 不要自己替用户做决定；\n\
             - 你停下而目标还没完成时，本应用会自动再发起一轮「继续」。",
            goal.objective
        )),
        GoalStatus::Paused => Some(format!(
            "## 长任务目标（已暂停）\n{}\n\n\
             用户暂停了这个目标：除非用户明确要求，不要主动推进它。",
            goal.objective
        )),
        // 已完成 / 被卡住：模型刚在上下文里说过，再复述一遍只是烧 token。
        GoalStatus::Completed | GoalStatus::Blocked => None,
    }
}

// ---------------------------------------------------------------------------
// 持久化
// ---------------------------------------------------------------------------
//
// `~/.carter-app/goals/<session_id>.json` —— 一个会话一个文件，与 `captures/` 同形。
// 不共用一个 `goals.json` 是因为主会话与侧边会话可能同时活着，共用就得处理
// 「读改写」的互相覆盖；而会话 id 本身就是索引键，分开存不花任何代价。

/// 会话 id → 文件名。id 是 uuid（侧边会话是 `side-<uuid>`），理论上不会有路径分隔符，
/// 但这里是**拼路径**的地方，一个 `../` 就能写到目录外面去 —— 稳妥起见过一遍白名单。
fn file_for(session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    paths::goals_dir().join(format!("{safe}.json"))
}

/// 读某个会话的目标。没有（或文件坏了）就是没有 —— 坏掉的目标不该让会话起不来。
pub fn load(session_id: &str) -> Option<Goal> {
    load_from(&file_for(session_id))
}

fn load_from(path: &Path) -> Option<Goal> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 写。**先写临时文件再改名** —— 直接覆盖时中途断电会留下半截 JSON，
/// 那份目标就永远读不回来了。
pub fn save(session_id: &str, goal: &Goal) -> Result<(), String> {
    save_to(&file_for(session_id), goal)
}

fn save_to(path: &Path, goal: &Goal) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let json = serde_json::to_string_pretty(goal).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("改名到 {} 失败：{e}", path.display()))
}

/// 删掉某个会话的目标。会话被删时调用，否则这个目录会跟着历史会话一直长。
pub fn remove(session_id: &str) -> bool {
    std::fs::remove_file(file_for(session_id)).is_ok()
}

/// 会话刚起来时该拿什么目标。
///
/// **盘上处于 `active` 的一律降级成 `paused`。** 用户关掉应用时目标还在跑，
/// 下次打开如果原样恢复，模型会在人不在的时候自己开工烧 token。降级 + 一句说明，
/// 让他自己点「继续」。
pub fn on_session_start(mut goal: Goal) -> (Goal, bool) {
    if goal.status != GoalStatus::Active {
        return (goal, false);
    }
    goal.status = GoalStatus::Paused;
    goal.note = None;
    goal.updated_at = crate::protocol::now_ms();
    (goal, true)
}


/// 界面用的目标视图（IPC）。
pub fn view(goal: &Goal) -> crate::protocol::GoalView {
    crate::protocol::GoalView {
        objective: goal.objective.clone(),
        status: goal.status.as_str().to_string(),
        status_label: goal.status.label().to_string(),
        auto_continues: goal.auto_continues,
        note: goal.note.clone(),
    }
}

/// 状态的迁移入口，供 `Command::Goal` 分支调用。返回 `None` = 这个动作在当前
/// 状态下没有意义（例如「继续」一个不存在的目标），调用方据此给提示。
pub fn apply_action(current: Option<Goal>, action: &crate::protocol::GoalCommand) -> Option<Option<Goal>> {
    use crate::protocol::GoalCommand as A;
    match action {
        A::Set { objective } => {
            let text = objective.trim();
            if text.is_empty() {
                return None;
            }
            // 已有目标时改的是**正文**，状态与计数保留 —— 用户说「改成 X」时
            // 多半是想接着干，而不是想重来。
            //
            // 例外：上一个目标已经收尾（完成 / 被卡住）。那时这就是一件**新事**，
            // 计数得从头来 —— 否则新目标会立刻撞上上一件活攒下的自动继续上限。
            Some(Some(match current {
                Some(mut g) => {
                    if matches!(g.status, GoalStatus::Completed | GoalStatus::Blocked) {
                        g.auto_continues = 0;
                        g.idle_streak = 0;
                    }
                    g.objective = text.to_string();
                    g.status = GoalStatus::Active;
                    g.note = None;
                    g.updated_at = crate::protocol::now_ms();
                    g
                }
                None => Goal::new(text),
            }))
        }
        A::Clear => current.map(|_| None),
        A::Pause => current.filter(|g| g.status == GoalStatus::Active).map(|g| {
            let note = g.note.clone();
            Some(g.with_status(GoalStatus::Paused, note))
        }),
        A::Resume => current
            .filter(|g| g.status != GoalStatus::Active)
            .map(|mut g| {
                // 恢复时把计数与空转计数归零：用户这一下点击就是一轮新的开始，
                // 否则「继续」会立刻又撞在上限上。
                g.auto_continues = 0;
                g.idle_streak = 0;
                Some(g.with_status(GoalStatus::Active, None))
            }),
        A::Complete => current.filter(|g| g.status != GoalStatus::Completed).map(|g| {
            let note = g.note.clone();
            Some(g.with_status(GoalStatus::Completed, note))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("carter-app-goals-{tag}-{}-{n}", std::process::id()))
            .join("goals.json")
    }

    fn active() -> Goal {
        Goal::new("把 crates/core 的告警清零")
    }

    fn outcome() -> TurnOutcome {
        TurnOutcome {
            used_tools: true,
            ..Default::default()
        }
    }

    // --- 持久化 -------------------------------------------------------------

    #[test]
    fn a_goal_round_trips_through_the_file() {
        let path = tmp_path("roundtrip");
        let mut g = active();
        g.auto_continues = 3;
        save_to(&path, &g).unwrap();

        assert_eq!(load_from(&path), Some(g));
        // 写的时候先落 `.tmp` 再改名，别把半截文件留在原地。
        assert!(!path.with_extension("json.tmp").exists(), "临时文件要清掉");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_corrupt_file_reads_as_no_goal_instead_of_panicking() {
        let path = tmp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_from(&path).is_none());
        assert!(load_from(&tmp_path("missing")).is_none(), "文件不存在也是 None");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// 会话 id 是拿来拼文件名的。带 `..` 或分隔符的 id 必须被挡在外面 ——
    /// 否则就是「删会话时顺手删掉别人文件」那一类事故。
    #[test]
    fn a_hostile_session_id_cannot_escape_the_goals_dir() {
        let name = file_for("../evil/../../etc/passwd").file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains('/') && !name.contains('\\') && !name.contains(".."), "{name}");
        assert!(file_for("side-0e1f").to_string_lossy().ends_with("side-0e1f.json"));
    }

    /// **盘上 active 的一律降级。** 用户关掉应用时目标还在跑，下次打开原样恢复
    /// 就意味着人不在的时候模型自己开工烧 token。
    #[test]
    fn a_goal_left_active_by_a_crash_comes_back_paused() {
        let (g, downgraded) = on_session_start(active());
        assert!(downgraded);
        assert_eq!(g.status, GoalStatus::Paused);

        // 已经收尾的目标原样带回，不算降级。
        let done = active().with_status(GoalStatus::Completed, Some("做完了".into()));
        let (g, downgraded) = on_session_start(done.clone());
        assert!(!downgraded);
        assert_eq!(g, done);
    }

    // --- 系统提示 -----------------------------------------------------------

    /// **这条是这套设计的地基之一**：系统提示的末段正是 cache 断点的位置，
    /// 里面出现任何每轮都变的东西，长任务越跑越贵。
    #[test]
    fn the_system_segment_never_carries_a_counter() {
        let mut g = active();
        g.auto_continues = 7;
        g.idle_streak = 2;
        let seg = system_segment(&g).expect("进行中的目标必须进提示");

        assert!(seg.contains(&g.objective), "目标正文要在：{seg}");
        assert!(!seg.contains('7'), "不能出现轮数：{seg}");
        assert!(!seg.contains('2'), "不能出现空转计数：{seg}");
        assert!(
            !seg.contains(&g.updated_at.to_string()),
            "不能出现时间戳：{seg}"
        );

        // 同一个目标、不同轮数 → 提示逐字节相同。
        let mut g2 = g.clone();
        g2.auto_continues = 11;
        assert_eq!(system_segment(&g), system_segment(&g2));
    }

    #[test]
    fn a_paused_goal_says_dont_push_on_it_but_stays_in_context() {
        let g = active().with_status(GoalStatus::Paused, None);
        let seg = system_segment(&g).unwrap();
        assert!(seg.contains("暂停"), "{seg}");
        assert!(seg.contains(&g.objective), "目标本身仍是上下文：{seg}");
    }

    /// 已完成 / 被卡住不再复述 —— 模型刚在上下文里说过这件事。
    #[test]
    fn finished_goals_leave_the_system_prompt_alone() {
        for st in [GoalStatus::Completed, GoalStatus::Blocked] {
            assert!(system_segment(&active().with_status(st, None)).is_none());
        }
        assert!(system_segment(&Goal::new("x")).is_some(), "active 要在");
    }

    // --- 决策：六条终止规则 --------------------------------------------------

    #[test]
    fn an_unfinished_turn_keeps_going() {
        assert_eq!(decide_after_turn(&active(), &outcome()), Decision::Continue);
    }

    #[test]
    fn the_model_saying_completed_stops_the_loop() {
        let o = TurnOutcome {
            signal: Some(Signal::Completed {
                note: Some("告警清零了".into()),
            }),
            ..outcome()
        };
        match decide_after_turn(&active(), &o) {
            Decision::Set { status, note, notice } => {
                assert_eq!(status, GoalStatus::Completed);
                assert_eq!(note.as_deref(), Some("告警清零了"));
                assert!(notice.contains("完成"), "{notice}");
            }
            other => panic!("期望 Set，得到 {other:?}"),
        }
    }

    /// 模型说「要你拍板」→ 暂停并把问题带出来。这条比「一味继续」重要：
    /// 否则模型会自己把「要不要删这个文件」拍了。
    #[test]
    fn needs_user_pauses_and_carries_the_question() {
        let o = TurnOutcome {
            signal: Some(Signal::NeedsUser {
                note: "要删掉旧的迁移脚本吗".into(),
            }),
            ..outcome()
        };
        match decide_after_turn(&active(), &o) {
            Decision::Set { status, notice, .. } => {
                assert_eq!(status, GoalStatus::Paused);
                assert!(notice.contains("要删掉旧的迁移脚本吗"), "{notice}");
            }
            other => panic!("期望 Set，得到 {other:?}"),
        }
    }

    /// 用户按了停止 → 不许继续。把「停止」变成假的比不做自动继续更坏。
    #[test]
    fn a_user_interrupt_beats_the_auto_continue() {
        let o = TurnOutcome {
            user_cancelled: true,
            ..outcome()
        };
        assert_eq!(decide_after_turn(&active(), &o), Decision::None);
    }

    /// 模型自己报了结局、同时用户也按了停止时，以模型的结局为准（它已经收尾了）。
    #[test]
    fn a_reported_outcome_wins_over_an_interrupt() {
        let o = TurnOutcome {
            user_cancelled: true,
            signal: Some(Signal::Completed { note: None }),
            ..outcome()
        };
        assert!(matches!(
            decide_after_turn(&active(), &o),
            Decision::Set {
                status: GoalStatus::Completed,
                ..
            }
        ));
    }

    #[test]
    fn a_failed_turn_pauses_instead_of_retrying() {
        let o = TurnOutcome {
            failed: true,
            ..outcome()
        };
        match decide_after_turn(&active(), &o) {
            Decision::Set { status, notice, .. } => {
                assert_eq!(status, GoalStatus::Paused);
                assert!(notice.contains("出错"), "{notice}");
            }
            other => panic!("期望 Set，得到 {other:?}"),
        }
    }

    /// 空转：连续两轮一个工具都没调 → 停。一轮不算 —— 模型可能只是在收尾说明。
    #[test]
    fn two_idle_turns_in_a_row_stop_the_loop() {
        let idle = |streak| TurnOutcome {
            used_tools: false,
            idle_streak: streak,
            ..Default::default()
        };
        assert_eq!(
            decide_after_turn(&active(), &idle(1)),
            Decision::Continue,
            "只是在说话一轮还不能定性"
        );
        match decide_after_turn(&active(), &idle(2)) {
            Decision::Set { status, notice, .. } => {
                assert_eq!(status, GoalStatus::Paused);
                assert!(notice.contains("打转"), "{notice}");
            }
            other => panic!("期望 Set，得到 {other:?}"),
        }
    }

    /// 到了上限：停，并且说清「点继续可以接着推」—— 否则用户会以为目标废了。
    #[test]
    fn the_cap_pauses_with_a_way_back() {
        let mut g = active();
        g.auto_continues = MAX_AUTO_CONTINUES;
        match decide_after_turn(&g, &outcome()) {
            Decision::Set { status, notice, .. } => {
                assert_eq!(status, GoalStatus::Paused);
                assert!(notice.contains("继续"), "{notice}");
            }
            other => panic!("期望 Set，得到 {other:?}"),
        }
    }

    /// 暂停 / 完成 / 被卡住的目标，任何一轮结束都不该再动它。
    #[test]
    fn an_inactive_goal_is_left_alone() {
        for st in [GoalStatus::Paused, GoalStatus::Completed, GoalStatus::Blocked] {
            let g = active().with_status(st, None);
            assert_eq!(decide_after_turn(&g, &outcome()), Decision::None);
            // 连模型的信号都不该改它：状态已经不在推进了。
            let o = TurnOutcome {
                signal: Some(Signal::Completed { note: None }),
                ..outcome()
            };
            assert_eq!(decide_after_turn(&g, &o), Decision::None);
        }
    }

    // --- 命令 -----------------------------------------------------------------

    use crate::protocol::GoalCommand;

    #[test]
    fn setting_a_goal_reactivates_an_existing_one_without_resetting_counts() {
        let mut old = active();
        old.status = GoalStatus::Paused;
        old.auto_continues = 5;
        let got = apply_action(Some(old), &GoalCommand::Set { objective: " 换个说法 ".into() })
            .unwrap()
            .unwrap();
        assert_eq!(got.objective, "换个说法", "要 trim");
        assert_eq!(got.status, GoalStatus::Active, "改正文 = 接着干");
        assert_eq!(got.auto_continues, 5, "换说法不该把计数清零");
    }

    #[test]
    fn resume_resets_the_counters_so_it_does_not_hit_the_cap_immediately() {
        let mut g = active();
        g.status = GoalStatus::Paused;
        g.auto_continues = MAX_AUTO_CONTINUES;
        g.idle_streak = 2;
        let got = apply_action(Some(g), &GoalCommand::Resume).unwrap().unwrap();
        assert_eq!(got.status, GoalStatus::Active);
        assert_eq!(got.auto_continues, 0);
        assert_eq!(got.idle_streak, 0);
    }

    /// 上一件活已经收尾（完成 / 被卡住）时再设目标 = 一件新事，计数必须从头来。
    /// 不重置的话，新目标会立刻撞上上一件活攒下的自动继续上限、设完就停。
    #[test]
    fn setting_a_goal_after_a_finished_one_starts_the_counters_over() {
        for status in [GoalStatus::Completed, GoalStatus::Blocked] {
            let mut old = active();
            old.status = status;
            old.auto_continues = MAX_AUTO_CONTINUES;
            old.idle_streak = 3;
            let got = apply_action(
                Some(old),
                &GoalCommand::Set {
                    objective: "换个活干".into(),
                },
            )
            .unwrap()
            .unwrap();
            assert_eq!(got.status, GoalStatus::Active);
            assert_eq!(got.auto_continues, 0, "{status:?} 之后重设要从零开始");
            assert_eq!(got.idle_streak, 0);
        }
    }

    /// 在不该生效的状态下做动作 → `None`，由调用方给一句提示，而不是静默无事发生。
    #[test]
    fn no_op_actions_report_themselves() {
        let g = active();
        assert!(apply_action(None, &GoalCommand::Clear).is_none(), "没有目标可清");
        assert!(
            apply_action(Some(g.clone()), &GoalCommand::Resume).is_none(),
            "已经在推进，无从恢复"
        );
        assert!(
            apply_action(Some(g.clone()), &GoalCommand::Set { objective: "   ".into() }).is_none(),
            "空正文不该建目标"
        );
        // 反过来：暂停一个正在推进的目标是**有意义**的，必须生效。
        let paused = apply_action(Some(g), &GoalCommand::Pause).unwrap().unwrap();
        assert_eq!(paused.status, GoalStatus::Paused);
    }

    #[test]
    fn the_signal_slot_hands_the_outcome_over_once() {
        let s = GoalSignal::default();
        assert!(s.take().is_none(), "没调过工具时是空的");
        s.set(Signal::Blocked { note: "没网".into() });
        assert_eq!(s.take(), Some(Signal::Blocked { note: "没网".into() }));
        assert!(s.take().is_none(), "读完就该清掉，不能影响下一轮");
    }
}
