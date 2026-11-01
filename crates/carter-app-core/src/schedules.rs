//! 定时任务：持久化、触发时间的计算，以及「到点了自动开一条新对话」。
//!
//! ## 为什么是「开新对话」而不是「往旧对话里塞一条消息」
//!
//! 定时任务跑的时候用户多半不在。往一条正在聊的会话里塞消息，会与用户自己说的
//! 话交错，而模型看到的历史里没有任何东西说明这一句从哪来；更要紧的是那条会话
//! 可能正卡在一个需要人拍板的工具上。开一条**独立的**会话，每一轮都是完整、可
//! 归档、可回看的 —— 用户回来时侧栏里就是一条「9:00 的日报」，点开就能读。
//!
//! ## 为什么先写盘再触发
//!
//! 触发顺序是「把 `next_at` 推到下一次 → 落盘 → 开会话」。反过来的话，开会话
//! 这一步如果崩了（或者用户正好在这一秒退出），盘上记的还是老时间，下次启动就会
//! 再触发一遍。宁可少跑一次，也不能在本地留一个每次都重复跑的任务。
//!
//! ## 为什么不在启动时补跑很久以前的任务
//!
//! 应用关着的时候时间照走。一个「每天早上 9 点」的任务，隔三天才打开应用时补跑
//! 一次是有意义的（那是用户想看的日报）；但如果隔了半个月，补比不补更烦人。
//! 所以只补 [`CATCH_UP_MS`] 以内的，更早的直接跳过去。
//!
//! ## 触发出来的会话不能弹表单
//!
//! 定时会话多半没人在看，`ask_user_question` 会把一整轮永远卡在 await 上（问题
//! 没有界面可以回答）。触发时给正文末尾追加一句说明，让模型把要问的事写进回答里
//! 而不是调用工具，见 [`FIRED_NOTE`]。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::protocol::now_ms;

/// 应用关着的时候错过的任务，最多补跑多久。
pub const CATCH_UP_MS: u64 = 24 * 60 * 60 * 1000;

pub const MIN_INTERVAL_MINUTES: u64 = 1;
pub const MAX_INTERVAL_MINUTES: u64 = 30 * 24 * 60;

/// 定时触发的会话追加在正文末尾的说明。
///
/// 放在正文而不是系统提示里：系统提示是会话身份的一部分（缓存前缀），按任务变
/// 会让每条定时会话的前缀都不一样；而这句说明本来就是「这一条消息的上下文」。
pub const FIRED_NOTE: &str = "\n\n---\n（这条消息由定时任务自动发出，触发时没有人在场。需要用户拍板的事请直接写进回答里，不要调用 ask_user_question —— 没有人会回答它，那一轮会一直卡住。）";

/// 触发方式。
///
/// 三个变体覆盖了绝大多数「定时」需求，而刻意没有做 cron 表达式：cron 的入口是
/// 一段字符串，模型写错一位（`0 9 * * 1` 里的星期从 0 还是 1 数）没有任何提示，
/// 用户得到的是一条永远不会跑的任务。结构化字段让「每天 9:00」在创建那一刻就能
/// 校验出对错。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleKind {
    /// 一次性：到点跑一次，然后自己停用。
    Once { at_ms: u64 },
    /// 每隔 N 分钟。相位从创建时刻起算，之后不再漂移。
    Interval { every_minutes: u64 },
    /// 每天的本机时间 `hour:minute`。
    Daily { hour: u32, minute: u32 },
}

impl ScheduleKind {
    /// 从「上一次的计划时间」往后推一格，推到现在之后为止。
    ///
    /// `None` = 没有下一次（一次性任务跑完了）。
    ///
    /// 间隔任务**按原来的相位往前数**，而不是从 `now` 重新起算：否则每次触发都
    /// 因为那几秒的调度延迟往后错一点，一天下来能漂出小半分钟，用户看到的时间
    /// 与设置里的就不是一回事了。
    fn advance_from(&self, from: u64, now: u64) -> Option<u64> {
        match *self {
            ScheduleKind::Once { .. } => None,
            ScheduleKind::Interval { every_minutes } => {
                let period = every_minutes.max(MIN_INTERVAL_MINUTES) * 60_000;
                let missed = now.saturating_sub(from) / period + 1;
                Some(from.saturating_add(missed.saturating_mul(period)))
            }
            ScheduleKind::Daily { hour, minute } => Some(next_daily(now, hour, minute)),
        }
    }

    /// 界面上那一行「每天 09:00」。
    pub fn label(&self) -> String {
        match *self {
            ScheduleKind::Once { at_ms } => format!("{} 触发一次", fmt_local(at_ms)),
            ScheduleKind::Interval { every_minutes } => match every_minutes {
                m if m % (24 * 60) == 0 => format!("每 {} 天", m / (24 * 60)),
                m if m % 60 == 0 => format!("每 {} 小时", m / 60),
                m => format!("每 {m} 分钟"),
            },
            ScheduleKind::Daily { hour, minute } => format!("每天 {hour:02}:{minute:02}"),
        }
    }
}

// ---------------------------------------------------------------------------
// 结构
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    /// 短标题，也是界面上那一行的名字。同时用作定时会话的标题前缀。
    pub name: String,
    /// 触发时作为用户消息发出去的正文。
    pub prompt: String,
    pub kind: ScheduleKind,
    /// 触发时在哪个目录开新对话。
    pub cwd: String,
    /// `渠道/显示名`。`None` = 用配置里当前的默认模型。
    #[serde(default)]
    pub model: Option<String>,
    pub enabled: bool,
    pub created_at: u64,
    /// 下一次预计触发（epoch 毫秒）。
    pub next_at: u64,
    #[serde(default)]
    pub last_fired_at: Option<u64>,
    /// 上一次触发开出来的会话 id —— 界面上「打开上次那条」直接跳过去。
    #[serde(default)]
    pub last_session_id: Option<String>,
    #[serde(default)]
    pub runs: u32,
    /// 上一次触发失败的原因（开不出会话时）。下一次成功就清掉。
    #[serde(default)]
    pub last_error: Option<String>,
}

impl Schedule {
    /// 造一条新任务并算好第一次触发时间。
    pub fn new(
        name: impl Into<String>,
        prompt: impl Into<String>,
        kind: ScheduleKind,
        cwd: impl Into<String>,
        model: Option<String>,
    ) -> Result<Self, String> {
        let now = now_ms();
        let name = name.into().trim().to_string();
        let prompt = prompt.into().trim().to_string();
        if name.is_empty() {
            return Err("定时任务的 name 不能为空".into());
        }
        if prompt.is_empty() {
            return Err("定时任务的 prompt（到点要发的那句话）不能为空".into());
        }
        validate_kind(&kind, now)?;
        let next_at = match kind {
            ScheduleKind::Once { at_ms } => at_ms,
            ScheduleKind::Interval { .. } => now + 60_000,
            ScheduleKind::Daily { hour, minute } => next_daily(now, hour, minute),
        };
        Ok(Self {
            id: new_id(now),
            name,
            prompt,
            kind,
            cwd: cwd.into(),
            model,
            enabled: true,
            created_at: now,
            next_at,
            last_fired_at: None,
            last_session_id: None,
            runs: 0,
            last_error: None,
        })
    }

    /// 按当前的 `kind` 重新算下一次（界面改了时间之后调用）。
    pub fn reschedule(&mut self) {
        let now = now_ms();
        self.next_at = match self.kind {
            ScheduleKind::Once { at_ms } => at_ms,
            ScheduleKind::Interval { .. } => now + 60_000,
            ScheduleKind::Daily { hour, minute } => next_daily(now, hour, minute),
        };
    }

    pub fn is_due(&self, now: u64) -> bool {
        self.enabled && self.next_at <= now
    }
}

fn validate_kind(kind: &ScheduleKind, now: u64) -> Result<(), String> {
    match *kind {
        ScheduleKind::Once { at_ms } => {
            if at_ms <= now {
                return Err(format!(
                    "一次性任务的时间（{}）已经过去了，请给一个将来的时间",
                    fmt_local(at_ms)
                ));
            }
            Ok(())
        }
        ScheduleKind::Interval { every_minutes } => {
            if !(MIN_INTERVAL_MINUTES..=MAX_INTERVAL_MINUTES).contains(&every_minutes) {
                return Err(format!(
                    "间隔应在 {MIN_INTERVAL_MINUTES} 分钟到 {} 天之间，收到 {every_minutes} 分钟",
                    MAX_INTERVAL_MINUTES / (24 * 60)
                ));
            }
            Ok(())
        }
        ScheduleKind::Daily { hour, minute } => {
            if hour > 23 || minute > 59 {
                return Err(format!("时间 {hour}:{minute} 不合法（小时 0-23，分钟 0-59）"));
            }
            Ok(())
        }
    }
}

fn new_id(now: u64) -> String {
    // 同一毫秒里连建两条（模型一轮里并行调用两次）时补一个序号，免得撞 id。
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("sch_{now}_{n}")
}

// ---------------------------------------------------------------------------
// 本机时间
// ---------------------------------------------------------------------------

/// 从 `after_ms` 之后算起，本机时间下一个 `hour:minute`。
///
/// 用 `chrono::Local` 而不是手算 UTC 偏移：夏令时、历史时区调整这些事，手写一遍
/// 就是给自己埋一个「某天早上没跑」的坑。夏令时切换当天的「不存在的那一个小时」
/// （春季跳表）会顺延到第二天，而不是在那天随便挑一个时刻。
pub fn next_daily(after_ms: u64, hour: u32, minute: u32) -> u64 {
    use chrono::{Local, LocalResult, NaiveDate, TimeZone};

    let after = Local
        .timestamp_millis_opt(after_ms as i64)
        .single()
        .unwrap_or_else(Local::now);
    let mut date: NaiveDate = after.date_naive();
    // 最多往后找两天：今天 / 明天，再加一天是给跳表那一天留的余量。
    for _ in 0..3 {
        if let Some(dt) = date.and_hms_opt(hour, minute, 0) {
            let candidate = match Local.from_local_datetime(&dt) {
                LocalResult::Single(t) => Some(t.timestamp_millis()),
                // 秋季回拨那一小时会出现两次：取**后一次**，与「钟面上还是这个点」一致。
                LocalResult::Ambiguous(a, b) => {
                    Some(a.timestamp_millis().max(b.timestamp_millis()))
                }
                // 春季跳表：这个钟点在今天不存在。
                LocalResult::None => None,
            };
            if let Some(ms) = candidate {
                if ms as u64 > after_ms {
                    return ms as u64;
                }
            }
        }
        match date.succ_opt() {
            Some(d) => date = d,
            None => break,
        }
    }
    // 走不到（只有「今天之后所有日子都不存在这个钟点」才会到这儿），兜底一天后。
    after_ms + 86_400_000
}

/// 本机时间的 `YYYY-MM-DD HH:MM`。界面上显示、以及模型读回来看用。
pub fn fmt_local(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(ms as i64).single() {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

/// 解析用户/模型给的本地时间：`YYYY-MM-DD HH:MM`（也认 `YYYY/MM/DD`）。
pub fn parse_local(text: &str) -> Result<u64, String> {
    use chrono::{Local, NaiveDateTime, TimeZone};

    let text = text.trim();
    let mut last: Option<chrono::ParseError> = None;
    for fmt in ["%Y-%m-%d %H:%M", "%Y/%m/%d %H:%M", "%Y-%m-%dT%H:%M"] {
        match NaiveDateTime::parse_from_str(text, fmt) {
            Ok(dt) => {
                return match Local.from_local_datetime(&dt) {
                    chrono::LocalResult::Single(t) => Ok(t.timestamp_millis() as u64),
                    chrono::LocalResult::Ambiguous(a, _) => Ok(a.timestamp_millis() as u64),
                    chrono::LocalResult::None => {
                        Err(format!("`{text}` 在这个时区里不存在（夏令时跳表）"))
                    }
                };
            }
            Err(e) => last = Some(e),
        }
    }
    Err(format!(
        "认不出时间 `{text}`：请写成 `2026-03-01 09:30` 这种本机时间（年-月-日 时:分）{}",
        last.map(|e| format!("（{e}）")).unwrap_or_default()
    ))
}

// ---------------------------------------------------------------------------
// 存储
// ---------------------------------------------------------------------------

/// 文件形状。包一层对象是为了以后加字段（比如全局暂停）时不必改文件格式。
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    schedules: Vec<Schedule>,
}

/// 全部定时任务。一个文件，进程内一把锁。
///
/// 与 `goals/` 那种「一个会话一个文件」不同：定时任务是全局的，条数少（几十条
/// 顶天），一个文件读写一次就完，界面与后台调度器看到的是同一份。
pub struct ScheduleStore {
    path: PathBuf,
    inner: Mutex<Vec<Schedule>>,
}

impl ScheduleStore {
    pub fn path() -> PathBuf {
        paths::schedules_path()
    }

    /// 读盘。文件坏了、不存在，都当作「没有任务」—— 一份坏掉的任务表不该让
    /// 应用起不来（与 `AppSettings::load` 同一个取舍）。
    pub fn open(path: PathBuf) -> Arc<Self> {
        let list = read_file(&path);
        Arc::new(Self {
            path,
            inner: Mutex::new(list),
        })
    }

    pub fn open_default() -> Arc<Self> {
        Self::open(Self::path())
    }

    /// 改一次并存盘。**先写盘再改内存**：写失败时磁盘与内存都还是旧的那份，
    /// 不会出现「界面显示已保存、重启就没了」。
    fn with<R>(&self, f: impl FnOnce(&mut Vec<Schedule>) -> R) -> Result<R, String> {
        let mut guard = self.inner.lock().unwrap();
        let mut next = guard.clone();
        let out = f(&mut next);
        write_file(&self.path, &next)?;
        *guard = next;
        Ok(out)
    }

    pub fn list(&self) -> Vec<Schedule> {
        let mut v = self.inner.lock().unwrap().clone();
        // 停用的沉到底下，其余按下次触发时间排 —— 界面上第一眼看到的就是
        // 「接下来要跑哪个」。
        v.sort_by_key(|s| (!s.enabled, s.next_at));
        v
    }

    pub fn get(&self, id: &str) -> Option<Schedule> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// 按 id 或名字找一条。模型拿到的名字是它自己起的，用户看到的是同一个 ——
    /// 两种都能用，免得「让它删掉 X，它说找不到」。
    pub fn find(&self, key: &str) -> Option<Schedule> {
        let key = key.trim();
        let list = self.inner.lock().unwrap();
        list.iter()
            .find(|s| s.id == key)
            .or_else(|| list.iter().find(|s| s.name == key))
            .cloned()
    }

    /// 新增或覆盖（同 id）。
    pub fn upsert(&self, s: Schedule) -> Result<(), String> {
        self.with(|list| match list.iter_mut().find(|x| x.id == s.id) {
            Some(slot) => *slot = s,
            None => list.push(s),
        })
    }

    pub fn remove(&self, id: &str) -> Result<bool, String> {
        self.with(|list| {
            let before = list.len();
            list.retain(|s| s.id != id);
            list.len() != before
        })
    }

    /// 开关。重新打开时接着用盘上原来的 `next_at`（如果还没过期），否则按当前
    /// 时间重算 —— 关了一个月再打开，不该立刻补跑。
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<Option<Schedule>, String> {
        let now = now_ms();
        self.with(|list| {
            let Some(s) = list.iter_mut().find(|s| s.id == id) else {
                return None;
            };
            s.enabled = enabled;
            if enabled && s.next_at <= now {
                s.reschedule();
            }
            Some(s.clone())
        })
    }

    /// 取出现在该跑的，并**先把下一次时间推到盘上**。
    ///
    /// 过了 [`CATCH_UP_MS`] 还没跑的不返回 —— 它只被静默推后（见模块头）。
    pub fn claim_due(&self, now: u64) -> Result<Vec<Schedule>, String> {
        self.with(|list| {
            let mut due = Vec::new();
            for s in list.iter_mut() {
                if !s.is_due(now) {
                    continue;
                }
                let overdue = now.saturating_sub(s.next_at);
                match s.kind.advance_from(s.next_at, now) {
                    Some(next) => s.next_at = next,
                    None => s.enabled = false, // 一次性任务：跑完就停用，留着做记录
                }
                if overdue <= CATCH_UP_MS {
                    due.push(s.clone());
                }
            }
            due
        })
    }

    /// 一次触发的结果写回盘上。
    pub fn record_fired(&self, id: &str, at: u64, session_id: Option<&str>, error: Option<&str>) {
        let _ = self.with(|list| {
            if let Some(s) = list.iter_mut().find(|s| s.id == id) {
                s.last_fired_at = Some(at);
                s.runs += 1;
                match session_id {
                    Some(sid) => {
                        s.last_session_id = Some(sid.to_string());
                        s.last_error = None;
                    }
                    None => s.last_error = error.map(str::to_string),
                }
            }
        });
    }

    /// 触发失败（开不出会话）也要推时间，否则下一轮还会再试、再失败。
    ///
    /// 与 `record_fired` 分开是因为「推时间」已经在 `claim_due` 里做过了 ——
    /// 这里只记错误。留着这个函数是为了让调用方不必知道这个细节。
    pub fn record_error(&self, id: &str, message: &str) {
        let _ = self.with(|list| {
            if let Some(s) = list.iter_mut().find(|s| s.id == id) {
                s.last_error = Some(message.to_string());
            }
        });
    }
}

fn read_file(path: &Path) -> Vec<Schedule> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<File>(&raw) {
        Ok(f) => f.schedules,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "定时任务文件读不出来，按空表处理");
            Vec::new()
        }
    }
}

/// 先写临时文件再改名。
///
/// 直接覆写的话，写到一半断电就剩一个截断的 JSON，**全部任务一起丢**。改名在
/// 两个平台上都是原子的（Windows 上是 `MoveFileEx`，目标存在时替换）。
fn write_file(path: &Path, list: &[Schedule]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let json = serde_json::to_string_pretty(&File {
        schedules: list.to_vec(),
    })
    .map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("更新 {} 失败：{e}", path.display()))
}

// ---------------------------------------------------------------------------
// 调度循环
// ---------------------------------------------------------------------------

/// 轮询间隔。定时任务的最小粒度是分钟，10 秒足以让「9:00 的日报」在 9:00:0x 跑起来，
/// 又不会让一个几乎空转的循环耗电。
const TICK: std::time::Duration = std::time::Duration::from_secs(10);

/// 在后台跑调度。**必须在 tokio runtime 里调用。**
pub fn spawn_scheduler(shell: Arc<crate::shell::AppShell>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        // 错过一拍（应用被挂起、机器睡眠）之后不要连着补跑几次，直接按现在算。
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            fire_due(&shell).await;
        }
    });
}

/// 现在就该跑的，逐条开新会话。
pub async fn fire_due(shell: &Arc<crate::shell::AppShell>) {
    let now = now_ms();
    let due = match shell.schedules.claim_due(now) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "定时任务表写不回去，这一拍跳过");
            return;
        }
    };
    for s in due {
        let _ = fire(shell, &s, now).await;
    }
}

/// 立即跑一条（界面上「立即运行」）。不等到点，也不改它原来的节奏。
pub async fn fire_now(shell: &Arc<crate::shell::AppShell>, id: &str) -> Result<String, String> {
    let s = shell
        .schedules
        .get(id)
        .ok_or_else(|| format!("没有 id 为 `{id}` 的定时任务"))?;
    let now = now_ms();
    fire(shell, &s, now).await
}

/// 开一条新会话并把任务正文发进去。
///
/// 返回新会话 id。失败不抛给调用方 —— 触发是后台行为，用户看到的是界面上那条
/// 红字（`AppEvent::ScheduleFired` 的 `error`），而不是一个没人接的错误。
async fn fire(shell: &Arc<crate::shell::AppShell>, s: &Schedule, now: u64) -> Result<String, String> {
    use crate::protocol::Command;
    use crate::session::{start_session, SessionOptions};

    let opts = SessionOptions {
        cwd: PathBuf::from(&s.cwd),
        model: s.model.clone(),
        resume_latest: false,
        resume_id: None,
    };
    let started = match start_session(shell.clone(), opts) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("定时任务「{}」启动失败：{e}", s.name);
            shell.schedules.record_error(&s.id, &msg);
            shell.emit(crate::protocol::AppEvent::ScheduleFired {
                schedule_id: s.id.clone(),
                name: s.name.clone(),
                session_id: None,
                error: Some(msg.clone()),
            });
            return Err(msg);
        }
    };
    let session_id = started.handle.id.clone();
    // 标题不在这里设：会话文件是惰性建的，此刻它还不存在，`rename` 只会失败。
    // 它本来也是「新会话首轮之后自动起名」的那条路，见 `Runner::title_pending`。

    let text = format!("{}{}", s.prompt, FIRED_NOTE);
    if !started.handle.send(Command::SendPrompt {
        text,
        id: None,
        interject: false,
    }) {
        let msg = format!("定时任务「{}」的会话没能接收消息", s.name);
        shell.schedules.record_error(&s.id, &msg);
        shell.emit(crate::protocol::AppEvent::ScheduleFired {
            schedule_id: s.id.clone(),
            name: s.name.clone(),
            session_id: Some(session_id.clone()),
            error: Some(msg.clone()),
        });
        return Err(msg);
    }

    shell.schedules.record_fired(&s.id, now, Some(&session_id), None);
    shell.emit(crate::protocol::AppEvent::ScheduleFired {
        schedule_id: s.id.clone(),
        name: s.name.clone(),
        session_id: Some(session_id.clone()),
        error: None,
    });
    Ok(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "carter-sched-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("schedules.json")
    }

    fn every(minutes: u64) -> Schedule {
        Schedule::new("测试", "跑一下", ScheduleKind::Interval { every_minutes: minutes }, "/tmp", None)
            .unwrap()
    }

    #[test]
    fn a_schedule_round_trips_through_the_file() {
        let path = tmp("roundtrip");
        let store = ScheduleStore::open(path.clone());
        let mut s = every(30);
        s.prompt = "看一遍 CI".into();
        store.upsert(s.clone()).unwrap();

        let again = ScheduleStore::open(path);
        let got = again.get(&s.id).expect("应当读回来");
        assert_eq!(got.name, s.name);
        assert_eq!(got.prompt, "看一遍 CI");
        assert_eq!(got.kind, s.kind);
        assert!(got.enabled);
    }

    #[test]
    fn a_corrupt_file_reads_as_empty_instead_of_failing() {
        let path = tmp("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(ScheduleStore::open(path).list().is_empty());
    }

    /// 同一条任务不会因为「改了几次」而在文件里出现两行。
    #[test]
    fn upsert_replaces_by_id() {
        let store = ScheduleStore::open(tmp("upsert"));
        let mut s = every(5);
        store.upsert(s.clone()).unwrap();
        s.name = "改名了".into();
        store.upsert(s.clone()).unwrap();
        let list = store.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "改名了");
    }

    #[test]
    fn remove_reports_whether_anything_went_away() {
        let store = ScheduleStore::open(tmp("remove"));
        let s = every(5);
        store.upsert(s.clone()).unwrap();
        assert!(store.remove(&s.id).unwrap());
        assert!(!store.remove(&s.id).unwrap());
    }

    /// 到点的被取走，而且**取走之后盘上已经是下一次** —— 否则崩溃会让它重复触发。
    #[test]
    fn claiming_advances_the_next_time_before_returning() {
        let path = tmp("claim");
        let store = ScheduleStore::open(path.clone());
        let s = every(10);
        store.upsert(s.clone()).unwrap();

        let due_at = s.next_at;
        let claimed = store.claim_due(due_at).unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, s.id);

        let after = ScheduleStore::open(path).get(&s.id).unwrap();
        assert!(after.next_at > due_at, "下一次必须往后推");
        assert!(after.enabled, "间隔任务不该停用");
        // 再取一次（同一时刻）不该重复拿到。
        assert!(store.claim_due(due_at).unwrap().is_empty());
    }

    #[test]
    fn an_interval_keeps_its_phase_instead_of_drifting() {
        let start = 1_000_000u64;
        let kind = ScheduleKind::Interval { every_minutes: 10 };
        // 晚了 25 分钟才被调度到：下一次应当落在 start + 30min，而不是 now + 10min。
        let now = start + 25 * 60_000;
        assert_eq!(kind.advance_from(start, now), Some(start + 30 * 60_000));
        // 刚好准点：推一个周期。
        assert_eq!(kind.advance_from(start, start), Some(start + 10 * 60_000));
    }

    #[test]
    fn a_once_schedule_disables_itself_after_running() {
        let store = ScheduleStore::open(tmp("once"));
        let s = Schedule::new(
            "一次性",
            "跑一下",
            ScheduleKind::Once {
                at_ms: now_ms() + 1000,
            },
            "/tmp",
            None,
        )
        .unwrap();
        store.upsert(s.clone()).unwrap();

        assert_eq!(store.claim_due(now_ms() + 2000).unwrap().len(), 1);
        let after = store.get(&s.id).unwrap();
        assert!(!after.enabled, "一次性任务跑完应当停用");
    }

    /// 隔了很久才打开应用：超过补跑窗口的不触发，只把时间往后推。
    #[test]
    fn a_long_overdue_schedule_is_skipped_not_caught_up() {
        let store = ScheduleStore::open(tmp("overdue"));
        let s = every(60);
        store.upsert(s.clone()).unwrap();

        let way_later = now_ms() + CATCH_UP_MS + 60 * 60 * 1000;
        assert!(store.claim_due(way_later).unwrap().is_empty());
        assert!(store.get(&s.id).unwrap().next_at > way_later);
    }

    #[test]
    fn re_enabling_a_stale_schedule_reschedules_it() {
        let store = ScheduleStore::open(tmp("reenable"));
        let mut s = every(5);
        store.upsert(s.clone()).unwrap();
        store.set_enabled(&s.id, false).unwrap();
        // 模拟放了很久。
        s.next_at = 1; // 直接改盘上的值
        store.upsert(s.clone()).unwrap();

        let back = store.set_enabled(&s.id, true).unwrap().unwrap();
        assert!(back.enabled);
        assert!(back.next_at > now_ms(), "重新打开时不该立刻补跑");
    }

    #[test]
    fn disabled_schedules_never_fire() {
        let store = ScheduleStore::open(tmp("disabled"));
        let s = every(1);
        store.upsert(s.clone()).unwrap();
        store.set_enabled(&s.id, false).unwrap();
        assert!(store.claim_due(u64::MAX / 2).unwrap().is_empty());
    }

    #[test]
    fn bad_parameters_are_rejected_with_an_explanation() {
        let err = |k: ScheduleKind| {
            Schedule::new("n", "p", k, "/tmp", None)
                .err()
                .unwrap_or_default()
        };
        assert!(err(ScheduleKind::Interval { every_minutes: 0 }).contains("间隔"));
        assert!(err(ScheduleKind::Interval {
            every_minutes: MAX_INTERVAL_MINUTES + 1
        })
        .contains("间隔"));
        assert!(err(ScheduleKind::Daily {
            hour: 24,
            minute: 0
        })
        .contains("不合法"));
        assert!(err(ScheduleKind::Once { at_ms: 1 }).contains("过去了"));
        assert!(Schedule::new("", "p", ScheduleKind::Interval { every_minutes: 5 }, "/tmp", None)
            .unwrap_err()
            .contains("name"));
    }

    #[test]
    fn local_times_parse_in_the_documented_format() {
        let ms = parse_local("2026-03-01 09:30").unwrap();
        assert_eq!(fmt_local(ms), "2026-03-01 09:30");
        assert_eq!(
            parse_local("2026/03/01 09:30").unwrap(),
            ms,
            "斜杠写法也认"
        );
        assert!(parse_local("明天早上").is_err());
    }

    /// 「每天 9:00」从任意时刻算起，结果都落在真正的 9:00 上。
    #[test]
    fn the_next_daily_lands_on_the_wall_clock_time() {
        let now = parse_local("2026-03-01 08:00").unwrap();
        let next = next_daily(now, 9, 0);
        assert_eq!(fmt_local(next), "2026-03-01 09:00");
        // 已经过了今天的 9:00 → 明天。
        let late = parse_local("2026-03-01 10:00").unwrap();
        assert_eq!(fmt_local(next_daily(late, 9, 0)), "2026-03-02 09:00");
        // 正好 9:00:00 → 严格往后，是明天，不会在同一毫秒重复触发。
        let exact = parse_local("2026-03-01 09:00").unwrap();
        assert_eq!(fmt_local(next_daily(exact, 9, 0)), "2026-03-02 09:00");
    }

    #[test]
    fn the_label_reads_like_the_thing_a_user_set() {
        assert_eq!(
            ScheduleKind::Interval { every_minutes: 60 }.label(),
            "每 1 小时"
        );
        assert_eq!(
            ScheduleKind::Interval { every_minutes: 90 }.label(),
            "每 90 分钟"
        );
        assert_eq!(
            ScheduleKind::Daily {
                hour: 9,
                minute: 5
            }
            .label(),
            "每天 09:05"
        );
    }

    /// 按名字也能找到 —— 模型删任务时用的往往是它自己起的名字。
    #[test]
    fn find_accepts_either_the_id_or_the_name() {
        let store = ScheduleStore::open(tmp("find"));
        let mut s = every(5);
        s.name = "早报".into();
        store.upsert(s.clone()).unwrap();
        assert_eq!(store.find(&s.id).map(|x| x.id), Some(s.id.clone()));
        assert_eq!(store.find("早报").map(|x| x.id), Some(s.id));
        assert!(store.find("不存在").is_none());
    }
}
