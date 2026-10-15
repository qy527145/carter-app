//! 侧边聊天（`/btw`，别名 `/side`）：一个临时分叉，用来随手问几句。
//!
//! ## 它是什么
//!
//! 用户在主对话里跑着一个长任务，忽然想插一句「顺便问下，`SessionHandle` 是干嘛的」。
//! 打断主任务要付代价（丢掉正在生成的内容），而把这句话插进主对话会污染后面所有轮次
//! 的上下文。所以给一个**侧边的临时会话**：继承当前对话的历史作为参考、独立并发跑、
//! 关掉即丢。
//!
//! ## 三条硬约束
//!
//! 1. **不落盘。** `Thread` 不挂 `Recorder`，会话文件不存在，于是它也不出现在侧栏
//!    （列表是扫文件的）、不会被导出、不会被误当成一条真对话。`close_session` 之后
//!    内存释放，干干净净。
//! 2. **只在干净边界上种历史。** 见 [`truncate_to_clean`]：中途被取消的一轮尾部会挂着
//!    一个没有结果的 `tool_use`，把它喂进去，第一次请求就会被上游以
//!    「tool_use 没有对应的 tool_result」拒掉 —— 而报错的是**侧边会话**，
//!    用户完全看不出跟主对话有什么关系。
//! 3. **继承的历史只是参考。** 那里面可能有「做完这个再做那个」之类的计划，侧边会话
//!    不该接着执行 —— 由 [`SIDE_INSTRUCTIONS`] 明确写给模型。
//!
//! ## 为什么不用 `session::load` 拿父会话的历史
//!
//! 那会给父会话文件挂上一个 `Recorder`：侧边会话每 append 一条，都会写进**父会话**
//! 的文件里（历史被改写，还多出一些不属于它的工具往返）。而且会话文件是惰性建的 ——
//! 一条还没发过消息的新会话根本没有文件。所以走 `SessionHandle::history` 那份快照。

use std::path::PathBuf;
use std::sync::Arc;

use carter::agent::Thread;
use carter::config::Config;
use carter::provider::WireMessage;
use carter::registry::{fetch, ModelInfo};
use llm_adapt_core::{Block, Role};

use crate::session::{start_side, SideWiring};
use crate::shell::{AppShell, SessionHandle};
use crate::view::MessageView;

/// 侧边会话 id 的前缀。
///
/// 加前缀不只是为了好看：`shell.sessions` 里混着主会话与侧边会话，日志和排障时
/// 一眼能分出哪个是临时的。
pub const SIDE_PREFIX: &str = "side-";

/// 侧边会话每次都会带上的一段说明。
///
/// 写得直白是刻意的：模型看到一大段以「用户」开头的历史，很自然会把它当成当前任务
/// 接着干（尤其历史尾部正是「还没做完的事」）。这段话必须把这件事说破。
const SIDE_INSTRUCTIONS: &str = "\
## 这是一次侧边聊天

用户在主对话之外开了这个临时会话，用来随手问几句。分割线以上的内容是**继承来的
参考上下文，不是你的当前任务**：

- 不要接着执行里面提到的计划、请求、工具调用或待办；
- 不要回应里面还没做完的事，也不要以为自己是那条对话的延续；
- 只有用户在这条消息之后说的话，才是这条会话的当前指令。

默认只做只读的查看与问答。除非用户明确要求改动，否则不要改文件、不要动 git 状态、
不要改配置。也不要派生子 agent —— 这条会话里没有这个工具。

这条会话是临时的：不会写进会话列表，关掉就没了。需要落地的改动，回主对话去说。";

pub struct StartedSide {
    pub handle: Arc<SessionHandle>,
    pub model: String,
    pub cwd: PathBuf,
    /// 继承下来的历史（已经截到干净边界），供界面折叠显示。
    pub inherited: Vec<MessageView>,
}

/// 起一个侧边会话。
///
/// `parent_id` 必须是一条活着的会话 —— 侧边聊天的全部意义就是「接着当前这段对话问」，
/// 主会话没了它没有可继承的东西。
pub fn start_side_session(shell: Arc<AppShell>, parent_id: &str) -> Result<StartedSide, String> {
    let parent = shell
        .session(parent_id)
        .ok_or_else(|| "主会话已经结束了，开不了侧边聊天".to_string())?;

    // 一个主会话同时只开一个：多个侧边会话在界面上没有地方放（抽屉只有一个），
    // 而且它们互相之间也没有关系。这里是兜底 —— 界面自己会先看一眼。
    if let Some(existing) = find_side_of(&shell, parent_id) {
        return Err(format!("已经有一个侧边聊天开着（{}）", existing.id));
    }

    let snapshot = parent.history.load_full();
    let seeded = truncate_to_clean(&snapshot);
    let inherited = crate::view::to_views(&seeded);

    let cwd = parent.cwd.clone();
    let model_ref = parent.model.load().key.clone();
    let config = shell.config.load_full();
    let cache_json = fetch::read_cache().map_err(|e| e.to_string())?;
    // 刻意**不挂抓包钩子**：侧边会话的 id 不进侧栏、不进报文面板，
    // 记下来的报文只会变成没人翻得到的垃圾文件。
    let resolved = carter::provider::resolve_provider(&config, &cache_json, &model_ref, Vec::new())
        .map_err(|e| e.to_string())?;
    let model: ModelInfo = resolved.model;

    let id = side_id();
    let thread = seed_thread(&id, &seeded);
    let system = side_system(&config, &cwd, &model);

    let handle = start_side(
        SideWiring {
            shell,
            cwd: cwd.clone(),
            model: model.clone(),
            provider: resolved.provider,
            config,
            unsupported_server_tools: resolved.unsupported_server_tools,
            thread,
            system,
        },
        id,
        parent_id.to_string(),
    );

    Ok(StartedSide {
        handle,
        model: model.api_name,
        cwd,
        inherited,
    })
}

/// 找某个主会话已经开着的侧边会话。
fn find_side_of(shell: &AppShell, parent_id: &str) -> Option<Arc<SessionHandle>> {
    shell
        .sessions
        .iter()
        .find(|e| e.value().parent_id.as_deref() == Some(parent_id))
        .map(|e| e.value().clone())
}

/// 侧边会话的 id。
///
/// 不引 `uuid`（core 里没有这个依赖）：毫秒时间戳 + 进程内自增就够 —— 这个 id
/// 只需要在同一台机器、同一个进程里唯一，它既不上网也不落盘。
fn side_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "{SIDE_PREFIX}{}-{}",
        crate::protocol::now_ms(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// 建侧边会话的 `Thread`：种上继承的历史，**不挂 recorder**。
///
/// 单独抽出来是为了能直接测「它不落盘」这条 —— 侧边的全部承诺就是这一条。
fn seed_thread(id: &str, messages: &[WireMessage]) -> Thread {
    let mut thread = Thread::new_empty();
    thread.set_session_id(id.to_string());
    for m in messages {
        thread.append(m.clone());
    }
    thread
}

/// 截到最后一个**干净的** assistant 消息为止。
///
/// 干净的 = 这一条不包含工具调用。它之后的内容要么是同一次工具往返的中间态
/// （assistant 说「我要调工具」→ user 回灌结果），要么是被中断的半截消息。
/// 两条都不能要：
///
/// * 留着悬空的 `tool_use`（没有配套的 `tool_result`）→ 上游直接 400；
/// * 留着尾部那条 `tool_result` 消息 → 侧边会话再 append 自己的用户消息，
///   就成了连续两条 user 消息，而 llm_adapt 的 anthropic codec 是**逐条映射**、
///   不做合并的。
///
/// 找不到这样一条（新会话、或者整条历史都没收干净）就返回空 —— 空历史是合法的，
/// 悬空的历史不是。
fn truncate_to_clean(messages: &[WireMessage]) -> Vec<WireMessage> {
    let end = messages.iter().rposition(|m| {
        m.role == Role::Assistant
            && !m
                .content
                .iter()
                .any(|b| matches!(b, Block::ToolUse { .. }))
    });
    match end {
        Some(i) => messages[..=i].to_vec(),
        None => Vec::new(),
    }
}

/// 侧边会话的系统提示：父会话那一套（人设 + 技能 + 记忆 + 运行环境）+ 侧边说明。
///
/// 刻意**不带长任务目标**：目标是主对话的事，侧边会话被它牵着走就跑偏了。
fn side_system(config: &Config, cwd: &std::path::Path, model: &ModelInfo) -> Vec<String> {
    let skill_metas = if config.skills.enabled {
        carter::skills::discover(&carter::config::paths::skills_dir())
    } else {
        Vec::new()
    };
    // git 传 None：侧边会话不该为了拿它去读一遍父会话的会话文件。少一段
    // 「分支 / commit」不影响问答。
    let mut system =
        carter::prompt::assemble_system(config, cwd, &model.key, None, &skill_metas, true);
    system.push(SIDE_INSTRUCTIONS.to_string());
    system
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_text(text: &str) -> WireMessage {
        WireMessage::new(Role::Assistant, vec![Block::text(text)])
    }

    fn assistant_tools() -> WireMessage {
        WireMessage::new(
            Role::Assistant,
            vec![Block::ToolUse {
                id: "t1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "ls"}),
                cache: None,
            }],
        )
    }

    fn tool_results() -> WireMessage {
        WireMessage::new(
            Role::User,
            vec![Block::ToolResult {
                tool_use_id: "t1".into(),
                content: llm_adapt_core::ToolResultContent::Text {
                    text: "a\nb".into(),
                },
                is_error: false,
                cache: None,
            }],
        )
    }

    fn user(text: &str) -> WireMessage {
        WireMessage::user_text(text)
    }

    /// 侧边会话的整个承诺就是这一条：**不落盘**。挂了 recorder 就会写出会话文件，
    /// 于是它出现在侧栏、能被导出、能被误当成一条真对话。
    #[test]
    fn a_side_thread_has_no_recorder() {
        let t = seed_thread("side-1", &[user("问题"), assistant_text("回答")]);
        assert!(t.recorder().is_none(), "侧边会话不能落盘");
        assert_eq!(t.messages.len(), 2, "种子历史要进去");
        assert_eq!(t.session_id.as_deref(), Some("side-1"));
    }

    /// 正常收尾的一轮：整段照种。
    #[test]
    fn a_conversation_ending_on_a_plain_answer_is_seeded_whole() {
        let msgs = vec![user("问题一"), assistant_text("回答一"), user("问题二")];
        // 末尾那条 user 是「刚开始但还没跑完」的一轮 —— 砍掉它。
        let seeded = truncate_to_clean(&msgs);
        assert_eq!(seeded.len(), 2);
        assert_eq!(seeded[1].role, Role::Assistant);
    }

    /// **中途被取消的一轮**：尾部挂着一个没有结果的 tool_use。原样种进去，
    /// 侧边会话第一次请求就会被上游以「tool_use 无对应 tool_result」400 掉。
    #[test]
    fn a_half_finished_tool_call_is_never_seeded() {
        let msgs = vec![
            user("问题"),
            assistant_text("我想想"),
            user("再来"),
            assistant_tools(),
        ];
        let seeded = truncate_to_clean(&msgs);
        assert_eq!(seeded.len(), 2, "最后那条悬空的 tool_use 必须被砍掉");
        assert!(
            !seeded.iter().any(|m| m
                .content
                .iter()
                .any(|b| matches!(b, Block::ToolUse { .. }))),
            "种子里不能有任何工具调用"
        );
    }

    /// 工具往返**完整**也不能要：尾部那条 `tool_result` 会让侧边紧接着的那条
    /// 用户消息变成连续两条 user（anthropic codec 逐条映射、不合并）。
    #[test]
    fn a_complete_tool_exchange_is_still_trimmed_from_the_tail() {
        let msgs = vec![
            user("问题"),
            assistant_text("回答"),
            assistant_tools(),
            tool_results(),
        ];
        assert_eq!(truncate_to_clean(&msgs).len(), 2);
    }

    /// 一次都没收干净（比如整条历史只有工具往返）→ 空种子。空是合法的，悬空不是。
    #[test]
    fn nothing_clean_means_an_empty_seed() {
        assert!(truncate_to_clean(&[]).is_empty());
        assert!(truncate_to_clean(&[user("只有用户"), assistant_tools()]).is_empty());
    }

    /// 侧边提示必须把「继承的历史只是参考」说破 —— 不说的话模型会接着干历史里
    /// 没做完的事，而那正是用户开侧边会话想躲开的东西。
    #[test]
    fn the_instructions_disown_the_inherited_history() {
        for needle in ["参考上下文", "不是你的当前任务", "临时"] {
            assert!(SIDE_INSTRUCTIONS.contains(needle), "缺少「{needle}」");
        }
    }
}
