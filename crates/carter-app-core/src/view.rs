//! 会话历史 → 前端的展示形态。
//!
//! 直接把 IR 丢给前端是不行的：`Block` 里有 `ImageSource::Base64`（一张图几百 KB
//! base64）这类东西，塞进 IPC 会让界面卡死。这里把它压成「够渲染、不背字节」的形状。
//!
//! 这个视图也是 M5 报文检视的基础 —— 那时要能把「这条消息」和「产生它的那次请求」
//! 对上号。

use serde::{Deserialize, Serialize};

use llm_adapt_core::{Block, Role, WireMessage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageView {
    /// `user` | `assistant` | `system`
    pub role: String,
    pub blocks: Vec<BlockView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlockView {
    Text {
        text: String,
    },
    Thinking {
        text: String,
        /// 有签名说明这段思考能原样回灌（Anthropic / Responses 都靠它保持推理链）。
        has_signature: bool,
    },
    ToolCall {
        id: String,
        name: String,
        /// 紧凑 JSON 字符串。不解析成对象是为了保住模型原样给的分片拼接结果。
        args: String,
    },
    ToolResult {
        tool_use_id: String,
        text: String,
        is_error: bool,
    },
    /// 厂商在服务端执行完的活动（`web_search_call`、`web_search_tool_result` ……）。
    /// 我们只呈现，不执行 —— 本地没有可跑的东西。
    ServerActivity {
        /// 原始块类型，便于界面区分图标。
        raw_type: String,
        /// 尽力取一段可读摘要；取不到就空。
        summary: String,
    },
    /// 图片只在视图里标明类型，**不传字节**。
    Image {
        media_type: String,
        /// 引用形态的图片带 hash，可用于将来按需取字节。
        hash: Option<String>,
    },
}

pub fn to_views(messages: &[WireMessage]) -> Vec<MessageView> {
    messages.iter().map(message_view).collect()
}

fn message_view(msg: &WireMessage) -> MessageView {
    MessageView {
        role: role_str(msg.role).to_string(),
        blocks: msg.content.iter().filter_map(block_view).collect(),
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn block_view(b: &Block) -> Option<BlockView> {
    Some(match b {
        Block::Text { text, .. } => BlockView::Text { text: text.clone() },
        Block::Thinking { text, signature } => BlockView::Thinking {
            text: text.clone(),
            has_signature: signature.is_some(),
        },
        Block::ToolUse {
            id, name, input, ..
        } => BlockView::ToolCall {
            id: id.clone(),
            name: name.clone(),
            args: input.to_string(),
        },
        Block::ToolResult {
            tool_use_id,
            content,
            is_error,
            ..
        } => BlockView::ToolResult {
            tool_use_id: tool_use_id.clone(),
            text: content.as_text(),
            is_error: *is_error,
        },
        Block::Image { source, .. } => BlockView::Image {
            media_type: source.media_type().unwrap_or("image/*").to_string(),
            hash: None,
        },
        Block::Unmodeled { raw } => BlockView::ServerActivity {
            raw_type: raw
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string(),
            summary: server_activity_summary(raw),
        },
        // 加密思考只有一段不透明的 blob，给界面看没有意义。
        Block::RedactedThinking { .. } => return None,
    })
}

/// 尽力从未建模块里抽一段人能看的摘要。
///
/// 服务端工具的块形状各家不同（`web_search_tool_result` 有 `content`，
/// `web_search_call` 有 `action`……），这里只做「有就用、没有就算了」的最优努力 ——
/// 抽不出不该让整条消息渲染不出来。
fn server_activity_summary(raw: &serde_json::Value) -> String {
    let ty = raw.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match ty {
        "web_search_call" => raw
            .pointer("/action/query")
            .and_then(|q| q.as_str())
            .unwrap_or("")
            .to_string(),
        "server_tool_use" => raw
            .get("input")
            .map(|i| i.to_string())
            .unwrap_or_default(),
        "web_search_tool_result" => {
            let n = raw
                .get("content")
                .and_then(|c| c.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if n == 0 {
                String::new()
            } else {
                format!("{n} 条结果")
            }
        }
        _ => String::new(),
    }
}
