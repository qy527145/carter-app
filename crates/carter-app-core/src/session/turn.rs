use super::helpers::*;
use super::runner::{Runner, TurnResult};
use super::sinks::*;
use super::*;

impl Runner {
    /// 记下这一轮跑完的时刻并发给界面。
    ///
    /// `turn` 必须由调用方在**进 `run_turn` 之前**取好传进来：carter 一进去就把
    /// `thread.user_turns` 加一，在这里现读会拿到下一轮的号（见 `run_sequence`）。
    ///
    /// 三条路径（正常响应、失败、被中断）都走这里：回答是不完整的也要显示时间 ——
    /// 界面上少一处时间比显示一个不完整回答的结束时刻更让人困惑。
    pub(super) fn emit_turn_ended(&mut self, turn: u32, result: &TurnResult) {
        let at = crate::protocol::now_ms();
        if let Err(e) = crate::timeline::ended(&self.ctx.handle.id, turn, at) {
            tracing::warn!("记录本轮结束时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnEnded {
            turn,
            at,
            failed: result.failed,
            cancelled: result.user_cancelled,
        });
    }

    /// 历史自愈 + 界面收尾。
    ///
    /// 补上结果的那几条工具调用，界面上可能正悬着一张没有结果的卡片 ——
    /// 这里补发一对 `tool_started` / `tool_finished` 把它画完整，否则界面显示的
    /// 历史与磁盘上的历史会不一样（下次打开时才突然多出一张卡）。
    pub(super) fn settle_history(&mut self) {
        let repaired = crate::history::repair_dangling_tool_uses(&mut self.thread);
        if repaired.is_empty() {
            return;
        }
        for call in repaired {
            self.emit(SessionEvent::ToolStarted {
                call_id: call.id.clone(),
                name: call.name,
                args: call.input,
            });
            self.emit(SessionEvent::ToolFinished {
                call_id: call.id,
                ok: false,
                content: crate::history::INTERRUPTED_RESULT.to_string(),
                elapsed_ms: 0,
            });
        }
        self.ctx.handle.publish_history(&self.thread.messages);
    }

    /// 首轮之后并行生成会话标题。
    ///
    /// ## 为什么是另一条任务
    ///
    /// 生成标题是一次完整的模型调用（哪怕用的是副模型也要几百毫秒到几秒）。串在
    /// `run_sequence` 里就会**挡住这一轮**：用户盯着的是第一句回答，而这里在等一个
    /// 只影响侧栏一行的东西。串在轮末也一样 —— 会话任务同时在处理队列里的命令，
    /// 标题那几秒里用户发第二条消息就得排队。
    ///
    /// ## 与手动改名的先后
    ///
    /// 写回走 [`crate::sessions::auto_title_allowed`]：**只在还没有标题时写**。用户在这几百
    /// 毫秒里自己改了名字的话（侧栏行内重命名），机器起的这个名字不该把它盖掉。
    ///
    /// ## 失败也不静默
    ///
    /// 副模型配错、网络不通、上游拒答 —— 重试之后仍然拿不到，就用首句话截出来的
    /// 兜底标题（见 [`crate::title::generate`]）。侧栏绝不能停在「未命名会话」上：
    /// 一排没名字的会话，用户分不清哪条是哪条。但兜底也不是完整的成功，所以
    /// 顺带发一条提示 —— 不弹错误条，只在对话里一行字。
    pub(super) fn spawn_title(&mut self, first_prompt: &str) {
        // 侧边会话没有 recorder（不落盘），也就没有「列表里那一行」要起名。
        let Some(recorder) = self.thread.recorder() else {
            return;
        };
        // 与 carter CLI 同一个函数（`generate_title`）：3-6 词中文、取首行、去引号。
        //
        // 这里显式解析一次副模型（不能靠 `run_one_turn` 里那份缓存：标题跑在它前面，
        // 那时缓存还没填）。解析过一次之后就是同一份缓存。
        let (provider, model) = match self.ctx.fast_model() {
            Some(f) => (f.provider, f.model),
            // 没配副模型（或它解析不出来）：用主模型起标题，也好过没有标题。
            None => (self.ctx.provider.clone(), self.ctx.model.clone()),
        };
        // 渠道种类决定用哪种写法关思考（见 `title.rs`：推理模型会把 32 个 token
        // 全花在思考上，正文一个字都不留）。副模型可能挂在另一个渠道上，所以取
        // **它自己那个** kind。
        let kind = self.ctx.config.provider_for(&model.provider).kind;
        let shell = self.ctx.shell.clone();
        let session_id = self.ctx.handle.id.clone();
        let prompt = first_prompt.to_string();

        tokio::spawn(async move {
            let outcome =
                crate::title::generate(&prompt, &session_id, provider, &model, &kind).await;
            let title = outcome.title.trim().to_string();
            if title.is_empty() {
                return;
            }
            // 用户在这几秒里自己改过名字的话，机器起的名字（哪怕是兜底的）不该盖掉它。
            if !crate::sessions::auto_title_allowed(
                crate::sessions::title_of(&session_id).as_deref(),
            ) {
                return;
            }
            // 用**会话自己那个 recorder** 追加，而不是新开一个句柄：同一个文件上的
            // 两个追加句柄，写一行会拆成两次 write（正文 + 换行），并发时可能把两条
            // 记录绞在一起。这个 recorder 内部有 Mutex，是唯一安全的那条路。
            //
            // 文件还没建出来也不怕：carter 的 recorder 是惰性的，第一次写会先把
            // 文件头（SessionMeta）落下去 —— 第一条消息早就写过了，这一步只是兜底。
            recorder.record(carter::session::RecordKind::Title {
                title: title.clone(),
            });
            if outcome.degraded {
                emit(
                    &shell,
                    &session_id,
                    SessionEvent::Notice {
                        message: "自动命名没成功，先用首句话当了标题，可以右键重命名".into(),
                    },
                );
            }
            emit(&shell, &session_id, SessionEvent::Title { title });
        });
    }

    /// 轮末看看用户这句话里有没有值得长期记住的东西，有就交给界面请用户确认
    /// （见 `memory_suggest.rs`）。与标题一样**不挡下一轮**、失败就静默。
    ///
    /// 只在用户亲自打字的主会话里做：侧边聊天、助理对话、工单会话里的话不是
    /// 「用户对这个项目的长期要求」，后两者还常常没人在看，弹了也没人点。
    pub(super) fn spawn_memory_suggest(&mut self, user_text: &str, turn: u32, turn_start: usize) {
        use crate::memory_suggest as ms;
        if self.fixed_system.is_some()
            || self.ctx.handle.id == crate::assistant::CHAT_SESSION_ID
            || crate::assistant::runtime::ticket_for_session(&self.ctx.shell, &self.ctx.handle)
                .is_some()
            || user_text.starts_with(crate::peers::PEER_MARK)
            || !ms::worth_checking(user_text)
        {
            return;
        }
        let guard = &self.thread;
        // 这一段是「这一轮说了什么」，与上面传进来的轮次号 `turn` 不是一回事。
        let messages = guard.messages.get(turn_start..).unwrap_or_default();
        // 模型自己已经记过了：同一件事再问一遍是打扰。
        if ms::saved_in(messages) {
            return;
        }
        let reply = ms::reply_text(messages);
        let model = self.ctx.fast_model().unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        });
        let existing = carter::memory::load(&self.ctx.cwd);
        let shell = self.ctx.shell.clone();
        let session_id = self.ctx.handle.id.clone();
        let user_text = user_text.to_string();
        tokio::spawn(async move {
            match ms::suggest(
                &*model.provider,
                &model.model,
                &session_id,
                turn,
                &user_text,
                &reply,
                &existing,
            )
            .await
            {
                Ok(candidates) if !candidates.is_empty() => {
                    emit(
                        &shell,
                        &session_id,
                        SessionEvent::MemorySuggestion { candidates },
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("记忆建议没生成出来：{e}"),
            }
        });
    }

    /// 这一轮的系统提示。主会话每轮现算（技能会变、目标会变），侧边会话是固定部分
    /// 加上主对话的最新状态。
    pub(super) fn turn_system(&self, git: Option<&carter::session::GitInfo>) -> Vec<String> {
        match &self.fixed_system {
            Some(s) => {
                let mut s = s.clone();
                // 侧边会话：主对话的最新状态（含进行中的那一轮）每轮现取。
                // 放在最后 —— 它是最「当下」的东西，也是每轮都会变的那段。
                if let Some(parent) = self.ctx.handle.parent_id.as_deref() {
                    match crate::side::parent_context(&self.ctx.shell, parent) {
                        Some(c) => s.push(c),
                        None => s.push("## 主对话\n\n（主对话已经关闭，看不到了。）".into()),
                    }
                }
                s
            }
            None => {
                let mut s = self.ctx.system(git);
                if self.ctx.handle.id == crate::assistant::CHAT_SESSION_ID {
                    s.push(crate::assistant::chat_segment(&self.ctx.shell.assistant));
                }
                // 目标段接在最后：它是最「当下」的指令，而且（将来 carter 打开
                // cache 断点后）末段正是断点位置 —— 所以它必须是稳定的，
                // 见 `goal.rs` 模块头。
                if let Some(seg) = self.goal.as_ref().and_then(goal::system_segment) {
                    s.push(seg);
                }
                s
            }
        }
    }

    /// 这一轮的工具表（工具开关、MCP、子 agent 都在这里装配）。
    pub(super) fn turn_tools(&self, system: &[String]) -> carter::tools::ToolRegistry {
        let is_chat = self.ctx.handle.id == crate::assistant::CHAT_SESSION_ID;
        // 汇报推起来的那一轮只给一个只读工具：汇报里有外部事件的文字，这一轮只该转述。
        // 不能一个都不给 —— 历史里有 tool_use 时，Anthropic 协议要求请求必须声明 tools。
        let report_turn = crate::assistant::chat::is_report_turn(&self.thread.messages);
        if is_chat && report_turn {
            let mut bare = carter::tools::ToolRegistry::empty();
            bare.push(Arc::new(
                crate::tools::assistant_ticket::AssistantStatusTool::new(
                    self.ctx.shell.assistant.clone(),
                ),
            ));
            return self.recorded(bare);
        }
        // 常驻助理的工单会话：挂上 `assistant_report`；助理在后台推着跑、没人在看时，
        // 不给提问表单（没人回答，这一轮会永远卡住）。用户自己打开会话接着聊时照常给。
        let ticket =
            crate::assistant::runtime::ticket_for_session(&self.ctx.shell, &self.ctx.handle);
        let unattended = ticket.as_ref().is_some_and(|(_, u)| *u);
        // 助理对话同样可能没人在看（汇报推起来的那一轮）：表单没人答，这一轮卡住，
        // 后面的汇报就全堵在队列里。要问什么直接写在回复里。
        let bare = build_tools(
            ToolBuildArgs {
                provider: self.ctx.provider.clone(),
                model: self.ctx.model.clone(),
                agent_cfg: self.ctx.config.agent.clone(),
                base_system: system.join("\n"),
                cancel: self.ctx.handle.cancel.clone(),
                mcp_tools: self.mcp_tools.clone(),
                cwd: self.ctx.cwd.clone(),
                skills_enabled: self.ctx.config.skills.enabled,
                tool_tx: self.tool_tx.clone().expect("事件通道在 run() 里建好"),
                ask_tx: if self.ctx.include_task && !unattended && !is_chat {
                    self.ask_tx.clone()
                } else {
                    // 侧边聊天不挂 ask_user：抽屉里再套一层提问表单，与「临时问一句」
                    // 的定位不符，而且那套回答链路要单独接一遍。
                    None
                },
                include_task: self.ctx.include_task,
                goal_signal: self.goal_signal.clone(),
                schedules: self.ctx.shell.schedules.clone(),
                activity: self.activity.clone(),
                live: self.live(),
                shells: Some(self.ctx.shells.clone()),
                session_id: self.ctx.handle.id.clone(),
                shell_cwd: Some(self.ctx.shell_cwd.clone()),
                running: Some(self.ctx.running.clone()),
                assistant: ticket.map(|(id, _)| (self.ctx.shell.assistant.clone(), id)),
                assistant_chat: is_chat.then(|| self.ctx.shell.clone()),
                peers: (self.ctx.include_task && !is_chat).then(|| self.ctx.shell.clone()),
            },
            &self.ctx.settings,
        );
        // 主会话这一层统一套记录包装：一次调用只画一张卡，而且编号用**模型给的**
        // `call_id` —— 界面上的「中断 / 转后台」按它找到那条命令，历史里的卡片也按它
        // 配对。子 agent 那份快照在 `build_tools` 里已经套过了（它们模型给的编号可能
        // 跨 agent 重复，所以现编）。
        self.recorded(bare)
    }

    /// 把工具池里的每个工具套进 [`crate::tools::record::Recorded`]。
    pub(super) fn recorded(
        &self,
        bare: carter::tools::ToolRegistry,
    ) -> carter::tools::ToolRegistry {
        wrap_recorded(
            bare,
            self.tool_tx.clone().expect("事件通道在 run() 里建好"),
            self.live(),
            Some(self.ctx.running.clone()),
            self.activity.clone(),
        )
    }

    /// 这一轮的用户说了什么 —— `UserPromptSubmit` 可阻断（拦下就不发请求）。
    ///
    /// carter 的 `run_turn` 不管这个 hook（它是宿主自己的「用户提交了一句话」语义），
    /// 所以放在进 `run_turn` 之前。返回 `None` = 放行。
    pub(super) async fn dispatch_prompt_hook(
        &self,
        run_opts: &carter::agent::RunOptions,
    ) -> Option<String> {
        if !run_opts.hooks.has(HookEvent::UserPromptSubmit) {
            return None;
        }
        let last = self
            .thread
            .messages
            .last()
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|b| match b {
                        llm_adapt_core::Block::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        match run_opts
            .hooks
            .dispatch(
                HookEvent::UserPromptSubmit,
                serde_json::json!({ "prompt": last }),
            )
            .await
        {
            HookDecision::Block { reason } => Some(reason),
            _ => None,
        }
    }

    /// 记下「历史之外」每次请求都要带的那部分 token：系统提示 + 工具声明。
    ///
    /// 百分比环要的是**整个请求**的大小，只数历史会在工具很多（MCP 动辄几十个）时
    /// 偏小一大截。这两样每轮才变一次，所以在装配时顺手量一下存起来。
    pub(super) fn measure_overhead(
        &mut self,
        system: &[String],
        tools: &carter::tools::ToolRegistry,
    ) {
        let tokenizer = &self.ctx.model.tokenizer;
        let specs = serde_json::to_string(&tools.specs()).unwrap_or_default();
        self.overhead = carter::tokens::count_text(&system.join("\n"), tokenizer)
            + carter::tokens::count_text(&specs, tokenizer);
    }

    /// 跑一轮：交给 carter 的 `run_turn`（发请求 → 流式 → 执行工具 → 回灌），
    /// 期间把界面上那几条「立刻生效」的命令读掉。
    ///
    /// 请求、工具调度、hook（`PreToolUse` / `PostToolUse` / `PostTurn` / `Stop`…）、
    /// 中途压缩、写前快照都在 carter 那一层 —— 这里**不再自己驱动循环**。曾经为了
    /// 「一条工具结果落地就发下一次请求」把整套循环搬过来，代价是一堆 carter 的行为
    /// 要在这里重新实现一遍（中途压缩的判定、todo 落盘与复诵、跨会话来信、检查点…），
    /// 漏掉哪一样都只在很远的表象上看得出来。而那个目标本身也做不到：协议要求同一条
    /// assistant 消息里的每个 `tool_use` 都在下一条 user 消息里有 `tool_result`。
    ///
    /// `git` 只有主会话有（侧边会话不重算系统提示），这里收一个 `Option` 是为了
    /// 让两条路共用同一个函数体。
    pub(super) async fn run_one_turn(
        &mut self,
        git: Option<carter::session::GitInfo>,
    ) -> TurnResult {
        let system = self.turn_system(git.as_ref());
        // 只有状态为 active 时模型才该调 goal_update。
        self.goal_signal.set_armed(
            self.goal
                .as_ref()
                .is_some_and(|g| g.status == GoalStatus::Active),
        );

        let hooks = Arc::new(HookRegistry::from_configs(self.ctx.config.hooks.clone()));
        let tools = self.turn_tools(&system);
        self.measure_overhead(&system, &tools);
        // 这一轮开始时历史里有几轮（**含**这一轮自己 —— 用户消息在 `run_sequence`
        // 里已经 append 了）。carter 中途压过的话，轮后靠它重排轮次号；与
        // `TurnRemap` 的口径必须是同一个数。
        let turns_before = crate::view::user_turn_count(&self.thread.messages);

        // 压缩用副模型：这是「把长历史揉短」的活儿，要的是便宜和快。
        let fast = self.ctx.fast_model();
        let compact_model = Some(fast.unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        }));

        // 一轮里能调多少次模型由模型自己决定（不再要工具就收尾）。给 carter 一个用不完
        // 的上限 —— `max_turns` 是它 CLI 里的防失控保险，而工单里「翻几十个文档、逐个
        // 核对」的活很容易超过 50 次，撞上之后剩下的工具一条都不执行。
        let mut agent = self.ctx.config.agent.clone();
        agent.max_turns = u32::MAX;

        let run_opts = carter::agent::RunOptions {
            show_thinking: true,
            system,
            compact_model,
            hooks,
            purpose: carter::provider::RequestPurpose::MainTurn,
            // 一轮进行中用户补的话，下一次请求就带上（见 `steer.rs`）。
            steer: Some(Arc::new(crate::steer::TurnSteer {
                queue: self.ctx.handle.steer.clone(),
                shell: self.ctx.shell.clone(),
                session_id: self.ctx.handle.id.clone(),
                live: self.live(),
            })),
        };

        // `thread.turns` 在 carter 里是「整个会话累计的模型调用次数」，每轮清零让它
        // 回到名字上的意思：**一次用户输入**内最多几次模型调用。
        self.thread.turns = 0;
        // 上一轮可能残留了取消标记（用户在流结束后、select 收尾前点的停止）。
        self.ctx.handle.cancel.reset();
        let cancel = self.ctx.handle.cancel.clone();
        let activity_before = self.activity.load(Ordering::Relaxed);
        let mut user_cancelled = false;
        let mut closed = false;

        // 这一轮用户说了什么 —— `UserPromptSubmit` 可阻断。拦下时照样占掉轮次号：
        // 那条消息已经进历史了，不占的话之后的轮次号与 `view` 数出来的对不上。
        let blocked = self.dispatch_prompt_hook(&run_opts).await;
        // carter 在这一轮**中途**自己压过上下文时，由 sink 记下摘要档位。
        let mid = Arc::new(std::sync::Mutex::new(None));
        let outcome = if let Some(reason) = blocked {
            self.thread.user_turns = self.thread.user_turns.saturating_add(1);
            Err(carter::CarterError::Config(format!(
                "被 hook 拦下：{reason}"
            )))
        } else {
            let mut sink = TxSink {
                shell: self.ctx.shell.clone(),
                session_id: self.ctx.handle.id.clone(),
                live: self.live(),
                todos: self.ctx.handle.todos.clone(),
                mid: mid.clone(),
            };
            let tap = ContextTap {
                inner: self.ctx.provider.clone(),
                shell: self.ctx.shell.clone(),
                session_id: self.ctx.handle.id.clone(),
                window: self.ctx.model.context_window,
                reported: self.reported.clone(),
            };
            // 包一层收件箱：这一轮里每次工具返回都顺手把新到的信送进去。
            let turn = crate::peers::scope(
                self.ctx.handle.inbox.clone(),
                carter::agent::run_turn(
                    &mut self.thread,
                    &tap,
                    &self.ctx.model,
                    &agent,
                    &run_opts,
                    &tools,
                    &mut sink,
                    &cancel,
                ),
            );
            tokio::pin!(turn);
            // 开门：这之后到的用户消息走插话，不再排到轮末。
            // 配了 `UserPromptSubmit` 的不开：那个 hook 能拦下用户的话，而它只在一轮
            // 开头跑，插话会绕过去 —— 这种配置下照旧排到轮末，每句都过一遍 hook。
            self.ctx
                .handle
                .steer
                .set_open(!run_opts.hooks.has(HookEvent::UserPromptSubmit));

            let out = loop {
                tokio::select! {
                    out = &mut turn => break out,
                    // 生成中能**立刻**生效的命令；其余进队列，这一轮跑完再处理。
                    cmd = self.cmd_rx.recv() => match cmd {
                        Some(cmd) => {
                            user_cancelled |= handle_live_command(
                                cmd,
                                &self.ctx,
                                &mut self.queued,
                                &mut self.goal,
                                &mut self.changes,
                                self.changes_turn,
                            );
                        }
                        // 命令通道关了 = 句柄都没了，按关会话收尾。
                        None => {
                            closed = true;
                            cancel.set();
                        }
                    },
                    // 关会话（换走时空闲就关、应用退出）要立刻断流。
                    //
                    // **只响应一次**（`!closed`）：`close` 置位之后 `cancelled()` 每次都
                    // 立即就绪，不加这道闸的话 select 每圈都选中它，一个 worker 线程被
                    // 忙转占满，而 carter 收到第一次取消就 `reset()` 了。
                    _ = self.ctx.handle.close.cancelled(), if !closed => {
                        closed = true;
                        cancel.set();
                    }
                }
            };
            // 关门，没被取走的退回队首作为下一轮（见 `steer.rs`）。
            self.ctx.handle.steer.set_open(false);
            unsteer(&self.ctx, &mut self.queued);
            out.map(|(o, _)| o)
        };

        self.ctx.handle.publish_history(&self.thread.messages);

        let used_tools = self.activity.load(Ordering::Relaxed) > activity_before;
        // 「失败」的几种来源，对自动继续是同一件事：**别接着推**。
        let failed = match &outcome {
            Err(_) => true,
            // 被中断：用户自己点的停止不算失败（那条规则在 `decide_after_turn` 里
            // 优先级更高）；自己断的（关会话等）算，免得在这儿打转。
            Ok(carter::agent::TurnOutcome::Cancelled) => !user_cancelled,
            // 只剩「输出被截断」会走到这里（轮数不设限了）。
            Ok(carter::agent::TurnOutcome::Limit) => true,
            Ok(carter::agent::TurnOutcome::Error(_)) => true,
            Ok(carter::agent::TurnOutcome::Assistant) => false,
        };
        if let Err(e) = &outcome {
            // 关会话造成的断流不是错误；用户点了停止也一样（取消之后上游那次请求
            // 常以「已取消」收场，当成错误弹出来会让人以为「停止」弄坏了什么）。
            if !closed && !user_cancelled {
                self.error(&format!("{e}"));
            }
        }
        let mid_compact = mid.lock().ok().and_then(|mut t| t.take());
        TurnResult {
            failed,
            used_tools,
            user_cancelled,
            turns_before,
            mid_compact,
        }
    }

    /// 一轮跑完后按目标状态决定要不要接着推。
    ///
    /// 判定全在 `goal.rs::decide_after_turn`（纯函数、逐条测过），这里只负责
    /// 「照做」：落盘、发事件、追加那条「继续」再跑一轮。
    pub(super) async fn drive_goal(
        &mut self,
        first: TurnResult,
        git: Option<carter::session::GitInfo>,
    ) {
        let mut result = first;
        loop {
            let Some(mut g) = self.goal.clone() else {
                return;
            };
            if g.status != GoalStatus::Active {
                return;
            }

            // 含本轮的连续无工具轮数。有工具调用就是「在干活」，清零。
            g.idle_streak = if result.used_tools {
                0
            } else {
                g.idle_streak.saturating_add(1)
            };
            let outcome = TurnOutcome {
                user_cancelled: result.user_cancelled,
                failed: result.failed,
                used_tools: result.used_tools,
                idle_streak: g.idle_streak,
                signal: self.goal_signal.take(),
            };

            match goal::decide_after_turn(&g, &outcome) {
                goal::Decision::None => {
                    // 无事发生，但空转计数变了，落一下盘。
                    self.set_goal(Some(g));
                    return;
                }
                goal::Decision::Set {
                    status,
                    note,
                    notice,
                } => {
                    g.status = status;
                    g.note = note;
                    g.updated_at = crate::protocol::now_ms();
                    self.set_goal(Some(g));
                    self.notice(&notice);
                    return;
                }
                goal::Decision::Continue => {
                    g.auto_continues = g.auto_continues.saturating_add(1);
                    self.set_goal(Some(g));

                    // 给「停止」留一点手感窗口，也避免退化成 tight loop。
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    if self.ctx.handle.close.is_cancelled() {
                        return;
                    }

                    // 这条消息会进历史（会话文件里也留一份，resume 后模型看得到自己
                    // 被推过；界面把它渲染成「自动继续」的分隔条而不是用户气泡）。
                    self.thread.append_user(goal::CONTINUE_PROMPT);
                    self.ctx.handle.publish_history(&self.thread.messages);
                    self.begin_changes();
                    // 同 `run_sequence`：`user_turns` 在进 `run_turn` 之后就被加一了。
                    let turn = self.thread.user_turns;
                    let started_at = crate::protocol::now_ms();
                    if let Err(e) = crate::timeline::started(&self.ctx.handle.id, turn, started_at)
                    {
                        tracing::warn!("记录本轮开始时刻失败：{e}");
                    }
                    self.emit(SessionEvent::TurnStarted {
                        turn,
                        // 自动继续没有用户消息编号 —— 界面据此画分隔条而不是气泡。
                        prompt_id: None,
                        prompt: None,
                        started_at,
                        report: None,
                    });
                    result = self.run_one_turn(git.clone()).await;
                    self.settle_history();
                    self.finish_changes(result.used_tools).await;
                    self.emit_turn_ended(turn, &result);
                    if self.ctx.handle.close.is_cancelled() {
                        return;
                    }
                    self.after_turn(&result).await;
                    if self.ctx.handle.close.is_cancelled() {
                        return;
                    }
                }
            }
        }
    }
}
