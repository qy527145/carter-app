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
//! 2. **主对话以文字转录的形式给出，每轮现取。** 见 [`parent_context`]。
//! 3. **主对话里的计划不接着执行。** 那里面可能有「做完这个再做那个」，侧边会话只回答
//!    问题 —— 由 [`SIDE_INSTRUCTIONS`] 写给模型。
//!
//! ## 为什么是文字转录，而不是把父会话的消息原样种进来
//!
//! 最早是原样种进 `Thread` 的，结果模型**认不出那是主对话**：一串以「用户」开头的消息
//! 摆在前面，再加一句「这些只是参考、你不是那条对话的延续」，它的理解是「我看不到主
//! 对话」，被问「主对话在干嘛」时直接说不知道。另外两个毛病也是原样种带来的：
//!
//! * 只能种在干净边界上（悬空的 `tool_use` 会被上游 400），而主对话正在跑长任务时
//!   —— 恰恰是最想开 `/btw` 的时候 —— 干净边界之后的那一整段（用户的要求、模型已经
//!   调过的工具）全被砍掉了；
//! * 开的那一刻就冻住了，主对话后来又干了什么，侧边一概不知。
//!
//! 现在每轮开头读父会话的快照（`history`）加上进行中那一轮的记录（`live`），渲染成
//! 一段明确标着「主对话」的文本放进系统提示。侧边自己的 `Thread` 只装侧边自己的问答。
//!
//! ## 为什么不用 `session::load` 拿父会话的历史
//!
//! 那会给父会话文件挂上一个 `Recorder`：侧边会话每 append 一条，都会写进**父会话**
//! 的文件里（历史被改写，还多出一些不属于它的工具往返）。而且会话文件是惰性建的 ——
//! 一条还没发过消息的新会话根本没有文件，进行中的那一轮也还没落盘。所以走
//! `SessionHandle` 上的 `history` + `live`。

use std::path::PathBuf;
use std::sync::Arc;

use carter::agent::Thread;
use carter::config::Config;
use carter::provider::WireMessage;
use carter::registry::{fetch, ModelInfo};
use llm_adapt_core::{Block, Role};

use crate::app_settings::AppSettings;
use crate::session::{start_side, SideWiring};
use crate::shell::{AppShell, LiveItem, SessionHandle};
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

用户在主对话旁边开了这个临时会话，用来随手问几句 —— 通常就是问主对话的事：
「它现在在干嘛」「刚才那个改动是什么意思」「这个思路靠谱吗」。主对话的内容就在
下面「主对话」一节里（每次用户发消息时现取的最新状态，可能还在进行中），
**你能看到它，要据此回答**，不要说自己看不到主对话。

但主对话里的任务不是你的任务：

- 不要接着执行里面提到的计划、请求、工具调用或待办 —— 那边自己会接着干；
- 只有用户在这条侧边会话里说的话，才是你的当前指令。

默认只做只读的查看与问答。除非用户明确要求改动，否则不要改文件、不要动 git 状态、
不要改配置。也不要派生子 agent —— 这条会话里没有这个工具。

这条会话是临时的：不会写进会话列表，关掉就没了。需要落地的改动，回主对话去说。";

pub struct StartedSide {
    pub handle: Arc<SessionHandle>,
    pub model: String,
    pub cwd: PathBuf,
    /// 打开时父会话的历史，供界面折叠显示。模型那边每轮现取，不受这份限制。
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

    let inherited = crate::view::to_views(&parent.history.load_full());

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
    let thread = side_thread(&id);
    // 系统提示的固定部分在这里算一次（见 [`side_system`] 的注释）；
    // 主对话那一段每轮现拼，见 `session.rs::run_one_turn`。
    let system = side_system(&config, &AppSettings::load(), &cwd, &model);

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

/// 建侧边会话的 `Thread`：空的，**不挂 recorder**。
///
/// 单独抽出来是为了能直接测「它不落盘」这条 —— 侧边的全部承诺就是这一条。
fn side_thread(id: &str) -> Thread {
    let mut thread = Thread::new_empty();
    thread.set_session_id(id.to_string());
    thread
}

/// 转录的总长上限（字符）。超了从**最早**的消息开始丢 —— 问「现在在干嘛」，
/// 要的是尾巴。
const CONTEXT_BUDGET: usize = 120_000;
/// 单个工具参数 / 结果的上限。全文留着的话，几次 `read` 就能把预算吃光，
/// 而侧边问答要的是「它干了什么」，不是文件内容本身。
const TOOL_ARGS_MAX: usize = 600;
const TOOL_RESULT_MAX: usize = 1_500;

/// 父会话当前的上下文，渲染成系统提示里的一段。父会话已经没了就返回 `None`。
pub(crate) fn parent_context(shell: &AppShell, parent_id: &str) -> Option<String> {
    let parent = shell.session(parent_id)?;
    let history = parent.history.load_full();
    let live = parent.live.snapshot();
    Some(render_context(&history, &live))
}

fn render_context(history: &[WireMessage], live: &[LiveItem]) -> String {
    // 先按条渲染，再从尾部往前装预算。
    let mut entries: Vec<String> = history.iter().filter_map(render_message).collect();
    if !live.is_empty() {
        let mut s = String::from("【进行中：主对话这一轮还没跑完，以下是到目前为止的进展】\n");
        for item in live {
            match item {
                LiveItem::Text(t) => {
                    s.push_str(&format!("助手：{}\n", t.trim()));
                }
                LiveItem::Tool { name, args, result, .. } => {
                    s.push_str(&format!("助手调用工具 {name}({})\n", clip(args, TOOL_ARGS_MAX)));
                    match result {
                        Some(r) => s.push_str(&format!("  → {}\n", clip(r, TOOL_RESULT_MAX))),
                        None => s.push_str("  → （还在执行）\n"),
                    }
                }
            }
        }
        entries.push(s);
    }

    let mut out = String::from("## 主对话\n\n");
    if entries.is_empty() {
        out.push_str("（主对话目前还是空的，用户还没在那边说过话。）");
        return out;
    }

    let mut kept = Vec::new();
    let mut used = 0;
    for e in entries.iter().rev() {
        let n = e.chars().count();
        if used + n > CONTEXT_BUDGET && !kept.is_empty() {
            break;
        }
        used += n;
        kept.push(e.as_str());
    }
    if kept.len() < entries.len() {
        out.push_str(&format!("（更早的 {} 条消息过长，已省略）\n\n", entries.len() - kept.len()));
    }
    kept.reverse();
    out.push_str(&kept.join("\n"));
    out
}

fn render_message(m: &WireMessage) -> Option<String> {
    let who = match m.role {
        Role::User => "用户",
        Role::Assistant => "助手",
        Role::System => "系统",
    };
    let mut s = String::new();
    for b in &m.content {
        match b {
            Block::Text { text, .. } => {
                if !text.trim().is_empty() {
                    s.push_str(&format!("{who}：{}\n", text.trim()));
                }
            }
            Block::ToolUse { name, input, .. } => {
                s.push_str(&format!(
                    "助手调用工具 {name}({})\n",
                    clip(&input.to_string(), TOOL_ARGS_MAX)
                ));
            }
            Block::ToolResult { content, is_error, .. } => {
                let mark = if *is_error { "（出错）" } else { "" };
                s.push_str(&format!(
                    "  → {mark}{}\n",
                    clip(&content.as_text(), TOOL_RESULT_MAX)
                ));
            }
            Block::Image { .. } => s.push_str(&format!("{who}：[图片]\n")),
            // 思考与服务端块对「主对话在干嘛」帮助不大，还很长。
            _ => {}
        }
    }
    (!s.is_empty()).then_some(s)
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…（已截断）")
}

/// 侧边会话的系统提示：父会话那一套（人设 + 技能 + 记忆 + 运行环境）+ 侧边说明。
///
/// 刻意**不带长任务目标**：目标是主对话的事，侧边会话被它牵着走就跑偏了。
///
/// 注意这份提示是**创建时算一次并固定**的（`SideWiring.system` → `Runner::fixed_system`），
/// 所以开着侧边聊天时去技能页改扫描路径，对**这一条**侧边会话不生效，关掉重开才行。
/// 主会话每轮重算，不受影响。这个行为本来就有（技能目录一改就是如此），但界面能改路径
/// 之后它第一次变得可见，所以写在这里。
fn side_system(
    config: &Config,
    settings: &AppSettings,
    cwd: &std::path::Path,
    model: &ModelInfo,
) -> Vec<String> {
    let skill_metas = if config.skills.enabled {
        crate::skills::union_metas(&crate::skills::Scan::from_settings(settings))
    } else {
        Vec::new()
    };
    // git 传 None：侧边会话不该为了拿它去读一遍父会话的会话文件。少一段
    // 「分支 / commit」不影响问答。
    let mut system =
        carter::prompt::assemble_system(config, cwd, &model.key, None, &skill_metas, true);
    // 侧边抽屉里同样是图形界面 —— 「能直接给图 / 给网页」这条能力说明一样成立。
    system.push(crate::session::GUI_INSTRUCTIONS.to_string());
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
        let t = side_thread("side-1");
        assert!(t.recorder().is_none(), "侧边会话不能落盘");
        assert!(t.messages.is_empty(), "主对话走系统提示，不种进消息");
        assert_eq!(t.session_id.as_deref(), Some("side-1"));
    }

    /// 主对话的问答要原样出现在转录里 —— 这正是「侧边看不到主对话」那个 bug。
    #[test]
    fn the_transcript_carries_the_main_conversation() {
        let ctx = render_context(&[user("你好，你能干啥"), assistant_text("我能写代码")], &[]);
        assert!(ctx.starts_with("## 主对话"));
        assert!(ctx.contains("用户：你好，你能干啥"));
        assert!(ctx.contains("助手：我能写代码"));
    }

    /// 主对话正在跑长任务时（最想开 `/btw` 的时候），已经调过的工具、
    /// 半截的工具往返都要看得到，而不是被截到上一个干净边界。
    #[test]
    fn an_unfinished_turn_is_visible() {
        let history = vec![user("修一下登录"), assistant_tools(), tool_results()];
        let live = vec![
            LiveItem::Text("我先看看测试".into()),
            LiveItem::Tool {
                call_id: "t2".into(),
                name: "bash".into(),
                args: r#"{"command":"cargo test"}"#.into(),
                result: None,
            },
        ];
        let ctx = render_context(&history, &live);
        assert!(ctx.contains("用户：修一下登录"));
        assert!(ctx.contains("助手调用工具 bash"));
        assert!(ctx.contains("进行中"));
        assert!(ctx.contains("我先看看测试"));
        assert!(ctx.contains("cargo test"));
        assert!(ctx.contains("还在执行"));
    }

    /// 超长时丢最早的，留最近的。
    #[test]
    fn an_overlong_transcript_keeps_the_tail() {
        let big = "x".repeat(CONTEXT_BUDGET);
        let ctx = render_context(&[user(&big), assistant_text("最近的回答")], &[]);
        assert!(ctx.contains("最近的回答"));
        assert!(ctx.contains("已省略"));
    }

    #[test]
    fn an_empty_parent_says_so() {
        assert!(render_context(&[], &[]).contains("还是空的"));
    }

    /// 侧边提示要同时说清两件事：主对话你看得到（否则模型会说「我看不到」），
    /// 但那边的任务不归你（否则它会接着干主对话里没做完的事）。
    #[test]
    fn the_instructions_disown_the_inherited_history() {
        for needle in ["主对话", "不要说自己看不到", "不是你的任务", "临时"] {
            assert!(SIDE_INSTRUCTIONS.contains(needle), "缺少「{needle}」");
        }
    }
}
