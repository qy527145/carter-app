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
    /// 这条消息属于**哪一轮用户输入**（0 起的序号），没有则为 `None`。
    ///
    /// 界面拿它把一条历史回答对回「产生它的那次请求」—— 报文面板按轮筛选，
    /// 点某条回答上的「查看报文」就筛到这一轮。实时的那一轮由
    /// `SessionEvent::TurnStarted` 带着同样的序号，两条路算出来的数必须一致。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
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
    let mut turn: Option<u32> = None;
    let mut next = 0;
    messages
        .iter()
        .map(|m| {
            if is_turn_start(m) {
                turn = Some(next);
                next += 1;
            }
            message_view(m, turn)
        })
        .collect()
}

/// 历史重放时，用户轮序号从零重数（`Thread::from_parts` 就是这么做的）。
/// 续接一条已有会话时，新的轮次要接在历史后面 —— 否则新一轮会拿到 0 号，
/// 与历史里那一轮撞号，报文面板把一个筛选条件套到两条不相干的请求上。
pub fn user_turn_count(messages: &[WireMessage]) -> u32 {
    messages.iter().filter(|m| is_turn_start(m)).count() as u32
}

/// 这条消息是不是「一轮用户输入」的开头。
///
/// 判据是**带正文的 user 消息**：
/// * 工具结果是 user 消息，但它是 `ToolResult` 块，不带正文 —— 不算新的一轮；
/// * carter 的待办复诵也是 user 正文，但它是**请求时临时注入的**，从不进历史
///   （`agent/turn.rs`：clone 历史 + push 复诵），所以这里不必特地排除它。
///
/// 唯一的例外是报文面板解出来的**请求体**：复诵确实在那份 messages 里，会被数成
/// 新的一轮。那不影响界面 —— 那一页渲染的是内容，不显示轮次号；按轮筛选用的是
/// 抓包记录里 carter 自己记的 `meta.turn`。真要在那一页显示轮次号的话，
/// 得先解决这个偏差。
///
/// 公开给 `history.rs`：重试要把历史回退到某一轮之前，靠同一个判据数轮次 ——
/// 两边用两套判据的话，界面上的「第 N 轮」与回退点迟早对不上。
pub(crate) fn is_turn_start(msg: &WireMessage) -> bool {
    msg.role == Role::User
        && msg.content.iter().any(|b| match b {
            Block::Text { text, .. } => !text.trim().is_empty(),
            _ => false,
        })
}

fn message_view(msg: &WireMessage, turn: Option<u32>) -> MessageView {
    MessageView {
        role: role_str(msg.role).to_string(),
        blocks: msg.content.iter().filter_map(block_view).collect(),
        // 系统消息不属于任何一轮。
        turn: (msg.role != Role::System).then_some(turn).flatten(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use llm_adapt_core::ToolResultContent;

    fn user(text: &str) -> WireMessage {
        WireMessage::user_text(text)
    }

    fn assistant(text: &str) -> WireMessage {
        WireMessage::new(Role::Assistant, vec![Block::text(text)])
    }

    /// 工具结果也是 user 消息，但它不是「一轮的开始」。
    fn tool_results() -> WireMessage {
        WireMessage::new(
            Role::User,
            vec![Block::ToolResult {
                tool_use_id: "t1".into(),
                content: ToolResultContent::Text { text: "ok".into() },
                is_error: false,
                cache: None,
            }],
        )
    }

    /// 轮次号要跟 carter 的 `user_turns` 对上：一次用户输入算一轮，
    /// 工具回灌的那几条 user 消息不算 —— 算错的话界面上的「查看报文」会指到别的请求上。
    #[test]
    fn turns_count_user_inputs_not_tool_results() {
        let views = to_views(&[
            user("第一轮"),
            assistant("看看"),
            tool_results(),
            assistant("好了"),
            user("第二轮"),
            assistant("嗯"),
        ]);

        assert_eq!(views[0].turn, Some(0)); // 用户输入本身
        assert_eq!(views[1].turn, Some(0)); // 同一轮里的回答
        assert_eq!(views[2].turn, Some(0)); // 工具结果：同属第 0 轮，不是新的一轮
        assert_eq!(views[3].turn, Some(0));
        assert_eq!(views[4].turn, Some(1)); // 第二轮
        assert_eq!(views[5].turn, Some(1));
        assert_eq!(user_turn_count(&[user("a"), tool_results(), user("b")]), 2);
    }

    /// 系统消息不属于任何一轮 —— 它可能出现在历史开头，不该把第一轮的号推走。
    #[test]
    fn system_messages_have_no_turn() {
        let msg = WireMessage::new(Role::System, vec![Block::text("人设")]);
        let views = to_views(&[msg, user("问题")]);
        assert_eq!(views[0].turn, None);
        assert_eq!(views[1].turn, Some(0));
    }

    /// 空白的用户消息（压缩摘要之类）不算一轮。
    #[test]
    fn a_blank_user_message_does_not_open_a_turn() {
        let views = to_views(&[user("   "), user("真的有人说话")]);
        assert_eq!(views[0].turn, None);
        assert_eq!(views[1].turn, Some(0));
    }
}
