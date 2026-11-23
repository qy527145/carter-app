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
//!    问题 —— 由 `carter::side::INSTRUCTIONS` 写给模型。
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

use crate::catalog;
use carter::agent::Thread;
use carter::config::Config;
use carter::registry::ModelInfo;

use crate::app_settings::AppSettings;
use crate::session::{SideWiring, start_side};
use crate::shell::{AppShell, LiveItem, SessionHandle};
use crate::view::MessageView;

/// 侧边会话 id 的前缀。
///
/// 加前缀不只是为了好看：`shell.sessions` 里混着主会话与侧边会话，日志和排障时
/// 一眼能分出哪个是临时的。
pub const SIDE_PREFIX: &str = "side-";

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
    let cache_json = catalog::read_cache().map_err(|e| e.to_string())?;
    // 刻意**不挂抓包钩子**：侧边会话的 id 不进侧栏、不进报文面板，
    // 记下来的报文只会变成没人翻得到的垃圾文件。
    let resolved = catalog::resolve_provider(&config, &cache_json, &model_ref, Vec::new())
        .map_err(|e| e.to_string())?;
    let model: ModelInfo = resolved.model;

    let id = side_id();
    let thread = side_thread(&id);
    // 系统提示的固定部分在这里算一次（见 [`side_system`] 的注释）；
    // 主对话那一段每轮现拼，见 `session/turn.rs::run_one_turn`。
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

/// 父会话当前的上下文，渲染成系统提示里的一段。父会话已经没了就返回 `None`。
///
/// 渲染在 carter（`carter::side::transcript`）：命令行的 `/btw` 用的是同一份。
/// 这里只把 app 的「进行中那一轮」映射成 carter 的 [`carter::side::LiveTurn`]。
pub(crate) fn parent_context(shell: &AppShell, parent_id: &str) -> Option<String> {
    let parent = shell.session(parent_id)?;
    let history = parent.history.load_full();
    let live: Vec<carter::side::LiveTurn> = parent
        .live
        .snapshot()
        .into_iter()
        .map(|item| match item {
            LiveItem::Text(t) => carter::side::LiveTurn::Text(t),
            LiveItem::User(t) => carter::side::LiveTurn::User(t),
            LiveItem::Tool {
                name, args, result, ..
            } => carter::side::LiveTurn::Tool { name, args, result },
        })
        .collect();
    Some(carter::side::transcript(&history, &live))
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
        // 项目级技能跟着 cwd 走 —— 侧边会话与主会话在同一个项目里。
        crate::skills::union_metas(&crate::skills::Scan::from_settings_at(settings, Some(cwd)))
    } else {
        Vec::new()
    };
    // git 传 None：侧边会话不该为了拿它去读一遍父会话的会话文件。少一段
    // 「分支 / commit」不影响问答。
    let mut system =
        carter::prompt::assemble_system(config, cwd, &model.key, None, &skill_metas, true);
    // 侧边抽屉里同样是图形界面 —— 「能直接给图 / 给网页」这条能力说明一样成立。
    system.push(crate::session::GUI_INSTRUCTIONS.to_string());
    system.push(carter::side::INSTRUCTIONS.to_string());
    system
}

#[cfg(test)]
mod tests {
    use super::*;
    use carter::provider::WireMessage;

    /// 侧边会话的整个承诺就是这一条：**不落盘**。挂了 recorder 就会写出会话文件，
    /// 于是它出现在侧栏、能被导出、能被误当成一条真对话。
    #[test]
    fn a_side_thread_has_no_recorder() {
        let t = side_thread("side-1");
        assert!(t.recorder().is_none(), "侧边会话不能落盘");
        assert!(t.messages.is_empty(), "主对话走系统提示，不种进消息");
        assert_eq!(t.session_id.as_deref(), Some("side-1"));
    }

    /// 「进行中那一轮」要映射进转录：主对话正在跑长任务时（最想开侧边的时候），
    /// 已经调过的工具、半截的工具往返都要看得到。
    #[test]
    fn the_live_turn_is_mapped_into_the_transcript() {
        let live = vec![
            LiveItem::Text("我先看看测试".into()),
            LiveItem::Tool {
                call_id: "t2".into(),
                name: "bash".into(),
                args: r#"{"command":"cargo test"}"#.into(),
                result: None,
                agent_id: None,
            },
        ];
        let mapped: Vec<carter::side::LiveTurn> = live
            .into_iter()
            .map(|item| match item {
                LiveItem::Text(t) => carter::side::LiveTurn::Text(t),
                LiveItem::User(t) => carter::side::LiveTurn::User(t),
                LiveItem::Tool {
                    name, args, result, ..
                } => carter::side::LiveTurn::Tool { name, args, result },
            })
            .collect();
        let ctx = carter::side::transcript(&[WireMessage::user_text("修一下登录")], &mapped);
        assert!(ctx.contains("用户：修一下登录"));
        assert!(ctx.contains("cargo test"));
        assert!(ctx.contains("还在执行"));
    }
}
