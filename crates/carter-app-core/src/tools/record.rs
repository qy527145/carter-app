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
//! 默认在这里定。包装层就是被调用的那个对象，所以「开始」与「结束」是天然配对的 ——
//! 不需要去猜 carter 内部怎么给调用编号（并发工具调用时那是会错的）。
//!
//! 会话自己派发调用时（`session.rs::Runner::run_one_call`）编号是现成的
//! （`ToolCall.id`），用 [`Recorded::with_call_id`] 指定；那时两张卡片由调用方
//! 自己发，这里只管执行。
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

use carter::tools::{Tool, ToolResult};

use super::control::RunningTools;
use crate::shell::LiveLog;

/// 送给界面的工具事件。与 `protocol::SessionEvent` 的两条对应。
#[derive(Clone)]
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
    /// 工具还在跑时产出的一段输出（**增量**，按时间先后拼起来就是到目前为止的输出）。
    ///
    /// 目前只有 `bash` 发：一条跑几分钟的构建，结束前卡片上什么都看不到的话，
    /// 用户分不清它是在干活还是卡死了。结果到达时以 `Finished` 的正文为准。
    Output { call_id: String, text: String },
    /// `agent` 工具里那几个子 agent 的进度（整表快照）。
    ///
    /// 不单独为「开始 / 结束」发事件：一张卡片上同时有好几行，整表替换比逐行增量
    /// 简单得多，也不会在并发写时拼错。
    Agent {
        call_id: String,
        agents: Vec<crate::protocol::AgentRun>,
    },
}

/// 工具事件送到哪去。
///
/// **同步**调用，而不是走 mpsc 再转一手：正文增量（`text_delta`）是产生那一刻直接
/// `emit` 的，工具卡片要是绕一条异步通道，转发那一拍就可能让「先这段字、后这张
/// 工具卡」的顺序反过来 —— 同一条时间线上两条路必须同序。
pub type ToolTx = Arc<dyn Fn(ToolEvent) + Send + Sync>;

/// 结果正文的上限。
///
/// bash 的结果本身可以到 30 KB，全量塞进 IPC 再存进前端 store，一个长会话下来
/// 会吃掉几十 MB 内存。16 KB 足够界面把关键部分画出来（卡片本身也只展示开头），
/// 超出的部分明确标注截断 —— 而不是悄悄丢。
const MAX_RESULT: usize = 16 * 1024;

pub struct Recorded {
    inner: Arc<dyn Tool>,
    tx: ToolTx,
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
    /// 编号用模型给的那个（`ToolCall.id`），拿不到才现编。
    ///
    /// carter 的 `Tool` trait 只拿得到参数、拿不到调用编号，所以默认这里自己发号。
    /// 但界面上的「中断 / 转后台」是按 `call_id` 找到那条命令的，而历史里的卡片编号
    /// 是模型给的 —— 两边对不上时，重开一次会话就再也点不动。carter 现在把这次调用的
    /// 编号放进了 task-local（`current_call_id`），主会话据此对齐；子 agent 仍然现编
    /// （它们模型给的编号跨 agent 可能重复）。
    model_call_id: bool,
}

impl Recorded {
    pub fn new(
        inner: Arc<dyn Tool>,
        tx: ToolTx,
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
            model_call_id: false,
        }
    }

    /// 编号用模型给的那个（见 [`Recorded::model_call_id`] 字段的说明）。
    pub fn with_model_call_id(mut self) -> Self {
        self.model_call_id = true;
        self
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

pub(crate) fn cap(content: &str) -> String {
    if content.len() <= MAX_RESULT {
        return content.to_string();
    }
    let mut end = MAX_RESULT;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n…（结果过长，界面只显示前 {MAX_RESULT} 字节）",
        &content[..end]
    )
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
        let call_id = self
            .model_call_id
            .then(carter::agent::current_call_id)
            .flatten()
            .unwrap_or_else(|| format!("t{}", self.seq.fetch_add(1, Ordering::Relaxed)));
        let started = Instant::now();
        self.activity.fetch_add(1, Ordering::Relaxed);

        if let Some(live) = &self.live {
            live.tool_started(&call_id, self.inner.name(), &args);
        }
        // 先发「开始」再执行：长时间跑的命令（构建、测试）应当立刻在界面上出现。
        (self.tx)(ToolEvent::Started {
            call_id: call_id.clone(),
            name: self.inner.name().to_string(),
            args: args.clone(),
        });

        // 把这次调用的控制句柄放进 task-local，工具（`bash`）执行时才能读到
        // 「用户按了中断 / 转后台」。包装层是唯一知道 `call_id` 的地方 —— 界面
        // 按钮回传的就是这个 id。
        let mut res = match &self.running {
            Some(running) => {
                let output: super::control::OutputSink = {
                    let tx = self.tx.clone();
                    let call_id = call_id.clone();
                    Arc::new(move |text| {
                        tx(ToolEvent::Output {
                            call_id: call_id.clone(),
                            text,
                        })
                    })
                };
                running
                    .scope(&call_id, Some(output), self.inner.execute(args))
                    .await
            }
            None => self.inner.execute(args).await,
        };
        // 这一轮进行中到的跨会话来信，搭这条结果送进去（见 `peers.rs`）。界面卡片上
        // 也照样带着 —— 模型看到了什么，用户就该看到什么。
        crate::peers::drain_into(&mut res.content);
        if let Some(live) = &self.live {
            live.tool_finished(&call_id, &res.content);
        }

        (self.tx)(ToolEvent::Finished {
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
    use tokio::sync::mpsc;

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

    /// 事件出口打成同步签名，测试照旧用一条通道观察。
    fn channel() -> (ToolTx, mpsc::UnboundedReceiver<ToolEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let tx: ToolTx = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });
        (tx, rx)
    }

    fn recorded(name: &'static str) -> (Arc<dyn Tool>, mpsc::UnboundedReceiver<ToolEvent>) {
        let (tx, rx) = channel();
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
            ToolEvent::Started {
                call_id,
                name,
                args: a,
            } => {
                assert_eq!(name, "grep");
                assert_eq!(a, args, "参数必须是完整的 JSON，不是预览字符串");
                // 记下 id 用于核对下一条。
                match rx.recv().await.unwrap() {
                    ToolEvent::Finished {
                        call_id: id2,
                        ok,
                        content,
                        ..
                    } => {
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
        let (tx, mut rx) = channel();
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
        let (tx, mut rx) = channel();
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
