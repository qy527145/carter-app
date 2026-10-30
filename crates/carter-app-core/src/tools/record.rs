//! 记录工具调用的包装层 —— 界面能做差异化渲染的前提。
//!
//! ## 为什么需要它
//!
//! carter 的 `UiEvent` 只带 `args_preview`（截断的字符串）和 `summary`
//! （**结果的第一行截断到 120 字符**）。于是界面上只能给每个工具画同一种卡片：
//! 一个名字、一行参数、一行结果。这就是「所有工具看起来都一样」的根因 ——
//! 不是样式问题，是**数据不够**。
//!
//! 这个包装层在工具外面套一层，把完整的参数 JSON、完整的结果正文、耗时都抓下来，
//! 通过自己的通道送给界面。不去改 carter：`Tool` trait 是公开的，包装一个
//! `Arc<dyn Tool>` 就够了。
//!
//! ## call_id 由谁定
//!
//! 由这里定。包装层就是被调用的那个对象，所以「开始」与「结束」是天然配对的 ——
//! 不需要去猜 carter 内部怎么给调用编号（并发工具调用时那是会错的）。
//!
//! ## 一个已知的空档
//!
//! 被 `PreToolUse` hook **阻断**的调用不会走到 `execute`，因此这里不会有卡片。
//! 目前本应用没有配置 hooks 的界面（hooks 只能写在 config.toml 里），所以这条
//! 路径实际上到不了；真到了再补。

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use serde_json::Value;
use tokio::sync::mpsc;

use carter::tools::{Tool, ToolResult};

use super::control::RunningTools;
use crate::shell::LiveLog;

/// 送给界面的工具事件。与 `protocol::SessionEvent` 的两条对应。
pub enum ToolEvent {
    Started {
        call_id: String,
        name: String,
        args: Value,
    },
    Finished {
        call_id: String,
        ok: bool,
        content: String,
        elapsed_ms: u64,
    },
}

/// 结果正文的上限。
///
/// bash 的结果本身可以到 30 KB，全量塞进 IPC 再存进前端 store，一个长会话下来
/// 会吃掉几十 MB 内存。16 KB 足够界面把关键部分画出来（卡片本身也只展示开头），
/// 超出的部分明确标注截断 —— 而不是悄悄丢。
const MAX_RESULT: usize = 16 * 1024;

pub struct Recorded {
    inner: Arc<dyn Tool>,
    tx: mpsc::UnboundedSender<ToolEvent>,
    seq: Arc<AtomicU64>,
    /// 会话级的调用计数。runner 在一轮前后各读一次，用来判断「这一轮模型到底
    /// 动没动手」—— 长任务目标下的空转检测靠它（见 `goal.rs::decide_after_turn`）。
    ///
    /// 放在这里而不是让 runner 去数事件：三条事件通道都被 `forward_events` 任务
    /// 占着，runner 自己看不到工具事件；而这里是**每个**工具（含 MCP、
    /// 含 `goal_update`）的必经之路。
    activity: Arc<AtomicU32>,
    /// 进行中那一轮的记录（侧边聊天读它，见 `shell.rs::LiveLog`）。
    live: Option<Arc<LiveLog>>,
    /// 正在跑的工具表。界面按 `call_id` 对单条命令做「中断 / 转后台」。
    running: Option<Arc<RunningTools>>,
}

impl Recorded {
    pub fn new(
        inner: Arc<dyn Tool>,
        tx: mpsc::UnboundedSender<ToolEvent>,
        seq: Arc<AtomicU64>,
        activity: Arc<AtomicU32>,
    ) -> Self {
        Self {
            inner,
            tx,
            seq,
            activity,
            live: None,
            running: None,
        }
    }

    pub fn with_live(mut self, live: Option<Arc<LiveLog>>) -> Self {
        self.live = live;
        self
    }

    pub fn with_running(mut self, running: Option<Arc<RunningTools>>) -> Self {
        self.running = running;
        self
    }
}

fn cap(content: &str) -> String {
    if content.len() <= MAX_RESULT {
        return content.to_string();
    }
    let mut end = MAX_RESULT;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…（结果过长，界面只显示前 {MAX_RESULT} 字节）", &content[..end])
}

#[async_trait::async_trait]
impl Tool for Recorded {
    // 三个元信息原样透传 —— 尤其是 `name`，`is_concurrent_safe` 与子 agent 的
    // 工具白名单都按名字判断，改一个字就会改变调度行为。
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> Value {
        self.inner.parameters()
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let call_id = format!("t{}", self.seq.fetch_add(1, Ordering::Relaxed));
        let started = Instant::now();
        self.activity.fetch_add(1, Ordering::Relaxed);

        if let Some(live) = &self.live {
            live.tool_started(&call_id, self.inner.name(), &args);
        }
        // 先发「开始」再执行：长时间跑的命令（构建、测试）应当立刻在界面上出现。
        let _ = self.tx.send(ToolEvent::Started {
            call_id: call_id.clone(),
            name: self.inner.name().to_string(),
            args: args.clone(),
        });

        // 把这次调用的控制句柄放进 task-local，工具（`bash`）执行时才能读到
        // 「用户按了中断 / 转后台」。包装层是唯一知道 `call_id` 的地方 —— 界面
        // 按钮回传的就是这个 id。
        let res = match &self.running {
            Some(running) => running.scope(&call_id, self.inner.execute(args)).await,
            None => self.inner.execute(args).await,
        };
        if let Some(live) = &self.live {
            live.tool_finished(&call_id, &res.content);
        }

        let _ = self.tx.send(ToolEvent::Finished {
            call_id,
            ok: res.ok,
            content: cap(&res.content),
            elapsed_ms: started.elapsed().as_millis() as u64,
        });

        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Echo(&'static str);

    #[async_trait::async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, args: Value) -> ToolResult {
            ToolResult::ok(format!("got: {args}"))
        }
    }

    /// 每个测试各用一份计数，免得并行跑时互相看见对方的数字。
    fn activity() -> Arc<AtomicU32> {
        Arc::new(AtomicU32::new(0))
    }

    fn recorded(name: &'static str) -> (Arc<dyn Tool>, mpsc::UnboundedReceiver<ToolEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let t: Arc<dyn Tool> = Arc::new(Recorded::new(
            Arc::new(Echo(name)),
            tx,
            Arc::new(AtomicU64::new(0)),
            activity(),
        ));
        (t, rx)
    }

    /// 元信息必须原样透传：`name` 一变，`is_concurrent_safe` 的判定与子 agent 的
    /// 工具白名单就跟着变 —— 那是调度行为，不是显示问题。
    #[test]
    fn metadata_passes_through_untouched() {
        let (t, _rx) = recorded("read_file");
        assert_eq!(t.name(), "read_file");
        assert_eq!(t.description(), "echo");
        assert!(t.parameters().is_object());
    }

    /// 一次调用产出配对的「开始 / 结束」，且参数与结果都是**完整的**。
    #[tokio::test]
    async fn a_call_emits_a_paired_start_and_finish_with_full_data() {
        let (t, mut rx) = recorded("grep");
        let args = json!({"pattern": "fn main", "path": "src"});
        let _ = t.execute(args.clone()).await;

        match rx.recv().await.unwrap() {
            ToolEvent::Started { call_id, name, args: a } => {
                assert_eq!(name, "grep");
                assert_eq!(a, args, "参数必须是完整的 JSON，不是预览字符串");
                // 记下 id 用于核对下一条。
                match rx.recv().await.unwrap() {
                    ToolEvent::Finished { call_id: id2, ok, content, .. } => {
                        assert_eq!(id2, call_id, "两条事件的 id 必须配对");
                        assert!(ok);
                        assert!(content.contains("fn main"), "{content}");
                    }
                    _ => panic!("期望 Finished"),
                }
            }
            _ => panic!("期望 Started"),
        }
    }

    /// 超长结果要截断并**明说**截断了 —— 悄悄丢一半解析起来会得到错误结论。
    #[tokio::test]
    async fn oversized_results_are_capped_with_a_visible_marker() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        struct Big;
        #[async_trait::async_trait]
        impl Tool for Big {
            fn name(&self) -> &str {
                "big"
            }
            fn description(&self) -> &str {
                ""
            }
            fn parameters(&self) -> Value {
                json!({})
            }
            async fn execute(&self, _: Value) -> ToolResult {
                ToolResult::ok("x".repeat(MAX_RESULT + 1000))
            }
        }
        let t: Arc<dyn Tool> = Arc::new(Recorded::new(
            Arc::new(Big),
            tx,
            Arc::new(AtomicU64::new(0)),
            activity(),
        ));
        let _ = t.execute(json!({})).await;
        let _ = rx.recv().await;
        match rx.recv().await.unwrap() {
            ToolEvent::Finished { content, .. } => {
                assert!(content.len() < MAX_RESULT + 200, "太长：{}", content.len());
                assert!(content.contains("界面只显示前"), "要标明截断了");
            }
            _ => panic!("期望 Finished"),
        }
    }

    /// 并发调用时 id 不能串。这是包装层自己发号的意义所在 ——
    /// 靠「上一条请求」配对在并发下必错。
    #[tokio::test]
    async fn concurrent_calls_get_distinct_ids() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let seq = Arc::new(AtomicU64::new(0));
        let act = activity();
        let mk = |name: &'static str| -> Arc<dyn Tool> {
            Arc::new(Recorded::new(
                Arc::new(Echo(name)),
                tx.clone(),
                seq.clone(),
                act.clone(),
            ))
        };
        let a = mk("grep");
        let b = mk("glob");
        let (ra, rb) = tokio::join!(a.execute(json!({})), b.execute(json!({})));
        assert!(ra.ok && rb.ok);
        assert_eq!(act.load(Ordering::Relaxed), 2, "两次调用都要被计到");

        let mut ids = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let ToolEvent::Started { call_id, .. } = ev {
                ids.push(call_id);
            }
        }
        ids.sort();
        assert_eq!(ids, vec!["t0".to_string(), "t1".to_string()]);
    }
}
