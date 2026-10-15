//! 应用状态根。
//!
//! 一个 `Arc<AppShell>`，用 `manage` 交给 Tauri，也被会话任务共享。
//! 刻意不依赖 Tauri —— 这样核心逻辑能在没有 webview 的情况下测。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use dashmap::DashMap;
use tokio::sync::{broadcast, mpsc, oneshot};

use carter::agent::CancelToken;
use carter::config::Config;
use carter::provider::WireMessage;
use carter::registry::ModelInfo;

use crate::protocol::{AppEvent, Command};

/// 模型通过 `ask_user_question` 提的问题，等前端作答。
///
/// 工具那一侧正 await 在一个 oneshot 上。所以这里必须把发送端**存住** ——
/// 走的通道是「工具 → 会话任务 → 前端 → 会话任务 → 工具」，中间要跨过事件广播，
/// 而 oneshot 只能发一次，不能随着广播被克隆出去。
#[derive(Default)]
pub struct AskRegistry {
    /// 答案形状与问题一一对应：外层是问题，内层是这一问选中的标签。
    pending: Mutex<HashMap<u64, oneshot::Sender<Vec<Vec<String>>>>>,
}

impl AskRegistry {
    pub fn insert(&self, id: u64, tx: oneshot::Sender<Vec<Vec<String>>>) {
        self.pending.lock().unwrap().insert(id, tx);
    }

    /// 作答。返回 `false` 表示该问题已失效（会话结束、或被重复作答）——
    /// 这不是错误，前端不必处理。
    pub fn answer(&self, id: u64, answers: Vec<Vec<String>>) -> bool {
        match self.pending.lock().unwrap().remove(&id) {
            Some(tx) => tx.send(answers).is_ok(),
            None => false,
        }
    }

    /// 会话结束时把没答的问题全部丢弃，让工具那一侧立刻拿到 Err 返回，
    /// 而不是永远卡在 await 上。
    pub fn clear(&self) {
        self.pending.lock().unwrap().clear();
    }
}

/// 一个会话的句柄：往它的命令通道投递，就能驱动那边跑着的那份 `Thread`。
pub struct SessionHandle {
    pub id: String,
    pub cwd: PathBuf,
    /// 当前模型。`/model` 切换后会变，界面要能立刻反映。
    pub model: ArcSwap<ModelInfo>,
    pub asks: Arc<AskRegistry>,
    /// 父会话 id。只有侧边聊天（`/btw`）有 —— 它继承父会话的上下文，
    /// 父会话一关就该跟着走，否则抽屉里那条对话会孤悬着。
    pub parent_id: Option<String>,
    /// 这一份对话**到目前为止**的消息快照，由会话任务在每轮结束时刷新。
    ///
    /// 侧边聊天靠它拿父会话的上下文。两条路都走不通所以只能这样：`session::load`
    /// 会给父会话文件挂上 `Recorder`（侧边的 append 会写进父文件里），而会话文件
    /// 是惰性建的 —— 一条还没发过消息的新会话根本没有文件。
    pub history: ArcSwap<Vec<WireMessage>>,
    /// 中断当前轮。会话任务用它来响应 `Command::Cancel`。
    pub cancel: CancelToken,
    /// 关闭整个会话（任务退出、MCP 回收）。与 `cancel` 分开是必须的：
    /// `cancel` 用完会 `reset()`（见 carter 的 `CancelToken`），拿它兼作关闭信号，
    /// 用户点一次「停止」就会把会话关掉。
    pub close: CancelToken,
    /// 只有 crate 内能构造 —— 命令通道的持有者是会话运行器，不该由外部塞进来。
    pub(crate) cmd_tx: mpsc::UnboundedSender<Command>,
}

impl SessionHandle {
    pub(crate) fn new(
        id: String,
        cwd: PathBuf,
        model: ModelInfo,
        parent_id: Option<String>,
        cmd_tx: mpsc::UnboundedSender<Command>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            cwd,
            model: ArcSwap::from_pointee(model),
            asks: Arc::new(AskRegistry::default()),
            parent_id,
            history: ArcSwap::from_pointee(Vec::<WireMessage>::new()),
            cancel: CancelToken::new(),
            close: CancelToken::new(),
            cmd_tx,
        })
    }

    /// 投递一条命令。返回 `false` 表示会话任务已经结束。
    pub fn send(&self, cmd: Command) -> bool {
        self.cmd_tx.send(cmd).is_ok()
    }

    /// 刷新历史快照（只有会话任务会调）。
    pub fn publish_history(&self, messages: &[WireMessage]) {
        self.history.store(Arc::new(messages.to_vec()));
    }
}

/// 全部会话 + 事件总线 + 配置。
pub struct AppShell {
    /// 热重载的配置快照。读方永远拿到一致的一份，不会被写方撕裂。
    pub config: ArcSwap<Config>,
    /// 原始报文仓库。放在这里而不是会话里：抓包是按会话分文件的，
    /// 但存储本身与哪个会话无关，界面要能独立地查它。
    pub capture: Arc<crate::capture::CaptureStore>,
    pub sessions: DashMap<String, Arc<SessionHandle>>,
    events: broadcast::Sender<AppEvent>,
}

impl AppShell {
    pub fn new(config: Config) -> Arc<Self> {
        // 容量给足：事件是「发完即忘」的，慢订阅者宁可丢历史也不要阻塞生产者。
        let (events, _) = broadcast::channel(1024);
        Arc::new(Self {
            config: ArcSwap::from_pointee(config),
            capture: Arc::new(crate::capture::CaptureStore::open(
                crate::capture::CaptureStore::default_root(),
            )),
            sessions: DashMap::new(),
            events,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    pub fn emit(&self, ev: AppEvent) {
        // 没有订阅者时 send 会 Err —— 那是正常情况（窗口还没建好），忽略。
        let _ = self.events.send(ev);
    }

    pub fn session(&self, id: &str) -> Option<Arc<SessionHandle>> {
        self.sessions.get(id).map(|s| s.clone())
    }

    /// 让某个会话回答模型的问题。
    pub fn answer_ask(&self, session_id: &str, id: u64, answers: Vec<Vec<String>>) -> bool {
        match self.session(session_id) {
            Some(h) => h.asks.answer(id, answers),
            None => false,
        }
    }

    /// 往某个会话投递命令。
    pub fn send(&self, session_id: &str, cmd: Command) -> bool {
        match self.session(session_id) {
            Some(h) => h.send(cmd),
            None => false,
        }
    }

    /// 关闭某个会话：通知它退出、丢掉待答问题、移出注册表。
    ///
    /// **光从注册表里移除是不够的。** 会话任务自己攥着一个 `Arc<SessionHandle>`
    /// （它要往里面读 `model`、写 `asks`），那个 `Arc` 又持有 `cmd_tx` 的发送端 ——
    /// 于是 `cmd_rx.recv()` 永远不会返回 `None`，任务永远等在那儿，`mcp_mgr.shutdown()`
    /// 也就永远走不到。表现是「关掉的会话其实还在后台活着」。所以这里显式 `close.set()`
    /// （任务的主循环在等它），再 `cancel.set()` 让正在跑的那一轮立刻断流。
    ///
    /// 侧边聊天（`/btw`）是挂在主会话上的，一起关掉。
    pub fn close_session(&self, id: &str) -> bool {
        // 先收集再关：`iter()` 期间握着分片锁，直接在里面 remove 是自找麻烦。
        let children: Vec<String> = self
            .sessions
            .iter()
            .filter(|e| e.value().parent_id.as_deref() == Some(id))
            .map(|e| e.key().clone())
            .collect();
        for child in children {
            self.close_session(&child);
        }

        match self.sessions.remove(id) {
            Some((_, handle)) => {
                handle.close.set();
                handle.cancel.set();
                handle.asks.clear();
                true
            }
            None => false,
        }
    }

    /// 当前活着的会话 id。
    pub fn session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.sessions.iter().map(|e| e.key().clone()).collect();
        ids.sort();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SessionEvent;

    fn shell() -> Arc<AppShell> {
        AppShell::new(Config::default())
    }

    #[tokio::test]
    async fn events_reach_subscribers() {
        let shell = shell();
        let mut rx = shell.subscribe();
        shell.emit(AppEvent::Session {
            session_id: "s1".into(),
            event: SessionEvent::Idle,
        });
        match rx.recv().await.unwrap() {
            AppEvent::Session { session_id, event } => {
                assert_eq!(session_id, "s1");
                assert!(matches!(event, SessionEvent::Idle));
            }
            other => panic!("期望 Session，得到 {other:?}"),
        }
    }

    /// 没有订阅者时 emit 不该 panic —— 窗口还没建好就会走到这条路径。
    #[test]
    fn emitting_without_subscribers_is_harmless() {
        shell().emit(AppEvent::Error {
            message: "x".into(),
        });
    }

    #[tokio::test]
    async fn answering_an_unknown_question_is_false_not_an_error() {
        // 会话结束、或用户重复点击时会走到这条路径 —— 它不是错误。
        let shell = shell();
        assert!(!shell.answer_ask("s1", 42, vec![vec!["x".into()]]));
    }

    #[tokio::test]
    async fn ask_registry_routes_the_answer_to_the_question_asker() {
        let asks = AskRegistry::default();
        let (tx, rx) = oneshot::channel();
        asks.insert(7, tx);
        // 两个问题，第一个多选。
        let answers = vec![vec!["a".to_string(), "b".to_string()], vec!["c".to_string()]];
        assert!(asks.answer(7, answers.clone()));
        assert_eq!(rx.await.unwrap(), answers, "答案要按问题分组原样送到");
        // 只能答一次。
        assert!(!asks.answer(7, vec![vec!["又答一次".into()]]));
    }

    /// 会话结束时未作答的问题要立刻失效，让工具那侧拿到 Err 返回 ——
    /// 否则它会永远卡在 await 上。
    #[tokio::test]
    async fn clearing_pending_questions_drops_the_senders() {
        let asks = AskRegistry::default();
        let (tx, rx) = oneshot::channel();
        asks.insert(1, tx);
        asks.clear();
        assert!(rx.await.is_err(), "发送端被丢弃时接收端应立刻得到 Err");
        assert!(!asks.answer(1, vec![vec!["太晚了".into()]]));
    }

    #[test]
    fn closing_an_unknown_session_is_false_not_an_error() {
        assert!(!shell().close_session("nope"));
    }

    /// 拿不到 provider 的小测试用模型。`ModelInfo` 只实现了 `Deserialize`，
    /// 所以从 JSON 造一份最省。
    fn model() -> ModelInfo {
        serde_json::from_str(
            r#"{"provider":"p","api_name":"m","context_window":1000,
                "max_output_tokens":100,"tokenizer":"t",
                "pricing":{"input":0.0,"output":0.0}}"#,
        )
        .unwrap()
    }

    fn add(shell: &Arc<AppShell>, id: &str, parent: Option<&str>) -> Arc<SessionHandle> {
        let (tx, _rx) = mpsc::unbounded_channel();
        let h = SessionHandle::new(
            id.to_string(),
            PathBuf::from("."),
            model(),
            parent.map(str::to_string),
            tx,
        );
        shell.sessions.insert(id.to_string(), h.clone());
        h
    }

    /// 关闭必须是**显式通知**，不能只从注册表里拿掉。
    ///
    /// 这条守的是一个真实存在的死结：会话任务自己攥着一个 `Arc<SessionHandle>`，
    /// 而它持有命令通道的发送端 —— 只 remove 的话 `cmd_rx.recv()` 永远不返回 `None`，
    /// 任务（连同 MCP 子进程）永远不退出。`/btw` 的「关掉即丢」正是靠这里成立。
    #[test]
    fn closing_a_session_signals_its_task_instead_of_just_forgetting_it() {
        let shell = shell();
        let h = add(&shell, "m1", None);
        assert!(!h.close.is_cancelled(), "刚建好时是活的");

        assert!(shell.close_session("m1"));
        assert!(h.close.is_cancelled(), "任务得知道要退出");
        assert!(h.cancel.is_cancelled(), "正在跑的那一轮要立刻断流");
    }

    /// 侧边聊天继承的是主会话的上下文，主会话都没了，它留着没有意义。
    #[test]
    fn closing_a_session_takes_its_side_conversations_with_it() {
        let shell = shell();
        let main = add(&shell, "m1", None);
        let side = add(&shell, "side-1", Some("m1"));
        let other = add(&shell, "m2", None);

        shell.close_session("m1");

        assert!(side.close.is_cancelled(), "侧边要跟着关");
        assert!(main.close.is_cancelled(), "主会话自己当然也要关");
        assert!(shell.session("side-1").is_none());
        assert!(shell.session("m1").is_none());
        assert!(!other.close.is_cancelled(), "别人的会话不能被误伤");
        assert!(shell.session("m2").is_some());
    }
}
