//! 应用状态根。
//!
//! 一个 `Arc<AppShell>`，用 `manage` 交给 Tauri，也被会话任务共享。
//! 刻意不依赖 Tauri —— 这样核心逻辑能在没有 webview 的情况下测。

use std::collections::{HashMap, HashSet};
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
    /// 这一份对话**到目前为止**的消息快照，由会话任务在每轮开始与结束时刷新。
    ///
    /// 侧边聊天靠它拿父会话的上下文。两条路都走不通所以只能这样：`session::load`
    /// 会给父会话文件挂上 `Recorder`（侧边的 append 会写进父文件里），而会话文件
    /// 是惰性建的 —— 一条还没发过消息的新会话根本没有文件。
    pub history: ArcSwap<Vec<WireMessage>>,
    /// 正在跑的这一轮里已经发生的事（模型说到一半的话、调过的工具）。
    ///
    /// `history` 只在轮次边界上刷新，而 carter 的 `run_turn` 在一轮之内独占 `Thread`
    /// —— 主对话跑长任务时（正是最想开 `/btw` 的时候），光看 `history` 只能看到
    /// 「用户提了个要求」，看不到它已经干到哪了。这一份由事件转发任务边收边记，
    /// 补上这段空白。不在跑的时候它是空的。
    pub live: Arc<LiveLog>,
    /// 中断当前轮。会话任务用它来响应 `Command::Cancel`。
    pub cancel: CancelToken,
    /// 关闭整个会话（任务退出、MCP 回收）。与 `cancel` 分开是必须的：
    /// `cancel` 用完会 `reset()`（见 carter 的 `CancelToken`），拿它兼作关闭信号，
    /// 用户点一次「停止」就会把会话关掉。
    pub close: CancelToken,
    /// 会话任务**真的退出**了。
    ///
    /// 与 `close` 不是一回事：`close` 是「请求你退出」，`done` 是「已经退出了」。
    /// 关会话之后立刻按 id 重新打开同一条会话时，必须等旧任务把 `Thread` 与
    /// `Recorder` 放掉 —— 否则两个 runner 会往同一个会话文件里追加，历史从此错位
    /// （见 `AppShell::close_session`）。
    pub done: CancelToken,
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
            live: Arc::new(LiveLog::default()),
            cancel: CancelToken::new(),
            close: CancelToken::new(),
            done: CancelToken::new(),
            cmd_tx,
        })
    }

    /// 投递一条命令。返回 `false` 表示会话任务已经结束。
    pub fn send(&self, cmd: Command) -> bool {
        self.cmd_tx.send(cmd).is_ok()
    }

    /// 刷新历史快照（只有会话任务会调）。
    ///
    /// 同时清空 `live`：快照里已经有的东西，不该再在「进行中」里出现一遍。
    pub fn publish_history(&self, messages: &[WireMessage]) {
        self.history.store(Arc::new(messages.to_vec()));
        self.live.clear();
    }
}

/// 进行中那一轮的一条记录，见 [`SessionHandle::live`]。
#[derive(Debug, Clone, PartialEq)]
pub enum LiveItem {
    /// 模型的正文（流式增量拼起来的）。
    Text(String),
    Tool {
        call_id: String,
        name: String,
        args: String,
        /// `None` = 还在跑。
        result: Option<String>,
    },
}

/// 进行中那一轮的记录。
///
/// 由**生产方**同步写入（`TxSink` 收文本增量、`Recorded` 包装层记工具），而不是由
/// 事件转发任务写：转发是异步的，它落后半拍收到的事件会在 `publish_history` 清空
/// 之后才写进来，于是同一段内容在快照与「进行中」里各出现一遍。
#[derive(Debug, Default)]
pub struct LiveLog(Mutex<Vec<LiveItem>>);

impl LiveLog {
    pub fn snapshot(&self) -> Vec<LiveItem> {
        self.0.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }

    /// 记一段流式正文。连续的增量并进同一条，免得一个字一条。
    pub fn text(&self, delta: &str) {
        let mut live = self.0.lock().unwrap();
        match live.last_mut() {
            Some(LiveItem::Text(t)) => t.push_str(delta),
            _ => live.push(LiveItem::Text(delta.to_string())),
        }
    }

    pub fn tool_started(&self, call_id: &str, name: &str, args: &serde_json::Value) {
        self.0.lock().unwrap().push(LiveItem::Tool {
            call_id: call_id.to_string(),
            name: name.to_string(),
            args: args.to_string(),
            result: None,
        });
    }

    pub fn tool_finished(&self, call_id: &str, content: &str) {
        let mut live = self.0.lock().unwrap();
        for item in live.iter_mut().rev() {
            if let LiveItem::Tool { call_id: id, result, .. } = item {
                if id == call_id {
                    *result = Some(content.to_string());
                    return;
                }
            }
        }
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
    /// 已经发出关闭信号、但任务还没退出的会话 id。
    ///
    /// 不能只靠 `sessions` 判断：关闭的第一步就是把它从注册表里拿掉（否则新的命令
    /// 会被投进一条正在死的会话），而任务可能还要几百毫秒才真正放开会话文件。
    /// 这段时间里按 id 重开同一条会话会造出第二个写者（见 `close_session`）。
    closing: Mutex<HashSet<String>>,
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
            closing: Mutex::new(HashSet::new()),
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
    ///
    /// **会等任务真的退出**（最多 5 秒），不只是发个通知。原因是一个真实踩过的
    /// 坑：用户在一条 `bash` 跑了很久的会话上点「新对话」，旧的 runner 还卡在
    /// 工具里；这时马上按 id 重开同一条会话，就会有**两个** `Thread`（各带一个
    /// `Recorder`）往同一个文件追加，模型看的历史与界面上显示的对不上，而且
    /// `tool_use` / `tool_result` 会被别的消息隔开 —— 那之后每一次请求都失败。
    ///
    /// 工具现在都响应取消（见 `tools/bash.rs`、`tools/ask_user.rs`），正常情况下
    /// 几十毫秒就退出了。5 秒是给「不认识的工具」（MCP）留的兜底。
    pub async fn close_session(&self, id: &str) -> bool {
        let handles = self.close_tree(id);
        if handles.is_empty() {
            return false;
        }
        let ids: Vec<String> = handles.iter().map(|h| h.id.clone()).collect();
        // 并行等：主会话与侧边聊天各 5 秒串起来就是 10 秒，界面会明显卡一下。
        // 总时限仍是 5 秒。
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            futures::future::join_all(handles.iter().map(|h| h.done.cancelled())),
        )
        .await;
        // 超时也照样清掉标记：一直卡着它只会让这条会话永远打不开。
        // 真出现这种情况，`start_session` 那边还有一道「同一 id 不能有两个 runner」
        // 的兜底。
        let mut closing = self.closing.lock().unwrap();
        for id in ids {
            closing.remove(&id);
        }
        true
    }

    /// 这条会话是不是正在关闭（已经发了信号，任务还没退出）。
    ///
    /// `start_session` 用它挡住「同一个会话文件被两个 runner 写」。
    pub fn is_closing(&self, id: &str) -> bool {
        self.closing.lock().unwrap().contains(id)
    }

    /// 会话任务退出了，把它从「正在关闭」里摘掉。
    ///
    /// 由 runner 在最后自己调，而不是交给 `close_session`：那个等待有 5 秒上限，
    /// 超时之后任务才真正退出的话，标记得在这里被清掉。
    pub(crate) fn mark_closed(&self, id: &str) {
        self.closing.lock().unwrap().remove(id);
    }

    /// 发关闭信号、移出注册表，返回要等的句柄。**不 await** —— 递归里用不了。
    fn close_tree(&self, id: &str) -> Vec<Arc<SessionHandle>> {
        // 先收集再关：`iter()` 期间握着分片锁，直接在里面 remove 是自找麻烦。
        let children: Vec<String> = self
            .sessions
            .iter()
            .filter(|e| e.value().parent_id.as_deref() == Some(id))
            .map(|e| e.key().clone())
            .collect();
        let mut out = Vec::new();
        for child in children {
            out.extend(self.close_tree(&child));
        }
        if let Some((_, handle)) = self.sessions.remove(id) {
            handle.close.set();
            handle.cancel.set();
            handle.asks.clear();
            self.closing.lock().unwrap().insert(id.to_string());
            out.push(handle);
        }
        out
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

    #[tokio::test]
    async fn closing_an_unknown_session_is_false_not_an_error() {
        assert!(!shell().close_session("nope").await);
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
    #[tokio::test]
    async fn closing_a_session_signals_its_task_instead_of_just_forgetting_it() {
        let shell = shell();
        let h = add(&shell, "m1", None);
        assert!(!h.close.is_cancelled(), "刚建好时是活的");
        // 这个测试里没有真的会话任务，`done` 永远不会置位 —— 关会话会在 5 秒
        // 兜底之后返回。为了不让测试等 5 秒，这里自己把它置位。
        let done = h.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            done.done.set();
        });

        assert!(shell.close_session("m1").await);
        assert!(h.close.is_cancelled(), "任务得知道要退出");
        assert!(h.cancel.is_cancelled(), "正在跑的那一轮要立刻断流");
    }

    /// 关会话必须等任务**真的退出**：旧 runner 还攥着 `Recorder` 的时候把同一个
    /// 会话 id 再打开，就会有两个写者往同一个文件追加。
    #[tokio::test]
    async fn closing_waits_for_the_runner_to_actually_stop() {
        let shell = shell();
        let h = add(&shell, "m1", None);
        let done = h.clone();
        let started = std::time::Instant::now();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            done.done.set();
        });
        shell.close_session("m1").await;
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(100),
            "关会话不能只发信号就返回"
        );
        assert!(h.done.is_cancelled());
    }

    /// 侧边聊天继承的是主会话的上下文，主会话都没了，它留着没有意义。
    #[tokio::test]
    async fn closing_a_session_takes_its_side_conversations_with_it() {
        let shell = shell();
        let main = add(&shell, "m1", None);
        let side = add(&shell, "side-1", Some("m1"));
        let other = add(&shell, "m2", None);
        for h in [&main, &side] {
            let h = h.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                h.done.set();
            });
        }

        shell.close_session("m1").await;

        assert!(side.close.is_cancelled(), "侧边要跟着关");
        assert!(main.close.is_cancelled(), "主会话自己当然也要关");
        assert!(shell.session("side-1").is_none());
        assert!(shell.session("m1").is_none());
        assert!(!other.close.is_cancelled(), "别人的会话不能被误伤");
        assert!(shell.session("m2").is_some());
    }
}
