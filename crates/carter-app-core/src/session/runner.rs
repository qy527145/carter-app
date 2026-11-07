use super::helpers::*;
use super::*;

/// 一轮跑完之后的账。自动继续的判定全看这几个数（见 `goal.rs::decide_after_turn`）。
pub(super) struct TurnResult {
    /// 这一轮以错误收场（上游报错、输出被截断）。
    pub(super) failed: bool,
    /// 这一轮调用过工具。
    pub(super) used_tools: bool,
    /// 用户在这一轮里点过「停止」。
    pub(super) user_cancelled: bool,
    /// 这一轮开始时历史里有几轮（含这一轮自己）。carter 中途压缩过的话，轮末靠它重排。
    pub(super) turns_before: u32,
    /// carter 在这一轮**中途**自己压缩过时的摘要档位（见 [`Runner::after_turn`]）。
    pub(super) mid_compact: Option<String>,
}

pub(super) struct Runner {
    pub(super) ctx: SessionCtx,
    /// 这份历史。会话任务**独占**它（`carter::agent::run_turn` 要 `&mut Thread`
    /// 一整轮攥着），别处要看进展走 [`SessionHandle::history`] 那份快照。
    ///
    /// 曾经包在 `Arc<Mutex<..>>` 里，为的是让自动压缩跑到后台。代价是压缩与这一轮
    /// 各改各的历史：压缩把结果写回共享那份，轮末又把整份换回去，压缩白做。现在
    /// 压缩在轮末**就地**做（见 [`Runner::compact`]）。
    pub(super) thread: Thread,
    pub(super) meta: session::SessionMeta,
    pub(super) cmd_rx: mpsc::UnboundedReceiver<Command>,
    /// 工具卡片的出口（**同步**调用，与正文增量同一条时间线，见 [`ToolTx`]）。
    pub(super) tool_tx: Option<crate::tools::record::ToolTx>,
    /// 工具发起的提问：接收端在 `forward_events` 任务手里。`None` = 还没建。
    pub(super) ask_tx: Option<mpsc::UnboundedSender<AskRequest>>,
    pub(super) mcp_tools: Vec<Arc<dyn Tool>>,
    pub(super) activity: Arc<AtomicU32>,
    pub(super) goal_signal: Arc<GoalSignal>,
    pub(super) goal: Option<Goal>,
    /// 启动时盘上的目标从 `active` 降级成了 `paused`，要向用户说一声。
    pub(super) downgraded_goal: bool,
    /// 生成中收到的、要等这一轮跑完再处理的命令。
    pub(super) queued: VecDeque<Command>,
    /// 侧边会话的系统提示固定部分（见 [`SideWiring`]），主会话每轮现算。
    pub(super) fixed_system: Option<Vec<String>>,
    /// 还欠这条会话一个自动标题（首轮开始时去生成，见 [`Runner::spawn_title`]）。
    pub(super) title_pending: bool,
    /// 起标题用的那句话。`None` = 用这一轮用户刚发的那句。
    ///
    /// 续接一条**没有标题**的老会话时，拿用户**现在**说的这句话去起名是错的
    /// （这条对话早就不是那个话题了）；该用历史里的第一句。所以那条路把这里
    /// 填成历史里的第一句用户消息。
    pub(super) title_seed: Option<String>,
    /// 最近一次有消息进历史的时间。助理对话靠它判断「空闲多久了」该不该切期。
    /// 启动时取会话文件的修改时间（重启之后也能接上空闲时长）。
    pub(super) last_message_at: Option<u64>,
    /// 正在收集的这一轮的文件变更。轮首建、轮末收（见 `changes.rs`）。
    ///
    /// `None` = 不在轮内（空闲时），或监听器没建起来 —— 两种情况都只是「这一轮
    /// 没有清单可列」，不该影响对话本身。
    pub(super) changes: Option<ChangeWatch>,
    /// 系统提示 + 工具声明的 token 数（见 [`Runner::measure_overhead`]）。
    pub(super) overhead: u64,
    /// 最近一次主对话请求、上游回报的上下文占用（见 [`Runner::run_one_turn`] 里
    /// 处理 `StreamChunk::Usage` 那一段）。0 = 还没有
    /// （刚打开 / 刚续接，或历史刚被改写过），这时百分比环回落到估算值。
    pub(super) reported: Arc<AtomicU64>,
    /// 上面那个监听器对应哪一轮。
    ///
    /// 必须在轮首记下来：`carter::agent::run_turn` 一进去就把 `thread.user_turns`
    /// 加一，轮末再去读就成了**下一轮**的号 —— 历史回看时清单会挂错回答。
    pub(super) changes_turn: u32,
}

impl Runner {
    pub(super) async fn run(mut self) {
        let session_id = self.ctx.handle.id.clone();

        // 异步的两条事件来源，最终都汇到 AppEvent 上：
        // * `tool_tx` —— 我们自己那层记录包装（**完整**的参数与结果）
        // * `ask_tx`  —— 工具发起的提问，需要等前端作答
        //
        // 正文增量不在这里：它们与工具卡片要严格保序，所以在产生的那一刻直接
        // `emit`（见模块文档）。工具记录与提问本来就不是 carter 的概念，走各自的
        // 通道由转发任务收口。
        let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
        let tx = emit_tool_event(self.ctx.shell.clone(), session_id.clone());
        self.tool_tx = Some(tx);
        self.ask_tx = Some(ask_tx);
        let forwarder = tokio::spawn(forward_events(
            self.ctx.shell.clone(),
            session_id.clone(),
            self.ctx.handle.asks.clone(),
            ask_rx,
        ));

        // MCP server 启动是异步的（要连子进程 / 网络）。失败不致命：
        // carter 的 McpManager 会跳过连不上的 server。
        //
        // 它在会话开头连一次就不再重连 —— 所以界面上改了 MCP 配置要**新开会话**
        // 才生效，这与「工具开关下一条消息就生效」不同，界面上分开提示。
        // 助理对话不连：它只有助理那一套工具（见 `tools::build_tools`），连上也用不到。
        let mcp_mgr =
            if self.ctx.include_task && self.ctx.handle.id != crate::assistant::CHAT_SESSION_ID {
                let (mgr, tools) = carter::mcp::McpManager::start(&self.ctx.config.mcp).await;
                self.mcp_tools = tools;
                Some(mgr)
            } else {
                None
            };

        self.emit(SessionEvent::Ready {
            session_id: session_id.clone(),
            model: self.ctx.model.api_name.clone(),
            model_ref: self.ctx.model.key.clone(),
            cwd: self.ctx.cwd.to_string_lossy().to_string(),
            resumed: self.ctx.resumed,
        });

        // 上下文占用也紧跟着发一次：百分比环要在第一条消息之前就有数（续接一条长会话时
        // 尤其要紧 —— 用户得先知道它已经快满了）。系统提示与工具声明这时先量一遍。
        let system = self.turn_system(self.meta.git.as_ref());
        let tools = self.turn_tools(&system);
        self.measure_overhead(&system, &tools);
        drop(tools);
        self.emit_context();

        // 目标状态紧跟 ready 发一次：界面上那条目标条靠它画出来。
        self.emit(SessionEvent::Goal {
            goal: self.goal.as_ref().map(goal::view),
        });
        if self.downgraded_goal {
            self.notice(
                "上次退出时这个目标还在进行中，已改为暂停。点目标条上的「继续」可以接着推进。",
            );
        }

        // 声明不出去的服务端工具**必须说出来**。静默丢弃是最坏的选项：
        // 用户以为开了联网搜索，模型其实一个结果都拿不到，而模型自己也不知道，
        // 只会一本正经地编。
        if !self.ctx.unsupported_server_tools.is_empty() {
            self.notice(&format!(
                "当前渠道不支持这些服务端工具，已忽略：{}",
                self.ctx.unsupported_server_tools.join("、")
            ));
        }

        // 后台命令自己结束时的通知。只在**空闲**时取：正在跑的那一轮里它就留在通道里，
        // 等这一轮（以及排在前面的用户消息）跑完再起新的一轮 —— 与 Claude Code 一样，
        // 模型起完后台命令就可以收工，结束了再被叫回来，而不是守着它轮询。
        let mut done_rx = self.ctx.shells.subscribe();
        // 别的会话发来的信。同样只在空闲时取；正在跑的那一轮里由工具结果捎带（见 `peers.rs`）。
        let inbox = self.ctx.handle.inbox.clone();

        loop {
            let cmd = match self.queued.pop_front() {
                Some(c) => c,
                None => {
                    tokio::select! {
                        cmd = self.cmd_rx.recv() => match cmd {
                            Some(c) => c,
                            // 命令通道关了（句柄全 drop）→ 会话结束。
                            None => break,
                        },
                        Some(id) = done_rx.recv() => {
                            // `None` = 模型这期间已经用 bash_output 看到它结束了，不必再报。
                            match self.ctx.shells.completion(&id) {
                                Some(text) => Command::SendPrompt { text, id: None, interject: false },
                                None => continue,
                            }
                        },
                        _ = inbox.notified() => match inbox.take_prompt() {
                            Some(text) => Command::SendPrompt { text, id: None, interject: false },
                            // 这一轮里已经搭着工具结果送过了。
                            None => continue,
                        },
                        // 显式关闭。光靠上面那条 `None` 是不够的 —— 会话任务自己攥着一个
                        // `Arc<SessionHandle>`，它持有发送端，所以 `recv()` 永远不会返回
                        // `None`。见 `shell.rs::close_session`。
                        _ = self.ctx.handle.close.cancelled() => break,
                    }
                }
            };
            self.handle_command(cmd).await;
            if self.ctx.handle.close.is_cancelled() {
                break;
            }
        }

        // 后台命令跟着会话走：会话没了，它们就不该继续占着端口 / CPU。
        // 这一步要在等转发任务之前做 —— 那些子进程的日志不会再有人读。
        self.ctx.shells.kill_all();
        if let Some(mgr) = mcp_mgr {
            mgr.shutdown().await;
        }
        self.ctx.handle.asks.clear();
        self.ctx.handle.close.set();
        drop(self.tool_tx.take());
        drop(self.ask_tx.take());
        let _ = forwarder.await;
        // 没读的信退回去、等着回执的告诉一声 —— 否则发信方会一直以为对方会回。
        crate::peers::on_exit(&self.ctx.shell, &self.ctx.handle);
        // 只摘**自己**那一条。按 id 摘的话，`close_session` 等超时之后已经开起来的
        // 接班 runner 会被一起摘掉 —— 注册表里看不到它，别人又按 id 开出第三个，
        // 同一个会话文件就有了多个写者。
        self.ctx
            .shell
            .sessions
            .remove_if(&session_id, |_, h| Arc::ptr_eq(h, &self.ctx.handle));
        self.ctx.shell.mark_closed(&session_id);
        // **最后**一句：告诉 `close_session` 那个等待方「文件与 Thread 都放掉了」。
        // 放在 remove 之后，保证它醒来时注册表里已经没有这条会话。
        self.ctx.handle.done.set();
    }

    pub(super) async fn handle_command(&mut self, cmd: Command) {
        match cmd {
            // 没有正在跑的轮次时取消是空操作。
            Command::Cancel => {}
            Command::AnswerForm { id, answers } => {
                self.ctx.handle.asks.answer(id, answers);
            }
            Command::Goal { action } => {
                let was_active = self
                    .goal
                    .as_ref()
                    .is_some_and(|g| g.status == GoalStatus::Active);
                let mut goal = self.goal.clone();
                apply_goal_action(
                    &mut goal,
                    &self.ctx.handle.id.clone(),
                    &self.ctx.shell.clone(),
                    action,
                );
                let now_active = goal
                    .as_ref()
                    .is_some_and(|g| g.status == GoalStatus::Active);
                self.goal = goal;
                // 刚刚从「不推进」变成「推进」（设定了新目标、或点了继续）→ 立刻开干。
                // 不这么做的话，按了「继续」却什么都不发生，得再发一条消息才动 ——
                // 那颗按钮就没有意义了。
                if now_active && !was_active {
                    self.kick_goal().await;
                }
            }
            // 先重读配置再解析：`ctx.config` 是开会话时的快照，而换模型常常紧跟在
            // 「在设置页改了默认模型 / 加了个模型」之后 —— 不刷新的话，新加的模型
            // 会在旧配置里查不到，或者渠道改过的地址还用着旧的。
            Command::SetModel { model } => {
                self.ctx.refresh();
                match switch_model(&self.ctx, &model) {
                    Ok(resolved) => {
                        self.ctx.model = resolved.model;
                        self.ctx.provider = resolved.provider;
                        self.ctx.unsupported_server_tools = resolved.unsupported_server_tools;
                        self.ctx
                            .handle
                            .model
                            .store(Arc::new(self.ctx.model.clone()));
                        // 换模型要同时告诉前端两件事：真正发给上游的名字（`Notice` 里
                        // 那句话用它），以及 `渠道/显示名`（界面上的模型标签用它）。
                        self.emit(SessionEvent::ModelChanged {
                            model: self.ctx.model.api_name.clone(),
                            model_ref: self.ctx.model.key.clone(),
                        });
                        self.notice(&format!("模型已切换为 {}", self.ctx.model.api_name));
                        // 窗口大小跟着模型变，百分比要重算。
                        self.emit_context();
                    }
                    Err(e) => self.error(&format!("切换模型失败：{e}")),
                }
            }
            // 排队中的消息真正轮到它时走到这里。`interject` 在这里没有意义
            // （已经没有正在跑的轮次可以打断了），直接当普通消息处理。
            Command::SendPrompt { text, id, .. } => self.submit(text, id).await,
            // 历史里存的是展开后的正文，原样重发 —— 不再过一次展开，
            // 否则 `@文件` 的内容会被贴第二遍。
            Command::Regenerate { turn } => self.regenerate(turn).await,
            Command::RetryReply { turn } => self.retry_reply(turn).await,
            // 空闲时「插队」= 把那条消息挪到队首。下一轮循环就会取到它。
            Command::PromotePrompt { id } => promote(&mut self.queued, id),
            Command::DropPrompt { id } => drop_queued(&mut self.queued, id),
            // 空闲时按「中断命令」= 那条命令已经结束了，什么都不用做。
            Command::ToolAction { .. } => {}
            Command::Compact => {
                self.ctx.refresh();
                self.compact(false).await;
            }
            Command::Record { text, report } => self.record_only(text, report),
            Command::NewEpisode => self.close_episode(true).await,
            // 空闲时才走到这里（生成中收到的排队等这一轮跑完），所以下一轮的号
            // 就是 `user_turns` —— 起点从它开始，已经跑完的轮次都算「清空之前」。
            Command::ClearChanges => {
                let since = self.thread.user_turns;
                if let Err(e) = crate::changes::clear(&self.ctx.handle.id, since) {
                    tracing::warn!("清空变更记录失败：{e}");
                }
                self.emit(SessionEvent::ChangesCleared { since });
            }
        }
    }

    /// 重试：历史回退到这一轮之前，把它原样再发一遍。
    ///
    /// **只回滚对话，不动磁盘**。文件已经改过、命令已经跑过的事不会撤销 ——
    /// 界面上那句确认里也这么说，用户想要的是「让它重来一遍」，不是「把世界推回去」
    /// （后者是 carter 的 `/rewind`，走的是另一套检查点）。
    ///
    /// **改写提问不走这里**：那是开一条新分支（见 `sessions::fork`），原会话
    /// 一个字都不动 —— 改写过的对话本来就与原来那句提问对不上，塞回同一份历史
    /// 只会让它自相矛盾。
    ///
    /// 回退与重发是同一件事的两半：中间不插任何 `Idle`，否则界面会先看到一路空下去、
    /// 再慢慢长出来。`TurnRetracted` 紧跟着的不带 `prompt` 的 `turn_started`
    /// 复用的是界面上原本那条用户气泡。
    pub(super) async fn regenerate(&mut self, turn: u32) {
        let git = self.meta.git.clone();
        // 上一轮可能有悬空的 tool_use；回退前先收口，免得把断口留在截断点之前。
        self.settle_history();
        let stored = match crate::history::retract_turn(&mut self.thread, turn) {
            Ok(t) => t,
            Err(e) => {
                self.error(&e);
                return;
            }
        };
        self.forget_reported();
        // 变更清单按轮存着，一起退 —— 否则被回退掉的那些回答的文件清单会留下来。
        // 重跑出来的那一轮会重新记自己那条（同一个轮次号）。
        if let Err(e) = crate::changes::retract(&self.ctx.handle.id, turn) {
            tracing::warn!("回退轮次变更记录失败：{e}");
        }
        // 原文只退这一轮**之后**的：这一轮的原文要留着（「编辑」框还得能回填）。
        if let Err(e) = crate::prompts::retract(&self.ctx.handle.id, turn + 1) {
            tracing::warn!("回退用户原文失败：{e}");
        }
        // 时刻退到这一轮**之前**：重跑出来的这一轮会重新记自己的开始时刻。
        if let Err(e) = crate::timeline::retract(&self.ctx.handle.id, turn) {
            tracing::warn!("回退时刻记录失败：{e}");
        }
        self.ctx.handle.publish_history(&self.thread.messages);
        self.emit(SessionEvent::TurnRetracted { turn });
        // `echo` 一律给 `None`：这一轮的用户气泡界面上已经有了，`TurnRetracted`
        // 刚刚交代过它 —— 再带一次 `prompt` 会多画一条。
        self.run_sequence(stored, git, None, None).await;
    }

    /// 就地重试一条回答：历史退到这一轮最后一条助手消息之前，**不重发提问**，
    /// 直接拿同一份上下文再请求一次。
    ///
    /// 与 [`Self::regenerate`] 的区别见 `history::rewind_reply`。截断点落在一轮
    /// 中间，界面没法自己删准，所以发一份 `HistoryReplaced` 让它整个重建，再接一条
    /// 不带 `prompt` 的 `turn_started` —— 新的增量接在重建出来的那一轮末尾。
    pub(super) async fn retry_reply(&mut self, turn: u32) {
        let git = self.meta.git.clone();
        self.settle_history();
        if let Err(e) = crate::history::rewind_reply(&mut self.thread, turn) {
            self.error(&e);
            return;
        }
        self.forget_reported();
        // 这一轮自己的变更清单与原文都留着（前面几次往返改过的文件还算这一轮的），
        // 只退后面的。
        let id = self.ctx.handle.id.clone();
        if let Err(e) = crate::changes::retract(&id, turn + 1) {
            tracing::warn!("回退轮次变更记录失败：{e}");
        }
        if let Err(e) = crate::prompts::retract(&id, turn + 1) {
            tracing::warn!("回退用户原文失败：{e}");
        }
        if let Err(e) = crate::timeline::retract(&id, turn + 1) {
            tracing::warn!("回退时刻记录失败：{e}");
        }
        self.ctx.handle.publish_history(&self.thread.messages);
        self.emit(SessionEvent::HistoryReplaced {
            history: crate::view::to_views(&self.thread.messages),
            prompts: crate::prompts::load(&id),
            changes: crate::changes::load(&id),
            changes_since: crate::changes::since(&id),
            times: crate::timeline::load(&id),
        });

        self.ctx.refresh();
        self.begin_changes();
        let started_at = crate::protocol::now_ms();
        if let Err(e) = crate::timeline::started(&id, turn, started_at) {
            tracing::warn!("记录本轮开始时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnStarted {
            turn,
            prompt_id: None,
            prompt: None,
            started_at,
            report: None,
        });
        let result = self.run_one_turn(git.clone()).await;
        self.settle_history();
        self.finish_changes(result.used_tools).await;
        self.emit_turn_ended(turn, &result);
        self.after_turn(&result).await;
        self.drive_goal(result, git).await;
        self.last_message_at = Some(crate::protocol::now_ms());
        self.emit(SessionEvent::Idle);
    }

    /// 只往历史里记一条消息，不跑模型（见 `Command::Record`）。
    ///
    /// 界面上与一轮的开头一样画出来：`turn_started` 带原文，紧跟一条 `idle`。
    /// 它在历史里算一轮用户输入（正文非空的 user 消息），号要占上，否则之后的轮次号
    /// 与 `view.rs` 从历史数出来的对不上。
    ///
    /// `report` 跟着这一轮存进 `timeline`，并随 `turn_started` 发给界面。
    pub(super) fn record_only(&mut self, text: String, report: Option<crate::timeline::ReportTag>) {
        self.settle_history();
        self.thread.append_user(text.clone());
        let turn = self.thread.user_turns;
        self.thread.user_turns = turn.saturating_add(1);
        self.ctx.handle.publish_history(&self.thread.messages);
        let started_at = crate::protocol::now_ms();
        if let Err(e) =
            crate::timeline::started_tagged(&self.ctx.handle.id, turn, started_at, report.clone())
        {
            tracing::warn!("记录本轮开始时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnStarted {
            turn,
            prompt_id: None,
            prompt: Some(text),
            started_at,
            report,
        });
        // 这条不跑模型，写完就算结束 —— 时间要立刻收口，否则界面上它会一直
        // 停在没有结束时刻的状态。
        let at = crate::protocol::now_ms();
        if let Err(e) = crate::timeline::ended(&self.ctx.handle.id, turn, at) {
            tracing::warn!("记录本轮结束时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnEnded {
            turn,
            at,
            failed: false,
            cancelled: false,
        });
        self.emit(SessionEvent::Idle);
        self.last_message_at = Some(crate::protocol::now_ms());
    }

    /// 助理对话：这一句话到来之前，看看这一期该不该结束（见 `assistant/memory.rs`）。
    ///
    /// 结束一期 = 原始消息另存进往期存档、用副模型整理出纪要与记忆、历史清空。
    /// 清空走 carter 的 `Compacted` 记录（空快照），会话文件只追加不改写，重启时
    /// 照样只加载本期。整理失败不挡这句话：纪要退成一句「整理失败」，历史照样切。
    ///
    /// `force`：用户在面板上点了「开新一期」（`Command::NewEpisode`），不看切期条件。
    pub(super) async fn close_episode(&mut self, force: bool) {
        use crate::assistant::memory;
        if self.ctx.handle.id != crate::assistant::CHAT_SESSION_ID {
            return;
        }
        if self.thread.messages.is_empty() {
            if force {
                self.notice("这一期还没有对话，不用切");
            }
            return;
        }
        let now = crate::protocol::now_ms() + self.ctx.handle.clock_skew_ms.load(Ordering::SeqCst);
        let store = self.ctx.shell.assistant.clone();
        let started_at = store
            .read(|s| s.episodes.last().map(|e| e.ended_at))
            .unwrap_or(self.meta.created_at);
        let last_at = self.last_message_at.unwrap_or(now);
        let turns = crate::view::user_turn_count(&self.thread.messages);
        let used = self.history_tokens() + self.overhead;
        let window = u64::from(self.ctx.model.context_window);
        let why = if force {
            "手动开新一期"
        } else {
            let Some(why) = memory::should_close(last_at, now, turns, used, window) else {
                return;
            };
            why
        };
        self.settle_history();
        let messages = self.thread.messages.clone();
        let number = store.read(|s| s.episodes.last().map_or(1, |e| e.number + 1));
        let dir = memory::archive_dir();
        if let Err(e) = memory::save_archive(&dir, number, &messages) {
            // 存档写不下就不切：切了的话这一期的原话就只剩一行纪要。
            tracing::warn!(error = %e, "助理对话：往期存档写不下，这一期先不切");
            return;
        }
        self.notice(&format!(
            "{why}：整理第 {number} 期对话，长期记忆与纪要会保留"
        ));

        let user = memory::user_lines(&messages);
        let existing = store.read(|s| s.memories.clone());
        let prompt =
            memory::consolidate_prompt(&existing, &user, &memory::transcript(&messages, 24_000));
        let model = self.ctx.fast_model().unwrap_or_else(|| CompactModel {
            provider: self.ctx.provider.clone(),
            model: self.ctx.model.clone(),
        });
        let consolidated = crate::assistant::eval::ask_with(
            &*model.provider,
            &model.model,
            memory::CONSOLIDATE_SYSTEM,
            prompt,
            1500,
        )
        .await
        .and_then(|a| memory::parse_consolidated(&a));
        let (summary, ops) = match consolidated {
            Ok(c) if !c.summary.trim().is_empty() => (c.summary.trim().to_string(), c.memory),
            Ok(_) => ("（整理没给出纪要）".to_string(), Vec::new()),
            Err(e) => {
                tracing::warn!(error = %e, "助理对话：整理失败，只存档不提取记忆");
                (
                    format!("（整理失败：{e}。原话在第 {number} 期存档里）"),
                    Vec::new(),
                )
            }
        };
        let changed = store.with(|s| {
            let n = memory::apply_ops(&mut s.memories, &ops, &user, now);
            s.episodes.push(memory::Episode {
                number,
                started_at,
                ended_at: last_at,
                summary: summary.clone(),
                turns,
            });
            n
        });
        if let Err(e) = &changed {
            tracing::warn!(error = %e, "助理对话：纪要写不进 assistant.json，这一期先不切");
            return;
        }

        if let Some(rec) = self.thread.recorder() {
            rec.record(carter::session::RecordKind::Compacted {
                tier: "episode".into(),
                messages: Vec::new(),
            });
        }
        self.thread.messages.clear();
        self.thread.user_turns = 0;
        self.thread.turns = 0;
        let id = self.ctx.handle.id.clone();
        crate::prompts::remove(&id);
        crate::changes::remove(&id);
        crate::timeline::remove(&id);
        self.forget_reported();
        self.ctx.handle.publish_history(&self.thread.messages);
        self.emit(SessionEvent::HistoryReplaced {
            history: Vec::new(),
            prompts: Vec::new(),
            changes: Vec::new(),
            changes_since: 0,
            times: Vec::new(),
        });
        self.notice(&format!(
            "第 {number} 期已整理：{}{}",
            summary.lines().next().unwrap_or(""),
            match changed {
                Ok(n) if n > 0 => format!("（长期记忆改了 {n} 条）"),
                _ => String::new(),
            }
        ));
        self.emit(SessionEvent::AssistantEpisode { number });
        self.emit_context();
    }

    /// 用户发来一条消息：展开、落进历史、跑一轮，再按目标决定要不要自动继续。
    pub(super) async fn submit(&mut self, raw: String, prompt_id: Option<u64>) {
        // 助理对话：用户开口前先看这一期该不该收。汇报（`record_only`）不走这里 ——
        // 只有用户的话才开新的一期。
        if !crate::assistant::is_injected(&raw) {
            self.close_episode(false).await;
        }
        // 用户亲口说话了：这一轮不再是哪封信的回信，跨会话的跳数从头数。
        if !crate::assistant::is_injected(&raw)
            && !raw.starts_with(crate::tools::background::DONE_MARK)
            && !raw.starts_with(crate::peers::PEER_MARK)
        {
            self.ctx.handle.inbox.reset_hops();
        }
        // 展开之后发给模型的是另一段文本，但界面上要显示用户**打的字**。
        let echo = Some(raw.clone());
        // 热重载落点：每轮开始前重读配置与应用设置。
        // 界面上改完工具开关、换完渠道、动过 system.md，下一条消息就按新的来。
        self.ctx.refresh();
        let git = self.meta.git.clone();

        // 输入框里的 `/命令`、`@文件`、`@图片` 在这里展开。
        //
        // 放在后端而不是前端：展开后的文本（几百行文件内容）不该出现在输入框
        // 里让用户看着它发出去。放在这里还有一个好处 —— 与 CLI 走的是同一套
        // 规则（自定义命令用 carter 的 `commands::expand`，图片用 carter 的
        // `media::inline_user_attachments`），两边行为对得上。
        //
        // 助理的汇报不展开：里面有外部事件的文字，写一个 `@~/.ssh/id_rsa` 不该把文件读进来。
        // 后台命令的完成通知同理：里面是命令输出。别的会话的来信也是别人写的字。
        let expansion = if crate::assistant::is_injected(&raw)
            || raw.starts_with(crate::tools::background::DONE_MARK)
            || raw.starts_with(crate::peers::PEER_MARK)
        {
            Ok((raw.clone(), None))
        } else {
            expand_input(&self.ctx.cwd, &raw)
        };
        let text = match expansion {
            Ok((expanded, note)) => {
                if let Some(note) = note {
                    self.notice(&note);
                }
                expanded
            }
            Err(e) => {
                // 展开失败不吞掉这条消息：把原文发出去，但要说清发生了什么。
                self.notice(&format!("{e}（已按原文发送）"));
                raw
            }
        };

        // 用户自己说话了 —— 上一轮「原地打转」的判定就此作废。
        if let Some(g) = self.goal.as_mut()
            && g.idle_streak != 0
        {
            g.idle_streak = 0;
            self.publish_goal();
        }

        self.run_sequence(text, git, prompt_id, echo).await;
    }

    /// 目标从「不推进」变成「推进」时踢一脚：不等用户再说话，直接开跑。
    ///
    /// 走的是与自动继续**同一条**消息与同一条路（`CONTINUE_PROMPT` + `run_sequence`），
    /// 所以界面上呈现成「自动继续」的分隔条而不是用户气泡 —— 它确实不是用户说的。
    pub(super) async fn kick_goal(&mut self) {
        let git = self.meta.git.clone();
        self.run_sequence(goal::CONTINUE_PROMPT.to_string(), git, None, None)
            .await;
    }

    /// 把一段文本作为一轮的用户输入跑完（含它引发的自动继续）。
    ///
    /// `prompt_id` 是排队队列里那条消息的编号：`turn_started` 会原样带回，界面
    /// 据此把「排队中」的那一条挪进对话。自动继续（`/goal`）没有编号。
    pub(super) async fn run_sequence(
        &mut self,
        text: String,
        git: Option<carter::session::GitInfo>,
        prompt_id: Option<u64>,
        echo: Option<String>,
    ) {
        // 上一轮如果以「模型发完 tool_use 就被中断」收场，历史里会留一个断口。
        // **必须在追加这条用户消息之前补上** —— 补在它之后，普通用户消息就会
        // 插在 tool_use 与 tool_result 中间，那仍然是一份非法历史。
        self.settle_history();
        let turn_start = self.thread.messages.len();
        self.thread.append_user(text.clone());
        // 标题在这一刻并行去生成，**不挡这一轮**（见 [`Runner::spawn_title`]）。
        // 放在 append 之后：carter 的 Recorder 是惰性建文件的，第一条消息落盘
        // 才让会话文件存在，而写标题要往那个文件里追加。
        if std::mem::take(&mut self.title_pending) {
            let seed = self.title_seed.take().unwrap_or_else(|| text.clone());
            self.spawn_title(&seed);
        }
        // 轮次开头也发布一次：这一轮可能很长，侧边聊天在这期间至少要能看到
        // 「用户提了什么要求」（之后的进展由 `live` 补上）。
        self.ctx.handle.publish_history(&self.thread.messages);
        self.begin_changes();
        // 用户**打的原文**按轮存一份：会话文件里是展开后的正文（`@文件` 的内容
        // 也可能在里面），拿它回填「编辑」框等于把整份文件摊在输入框里。
        // 自动继续推起来的那一轮没有原文（`echo` 是 `None`），本来也不给它编辑。
        if let Some(raw) = echo.as_deref()
            && let Err(e) = crate::prompts::record(&self.ctx.handle.id, self.thread.user_turns, raw)
        {
            tracing::warn!("记录用户原文失败：{e}");
        }
        // 轮末的记忆建议要看用户**打的原文**（`@文件` 展开前）。
        let raw = echo.clone();
        // 轮次号在**进 run_turn 之前**记下：carter 的 `run_turn` 一进去就把
        // `thread.user_turns` 加一（见 `changes_turn` 的同一段说明），轮末再读就成了
        // 下一轮的号 —— 完成时刻会被记到下一轮上，这一轮的回答永远等不到它。
        let turn = self.thread.user_turns;
        let started_at = crate::protocol::now_ms();
        // 时间按轮落盘：切走再回来、重启之后显示的还是当时的那个数，而不是
        // 「重开会话的时刻」。老会话没有这份记录，界面就不显示时间。
        if let Err(e) = crate::timeline::started(&self.ctx.handle.id, turn, started_at) {
            tracing::warn!("记录本轮开始时刻失败：{e}");
        }
        self.emit(SessionEvent::TurnStarted {
            turn,
            prompt_id,
            prompt: echo,
            started_at,
            report: None,
        });

        let result = self.run_one_turn(git.clone()).await;
        // 一轮结束立刻收口：中断留下的断口现在就补进历史（并落盘），
        // 不必等用户下一次发消息。
        self.settle_history();
        self.finish_changes(result.used_tools).await;
        self.emit_turn_ended(turn, &result);
        // 压缩放在 `turn_ended` 与 `Idle` **之间**，而不是更早：助理之类的监听者
        // 一收到 `turn_ended` 就开始处理收尾（判「没汇报」、发通知、排下一轮），
        // 而压缩要发模型请求、要几秒。按源顺序处理事件的监听者会被它拖住，
        // 那几秒里什么都干不了；排进去的新消息还会因为这条会话还「忙」而多等一层。
        self.after_turn(&result).await;
        if !result.failed
            && !result.user_cancelled
            && let Some(raw) = raw.as_deref()
        {
            self.spawn_memory_suggest(raw, turn_start);
        }
        self.drive_goal(result, git).await;
        self.last_message_at = Some(crate::protocol::now_ms());
        self.emit(SessionEvent::Idle);
    }
}
