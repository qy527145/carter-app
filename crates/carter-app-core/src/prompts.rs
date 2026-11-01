//! 用户**打的原文**（`/命令`、`@文件` 展开之前）—— 只服务界面，一个会话一个文件。
//!
//! ## 为什么要单独存
//!
//! 会话文件里是**展开之后**的正文：`@src/main.rs` 变成一个几百行的 `--- src/main.rs ---`
//! 块，`/review` 变成整段命令正文。那份东西回填进「编辑」框等于把整份文件摊在输入
//! 框里，而用户要改的明明是「我刚才问的那句话」。
//!
//! 也不能写进 carter 的会话文件：`RecordKind` 是封闭枚举，多一种记录类型会让整条
//! 会话**打不开**（见 `goal.rs` 模块头的同一段说明）。所以按会话 id 另存一份。
//!
//! ## 缺了这份数据会怎样
//!
//! 什么都不影响 —— 它只是「编辑框的初值」。以前录下的会话没有这份文件，界面
//! 回填时退回历史里那段展开后的正文（见 `store.ts::historyToTurns`）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/// 某一轮用户输入的原文（IPC 用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnPrompt {
    pub turn: u32,
    pub text: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    /// 轮次 → 原文。`BTreeMap` 让落盘顺序稳定（人看得懂，diff 也不会跳）。
    #[serde(default)]
    turns: BTreeMap<u32, String>,
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
    paths::prompts_dir().join(format!("{safe}.json"))
}

/// 读一条会话的全部原文。文件不存在或坏了就是空。
pub fn load(session_id: &str) -> Vec<TurnPrompt> {
    load_from(&file_for(session_id))
}

fn load_from(path: &Path) -> Vec<TurnPrompt> {
    read(path)
        .turns
        .into_iter()
        .map(|(turn, text)| TurnPrompt { turn, text })
        .collect()
}

/// 记下某一轮的原文。同一轮重写时以最后一次为准（重试会重跑同一轮）。
pub fn record(session_id: &str, turn: u32, text: &str) -> Result<(), String> {
    record_to(&file_for(session_id), turn, text)
}

fn record_to(path: &Path, turn: u32, text: &str) -> Result<(), String> {
    let mut stored = read(path);
    stored.turns.insert(turn, text.to_string());
    write(path, &stored)
}

/// 第 `turn` 轮及之后都丢掉（重试把后面的对话回退掉了，原文也跟着一起退）。
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

/// fork 出来的新会话继承原文。`keep_before` = 只留轮次号小于它的那些
/// （分叉点之后的提问已经不在这条对话里了）。
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
            "carter-prompts-{tag}-{}-{}",
            std::process::id(),
            crate::protocol::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 记录 / 读回；同一轮重写以最后一次为准。
    #[test]
    fn records_roundtrip_per_turn() {
        let dir = tmp_dir("rt");
        let path = dir.join("s.json");
        record_to(&path, 0, "@src/main.rs 看看").unwrap();
        record_to(&path, 2, "/review").unwrap();
        record_to(&path, 2, "/review 再看一遍").unwrap();
        assert_eq!(
            load_from(&path),
            vec![
                TurnPrompt {
                    turn: 0,
                    text: "@src/main.rs 看看".into()
                },
                TurnPrompt {
                    turn: 2,
                    text: "/review 再看一遍".into()
                },
            ]
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
            record_to(&path, t, &format!("第{t}问")).unwrap();
        }
        retract_from(&path, 2).unwrap();
        let turns: Vec<u32> = load_from(&path).iter().map(|p| p.turn).collect();
        assert_eq!(turns, vec![0, 1]);
        // 没有文件时什么都不做，不报错。
        assert!(retract_from(&dir.join("nope.json"), 1).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 分叉继承原文，但只继承切点之前的 —— 切点之后那句已经不在新对话里了。
    #[test]
    fn copying_keeps_only_prompts_before_the_cut() {
        let dir = tmp_dir("copy");
        let from = dir.join("a.json");
        let to = dir.join("b.json");
        for t in 0..4 {
            record_to(&from, t, &format!("第{t}问")).unwrap();
        }
        copy_between(&from, &to, Some(2)).unwrap();
        let turns: Vec<u32> = load_from(&to).iter().map(|p| p.turn).collect();
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