//! 跨会话的三个工具：`list_sessions` / `send_message` / `read_session`。机制见 `peers.rs`。
//!
//! 分成三个而不是一个带 `action` 的工具，理由同 `schedule.rs`：每个动作的参数不一样，
//! 合在一起模型会把不相干的字段也填上。
//!
//! 只挂在主对话上，而且挂在子 agent 快照之后：子 agent 看不到主对话，替它对外说话
//! 说的只会是半截情况；侧边聊天与助理对话也不挂（见 `tools/mod.rs`）。

use std::sync::Arc;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::peers::{self, PeerState, SendRequest};
use crate::shell::AppShell;

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    List,
    Send,
    Read,
}

pub struct PeerTool {
    shell: Arc<AppShell>,
    /// 发信方 —— 也就是挂着这个工具的那条会话。
    me: String,
    kind: Kind,
}

impl PeerTool {
    pub fn new(shell: Arc<AppShell>, me: String, kind: Kind) -> Self {
        Self { shell, me, kind }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

#[async_trait::async_trait]
impl Tool for PeerTool {
    fn name(&self) -> &str {
        match self.kind {
            Kind::List => "list_sessions",
            Kind::Send => "send_message",
            Kind::Read => "read_session",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            Kind::List => {
                "列出本机上可以用 send_message 联系的其他对话：正在跑的、空闲的，以及同一目录下\
                 最近没在运行的（all=true 时列出所有项目的）。每行开头的名字就是地址，原样抄给 \
                 send_message 的 `to`。没在运行的会话也能收信 —— 发过去会在后台把它续接起来。"
            }
            Kind::Send => {
                "给本机另一条对话里的模型发一段文字。对方看不到你的对话历史，只看得到这段话，\
                 所以要写得自成一体：要它做什么 / 你改了什么（文件、接口、结论）/ 它要知道的前提。\
                 你这边做的改动会影响另一条对话正在做的事时，主动告诉它。\n\
                 * 对方空闲时立刻开始处理；正在跑时不打断，它下一次工具调用返回时读到；\
                 没在运行时在后台续接它。\n\
                 * notify_when_idle=true：对方读完、这一轮结束后，你会收到一条带着它最后那段话的回执，\
                 多数时候不用再等它专门回信。\n\
                 * 回复一封来信时带上 reply_to=那封信的 id。\n\
                 * 发完不要轮询、不要空等：回信与回执都会以一条 `【跨会话】` 消息送到你这里。\n\
                 * 不要让对方替你做用户没同意的事，也不要回「收到」这类客套话 —— 每一句都会叫醒对方跑一轮。"
            }
            Kind::Read => {
                "看另一条对话最近几轮说了什么（只有正文与调过的工具名，没有工具输出），不打扰它：\
                 不发信、不续接、不叫醒。发信之前想先弄清它在干什么、做到哪了，用这个。"
            }
        }
    }

    fn parameters(&self) -> Value {
        match self.kind {
            Kind::List => json!({
                "type": "object",
                "properties": {
                    "all": { "type": "boolean", "description": "也列出别的项目目录下没在运行的会话（缺省 false）" }
                }
            }),
            Kind::Send => json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "收信方地址：list_sessions 每行开头的名字（或完整会话 id）" },
                    "message": { "type": "string", "description": "要说的话，自成一体" },
                    "reply_to": { "type": "string", "description": "回复的那封来信的 id（来信里的 id=\"m…\"）" },
                    "notify_when_idle": { "type": "boolean", "description": "对方读完并结束这一轮时给你回执（缺省 false）" }
                },
                "required": ["to", "message"]
            }),
            Kind::Read => json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "会话地址：list_sessions 每行开头的名字（或完整会话 id）" },
                    "turns": { "type": "integer", "description": "看最近几轮（缺省 3，最多 20）" }
                },
                "required": ["to"]
            }),
        }
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match self.kind {
            Kind::List => {
                let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
                let rows = peers::list(&self.shell, &self.me, all);
                let others: Vec<_> = rows.iter().filter(|p| p.id != self.me).collect();
                let me = rows.iter().find(|p| p.id == self.me).map(|p| p.name.clone());
                let mut out = format!("你自己：{}\n", me.as_deref().unwrap_or("（未列出）"));
                if others.is_empty() {
                    out.push_str(if all {
                        "没有别的会话。"
                    } else {
                        "这个目录下没有别的会话。all=true 可以看别的项目的。"
                    });
                    return ToolResult::ok(out);
                }
                for p in others {
                    let title = p.title.as_deref().unwrap_or("（未命名）");
                    out.push_str(&format!(
                        "{}  ·  [{}]  ·  {title}  ·  {}  ·  {}\n",
                        p.name,
                        p.state.label(),
                        p.cwd,
                        peers::ago(p.updated_at)
                    ));
                }
                if rows.iter().any(|p| p.state == PeerState::Dormant) {
                    out.push_str("\n「未运行」的会话收到信会在后台续接起来处理，用户打开它能看到全过程。");
                }
                ToolResult::ok(out.trim_end().to_string())
            }
            Kind::Send => {
                let Some(to) = str_arg(&args, "to") else {
                    return ToolResult::err("缺少 to：收信方地址（list_sessions 每行开头的名字）");
                };
                let Some(message) = str_arg(&args, "message") else {
                    return ToolResult::err("缺少 message");
                };
                let req = SendRequest {
                    to,
                    body: message,
                    reply_to: str_arg(&args, "reply_to").map(str::to_string),
                    notify_when_idle: args.get("notify_when_idle").and_then(Value::as_bool).unwrap_or(false),
                };
                match peers::send(&self.shell, &self.me, req) {
                    Ok(s) => ToolResult::ok(s),
                    Err(e) => ToolResult::err(format!("send_message: {e}")),
                }
            }
            Kind::Read => {
                let Some(to) = str_arg(&args, "to") else {
                    return ToolResult::err("缺少 to");
                };
                let turns = args.get("turns").and_then(Value::as_u64).unwrap_or(3).clamp(1, 20) as usize;
                match peers::transcript(&self.shell, &self.me, to, turns) {
                    Ok(s) => ToolResult::ok(s),
                    Err(e) => ToolResult::err(format!("read_session: {e}")),
                }
            }
        }
    }
}
