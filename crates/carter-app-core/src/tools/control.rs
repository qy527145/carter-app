//! 按**单次工具调用**的粒度控制正在跑的工具。
//!
//! ## 它与另外两种「停」不是一回事
//!
//! | 动作 | 范围 | 结果 |
//! |---|---|---|
//! | 输入框上的「停止」 | 整轮 | 中断这一轮，模型不再继续 |
//! | 本模块的「中断命令」 | 一条工具调用 | 杀掉这条命令，把结果回灌给模型，**这一轮继续** |
//! | 本模块的「转后台」 | 一条工具调用 | 命令继续跑，工具立刻返回 `shell_id` |
//!
//! 后两个是界面上那条「运行中…」的 bash 卡片上那两颗按钮。没有它们的话，用户
//! 唯一的出路是整轮停止 —— 一条跑偏的 `brew install` 会顺带掐掉模型已经想好的
//! 后续步骤。
//!
//! ## 怎么把「界面上的 call_id」送到工具里
//!
//! `Tool::execute` 的签名里没有上下文，而工具是 `Arc<dyn Tool>` 全局共享的
//! （子 agent 也会拿到同一份）。所以走**任务局部变量**：[`Recorded`] 在调用内层
//! 工具之前把这次调用的控制句柄放进 task-local，工具执行时用 [`current`] 取出来。
//! 一个工具调用本来就在一个 tokio 任务里跑，并发调用各在自己的任务上，互不串。
//!
//! [`Recorded`]: super::record::Recorded

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use carter::agent::CancelToken;

use crate::protocol::ToolActionKind;

tokio::task_local! {
    /// 当前正在执行的那一次工具调用的控制句柄。
    static CURRENT: Arc<ToolControl>;
}

/// 工具执行时取自己的控制句柄。不在 [`Recorded`] 包装下执行时返回 `None`
/// （设置页展示工具、单测直接调工具都会走到这里）。
///
/// [`Recorded`]: super::record::Recorded
pub fn current() -> Option<Arc<ToolControl>> {
    CURRENT.try_with(|c| c.clone()).ok()
}

/// 单次工具调用的两个开关。
///
/// 用 `CancelToken` 而不是 oneshot：调用方只需要「按一下」，不需要传值；而且它
/// 天然支持「按之前工具已经结束了」—— 置位无害。
#[derive(Default)]
pub struct ToolControl {
    /// 这条调用在界面上的编号。`agent` 工具要拿它给自己的进度事件配对。
    call_id: String,
    interrupt: CancelToken,
    background: CancelToken,
}

impl ToolControl {
    /// 这条调用的编号（与 `tool_started` / `tool_finished` 里那个一致）。
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// 等「中断这条命令」被按下。
    pub async fn interrupted(&self) {
        self.interrupt.cancelled().await
    }

    /// 等「转后台」被按下。
    pub async fn backgrounded(&self) {
        self.background.cancelled().await
    }

    pub fn is_interrupted(&self) -> bool {
        self.interrupt.is_cancelled()
    }

    fn apply(&self, action: ToolActionKind) {
        match action {
            ToolActionKind::Interrupt => self.interrupt.set(),
            ToolActionKind::Background => self.background.set(),
        }
    }
}

/// 一条会话里正在跑的工具。
///
/// 键是 `Recorded` 发号的 `call_id` —— 与 `tool_started` / `tool_finished` 事件里
/// 那个 id 是同一个，界面按它回传操作。
#[derive(Default)]
pub struct RunningTools {
    entries: Mutex<HashMap<String, Arc<ToolControl>>>,
}

impl RunningTools {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn register(&self, call_id: &str) -> Arc<ToolControl> {
        let control = Arc::new(ToolControl {
            call_id: call_id.to_string(),
            ..Default::default()
        });
        self.entries
            .lock()
            .unwrap()
            .insert(call_id.to_string(), control.clone());
        control
    }

    fn finish(&self, call_id: &str) {
        self.entries.lock().unwrap().remove(call_id);
    }

    /// 界面上按了一颗按钮。返回 `false` = 这条调用已经结束了（或从没注册过）。
    pub fn apply(&self, call_id: &str, action: ToolActionKind) -> bool {
        match self.entries.lock().unwrap().get(call_id) {
            Some(c) => {
                c.apply(action);
                true
            }
            None => false,
        }
    }

    /// 正在跑的数量（测试与诊断用）。
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 把控制句柄放进 task-local 执行一段工具调用。
    pub(crate) async fn scope<F, T>(&self, call_id: &str, fut: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let control = self.register(call_id);
        let out = CURRENT.scope(control, fut).await;
        self.finish(call_id);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_pressed_button_reaches_the_running_call() {
        let running = RunningTools::new();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let r = running.clone();
        let task = tokio::spawn(async move {
            r.scope("t0", async move {
                let c = current().expect("在 scope 里必须拿得到控制句柄");
                tx.send(()).ok();
                c.interrupted().await;
                c.is_interrupted()
            })
            .await
        });

        // 等工具真的开始跑再按按钮（否则注册还没发生）。
        rx.await.unwrap();
        assert!(running.apply("t0", ToolActionKind::Interrupt));
        assert!(task.await.unwrap(), "按钮要真的叫醒工具");
        // 结束后就收走了 —— 再按一次应当被忽略。
        assert!(!running.apply("t0", ToolActionKind::Interrupt));
        assert!(running.is_empty());
    }

    /// scope 结束之后 task-local 不该再留下任何东西 —— 否则下一个工具会读到
    /// 上一条调用的控制句柄，按「中断」会打到已经结束的命令上。
    #[tokio::test]
    async fn the_control_handle_is_scoped_to_one_call() {
        let running = RunningTools::new();
        running.scope("t0", async {}).await;
        assert!(current().is_none());
    }

    #[test]
    fn pressing_a_button_for_an_unknown_call_is_harmless() {
        let running = RunningTools::new();
        assert!(!running.apply("nope", ToolActionKind::Background));
    }
}