//! 插话：一轮**进行中**用户又发了一条消息，不等这一轮结束，在**下一次请求**里就带给模型。
//!
//! ## 为什么不再排到轮末
//!
//! 一轮里模型可能连调几十次工具、跑好几分钟。用户看着它干活补一句「顺便把测试也跑一下」，
//! 排到轮末才被看到时它往往已经做完、甚至做偏了。carter 的 `run_turn` 每次发请求前问一次
//! [`carter::agent::Steer`]，这里把排着的话交出去，作为一条 user 消息紧跟在工具结果后面。
//!
//! ## 什么时候收、什么时候不收
//!
//! 只在 `run_turn` 跑着时「开门」（[`SteerQueue::open`]）：压缩、空闲时没人来取，照旧进
//! 会话的命令队列。这一轮后面不再发请求（模型已经在写最后一段话了）时没被取走的，由会话
//! 退回命令队列的队首，按原来的路作为下一轮跑（见 `session/turn.rs::run_one_turn`）。
//!
//! ## 它不算新的一轮
//!
//! 送进历史的正文带一行 [`STEER_HEAD`] 抬头，`view::is_turn_start` 认出它就不数成一轮：
//! 轮次号是界面「重试 / 编辑」、报文筛选、时间线、变更清单共同的钥匙，一轮中途多出一个
//! 号，这几样就全错位了。carter 自己的 `user_turns` 也是一次 `run_turn` 只加一，两边一致。
//! 抬头同时告诉模型：这是用户在它干活途中补的话，不是工具输出。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::protocol::{AppEvent, SessionEvent};
use crate::shell::{AppShell, LiveLog};

/// 插话正文的第一行：标记 + 给模型的一句说明。界面显示时整行去掉。
/// 与前端 `store.ts` 里那份逐字对应。
pub const STEER_HEAD: &str = "【插话】用户在这一轮进行中补充的话（不是工具输出）：";

/// 给模型看的正文。
pub fn wrap(text: &str) -> String {
    format!("{STEER_HEAD}\n{text}")
}

/// 这段用户正文是不是插话。
///
/// 认**整行抬头**而不只是标记：用户自己打一句以「【插话】」开头的话是普通的一轮，
/// 被认成插话的话轮次号就少数一个。
pub fn is_steer(text: &str) -> bool {
    text.starts_with(STEER_HEAD)
}

/// 一条排着的插话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// 前端给这条消息编的号，被取走时原样带回（界面据此把它从待发条挪进对话）。
    pub id: u64,
    /// 用户打的原文（界面显示它；退回命令队列时也用它重新展开）。
    pub raw: String,
    /// 展开 `/命令`、`@文件` 之后、加了抬头的正文 —— 发给模型的就是它。
    pub text: String,
}

/// 一条会话的插话队列。挂在 `SessionHandle` 上，与收件箱一样：正在跑的那一轮要能
/// 当场取走它，而命令通道那时是会话任务自己在读。
#[derive(Debug, Default)]
pub struct SteerQueue {
    open: AtomicBool,
    pending: Mutex<VecDeque<Pending>>,
}

impl SteerQueue {
    /// `run_turn` 开跑时开门、跑完关门。
    pub fn set_open(&self, open: bool) {
        self.open.store(open, Ordering::SeqCst);
    }

    /// 此刻有没有一轮在跑、能在下一次请求里把话带过去。
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    pub fn push(&self, p: Pending) {
        self.pending.lock().unwrap().push_back(p);
    }

    /// 撤回一条（界面上待发条的「×」）。已经被取走了就是 `None`。
    pub fn remove(&self, id: u64) -> Option<Pending> {
        let mut q = self.pending.lock().unwrap();
        let pos = q.iter().position(|p| p.id == id)?;
        q.remove(pos)
    }

    /// 测试用：排着几条（不取走）。
    #[cfg(test)]
    pub fn drain_peek_len(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// 全部取走（按到达顺序）。
    pub fn drain(&self) -> Vec<Pending> {
        self.pending.lock().unwrap().drain(..).collect()
    }
}

/// 交给 carter 的那一头：取走的同时告诉界面「这几条模型这一次就看到了」。
///
/// 事件在 `take` 里**同步**发：它与正文增量、工具卡片在同一条时间线上（见 `session/mod.rs`
/// 模块头），界面上插话气泡才会落在「上一批工具」与「模型接下来的话」之间。
pub struct TurnSteer {
    pub queue: Arc<SteerQueue>,
    pub shell: Arc<AppShell>,
    pub session_id: String,
    pub live: Option<Arc<LiveLog>>,
}

impl carter::agent::Steer for TurnSteer {
    fn take(&self) -> Vec<String> {
        self.queue
            .drain()
            .into_iter()
            .map(|p| {
                if let Some(live) = &self.live {
                    live.user(&p.raw);
                }
                self.shell.emit(AppEvent::Session {
                    session_id: self.session_id.clone(),
                    event: SessionEvent::PromptInjected {
                        prompt_id: p.id,
                        prompt: p.raw,
                    },
                });
                p.text
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use carter::agent::Steer;

    fn pending(id: u64) -> Pending {
        Pending {
            id,
            raw: format!("m{id}"),
            text: wrap(&format!("m{id}")),
        }
    }

    #[test]
    fn wrapped_text_is_recognised_and_strips_back_to_the_original() {
        let t = wrap("顺便跑一下测试");
        assert!(is_steer(&t));
        assert_eq!(t.strip_prefix(STEER_HEAD).unwrap(), "\n顺便跑一下测试");
        assert!(!is_steer("顺便跑一下测试"));
        assert!(!is_steer("【插话】这是用户自己打的"));
    }

    #[test]
    fn removing_takes_exactly_that_one() {
        let q = SteerQueue::default();
        q.push(pending(1));
        q.push(pending(2));
        assert_eq!(q.remove(1).map(|p| p.id), Some(1));
        assert_eq!(q.remove(1), None, "撤回过的再撤一次不出错");
        assert_eq!(q.drain(), vec![pending(2)]);
    }

    /// 取走即消费，同时按到达顺序给界面发事件、记进「进行中」。
    #[tokio::test]
    async fn taking_hands_over_in_order_and_tells_the_ui() {
        let shell = AppShell::new(carter::config::Config::default());
        let mut rx = shell.subscribe();
        let queue = Arc::new(SteerQueue::default());
        queue.push(pending(1));
        queue.push(pending(2));
        let live = Arc::new(LiveLog::default());
        let steer = TurnSteer {
            queue: queue.clone(),
            shell: shell.clone(),
            session_id: "s".into(),
            live: Some(live.clone()),
        };

        assert_eq!(steer.take(), vec![wrap("m1"), wrap("m2")]);
        assert!(steer.take().is_empty(), "取走即消费");

        let mut ids = Vec::new();
        while let Ok(AppEvent::Session { event, .. }) = rx.try_recv() {
            if let SessionEvent::PromptInjected { prompt_id, prompt } = event {
                assert_eq!(prompt, format!("m{prompt_id}"), "界面拿到的是原文");
                ids.push(prompt_id);
            }
        }
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(
            live.snapshot(),
            vec![
                crate::shell::LiveItem::User("m1".into()),
                crate::shell::LiveItem::User("m2".into())
            ]
        );
    }
}
