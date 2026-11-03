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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/// 某一轮的时刻（IPC 用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnTime {
    pub turn: u32,
    /// 用户发问的时刻（epoch 毫秒）。
    pub started_at: u64,
    /// 这一轮跑完的时刻。`None` = 还在跑（或没跑完就被中断了）。
    pub ended_at: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    /// 轮次 → 时刻。`BTreeMap` 让落盘顺序稳定（人看得懂，diff 也不会跳）。
    #[serde(default)]
    turns: BTreeMap<u32, Entry>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Entry {
    started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ended_at: Option<u64>,
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
        })
        .collect()
}

/// 记下某一轮**开始**的时刻。
///
/// 同一轮重跑（重试）时以最后一次为准：用户看到的是「刚才又问了一遍」，显示的
/// 该是这一遍的时间，不是上一遍的。结束时刻一并清掉 —— 上一遍的「答完」不属于这一遍。
pub fn started(session_id: &str, turn: u32, at: u64) -> Result<(), String> {
    if ephemeral(session_id) {
        return Ok(());
    }
    started_at(&file_for(session_id), turn, at)
}

fn started_at(path: &Path, turn: u32, at: u64) -> Result<(), String> {
    let mut stored = read(path);
    stored.turns.insert(
        turn,
        Entry {
            started_at: at,
            ended_at: None,
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
        started_at(&path, 0, 1000).unwrap();
        ended_at(&path, 0, 1500).unwrap();
        started_at(&path, 1, 2000).unwrap();
        assert_eq!(
            load_from(&path),
            vec![
                TurnTime {
                    turn: 0,
                    started_at: 1000,
                    ended_at: Some(1500),
                },
                TurnTime {
                    turn: 1,
                    started_at: 2000,
                    ended_at: None,
                },
            ]
        );
        // 重跑第 0 轮：开始时刻换新，上一遍的结束时刻不该留着。
        started_at(&path, 0, 3000).unwrap();
        assert_eq!(
            load_from(&path)[0],
            TurnTime {
                turn: 0,
                started_at: 3000,
                ended_at: None,
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
            started_at(&path, t, u64::from(t) * 100).unwrap();
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
            started_at(&path, t, u64::from(t) * 100).unwrap();
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
                },
                TurnTime {
                    turn: 1,
                    started_at: 200,
                    ended_at: None,
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
            started_at(&from, t, u64::from(t) * 100).unwrap();
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
}
