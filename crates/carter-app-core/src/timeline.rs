//! 每一轮对话的**起止时刻** —— 只服务界面，一个会话一个文件。
//!
//! ## 为什么要单独存
//!
//! 界面上每条消息旁边要显示时间。会话文件里其实**没有**消息时刻：carter 的
//! `Recorder` 只在记录行上带 `ts`（`{ts, type, payload}`），折叠成 `WireMessage`
//! 时那一层就丢了，而 `fold_file` 也不往回带。要拿到它就得自己再解析一遍 JSONL ——
//! 那等于把 carter 的文件格式复制一份，它一改我们就静默读错。
//!
//! 真正需要的时间只有两种，而且都是**轮**的属性，不是消息的属性：
//!
//! * 这一轮什么时候被问的（用户气泡下方）；
//! * 这一轮什么时候答完的（回答末尾）。
//!
//! 一次用户输入可能引发多条助手消息（每次工具往返各一条），它们共用同一个「答完」
//! 时刻。所以按轮存就够了，不必按消息存。
//!
//! 也不能写进 carter 的会话文件：`RecordKind` 是封闭枚举，多一种记录类型会让整条
//! 会话**打不开**（见 `goal.rs` 模块头的同一段说明）。所以按会话 id 另存一份。
//!
//! ## 缺了这份数据会怎样
//!
//! 什么都不影响 —— 界面上那条消息就不显示时间（老会话、fork 出去的会话在补上
//! 之前都是这个状态）。时间纯粹是给人看的，不值得为了它让界面报错。
//!
//! ## 汇报的归属也挂在这里
//!
//! 助理对话里的汇报要知道「说的是哪张工单、那张工单的对话是哪条」。这件事**不能**
//! 从正文里读：正文是给模型看的文字，标题怎么写随时会改，也可能被外部内容带偏。
//! 同样不能写进会话文件（理由同上）。它与时刻一样是**轮**的属性、生命周期也完全
//! 一样（重试回退、压缩重排、分叉复制、开新一期清空），所以跟着时刻一起存。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/// 某一轮的时刻（IPC 用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnTime {
    pub turn: u32,
    /// 用户发问的时刻（epoch 毫秒）。
    pub started_at: u64,
    /// 这一轮跑完的时刻。`None` = 还在跑（或没跑完就被中断了）。
    pub ended_at: Option<u64>,
    /// 这一轮是助理记进来的一条工单消息时，它属于哪张工单。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<ReportTag>,
}

/// 一条汇报属于哪张工单 —— 记的时候由后端按工单本身填，不从正文里解析。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportTag {
    /// 工单 id（`assistant.json` 里那张）。
    pub ticket_id: String,
    /// 记这条时工单的执行会话。还没开工的工单没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 这条是在**等用户回答**（汇报到了要人处理的状态），而不是事后记的一笔
    /// （已答复、已结束、已忽略、工单来信）。界面只给前者回答框。
    #[serde(default)]
    pub ask: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    /// 轮次 → 时刻。`BTreeMap` 让落盘顺序稳定（人看得懂，diff 也不会跳）。
    #[serde(default)]
    turns: BTreeMap<u32, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report: Option<ReportTag>,
}

/// 会话 id → 文件名。与 `changes.rs::file_for` 同一条白名单：拼路径的地方不信任输入。
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
    paths::timeline_dir().join(format!("{safe}.json"))
}

/// 读一条会话的全部时刻。文件不存在或坏了就是空。
pub fn load(session_id: &str) -> Vec<TurnTime> {
    load_from(&file_for(session_id))
}

fn load_from(path: &Path) -> Vec<TurnTime> {
    read(path)
        .turns
        .into_iter()
        .map(|(turn, e)| TurnTime {
            turn,
            started_at: e.started_at,
            ended_at: e.ended_at,
            report: e.report,
        })
        .collect()
}

/// 记下某一轮**开始**的时刻。
///
/// 同一轮重跑（重试）时以最后一次为准：用户看到的是「刚才又问了一遍」，显示的
/// 该是这一遍的时间，不是上一遍的。结束时刻一并清掉 —— 上一遍的「答完」不属于这一遍。
pub fn started(session_id: &str, turn: u32, at: u64) -> Result<(), String> {
    started_tagged(session_id, turn, at, None)
}

/// 与 [`started`] 一样，同时记下这一轮属于哪张工单（助理对话里记进来的汇报）。
pub fn started_tagged(
    session_id: &str,
    turn: u32,
    at: u64,
    report: Option<ReportTag>,
) -> Result<(), String> {
    if ephemeral(session_id) {
        return Ok(());
    }
    started_at(&file_for(session_id), turn, at, report)
}

fn started_at(path: &Path, turn: u32, at: u64, report: Option<ReportTag>) -> Result<(), String> {
    let mut stored = read(path);
    stored.turns.insert(
        turn,
        Entry {
            started_at: at,
            ended_at: None,
            report,
        },
    );
    write(path, &stored)
}

/// 记下某一轮**跑完**的时刻。
///
/// 没有这一轮的开始记录时补一条：正常情况下不会发生（`started` 总在前面），但
/// 中途挂掉之后重建出来的会话可能只赶上收尾，那种时候宁可显示一个时间，也不要
/// 让整轮没有时间。
pub fn ended(session_id: &str, turn: u32, at: u64) -> Result<(), String> {
    if ephemeral(session_id) {
        return Ok(());
    }
    ended_at(&file_for(session_id), turn, at)
}

/// 侧边聊天（`/btw`）不落盘：它关掉即丢、不会被重新打开，界面上的时间只靠事件就够。
/// 给它写文件只会在目录里留下一堆永远没人读、也没人删的 `side-…` 文件。
fn ephemeral(session_id: &str) -> bool {
    session_id.starts_with(crate::side::SIDE_PREFIX)
}

fn ended_at(path: &Path, turn: u32, at: u64) -> Result<(), String> {
    let mut stored = read(path);
    let entry = stored.turns.entry(turn).or_insert(Entry {
        started_at: at,
        ended_at: None,
        report: None,
    });
    entry.ended_at = Some(at);
    write(path, &stored)
}

/// 第 `turn` 轮及之后都丢掉（重试把后面的对话回退掉了，时刻也跟着一起退）。
pub fn retract(session_id: &str, turn: u32) -> Result<(), String> {
    retract_from(&file_for(session_id), turn)
}

fn retract_from(path: &Path, turn: u32) -> Result<(), String> {
    let mut stored = read(path);
    if stored.turns.split_off(&turn).is_empty() {
        return Ok(());
    }
    write(path, &stored)
}

/// 压缩之后按新轮次号重排（见 `history::TurnRemap`）。被摘要吃掉的那几轮直接丢掉。
pub fn remap(session_id: &str, f: impl Fn(u32) -> Option<u32>) -> Result<(), String> {
    remap_at(&file_for(session_id), f)
}

fn remap_at(path: &Path, f: impl Fn(u32) -> Option<u32>) -> Result<(), String> {
    let stored = read(path);
    if stored.turns.is_empty() {
        return Ok(());
    }
    let turns = stored
        .turns
        .into_iter()
        .filter_map(|(turn, e)| f(turn).map(|t| (t, e)))
        .collect();
    write(path, &Stored { turns })
}

/// 按「第几轮」重排一次，修正历史遗留的轮次号错位。
///
/// 曾经有一版 `run_one_turn` 把「含这一轮」的个数当成了这一轮的号（少减一），
/// 于是第 0 轮之后每轮的键都大了一号：界面拿消息上的号去查时刻查不到，汇报卡
/// 上挂的工单归属（`ReportTag`）也跟着落到不存在的轮次上 —— 卡片就没有了
/// 「打开工单对话」和回答框。记录写下去之后不会再自己对齐，只能在这里修。
///
/// 只有在**条目数恰好等于这一期的用户轮数**时才动：时刻是一条一轮、按轮次升序
/// 存的，条目数对得上就说明一条不缺，顺序重排 0..n 就是对的。对不上（时间线功能
/// 上线前的老会话、压缩吃掉过轮次的会话）说明缺的那一轮说不清，宁可不碰 ——
/// 排错比不排更糟。
pub fn realign(session_id: &str, turns: usize) -> Result<(), String> {
    if ephemeral(session_id) {
        return Ok(());
    }
    realign_at(&file_for(session_id), turns)
}

fn realign_at(path: &Path, turns: usize) -> Result<(), String> {
    let stored = read(path);
    if stored.turns.len() != turns || stored.turns.keys().copied().eq(0..turns as u32) {
        return Ok(());
    }
    let turns = stored
        .turns
        .into_values()
        .enumerate()
        .map(|(i, e)| (i as u32, e))
        .collect();
    write(path, &Stored { turns })
}

/// fork 出来的新会话继承时刻。`keep_before` = 只留轮次号小于它的那些
/// （分叉点之后的对话已经不在这条新会话里了）。
pub fn copy(from: &str, to: &str, keep_before: Option<u32>) -> Result<(), String> {
    copy_between(&file_for(from), &file_for(to), keep_before)
}

fn copy_between(from: &Path, to: &Path, keep_before: Option<u32>) -> Result<(), String> {
    let mut stored = read(from);
    if let Some(cut) = keep_before {
        stored.turns.split_off(&cut);
    }
    if stored.turns.is_empty() {
        return Ok(());
    }
    write(to, &stored)
}

/// 会话被删时一并清掉，否则这个目录会跟着历史会话一直长。
pub fn remove(session_id: &str) -> bool {
    std::fs::remove_file(file_for(session_id)).is_ok()
}

fn read(path: &Path) -> Stored {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Stored>(&raw).ok())
        .unwrap_or_default()
}

fn write(path: &Path, stored: &Stored) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let json = serde_json::to_string(stored).map_err(|e| e.to_string())?;
    // 先写临时文件再改名：中途断电不能留下半截 JSON（与 `goal.rs::save_to` 同理）。
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("改名到 {} 失败：{e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "carter-timeline-{tag}-{}-{}",
            std::process::id(),
            crate::protocol::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 开始 / 结束 / 读回；同一轮重跑时开始时刻覆盖、结束时刻清掉。
    #[test]
    fn records_start_and_end_per_turn() {
        let dir = tmp_dir("rt");
        let path = dir.join("s.json");
        started_at(&path, 0, 1000, None).unwrap();
        ended_at(&path, 0, 1500).unwrap();
        started_at(&path, 1, 2000, None).unwrap();
        assert_eq!(
            load_from(&path),
            vec![
                TurnTime {
                    turn: 0,
                    started_at: 1000,
                    ended_at: Some(1500),
                    report: None,
                },
                TurnTime {
                    turn: 1,
                    started_at: 2000,
                    ended_at: None,
                    report: None,
                },
            ]
        );
        // 重跑第 0 轮：开始时刻换新，上一遍的结束时刻不该留着。
        started_at(&path, 0, 3000, None).unwrap();
        assert_eq!(
            load_from(&path)[0],
            TurnTime {
                turn: 0,
                started_at: 3000,
                ended_at: None,
                report: None,
            }
        );
        // 坏文件按「没有记录」处理。
        std::fs::write(&path, "{nope").unwrap();
        assert!(load_from(&path).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 回退：第 N 轮及之后丢掉，之前的留着。
    #[test]
    fn retracting_a_turn_drops_it_and_everything_after() {
        let dir = tmp_dir("retract");
        let path = dir.join("s.json");
        for t in 0..4 {
            started_at(&path, t, u64::from(t) * 100, None).unwrap();
        }
        retract_from(&path, 2).unwrap();
        let turns: Vec<u32> = load_from(&path).iter().map(|t| t.turn).collect();
        assert_eq!(turns, vec![0, 1]);
        // 没有文件时什么都不做，不报错。
        assert!(retract_from(&dir.join("nope.json"), 1).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 压缩重排：被吃掉的轮次丢掉，留下来的换成新号。
    #[test]
    fn remapping_follows_the_new_turn_numbers() {
        let dir = tmp_dir("remap");
        let path = dir.join("s.json");
        for t in 0..3 {
            started_at(&path, t, u64::from(t) * 100, None).unwrap();
        }
        // 第 1 轮被摘要吃掉，第 2 轮变成第 1 轮。
        remap_at(&path, |t| match t {
            0 => Some(0),
            2 => Some(1),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            load_from(&path),
            vec![
                TurnTime {
                    turn: 0,
                    started_at: 0,
                    ended_at: None,
                    report: None,
                },
                TurnTime {
                    turn: 1,
                    started_at: 200,
                    ended_at: None,
                    report: None,
                },
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 分叉继承时刻，但只继承切点之前的。
    #[test]
    fn copying_keeps_only_turns_before_the_cut() {
        let dir = tmp_dir("copy");
        let from = dir.join("a.json");
        let to = dir.join("b.json");
        for t in 0..4 {
            started_at(&from, t, u64::from(t) * 100, None).unwrap();
        }
        copy_between(&from, &to, Some(2)).unwrap();
        let turns: Vec<u32> = load_from(&to).iter().map(|t| t.turn).collect();
        assert_eq!(turns, vec![0, 1]);

        // 不带切点 = 整份复制；源文件没有时也不报错、不建空文件。
        copy_between(&from, &to, None).unwrap();
        assert_eq!(load_from(&to).len(), 4);
        let empty = dir.join("c.json");
        copy_between(&dir.join("nope.json"), &empty, None).unwrap();
        assert!(!empty.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 轮次号整体错了一号（老版本 `run_one_turn` 少减一）时按顺序扶正；对得上、
    /// 或条目数与轮数对不上的，一律不动。
    #[test]
    fn realigning_fixes_the_off_by_one_keys() {
        let dir = tmp_dir("realign");
        let path = dir.join("s.json");
        let tag = ReportTag {
            ticket_id: "ticket_1".into(),
            session_id: None,
            ask: true,
        };
        // 这一期两轮：0 号正常，第 1 轮被记成了 2。
        started_at(&path, 0, 100, None).unwrap();
        started_at(&path, 2, 200, Some(tag.clone())).unwrap();
        realign_at(&path, 2).unwrap();
        let got = load_from(&path);
        assert_eq!(got.iter().map(|t| t.turn).collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(
            got[1].report.as_ref(),
            Some(&tag),
            "归属要跟着这一轮一起挪过去"
        );

        // 已经对得上：不重写文件（时刻原样）。
        let before = std::fs::read_to_string(&path).unwrap();
        realign_at(&path, 2).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // 条目数与轮数对不上：缺的是哪一轮说不清，不动。
        realign_at(&path, 3).unwrap();
        assert_eq!(load_from(&path)[1].turn, 1);

        // 没有文件时什么都不做，不报错、不建文件。
        let none = dir.join("nope.json");
        assert!(realign_at(&none, 1).is_ok());
        assert!(!none.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 汇报的归属跟着这一轮一起存、一起读回；同一轮重跑（重试）时以新的为准。
    #[test]
    fn a_report_tag_rides_along_with_its_turn() {
        let dir = tmp_dir("tag");
        let path = dir.join("s.json");
        let tag = ReportTag {
            ticket_id: "ticket_1".into(),
            session_id: Some("1-abcd-w".into()),
            ask: true,
        };
        started_at(&path, 0, 100, None).unwrap();
        started_at(&path, 1, 200, Some(tag.clone())).unwrap();
        ended_at(&path, 1, 250).unwrap();
        let got = load_from(&path);
        assert_eq!(got[0].report, None);
        assert_eq!(got[1].report.as_ref(), Some(&tag));
        assert_eq!(got[1].ended_at, Some(250), "收尾不能把归属冲掉");

        // 分叉、重排都带着它走。
        let to = dir.join("b.json");
        copy_between(&path, &to, None).unwrap();
        assert_eq!(load_from(&to)[1].report.as_ref(), Some(&tag));
        remap_at(&path, |t| (t == 1).then_some(0)).unwrap();
        assert_eq!(load_from(&path)[0].report.as_ref(), Some(&tag));
        std::fs::remove_dir_all(&dir).ok();
    }
}
