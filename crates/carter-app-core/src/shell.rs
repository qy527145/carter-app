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

use carter::config::Config;
use carter::registry::ModelInfo;

use crate::protocol::{AppEvent, Command};

/// 模型通过 `ask_user_question` 提的问题，等前端作答。
///
/// 问句里带着一个 oneshot 发送端，工具那一侧正 await 在它上面。所以这里必须把
/// 发送端**从事件里取出来存住** —— 事件本身会被广播给多个订阅者，而 oneshot 只能发一次。
#[derive(Default)]
pub struct AskRegistry {
    pending: Mutex<HashMap<u64, oneshot::Sender<String>>>,
}

impl AskRegistry {
    pub fn insert(&self, id: u64, tx: oneshot::Sender<String>) {
        self.pending.lock().unwrap().insert(id, tx);
    }

    /// 作答。返回 `false` 表示该问题已失效（会话结束、或被重复作答）——
    /// 这不是错误，前端不必处理。
    pub fn answer(&self, id: u64, answer: String) -> bool {
        let tx = self.pending.lock().unwrap().remove(&id);
        match tx {
            Some(tx) => tx.send(answer).is_ok(),
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
    /// 只有 crate 内能构造 —— 命令通道的持有者是会话运行器，不该由外部塞进来。
    pub(crate) cmd_tx: mpsc::UnboundedSender<Command>,
}

impl SessionHandle {
    /// 投递一条命令。返回 `false` 表示会话任务已经结束。
    pub fn send(&self, cmd: Command) -> bool {
        self.cmd_tx.send(cmd).is_ok()
    }
}

/// 全部会话 + 事件总线 + 配置。
pub struct AppShell {
    /// 热重载的配置快照。读方永远拿到一致的一份，不会被写方撕裂。
    pub config: ArcSwap<Config>,
    pub sessions: DashMap<String, Arc<SessionHandle>>,
    events: broadcast::Sender<AppEvent>,
}

impl AppShell {
    pub fn new(config: Config) -> Arc<Self> {
        // 容量给足：事件是「发完即忘」的，慢订阅者宁可丢历史也不要阻塞生产者。
        let (events, _) = broadcast::channel(1024);
        Arc::new(Self {
            config: ArcSwap::from_pointee(config),
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
    pub fn answer_ask(&self, session_id: &str, id: u64, answer: String) -> bool {
        match self.session(session_id) {
            Some(h) => h.asks.answer(id, answer),
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

    /// 关闭某个会话：丢掉待答问题、移出注册表。
    ///
    /// 命令通道随之 drop，会话任务会读到 `None` 自然退出。
    pub fn close_session(&self, id: &str) -> bool {
        match self.sessions.remove(id) {
            Some((_, handle)) => {
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
