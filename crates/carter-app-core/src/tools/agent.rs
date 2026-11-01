//! `agent` 工具：主对话一次派生多个子 agent，并发跑、各自独立上下文。
//!
//! ## 为什么不用 carter 自带的 `task`
//!
//! carter 的 `TaskTool` 其实已经支持 `prompts: [...]` 并发派生，但它在整段执行期间
//! **一个事件都不往外发**（收尾才把汇总文本交回去）。界面上就是一个转圈的卡片，
//! 跑二十分钟也看不出里面在干什么、有几个已经完成、卡在哪个子任务上。
//!
//! 这里换成自己的实现，参数与结果形状都更直白：
//!
//! ```json
//! { "tasks": [{ "description": "查依赖", "prompt": "……完整指令……" }] }
//! ```
//!
//! ## 进度怎么到界面上
//!
//! 每个子 agent 各有一个 [`ProgressSink`]，把「当前在跑哪个工具、已经几步、最近
//! 一段输出」写进共享的槽位，并顺手推一条 `ToolEvent::Agent`（整表快照，不是增量 ——
//! 并发写时增量很容易次序错乱，而这张表本来就只有几条）。界面据此画出一张会动的卡片。
//!
//! 子 agent 内部的**每一次**工具调用不单独上报：它们和主对话的卡片共用一条工具流，
//! 混进来的话主对话会被几十张子卡片冲得看不清。卡片里给「当前工具 + 步数」就够了。
//!
//! ## 并发与取消
//!
//! 每个子 agent 一个独立的 `tokio` 任务与自己的取消令牌；父令牌取消（用户按停止）
//! 或这张卡片被「中断」时，一并传下去。子 agent 的令牌**绝不**直接用父的 ——
//! `run_turn` 取消后会 reset，会把父信号一起清掉。
//!
//! ## 递归
//!
//! 子 agent 的工具池是主工具池去掉 `agent` / `goal_update` 之后的快照，所以子 agent
//! 不能再派生，也改不了父会话的目标状态。

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use carter::agent::ui::{CancelToken, UiEvent, UiSink};
use carter::agent::{RunOptions, Thread, TurnOutcome, run_turn};
use carter::config::AgentConfig;
use carter::provider::{LlmProvider, RequestPurpose};
use carter::registry::ModelInfo;
use carter::tools::{Tool, ToolRegistry, ToolResult};

use super::control;
use super::record::ToolEvent;
use crate::protocol::AgentRun;

/// 子 agent 的轮数上限（与 carter 一致，独立于主 agent，防失控）。
const SUBAGENT_MAX_TURNS: u32 = 20;
/// 进度事件的最小间隔。文本增量一个 token 一条，全转发会把 IPC 打满。
const PROGRESS_GAP: Duration = Duration::from_millis(120);
/// 卡片里显示的「最近输出」长度。
const TAIL: usize = 600;

pub struct AgentTool {
    provider: Arc<dyn LlmProvider>,
    model: ModelInfo,
    agent_cfg: AgentConfig,
    /// 子 agent 的 system，与主对话同一份（人设、工程纪律、工作目录说明）。
    base_system: String,
    parent_cancel: CancelToken,
    /// 子 agent 的工具池（已换实现、已过开关、不含 `agent` / `goal_update`）。
    tools: Vec<Arc<dyn Tool>>,
    /// 进度事件的出口。
    tx: tokio::sync::mpsc::UnboundedSender<ToolEvent>,
    /// 子 agent 的调用计数（runner 的空转检测要用），与主工具池同一个计数。
    activity: Arc<AtomicU32>,
    seq: Arc<AtomicU64>,
}

impl AgentTool {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: ModelInfo,
        agent_cfg: AgentConfig,
        base_system: String,
        parent_cancel: CancelToken,
        tools: Vec<Arc<dyn Tool>>,
        tx: tokio::sync::mpsc::UnboundedSender<ToolEvent>,
        activity: Arc<AtomicU32>,
    ) -> Self {
        Self {
            provider,
            model,
            agent_cfg,
            base_system,
            parent_cancel,
            tools,
            tx,
            activity,
            seq: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// 一个子任务的入口参数。
#[derive(Debug)]
struct Task {
    description: String,
    prompt: String,
}

fn parse_tasks(args: &Value) -> Result<Vec<Task>, ToolResult> {
    let Some(arr) = args.get("tasks").and_then(Value::as_array) else {
        return Err(ToolResult::err(
            "缺少 tasks 参数：传 `tasks: [{ \"description\": \"…\", \"prompt\": \"…\" }]`（一个也行）",
        ));
    };
    if arr.is_empty() {
        return Err(ToolResult::err("tasks 是空的，至少要一个子任务"));
    }
    if arr.len() > 8 {
        return Err(ToolResult::err(format!(
            "一次最多派生 8 个子 agent（收到 {} 个）。拆成几轮，或者把相近的合成一个",
            arr.len()
        )));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, t) in arr.iter().enumerate() {
        let prompt = t.get("prompt").and_then(Value::as_str).unwrap_or("");
        if prompt.trim().is_empty() {
            return Err(ToolResult::err(format!("第 {} 个子任务没有 prompt", i + 1)));
        }
        let description = t
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("子任务 {}", i + 1));
        out.push(Task {
            description,
            prompt: prompt.to_string(),
        });
    }
    Ok(out)
}

#[async_trait::async_trait]
impl Tool for AgentTool {
    fn name(&self) -> &str {
        "agent"
    }

    fn description(&self) -> &str {
        "派生多个隔离子 agent 并行完成任务。每个子 agent 有独立的上下文与完整的工具池，\
         自己读文件、跑命令，最后只把结论交回来 —— 主对话不必装下它们翻过的东西。\n\n\
         什么时候用：\n\
         - 好几个互不依赖的调研 / 搜索 / 改造可以同时做（一次 tasks 里全给它们）；\n\
         - 某件事要翻很多文件、试很多次，过程对主对话没用，只要结论。\n\
         什么时候别用：几件事有先后依赖（那就一件一件做，或者自己直接做）。\n\n\
         每个子任务的 prompt 要**自包含**：它看不到主对话，不知道你们刚聊过什么。\
         把需要的路径、约定、输出格式写清楚。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 8,
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": {
                                "type": "string",
                                "description": "这一行的短标题（几字，显示在界面上）"
                            },
                            "prompt": {
                                "type": "string",
                                "description": "完整、自包含的指令：要看哪些文件、做什么、最后输出什么"
                            }
                        },
                        "required": ["description", "prompt"]
                    },
                    "description": "要并行跑的子任务。只有一个时也用它。"
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "子 agent 可用的工具白名单（如 [\"read_file\",\"grep\"]）。缺省 = 全部。"
                }
            },
            "required": ["tasks"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let tasks = match parse_tasks(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        // 白名单按名字过滤；空数组视为不限制（模型偶尔会传 [] 表示「默认」）。
        let whitelist: Option<Vec<String>> = args
            .get("tools")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect());
        let tools: Vec<Arc<dyn Tool>> = match &whitelist {
            Some(wl) => self
                .tools
                .iter()
                .filter(|t| wl.iter().any(|n| n == t.name()))
                .cloned()
                .collect(),
            None => self.tools.clone(),
        };
        // 白名单是模型自己写的，写错名字时静默跑一个没工具的子 agent 最难查。
        // 主工具池本身为空（侧边会话之类）不算错 —— 纯推理的子任务照样能跑。
        if whitelist.is_some() && tools.is_empty() {
            return ToolResult::err("tools 白名单里的工具一个都不存在，子 agent 无事可做");
        }

        let call_id = control::current()
            .map(|c| c.call_id().to_string())
            .unwrap_or_else(|| format!("agent{}", self.seq.fetch_add(1, Ordering::Relaxed)));

        let runs: Arc<Mutex<Vec<AgentRun>>> = Arc::new(Mutex::new(
            tasks
                .iter()
                .map(|t| AgentRun {
                    description: t.description.clone(),
                    status: "running".into(),
                    steps: 0,
                    tool: None,
                    text: String::new(),
                })
                .collect(),
        ));
        // 整表快照，不是增量：并发写时增量很容易次序错乱，而这张表只有几条。
        let publish = |runs: &Arc<Mutex<Vec<AgentRun>>>| {
            let snapshot = runs.lock().map(|v| v.clone()).unwrap_or_default();
            let _ = self.tx.send(ToolEvent::Agent {
                call_id: call_id.clone(),
                agents: snapshot,
            });
        };
        publish(&runs);

        // 卡片上的「中断」与整轮停止都往下传。
        let child_root = CancelToken::new();
        {
            let parent = self.parent_cancel.clone();
            let root = child_root.clone();
            tokio::spawn(async move {
                parent.cancelled().await;
                root.set();
            });
            if let Some(ctl) = control::current() {
                let root = child_root.clone();
                tokio::spawn(async move {
                    ctl.interrupted().await;
                    root.set();
                });
            }
        }

        let mut handles = Vec::with_capacity(tasks.len());
        for (i, task) in tasks.into_iter().enumerate() {
            let provider = self.provider.clone();
            let model = self.model.clone();
            let mut cfg = self.agent_cfg.clone();
            cfg.max_turns = SUBAGENT_MAX_TURNS;
            let mut system = self.base_system.clone();
            // 子 agent 看不到主对话，得先知道自己是谁、在哪 —— 补一句，也免得它
            // 以为「刚才说的那几个文件」还在上下文里。
            system.push_str(
                "\n\n## 当前身份\n\n\
                 你是一个被派出来的子 agent。你看不到主对话的历史，只有下面这一段任务。\
                 做完就停，最后一条消息要能直接当作结论交回去（写清做了什么、看到了什么、\
                 还差什么），不要问问题 —— 没有人能回答你。",
            );
            let tools = tools.clone();
            let runs = runs.clone();
            let cancel = {
                // 每个子 agent 一个独立令牌；父令牌置位时一起停。
                let c = CancelToken::new();
                let watch = c.clone();
                let root = child_root.clone();
                tokio::spawn(async move {
                    root.cancelled().await;
                    watch.set();
                });
                c
            };
            let activity = self.activity.clone();
            let tx = self.tx.clone();
            let call_id = call_id.clone();
            handles.push(tokio::spawn(async move {
                let mut sink = ProgressSink {
                    runs: runs.clone(),
                    index: i,
                    tx,
                    call_id,
                    steps: 0,
                    buf: String::new(),
                    last: String::new(),
                    last_emit: Instant::now() - PROGRESS_GAP,
                    activity,
                };
                let out = run_one(
                    task, system, provider, model, cfg, tools, cancel, &mut sink,
                )
                .await;
                // 收尾状态写回表里 —— 失败与中断的也要能看出来，而不是一直转圈。
                let (status, text) = match &out {
                    Ok((t, _)) => ("done", ProgressSink::tail_static(t)),
                    Err(e) => (
                        if e.contains("中断") { "cancelled" } else { "failed" },
                        ProgressSink::tail_static(e),
                    ),
                };
                sink.set(|run| {
                    run.status = status.into();
                    run.tool = None;
                    if !text.is_empty() {
                        run.text = text;
                    }
                });
                sink.publish();
                (i, out)
            }));
        }

        let mut results: Vec<(usize, Result<(String, u32), String>)> = Vec::new();
        for h in handles {
            match h.await {
                Ok(v) => results.push(v),
                Err(e) => results.push((usize::MAX, Err(format!("子 agent 任务异常退出：{e}")))),
            }
        }
        results.sort_by_key(|(i, _)| *i);
        publish(&runs);

        let mut combined = String::new();
        let mut any_err = false;
        for (i, r) in results {
            if !combined.is_empty() {
                combined.push_str("\n\n");
            }
            let label = runs
                .lock()
                .ok()
                .and_then(|v| v.get(i).map(|a| a.description.clone()))
                .unwrap_or_else(|| format!("子任务 {}", i + 1));
            combined.push_str(&format!("--- subagent {}：{label} ---\n", i + 1));
            match r {
                Ok((text, _)) => combined.push_str(&text),
                Err(msg) => {
                    any_err = true;
                    combined.push_str(&format!("失败：{msg}"));
                }
            }
        }
        if any_err {
            ToolResult::err(combined)
        } else {
            ToolResult::ok(combined)
        }
    }
}

/// 跑一个子 agent。返回（结论正文，步数）。
#[allow(clippy::too_many_arguments)]
async fn run_one(
    task: Task,
    system: String,
    provider: Arc<dyn LlmProvider>,
    model: ModelInfo,
    cfg: AgentConfig,
    tools: Vec<Arc<dyn Tool>>,
    cancel: CancelToken,
    sink: &mut ProgressSink,
) -> Result<(String, u32), String> {
    let mut registry = ToolRegistry::empty();
    for t in tools {
        registry.push(t);
    }

    let run_opts = RunOptions {
        show_thinking: false,
        system: vec![system],
        compact_model: None,
        hooks: Arc::new(carter::hooks::HookRegistry::default()),
        purpose: RequestPurpose::Subagent,
    };

    let mut thread = Thread::new_empty();
    thread.append_user(task.prompt);

    let out = run_turn(
        &mut thread,
        &*provider,
        &model,
        &cfg,
        &run_opts,
        &registry,
        sink,
        &cancel,
    )
    .await;

    match out {
        Err(e) => Err(format!("跑子 agent 出错：{e}")),
        Ok((TurnOutcome::Cancelled, _)) => Err("已中断".into()),
        Ok(_) => {
            let text = sink.final_text();
            if text.trim().is_empty() {
                Err("子 agent 没有产出任何正文".into())
            } else {
                Ok((text, sink.steps))
            }
        }
    }
}

/// 把子 agent 的动态写进共享槽位，并节流地推事件。
struct ProgressSink {
    runs: Arc<Mutex<Vec<AgentRun>>>,
    index: usize,
    tx: tokio::sync::mpsc::UnboundedSender<ToolEvent>,
    call_id: String,
    steps: u32,
    buf: String,
    last: String,
    last_emit: Instant,
    activity: Arc<AtomicU32>,
}

impl ProgressSink {
    fn final_text(&self) -> String {
        if !self.last.is_empty() {
            self.last.clone()
        } else {
            self.buf.clone()
        }
    }

    /// 推一次整表快照。
    fn publish(&self) {
        let snapshot = self.runs.lock().map(|v| v.clone()).unwrap_or_default();
        let _ = self.tx.send(ToolEvent::Agent {
            call_id: self.call_id.clone(),
            agents: snapshot,
        });
    }

    /// 改一行，立刻推（工具切换、收尾这类关键节点不值得省）。
    fn set(&mut self, f: impl FnOnce(&mut AgentRun)) {
        if let Ok(mut v) = self.runs.lock() {
            if let Some(run) = v.get_mut(self.index) {
                f(run);
            }
        }
        self.publish();
    }

    /// 改一行，但最多每 [`PROGRESS_GAP`] 推一次（文本增量走这里）。
    fn maybe(&mut self, f: impl FnOnce(&mut AgentRun)) {
        if let Ok(mut v) = self.runs.lock() {
            if let Some(run) = v.get_mut(self.index) {
                f(run);
            }
        }
        if self.last_emit.elapsed() >= PROGRESS_GAP {
            self.last_emit = Instant::now();
            self.publish();
        }
    }

    fn tail_static(text: &str) -> String {
        if text.len() <= TAIL {
            return text.to_string();
        }
        let mut start = text.len() - TAIL;
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        format!("…{}", &text[start..])
    }
}

impl UiSink for ProgressSink {
    fn emit(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::AssistantTextDelta(t) => {
                self.buf.push_str(&t);
                let tail = Self::tail_static(&self.buf);
                self.maybe(|run| {
                    run.text = tail;
                });
            }
            UiEvent::AssistantTextEnd => {
                if !self.buf.is_empty() {
                    self.last = std::mem::take(&mut self.buf);
                    let tail = Self::tail_static(&self.last);
                    self.set(|run| run.text = tail);
                }
            }
            UiEvent::ToolCallStarted { name, .. } => {
                self.steps += 1;
                self.activity.fetch_add(1, Ordering::Relaxed);
                let steps = self.steps;
                self.set(|run| {
                    run.steps = steps;
                    run.tool = Some(name);
                });
            }
            UiEvent::ToolResult { .. } => {
                self.set(|run| run.tool = None);
            }
            UiEvent::Notice(n) => {
                let tail = Self::tail_static(&n);
                self.set(|run| run.text = tail);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use carter::provider::{ChatRequest, EventStream, FinishReason, StreamChunk};

    fn tasks(v: Value) -> Result<Vec<Task>, ToolResult> {
        parse_tasks(&v)
    }

    #[test]
    fn one_task_is_enough() {
        let t = tasks(json!({"tasks": [{"prompt": "读一下 a.rs"}]})).unwrap();
        assert_eq!(t.len(), 1);
        // 没给标题时也要有一行可显示的。
        assert!(!t[0].description.is_empty());
    }

    #[test]
    fn empty_or_malformed_tasks_are_rejected_with_a_usable_message() {
        assert!(tasks(json!({})).is_err());
        assert!(tasks(json!({"tasks": []})).is_err());
        // 空 prompt 比缺 prompt 更容易发生 —— 模型写了个只剩 description 的条目。
        let e = tasks(json!({"tasks": [{"description": "x", "prompt": "  "}]})).unwrap_err();
        assert!(e.content.contains("第 1 个"), "{}", e.content);
    }

    /// 上限不是安全阀，是「一次派太多没人看得过来」的提醒。
    #[test]
    fn more_than_eight_is_refused() {
        let many: Vec<Value> = (0..9).map(|i| json!({"prompt": format!("t{i}")})).collect();
        let e = tasks(json!({ "tasks": many })).unwrap_err();
        assert!(e.content.contains("8"), "{}", e.content);
    }

    /// 假 provider：每次 stream 直接给一段固定正文，不调工具。
    struct Fixed(String);
    #[async_trait::async_trait]
    impl LlmProvider for Fixed {
        async fn stream(&self, _req: ChatRequest) -> carter::Result<EventStream> {
            let text = self.0.clone();
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamChunk::TextDelta { index: 0, text }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                }),
            ])))
        }
    }

    fn tool(provider: Arc<dyn LlmProvider>) -> (AgentTool, tokio::sync::mpsc::UnboundedReceiver<ToolEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let model = carter::registry::ModelInfo {
            key: "k".into(),
            provider: "p".into(),
            api_name: "m".into(),
            context_window: 100_000,
            max_output_tokens: 8_000,
            tokenizer: "cl100k".into(),
            capabilities: vec![],
            pricing: carter::registry::Pricing {
                input: 0.0,
                output: 0.0,
                cache_write: None,
                cache_read: None,
            },
            default_reasoning: None,
        };
        let t = AgentTool::new(
            provider,
            model,
            AgentConfig::default(),
            "system".into(),
            CancelToken::new(),
            Vec::new(),
            tx,
            Arc::new(AtomicU32::new(0)),
        );
        (t, rx)
    }

    /// 多个子任务并发跑完，按**声明顺序**合并，每段带自己的标题。
    #[tokio::test]
    async fn parallel_tasks_come_back_in_order_and_progress_is_emitted() {
        let (t, mut rx) = tool(Arc::new(Fixed("结论".into())));
        let res = t
            .execute(json!({"tasks": [
                {"description": "甲", "prompt": "a"},
                {"description": "乙", "prompt": "b"},
            ]}))
            .await;
        assert!(res.ok, "{}", res.content);
        assert_eq!(res.content.matches("结论").count(), 2);
        assert!(res.content.find("甲").unwrap() < res.content.find("乙").unwrap());
        // 至少一条进度：开始就有整表快照。
        assert!(matches!(rx.recv().await, Some(ToolEvent::Agent { .. })));
    }

    /// 没有工具池时白名单直接拒绝 —— 否则子 agent 跑一轮什么都做不了，
    /// 报错要看半天才知道是配置问题。
    #[tokio::test]
    async fn an_empty_whitelist_is_reported_early() {
        let (t, _rx) = tool(Arc::new(Fixed("x".into())));
        let res = t
            .execute(json!({"tasks": [{"prompt": "a"}], "tools": ["read_file"]}))
            .await;
        assert!(!res.ok);
        assert!(res.content.contains("白名单"), "{}", res.content);
    }
}
