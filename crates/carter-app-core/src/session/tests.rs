use super::runner::Runner;
use super::*;

/// 排队操作的结果必须能一眼看出来：插队要真的到队首，撤回要真的删掉，
/// 而且都不能误伤别的命令。
#[cfg(test)]
mod queue_tests {
    use super::*;

    pub(super) fn prompt(id: u64) -> Command {
        Command::SendPrompt {
            text: format!("m{id}"),
            id: Some(id),
            interject: false,
        }
    }

    pub(super) fn ids(q: &VecDeque<Command>) -> Vec<Option<u64>> {
        q.iter()
            .map(|c| match c {
                Command::SendPrompt { id, .. } => *id,
                _ => None,
            })
            .collect()
    }

    #[test]
    pub(super) fn promoting_moves_only_that_message_to_the_front() {
        let mut q = VecDeque::from(vec![prompt(1), Command::Cancel, prompt(2), prompt(3)]);
        promote(&mut q, 3);
        assert_eq!(ids(&q), vec![Some(3), Some(1), None, Some(2)]);
    }

    #[test]
    pub(super) fn dropping_removes_exactly_one_and_is_harmless_if_unknown() {
        let mut q = VecDeque::from(vec![prompt(1), prompt(2), prompt(3)]);
        drop_queued(&mut q, 2);
        assert_eq!(ids(&q), vec![Some(1), Some(3)]);
        // 已经开始跑（或已经撤回过）的编号再点一次不该出错。
        drop_queued(&mut q, 99);
        assert_eq!(ids(&q), vec![Some(1), Some(3)]);
    }
}

#[cfg(test)]
mod live_close {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    pub(super) async fn live_close_kills_a_running_bash_promptly() {
        assert!(std::env::var_os("CARTER_HOME").is_some());
        let shell = AppShell::new(carter::config::Config::load().unwrap());
        let dir =
            std::env::temp_dir().join(format!("carter-live-close-{}", crate::protocol::now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut rx = shell.subscribe();
        let started = start_session(
            shell.clone(),
            SessionOptions {
                cwd: dir.clone(),
                model: None,
                resume_latest: false,
                resume_id: None,
                new_id: None,
                title: None,
            },
        )
        .unwrap();
        let id = started.handle.id.clone();
        started.handle.send(Command::SendPrompt {
            text: "用 bash 工具执行 `sleep 40 && echo done`，前台执行，不要后台。".into(),
            id: None,
            interject: false,
        });
        loop {
            match tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .unwrap()
                .unwrap()
            {
                AppEvent::Session {
                    event: SessionEvent::ToolStarted { name, .. },
                    ..
                } if name == "bash" => break,
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let t = Instant::now();
        shell.close_session(&id).await;
        let waited = t.elapsed();
        println!(
            "close_session 返回用了 {waited:?}，done={}",
            started.handle.done.is_cancelled()
        );
        let t = Instant::now();
        while !started.handle.done.is_cancelled() && t.elapsed() < Duration::from_secs(60) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!("runner 真正退出又过了 {:?}", t.elapsed());
        assert!(started.handle.done.is_cancelled());
        assert!(
            waited < Duration::from_secs(5),
            "关会话要在 5 秒内收掉正在跑的 bash"
        );
    }
}

#[cfg(test)]
mod start_session_tests {
    use super::*;

    /// 同步上下文里调用 `start_session` 必须拿到一句能读懂的错，而不是 panic。
    ///
    /// 这条守的是一个真实踩过的坑：Tauri 的**同步**命令跑在主线程上、没有 tokio
    /// runtime，而 `start_session` 要 spawn 会话任务。当时的表现是窗口一亮就闪退，
    /// panic 信息是 "there is no reactor running" —— 从现象完全看不出是命令该写成
    /// async。加这道检查后，同样的错误会自己说清楚。
    ///
    /// 用普通 `#[test]` 而不是 `#[tokio::test]`：前者没有 runtime 上下文，正是要测的场景。
    #[test]
    pub(super) fn start_session_without_a_runtime_says_so_instead_of_panicking() {
        let shell = AppShell::new(carter::config::Config::default());
        let opts = SessionOptions {
            cwd: std::path::PathBuf::from("."),
            model: None,
            resume_latest: false,
            resume_id: None,
            new_id: None,
            title: None,
        };
        // 不用 `expect_err`：`StartedSession` 没实现 Debug，而且这里关心的是「返回 Err」。
        let msg = match start_session(shell, opts) {
            Ok(_) => panic!("没有 runtime 时应当报错而不是成功"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("tokio runtime"),
            "错误信息要指明根因，实际是：{msg}"
        );
    }

    /// 动作在当前状态下没有意义时，提示必须说清是哪种「没意义」。
    #[test]
    pub(super) fn a_goal_no_op_explains_itself() {
        let none = goal_noop_note(&GoalCommand::Pause, None);
        assert!(none.contains("/goal"), "要告诉用户怎么设：{none}");

        let g = Goal::new("x");
        assert!(goal_noop_note(&GoalCommand::Pause, Some(&g)).contains("不在进行中"));
        let paused = {
            let mut g = g.clone();
            g.status = GoalStatus::Paused;
            g
        };
        assert!(goal_noop_note(&GoalCommand::Resume, Some(&paused)).contains("已经在推进"));
    }

    /// 续接历史会话时按会话自己记的目录跑；目录没了就退回请求里的 cwd。
    ///
    /// 不这么做的话，从「会话」页点开一条别的项目下的会话，会拿家目录去当工作目录 ——
    /// 工具、系统提示、git 快照全错位，而且界面上只表现为「工作目录跳了」。
    #[test]
    pub(super) fn a_resumed_session_keeps_its_own_directory_and_falls_back_when_its_gone() {
        let mine = std::env::temp_dir().join(format!(
            "carter-app-resume-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&mine).unwrap();

        let requested = std::path::Path::new("/definitely/not/here");
        let recorded = mine.to_string_lossy().to_string();
        assert_eq!(resumed_cwd(&recorded, requested), mine);

        // 记下来的目录已经不在了：别把会话卡死，退回去继续能用。
        let gone = mine.join("deleted-just-now");
        assert_eq!(
            resumed_cwd(&gone.to_string_lossy(), requested),
            requested.to_path_buf()
        );

        std::fs::remove_dir_all(&mine).unwrap();
    }

    /// 一次工具调用只能画**一张**卡，编号用模型给的那个。
    ///
    /// 守的是「明明只调了一次，界面上却有两行一模一样的 `bash`」那个 bug：工具池与
    /// 回调层各套一层 `Recorded` 的话，同一句命令会画两张卡，一张用模型的 `call_id`、
    /// 一张用现编的号，两张还都能拿到结果、都打勾。主会话只在 `wrap_recorded` 这一处
    /// 套（`build_tools` 返回的是裸工具），所以这里数一遍就够。
    #[tokio::test]
    pub(super) async fn one_dispatch_draws_exactly_one_card() {
        struct Echo;
        #[async_trait::async_trait]
        impl Tool for Echo {
            fn name(&self) -> &str {
                "bash"
            }
            fn description(&self) -> &str {
                "echo"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(&self, _args: serde_json::Value) -> carter::tools::ToolResult {
                carter::tools::ToolResult::ok("ok")
            }
        }

        let mut bare = carter::tools::ToolRegistry::empty();
        bare.push(Arc::new(Echo));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink: crate::tools::record::ToolTx = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });
        let pool = wrap_recorded(bare, sink, None, None, Arc::new(AtomicU32::new(0)));
        assert_eq!(pool.tools().len(), 1, "一个工具只该被套一层");
        let _ = pool.dispatch("bash", serde_json::json!({})).await;

        let mut started = Vec::new();
        let mut finished = 0;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ToolEvent::Started { call_id, .. } => started.push(call_id),
                ToolEvent::Finished { .. } => finished += 1,
                ToolEvent::Agent { .. } => {}
            }
        }
        assert_eq!(started.len(), 1, "一次调用只该画一张卡：{started:?}");
        assert_eq!(finished, 1);
    }
}

#[cfg(test)]
mod no_turn_cap {
    /// 一轮里能调多少次模型，不该由配置拍脑袋定：翻几十个文档的工单很容易超过
    /// carter 默认的 50，撞上之后剩下的工具一条都不执行。
    ///
    /// 现在不再自己驱动循环，这条守的是「交给 carter 的上限被抬掉了」。
    #[test]
    pub(super) fn the_running_loop_has_no_model_call_cap() {
        let src = include_str!("turn.rs");
        let body = src
            .split("async fn run_one_turn(")
            .nth(1)
            .and_then(|s| s.split("async fn drive_goal(").next())
            .expect("找得到 run_one_turn");
        assert!(
            body.contains("agent.max_turns = u32::MAX"),
            "交给 carter 的 agent 配置必须把轮数上限抬掉"
        );
        assert!(
            !body.contains("self.ctx.config.agent.max_turns"),
            "run_one_turn 里不该读配置里的上限"
        );
    }
}
#[cfg(test)]
mod live_commands {
    use super::*;
    use crate::protocol::ToolActionKind;

    /// 一条能调 [`handle_live_command`] 的最小会话上下文：不碰网络、不建工具池。
    pub(super) fn live_ctx() -> SessionCtx {
        struct Quiet;
        #[async_trait::async_trait]
        impl carter::provider::LlmProvider for Quiet {
            async fn stream(
                &self,
                _req: carter::provider::ChatRequest,
            ) -> carter::Result<carter::provider::EventStream> {
                unreachable!("这条测试不发请求")
            }
        }

        let dir = std::env::temp_dir();
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
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let handle = SessionHandle::new(
            "live-cmd-test".into(),
            dir.clone(),
            model.clone(),
            None,
            cmd_tx,
        );
        SessionCtx {
            shell: AppShell::new(Config::default()),
            handle: handle.clone(),
            model,
            provider: Arc::new(Quiet),
            config: Arc::new(Config::default()),
            settings: AppSettings::default(),
            resumed: false,
            cwd: dir.clone(),
            unsupported_server_tools: Vec::new(),
            capture: None,
            include_task: false,
            fast: None,
            fast_ref: None,
            shells: handle.background.clone(),
            shell_cwd: crate::tools::bash::ShellCwd::new(dir),
            running: crate::tools::control::RunningTools::new(),
        }
    }

    pub(super) fn prompt(id: u64, interject: bool) -> Command {
        Command::SendPrompt {
            text: format!("m{id}"),
            id: Some(id),
            interject,
        }
    }

    pub(super) fn ids(q: &VecDeque<Command>) -> Vec<Option<u64>> {
        q.iter()
            .map(|c| match c {
                Command::SendPrompt { id, .. } => *id,
                _ => None,
            })
            .collect()
    }

    /// 这一条守的就是「界面上点『转后台』没反应」：在**一轮正在跑**的时候把命令
    /// 送进 `handle_live_command`，它必须当场打到那条正在执行的工具调用上。
    #[tokio::test]
    pub(super) async fn a_tool_action_mid_turn_reaches_the_running_call() {
        let ctx = live_ctx();
        let (tx, rx) = tokio::sync::oneshot::channel();

        let running = ctx.running.clone();
        let task = tokio::spawn(async move {
            running
                .scope("call_1", async move {
                    let control = crate::tools::control::current().expect("scope 里应有控制句柄");
                    tx.send(()).ok();
                    control.backgrounded().await;
                    control.call_id().to_string()
                })
                .await
        });

        // 等工具真的注册进去，再按按钮 —— 与界面上那条命令跑起来之后才出现按钮一样。
        rx.await.unwrap();
        let mut queued = VecDeque::new();
        let (mut goal, mut changes) = (None, None);
        let cancellable = handle_live_command(
            Command::ToolAction {
                call_id: "call_1".into(),
                action: ToolActionKind::Background,
            },
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );

        assert_eq!(task.await.unwrap(), "call_1", "按钮要打到正在跑的那一条上");
        assert!(!cancellable, "工具操作不是「停下这一轮」");
        assert!(queued.is_empty(), "工具操作不该进排队");
    }

    /// 找不到对应调用的按钮不算错误：用户点下去与命令结束可能撞在同一毫秒。
    #[test]
    pub(super) fn a_tool_action_for_a_finished_call_is_dropped_quietly() {
        let ctx = live_ctx();
        let mut queued = VecDeque::new();
        let (mut goal, mut changes) = (None, None);
        handle_live_command(
            Command::ToolAction {
                call_id: "never-existed".into(),
                action: ToolActionKind::Interrupt,
            },
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(queued.is_empty());
    }

    /// 生成中点「停止」要立刻断流（置取消标记），不能等这一轮跑完。
    #[test]
    pub(super) fn cancel_mid_turn_sets_the_token_immediately() {
        let ctx = live_ctx();
        assert!(!ctx.handle.cancel.is_cancelled());
        let mut queued = VecDeque::new();
        let (mut goal, mut changes) = (None, None);
        let cancellable = handle_live_command(
            Command::Cancel,
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(cancellable, "停止是「停下这一轮」");
        assert!(ctx.handle.cancel.is_cancelled());
        assert!(queued.is_empty(), "取消不是一条要排队的消息");
    }

    /// 插队 = 中断本轮 + 排到队首；普通发送只是排队。
    #[test]
    pub(super) fn interjecting_cancels_and_jumps_the_queue() {
        let ctx = live_ctx();
        let mut queued = VecDeque::from(vec![prompt(1, false)]);
        let (mut goal, mut changes) = (None, None);

        let cancellable = handle_live_command(
            prompt(2, true),
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(cancellable, "插队要打断这一轮");
        assert_eq!(ids(&queued), vec![Some(2), Some(1)]);
        assert!(ctx.handle.cancel.is_cancelled());

        ctx.handle.cancel.reset();
        let mut queued = VecDeque::from(vec![prompt(1, false)]);
        let cancellable = handle_live_command(
            prompt(3, false),
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(!cancellable, "普通发送不该打断这一轮");
        assert_eq!(ids(&queued), vec![Some(1), Some(3)]);
        assert!(!ctx.handle.cancel.is_cancelled(), "普通发送不该打断这一轮");
    }

    /// 其它命令（压缩、开新一期…）照旧排队，等这一轮跑完再处理。
    #[test]
    pub(super) fn everything_else_waits_for_the_turn_to_end() {
        let ctx = live_ctx();
        let mut queued = VecDeque::new();
        let (mut goal, mut changes) = (None, None);
        handle_live_command(
            Command::Compact,
            &ctx,
            &mut queued,
            &mut goal,
            &mut changes,
            0,
        );
        assert!(matches!(queued.front(), Some(Command::Compact)));
    }
}

#[cfg(test)]
mod live_turn {
    //! 一轮**真的在跑**的时候，界面上的命令要当场生效。
    //!
    //! 这里驱动的是完整的 `run_one_turn`（内部就是 carter 的 `run_turn`），只是不走
    //! `run_sequence` —— 于是不碰标题、`prompts/`、`timeline/` 这些落盘的东西，测试
    //! 可以随便跑，也不会在用户机器上留下文件。
    //!
    //! 守的是「点『转后台』没反应」：那条命令必须打到正在跑的工具调用上，工具立刻
    //! 返回 `shell_id` 而不是等满整条命令。

    use super::*;
    use carter::provider::{ChatRequest, EventStream, FinishReason, StreamChunk};
    use std::time::Duration;

    /// 第一次请求：让模型调 `bash` 跑一条很久的命令。第二次：说一句话收尾。
    struct Scripted(std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl carter::provider::LlmProvider for Scripted {
        async fn stream(&self, _req: ChatRequest) -> carter::Result<EventStream> {
            use std::sync::atomic::Ordering;
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            let chunks: Vec<carter::Result<StreamChunk>> = if n == 0 {
                vec![
                    Ok(StreamChunk::ToolCallDelta {
                        index: 0,
                        id: "call_from_model".into(),
                        name_delta: Some("bash".into()),
                        arguments_delta: Some(
                            serde_json::json!({ "command": "sleep 60", "timeout": 120 })
                                .to_string(),
                        ),
                    }),
                    Ok(StreamChunk::Finish {
                        reason: FinishReason::ToolCall,
                    }),
                ]
            } else {
                vec![
                    Ok(StreamChunk::text("好了")),
                    Ok(StreamChunk::Finish {
                        reason: FinishReason::Stop,
                    }),
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    pub(super) fn model() -> ModelInfo {
        ModelInfo {
            key: "k".into(),
            provider: "p".into(),
            api_name: "m".into(),
            context_window: 100_000,
            max_output_tokens: 8_000,
            tokenizer: "cl100k".into(),
            capabilities: vec![carter::registry::Capability::Tools],
            pricing: carter::registry::Pricing {
                input: 0.0,
                output: 0.0,
                cache_write: None,
                cache_read: None,
            },
            default_reasoning: None,
        }
    }

    /// 等一条事件，超时就失败 —— 卡住的测试要自己说清卡在哪一步。
    pub(super) async fn wait_for(
        events: &mut tokio::sync::broadcast::Receiver<AppEvent>,
        what: &str,
        mut hit: impl FnMut(&SessionEvent) -> bool,
    ) -> SessionEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ev = tokio::time::timeout(left, events.recv())
                .await
                .unwrap_or_else(|_| panic!("等「{what}」超时"))
                .expect("事件通道关了");
            if let AppEvent::Session { event, .. } = ev
                && hit(&event)
            {
                return event;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    pub(super) async fn a_background_action_reaches_the_command_running_inside_the_turn() {
        use std::sync::atomic::AtomicU32;

        let dir = std::env::temp_dir().join(format!(
            "carter-app-live-turn-{}-{}",
            std::process::id(),
            crate::protocol::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let shell = AppShell::new(Config::default());
        let mut events = shell.subscribe();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let m = model();
        let handle = SessionHandle::new(
            "live-turn-test".into(),
            dir.clone(),
            m.clone(),
            None,
            cmd_tx,
        );

        let ctx = SessionCtx {
            shell: shell.clone(),
            handle: handle.clone(),
            model: m.clone(),
            provider: Arc::new(Scripted(std::sync::atomic::AtomicUsize::new(0))),
            config: Arc::new(Config::default()),
            settings: AppSettings::default(),
            resumed: false,
            cwd: dir.clone(),
            unsupported_server_tools: Vec::new(),
            capture: None,
            include_task: false,
            fast: None,
            fast_ref: None,
            shells: handle.background.clone(),
            shell_cwd: crate::tools::bash::ShellCwd::new(dir.clone()),
            running: crate::tools::control::RunningTools::new(),
        };
        let mut thread = Thread::new_empty();
        thread.append_user("跑一条很久的命令");
        let meta = session::SessionMeta {
            id: handle.id.clone(),
            format_version: session::CURRENT_FORMAT_VERSION,
            parent_id: None,
            forked_from: None,
            cwd: dir.to_string_lossy().to_string(),
            git: None,
            title: Some("测试".into()),
            carter_version: env!("CARGO_PKG_VERSION").to_string(),
            model: m.key.clone(),
            created_at: crate::protocol::now_ms(),
        };
        let mut runner = Runner {
            ctx,
            thread,
            meta,
            cmd_rx,
            tool_tx: Some(emit_tool_event(shell.clone(), handle.id.clone())),
            ask_tx: None,
            mcp_tools: Vec::new(),
            activity: Arc::new(AtomicU32::new(0)),
            goal_signal: Arc::new(GoalSignal::default()),
            goal: None,
            downgraded_goal: false,
            queued: VecDeque::new(),
            // 固定系统提示：不去读用户机器上的 `~/.carter/system.md` / 技能目录。
            fixed_system: Some(vec!["测试系统提示".into()]),
            title_pending: false,
            title_seed: None,
            last_message_at: None,
            changes: None,
            changes_turn: 0,
            overhead: 0,
            reported: Arc::default(),
        };

        let turn = tokio::spawn(async move { runner.run_one_turn(None).await });

        // 工具卡片上的编号必须是**模型给的**那个 —— 界面靠它把按钮送回后端。
        let started = wait_for(&mut events, "工具开始", |e| {
            matches!(e, SessionEvent::ToolStarted { .. })
        })
        .await;
        let SessionEvent::ToolStarted { call_id, name, .. } = started else {
            unreachable!()
        };
        assert_eq!(name, "bash");
        assert_eq!(call_id, "call_from_model", "卡片编号要用模型给的那个");

        // 界面上就是这一刻点「转后台」。
        assert!(handle.send(Command::ToolAction {
            call_id: call_id.clone(),
            action: crate::protocol::ToolActionKind::Background,
        }));

        let finished = wait_for(&mut events, "工具结束", |e| {
            matches!(e, SessionEvent::ToolFinished { .. })
        })
        .await;
        let SessionEvent::ToolFinished {
            call_id: done_id,
            content,
            ..
        } = finished
        else {
            unreachable!()
        };
        assert_eq!(done_id, call_id);
        assert!(content.contains("shell_id="), "应当转成后台：{content}");

        // 这一轮照常收尾（模型拿到了结果、说了最后一句话）。
        let result = tokio::time::timeout(Duration::from_secs(30), turn)
            .await
            .expect("转后台之后这一轮应当立刻收尾")
            .unwrap();
        assert!(!result.failed, "转后台不该把这一轮弄失败");
        assert!(result.used_tools);

        handle.background.kill_all();
        std::fs::remove_dir_all(&dir).ok();
    }
}
