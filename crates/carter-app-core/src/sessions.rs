//! 对话管理：列举、重命名、fork、删除、搜索、导出。
//!
//! carter 提供了 `session::{list, load, fork}` 与 `Recorder` / `RecordKind`，
//! 但**没有** rename / delete / search / export —— 那些由这里补。
//! 补的方式是复用 carter 的读写原语，而不是自己重写一遍文件格式：
//!
//! * 重命名 = 往会话文件追加一条 `RecordKind::Title`，与自动标题走的是同一条记录。
//! * fork = 直接调 `carter::session::fork`，它会把种子历史写成一个**自包含**的新文件
//!   并带上 `forked_from` 血缘，我们只负责把新 id 报出去。
//! * 删除 = 删文件（调用方要先关掉会话任务，见 src-tauri 的命令层）。

use std::path::Path;

use serde::Serialize;

use carter::provider::{Block, WireMessage};
use carter::session::{Record, RecordKind, Recorder, SessionEntry};

/// 会话列表项。字段与 `SessionMeta` 对齐，另加两个界面用得上的。
#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub model: String,
    pub cwd: String,
    /// epoch 毫秒。
    pub created_at: u64,
    /// 从哪条会话 fork 来的。
    pub forked_from: Option<String>,
    /// 文件字节数。界面上用来判断「这条会话是不是已经很大了」。
    pub bytes: u64,
}

fn row(entry: &SessionEntry) -> SessionRow {
    SessionRow {
        id: entry.meta.id.clone(),
        title: entry.meta.title.clone(),
        model: entry.meta.model.clone(),
        cwd: entry.meta.cwd.clone(),
        created_at: entry.meta.created_at,
        forked_from: entry.meta.forked_from.clone(),
        bytes: std::fs::metadata(&entry.path).map(|m| m.len()).unwrap_or(0),
    }
}

/// 列举会话。`all = false` 只列当前目录，`true` 跨所有项目。
pub fn list(cwd: &Path, all: bool) -> Vec<SessionRow> {
    carter::session::list(cwd, all).iter().map(row).collect()
}

/// 按 id 找会话，**跨所有项目**。
///
/// 刻意不接受 cwd：会话 id 是 uuid，全局唯一，而界面上能点到的会话就应该能操作 ——
/// 包括那些属于别的目录的。让调用方传 cwd 只会引入「传错了目录所以找不到」这类
/// 沉默的失败。
fn find(id: &str) -> Option<SessionEntry> {
    // `all = true` 时 cwd 不参与，随便给一个。
    carter::session::list(Path::new("."), true)
        .into_iter()
        .find(|e| e.meta.id == id)
}

/// 给会话改标题。
///
/// 走「追加一条 Title 记录」而不是原地改文件头：会话文件是**追加写**的，
/// 原地改要先读全量再重写，中途失败就是一个半截文件。追加一条记录则是原子的，
/// 而且 `fold_file` 本来就按「后出现的 Title 覆盖先前的」来折叠。
pub fn rename(id: &str, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("标题不能为空".into());
    }
    let entry = find(id).ok_or_else(|| format!("找不到会话 {id}"))?;
    // 文件还没建出来 = 这条会话一条消息都没有。`Recorder::open` 会把文件建出来，
    // 那样会得到一个只有 Title、没有 SessionMeta 的文件，fold 时读不出 meta，
    // 会话反而从列表里消失。
    if !entry.path.exists() {
        return Err("这条会话还没有任何消息，不需要命名".into());
    }
    let rec = Recorder::open(&entry.path).map_err(|e| format!("打开会话文件失败：{e}"))?;
    rec.record(RecordKind::Title {
        title: title.to_string(),
    });
    Ok(())
}

/// fork 一条会话。返回新会话的 id。
pub fn fork(id: &str) -> Result<String, String> {
    let entry = find(id).ok_or_else(|| format!("找不到会话 {id}"))?;
    if !entry.path.exists() {
        return Err("这条会话还没有任何消息，无法 fork".into());
    }
    let (_, meta) = carter::session::fork(&entry).map_err(|e| format!("fork 失败：{e}"))?;
    Ok(meta.id)
}

/// 删除会话文件。调用方要保证那会话的后台任务已经关掉 ——
/// 任务持有同一个文件的 `Recorder`，不先关掉的话下一轮 append 会把文件重新写出来
/// （表现为「删了又回来了」）。
pub fn delete(id: &str) -> Result<(), String> {
    let entry = find(id).ok_or_else(|| format!("找不到会话 {id}"))?;
    match std::fs::remove_file(&entry.path) {
        Ok(()) => Ok(()),
        // 文件本来就没有（会话建成后一条消息都没发）—— 目标达成，不算错。
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("删除失败：{e}")),
    }
}

// ---------------------------------------------------------------------------
// 搜索
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub id: String,
    pub title: Option<String>,
    pub model: String,
    pub created_at: u64,
    /// 命中的那一小段上下文。
    pub snippet: String,
    /// 这条会话里一共命中多少次。让「哪条会话更相关」有个依据。
    pub matches: usize,
}

/// 把一条消息里的可读文本拼出来。
///
/// 只取 `Text` 块：思考块是模型的草稿，工具结果通常是大段无关输出 ——
/// 拿它们去匹配只会让搜索变成噪声源。
fn message_text(msg: &WireMessage) -> String {
    msg.content
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 会话文件里能参与匹配的那些文本（按记录顺序）。
fn searchable(entry: &SessionEntry) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(&entry.path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let Ok(rec) = serde_json::from_str::<Record>(line) else {
            continue; // 坏行不该让整条搜索失败
        };
        match rec.kind {
            RecordKind::Message(m) => out.push(message_text(&m)),
            RecordKind::Compacted { messages, .. } => {
                for m in &messages {
                    out.push(message_text(m));
                }
            }
            RecordKind::Title { title } => out.push(title),
            _ => {}
        }
    }
    out
}

/// 拿命中位置附近的一小段做摘要。
///
/// 按**字节**切会切碎中文（一个汉字三字节），所以先按 char 定位再切。
fn snippet_around(text: &str, needle_lower: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let hit = lower.find(needle_lower)?;
    let chars: Vec<char> = text.chars().collect();
    // 命中的字节偏移要换算成 char 下标，否则中英文混排时位置会错。
    let char_idx = text[..hit].chars().count();
    let start = char_idx.saturating_sub(30);
    let end = (char_idx + 70).min(chars.len());
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.extend(&chars[start..end]);
    if end < chars.len() {
        s.push('…');
    }
    Some(s.trim().replace('\n', " "))
}

/// 在所有会话里全文搜索。
///
/// 目前是「每条会话读一遍文件」的朴素实现。会话文件是本地的、按会话分片，
/// 这个量级下建索引不划算 —— 真到了扫不动的时候再说。
pub fn search(query: &str) -> Vec<SearchHit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    // `all = true` 时 cwd 参数其实不参与，随便给一个。
    let mut hits: Vec<SearchHit> = Vec::new();
    for entry in carter::session::list(Path::new("."), true) {
        let texts = searchable(&entry);
        let mut matches = 0;
        let mut snippet: Option<String> = None;
        for t in &texts {
            if t.to_lowercase().contains(&q) {
                matches += 1;
                if snippet.is_none() {
                    snippet = snippet_around(t, &q);
                }
            }
        }
        if matches > 0 {
            hits.push(SearchHit {
                id: entry.meta.id.clone(),
                title: entry.meta.title.clone(),
                model: entry.meta.model.clone(),
                created_at: entry.meta.created_at,
                snippet: snippet.unwrap_or_default(),
                matches,
            });
        }
    }
    // 命中多的排前面，其次新的在前。
    hits.sort_by(|a, b| {
        b.matches
            .cmp(&a.matches)
            .then(b.created_at.cmp(&a.created_at))
    });
    hits
}

// ---------------------------------------------------------------------------
// 导出
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Json,
}

/// 把一条会话导出成可读文本。返回 (文件名, 内容)。
pub fn export(id: &str, format: ExportFormat) -> Result<(String, String), String> {
    let entry = find(id).ok_or_else(|| format!("找不到会话 {id}"))?;
    let (thread, meta) =
        carter::session::load(&entry).map_err(|e| format!("读取会话失败：{e}"))?;

    let title = meta.title.clone().unwrap_or_else(|| "未命名会话".into());
    let stem: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    match format {
        ExportFormat::Json => {
            let body = serde_json::json!({
                "meta": {
                    "id": meta.id,
                    "title": meta.title,
                    "model": meta.model,
                    "cwd": meta.cwd,
                    "created_at": meta.created_at,
                    "forked_from": meta.forked_from,
                },
                "messages": thread.messages.iter().map(message_json).collect::<Vec<_>>(),
            });
            Ok((
                format!("{stem}.json"),
                serde_json::to_string_pretty(&body).map_err(|e| e.to_string())?,
            ))
        }
        ExportFormat::Markdown => {
            let mut out = format!("# {title}\n\n");
            out.push_str(&format!(
                "- 模型：`{}`\n- 目录：`{}`\n- 会话 id：`{}`\n\n---\n\n",
                meta.model, meta.cwd, meta.id
            ));
            for msg in &thread.messages {
                let who = match msg.role {
                    carter::provider::Role::User => "用户",
                    carter::provider::Role::Assistant => "助手",
                    carter::provider::Role::System => "系统",
                };
                out.push_str(&format!("## {who}\n\n"));
                for b in &msg.content {
                    match b {
                        Block::Text { text, .. } => {
                            out.push_str(text);
                            out.push_str("\n\n");
                        }
                        Block::Thinking { text, .. } => {
                            out.push_str("<details><summary>思考</summary>\n\n");
                            out.push_str(text);
                            out.push_str("\n\n</details>\n\n");
                        }
                        Block::ToolUse { name, input, .. } => {
                            out.push_str(&format!(
                                "> 工具调用 `{name}`\n> ```json\n> {input}\n> ```\n\n"
                            ));
                        }
                        Block::ToolResult { content, .. } => {
                            out.push_str("> 工具结果\n");
                            for line in content.as_text().lines() {
                                out.push_str(&format!("> {line}\n"));
                            }
                            out.push('\n');
                        }
                        _ => {}
                    }
                }
                out.push_str("---\n\n");
            }
            Ok((format!("{stem}.md"), out))
        }
    }
}

fn message_json(msg: &WireMessage) -> serde_json::Value {
    serde_json::json!({
        "role": match msg.role {
            carter::provider::Role::User => "user",
            carter::provider::Role::Assistant => "assistant",
            carter::provider::Role::System => "system",
        },
        "blocks": msg.content.iter().filter_map(|b| match b {
            Block::Text { text, .. } => Some(serde_json::json!({"type": "text", "text": text})),
            Block::Thinking { text, .. } => Some(serde_json::json!({"type": "thinking", "text": text})),
            Block::ToolUse { id, name, input, .. } => Some(serde_json::json!({
                "type": "tool_call", "id": id, "name": name, "input": input
            })),
            Block::ToolResult { tool_use_id, content, is_error, .. } => Some(serde_json::json!({
                "type": "tool_result", "tool_use_id": tool_use_id,
                "text": content.as_text(), "is_error": is_error
            })),
            _ => None,
        }).collect::<Vec<_>>(),
    })
}

