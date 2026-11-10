use super::*;

/// 把目标落盘并广播给界面。
pub(super) fn publish_goal(goal: &Option<Goal>, session_id: &str, shell: &AppShell) {
    match goal {
        Some(g) => {
            if let Err(e) = goal::save(session_id, g) {
                emit(
                    shell,
                    session_id,
                    SessionEvent::Error {
                        message: format!("目标没能写进磁盘：{e}"),
                    },
                );
            }
        }
        None => {
            goal::remove(session_id);
        }
    }
    emit(
        shell,
        session_id,
        SessionEvent::Goal {
            goal: goal.as_ref().map(goal::view),
        },
    );
}

/// 执行一条 `/goal` 动作。**自由函数**，不碰 `Runner`：它要在跑一轮的中途被调用，
/// 而那时 `self.thread` 正被 `run_turn` 的可变借用占着 —— 只有「不碰 self」的代码
/// 才进得去那个 select 分支。
pub(super) fn apply_goal_action(
    goal: &mut Option<Goal>,
    session_id: &str,
    shell: &AppShell,
    action: GoalCommand,
) {
    let Some(next) = goal::apply_action(goal.clone(), &action) else {
        emit(
            shell,
            session_id,
            SessionEvent::Notice {
                message: goal_noop_note(&action, goal.as_ref()).to_string(),
            },
        );
        return;
    };
    *goal = next;
    publish_goal(goal, session_id, shell);

    let message = match (goal.as_ref(), &action) {
        (Some(_), GoalCommand::Set { .. }) => "目标已设定，开始推进。".to_string(),
        (Some(_), GoalCommand::Resume) => "目标已继续。".to_string(),
        (Some(_), GoalCommand::Pause) => {
            "目标已暂停：不会再有自动继续（正在跑的那一轮会跑完）。".to_string()
        }
        (Some(_), GoalCommand::Complete) => "目标已标记为完成。".to_string(),
        _ => "目标已清除。".to_string(),
    };
    emit(shell, session_id, SessionEvent::Notice { message });
}
/// 动作在当前状态下没意义时，说清是为什么 —— 静默无事发生最难查。
pub(super) fn goal_noop_note(action: &GoalCommand, current: Option<&Goal>) -> &'static str {
    match (action, current) {
        (_, None) => "还没有设定目标。用 `/goal <目标>` 设一个。",
        (GoalCommand::Pause, _) => "目标不在进行中，无需暂停。",
        (GoalCommand::Resume, _) => "目标已经在推进了。",
        (GoalCommand::Complete, _) => "目标已经是完成状态。",
        (GoalCommand::Set { .. }, _) => "目标正文不能是空的。",
        (GoalCommand::Clear, _) => "没有可清除的目标。",
    }
}

/// 一轮进行中到的用户消息 → 插话队列（见 `steer.rs`）。展开在这里就做：发出去的时刻
/// 用户还在看着，`@文件` 读的是此刻的内容，展开失败的说明也该此刻给。
fn steer_prompt(ctx: &SessionCtx, raw: String, id: u64) {
    let text = match expand_input(&ctx.cwd, &raw) {
        Ok((expanded, note)) => {
            if let Some(message) = note {
                emit(&ctx.shell, &ctx.handle.id, SessionEvent::Notice { message });
            }
            expanded
        }
        Err(e) => {
            emit(
                &ctx.shell,
                &ctx.handle.id,
                SessionEvent::Notice {
                    message: format!("{e}（已按原文发送）"),
                },
            );
            raw.clone()
        }
    };
    // 用户亲口说话了：这一轮之后再发出去的信，跳数从头数（与 `submit` 同一条规矩）。
    ctx.handle.inbox.reset_hops();
    ctx.handle.steer.push(crate::steer::Pending {
        id,
        raw,
        text: crate::steer::wrap(&text),
    });
}

/// 还没被取走的插话退回命令队列的**队首**（按原来的先后），作为下一轮跑。
///
/// 两种时候要退：这一轮跑完了没再发请求（模型已经在写最后一段话了）；用户要这一轮
/// 停下（停止 / 插队）—— 这时它们排在插队那一条后面，比之前就排着的那些靠前。
pub(super) fn unsteer(ctx: &SessionCtx, queued: &mut VecDeque<Command>) {
    for p in ctx.handle.steer.drain().into_iter().rev() {
        queued.push_front(Command::SendPrompt {
            text: p.raw,
            id: Some(p.id),
            interject: false,
        });
    }
}

/// 把排队中的某条用户消息提到最前面。
///
/// 只在队列里找 `SendPrompt`：其它命令（换模型、目标操作）没有「下一条用户输入」
/// 的语义，把它们提到队首只会让「插队」变得莫名其妙。
pub(super) fn promote(queue: &mut VecDeque<Command>, id: u64) {
    let pos = queue
        .iter()
        .position(|c| matches!(c, Command::SendPrompt { id: Some(q), .. } if *q == id));
    if let Some(pos) = pos
        && let Some(cmd) = queue.remove(pos)
    {
        queue.push_front(cmd);
    }
}

/// 撤回一条还没开始跑的消息。找不到就当作已经跑了，什么都不做。
pub(super) fn drop_queued(queue: &mut VecDeque<Command>, id: u64) {
    queue.retain(|c| !matches!(c, Command::SendPrompt { id: Some(q), .. } if *q == id));
}

/// 一轮**正在跑**时到达的命令：立刻生效的当场处理，其余进 `queued` 等这一轮跑完。
///
/// 读命令通道的地方有三个（等模型首字、读流、跑工具），它们都在 `run_one_turn`
/// 里，而那时 `self.ctx` 正被工具调用的 `CallCtx` 借着 —— 所以这里做成自由函数、
/// 只拿互不重叠的几个字段，而不是 `&mut Runner` 的方法。
///
/// **少了这套即时处理会出现什么**：用户点「转后台」之后，命令要等这一轮（以及
/// 那条命令）跑完才被读到；那时 `call_id` 早就失效了，`RunningTools::apply` 找不到
/// 它 —— 界面上就是「点了没反应」。停止、插队同样是这个道理。
///
/// 返回 `true` = 这条命令是用户在要求停下这一轮（停止 / 插队 / 把排队消息提到最前）。
/// 调用方靠它区分「用户主动停」与「会话被关掉」—— 前者不该报成失败（见 `run_one_turn`）。
pub(super) fn handle_live_command(
    cmd: Command,
    ctx: &SessionCtx,
    queued: &mut VecDeque<Command>,
    goal: &mut Option<Goal>,
    changes: &mut Option<ChangeWatch>,
    changes_turn: u32,
) -> bool {
    match cmd {
        // 停止整轮：只置取消标记，流与工具都在听它，这一轮自己收拾。
        Command::Cancel => {
            ctx.handle.cancel.set();
            return true;
        }
        // 生成中也能发消息：用户发的默认作为插话，下一次请求就带上；勾了「插队」就
        // 中断本轮、排到队首作为新的一轮。没编号的（系统送进来的）与压缩期间到的
        // （没有正在跑的 `run_turn` 来取）照旧排队。
        Command::SendPrompt {
            text,
            id,
            interject,
        } => {
            if interject {
                ctx.handle.cancel.set();
                unsteer(ctx, queued);
                queued.push_front(Command::SendPrompt {
                    text,
                    id,
                    interject: false,
                });
                return true;
            }
            match id {
                Some(id) if ctx.handle.steer.is_open() => steer_prompt(ctx, text, id),
                _ => queued.push_back(Command::SendPrompt {
                    text,
                    id,
                    interject: false,
                }),
            }
        }
        Command::PromotePrompt { id } => {
            let steered = ctx.handle.steer.remove(id);
            unsteer(ctx, queued);
            match steered {
                Some(p) => queued.push_front(Command::SendPrompt {
                    text: p.raw,
                    id: Some(p.id),
                    interject: false,
                }),
                None => promote(queued, id),
            }
            ctx.handle.cancel.set();
            return true;
        }
        Command::DropPrompt { id } => {
            if ctx.handle.steer.remove(id).is_none() {
                drop_queued(queued, id);
            }
        }
        // 单条工具调用的「中断 / 转后台」。立刻打到那条命令上，不影响这一轮本身 ——
        // 工具会把结果回灌给模型，模型接着往下走。
        Command::ToolAction { call_id, action } => {
            // 找不到说明它刚好跑完了。不是错误，静默即可 —— 用户点按钮与命令结束
            // 本来就可能撞在同一毫秒。
            ctx.running.apply(&call_id, action);
        }
        Command::AnswerForm { id, answers } => {
            ctx.handle.asks.answer(id, answers);
        }
        // `/goal pause` 必须立刻生效：否则一条正在自动继续的长任务要等整串跑完
        // 才停得下来，而用户按暂停正是因为不想再等了。
        Command::Goal { action } => {
            let mut next = goal.clone();
            apply_goal_action(&mut next, &ctx.handle.id, &ctx.shell, action);
            *goal = next;
        }
        // 清空也要立刻生效：起点就是**正在跑的这一轮**，监听器重建一个 —— 新基线、
        // 空清单，这一轮清空之前动过的文件不再算。
        Command::ClearChanges => {
            if let Err(e) = crate::changes::clear(&ctx.handle.id, changes_turn) {
                tracing::warn!("清空变更记录失败：{e}");
            }
            // 只有正在记这一轮时才重建监听器 —— 空闲时没有可清的那一份，凭空起一个
            // 只会在下一次轮首被丢掉。
            if changes.is_some() {
                *changes = ChangeWatch::start(&ctx.cwd, &ctx.settings.changes_ignore);
            }
            emit(
                &ctx.shell,
                &ctx.handle.id,
                SessionEvent::ChangesCleared {
                    since: changes_turn,
                },
            );
        }
        other => queued.push_back(other),
    }
    false
}

/// 展开输入框里的 `/命令` 与 `@文件`。
///
/// 返回 `(展开后的文本, 给用户看的说明)`。说明只在真的发生了展开时才给 ——
/// 普通消息不该多出一条提示。
///
/// 三步，顺序不能换：
/// 1. **自定义命令**：`/name args` 且 `name` 是 `~/.carter/commands/` 或
///    `.carter/commands/` 里的命令时，用 carter 的 `commands::expand` 展开成 prompt
///    （它自己会注入正文里的 `@文件` 与 `!shell`）。
/// 2. **`@文件`**：展开成 `--- path ---` 包裹的上下文块。
/// 3. **`@图片`**：转成 `[img:...]` 引用，发请求时由 provider 层取字节。
///
/// 第 2 步对图片路径返回 None（不注入），正好把图片留给第 3 步。
pub(super) fn expand_input(
    cwd: &std::path::Path,
    raw: &str,
) -> Result<(String, Option<String>), String> {
    let mut text = raw.to_string();
    let mut note = None;

    // 先把（可能存在的）命令名与参数取成自有字符串，再动 `text` ——
    // 否则 `strip_prefix` 借出去的那一段会一直活到赋值处。
    let command = text.strip_prefix('/').map(|rest| {
        let (name, args) = match rest.split_once(char::is_whitespace) {
            Some((n, a)) => (n.to_string(), a.trim().to_string()),
            None => (rest.to_string(), String::new()),
        };
        (name, args)
    });

    if let Some((name, args)) = command {
        // 只认真的存在的自定义命令。`/usr/bin/env` 这种路径不能当命令处理。
        if !name.is_empty()
            && carter::commands::discover(cwd)
                .iter()
                .any(|c| c.name == name)
        {
            text = crate::input::expand_custom(cwd, &name, &args)?;
            note = Some(format!("已展开自定义命令 /{name}"));
        }
    }

    let before = text.clone();
    text = crate::input::expand_file_refs(&text, cwd);
    text = carter::media::inline_user_attachments(&text, cwd);

    if note.is_none() && text != before {
        let files = crate::input::count_file_refs(&before, cwd);
        if files > 0 {
            note = Some(format!("已把 {files} 个文件的内容附进这条消息"));
        }
    }
    Ok((text, note))
}

/// 按 id 找会话，**跨所有项目**。
///
/// 不按 cwd 过滤：界面上能点到的会话（包括「会话」页里别的目录下的那些）都应该能
/// 打开 —— 否则会出现「列表里有、点开却是另一个会话」这种最令人困惑的失败。
/// 按 id 找是 uuid，全局唯一，让调用方传对目录只会引入「传错了所以打不开」
/// 这类沉默的失败。
pub(super) fn find_by_id(id: &str) -> Option<session::SessionEntry> {
    session::list(std::path::Path::new("."), true)
        .into_iter()
        .find(|e| e.meta.id == id)
}

/// 续接一条历史会话时，这一轮该在哪个目录里跑。
///
/// 优先用会话文件里记的那份：从「会话」页点开别的目录下的会话时，工具、系统提示、
/// git 快照都该落在那条对话自己的目录里，而不是启动应用时的家目录。
///
/// 目录已经被删掉（换机器、清了盘）时退回请求里的 cwd —— 宁可在一个尚可的目录里
/// 继续聊，也不要因为一条历史会话打不开。抽成纯函数是为了给这条兜底一个能测的落点。
pub(super) fn resumed_cwd(recorded: &str, requested: &std::path::Path) -> PathBuf {
    let recorded = PathBuf::from(recorded);
    if recorded.is_dir() {
        recorded
    } else {
        requested.to_path_buf()
    }
}

pub(super) fn switch_model(
    ctx: &SessionCtx,
    model_ref: &str,
) -> carter::Result<carter::provider::resolve::ResolvedProvider> {
    let cache_json = catalog::read_cache().map_err(carter::CarterError::Config)?;
    // 换模型时也保留抓包 —— 否则一切换就静默停止记录了。
    let hook = ctx
        .capture
        .as_ref()
        .map(|store| Arc::new(crate::capture::CaptureHook::new(store.clone())));
    let hooks: Vec<Arc<dyn llm_adapt_core::RequestHook>> = hook
        .iter()
        .map(|h| h.clone() as Arc<dyn llm_adapt_core::RequestHook>)
        .collect();
    let resolved = crate::catalog::resolve_provider(&ctx.config, &cache_json, model_ref, hooks)?;
    // 换模型可能换协议（anthropic 渠道 → deepseek 渠道），钩子的协议要跟着走，
    // 否则新报文会按旧协议的语法去解。
    if let Some(h) = hook {
        label_capture(&h, &ctx.config, &resolved.model);
    }
    Ok(resolved)
}

/// 把「这条请求走的是哪条路线」告诉抓包钩子：协议、渠道、模型。
///
/// 钩子拿不到这些（它只看得见最终字节），而它们是解析 provider 时才定下来的 ——
/// 所以是「先建钩子、解析完再回填」。回填在任何请求之前完成。
///
/// 渠道（provider 名）必须记：同一个模型可能同时配在多个渠道上（官方源、中转、
/// 自建镜像），事后光看 URL 未必分得出来。
pub(super) fn label_capture(
    hook: &crate::capture::CaptureHook,
    config: &Config,
    model: &ModelInfo,
) {
    hook.set_route(
        config.provider_for(&model.provider).protocol().as_str(),
        model.provider.as_str(),
        model.key.as_str(),
    );
}

pub(super) fn emit(shell: &AppShell, session_id: &str, event: SessionEvent) {
    shell.emit(AppEvent::Session {
        session_id: session_id.to_string(),
        event,
    });
}

/// 把两条异步来源翻译成统一的 IPC 事件。
///
/// 正文增量不经过这里，见模块文档：它们要跟工具卡片严格保序，所以同步 `emit`。
///
/// carter 的 `ToolCallStarted` / `ToolResult` / `AskUser` **刻意丢弃**：
/// 前两者只带截断的预览（结果只有第一行 120 字符），界面上画不出有意义的卡片；
/// 后者是 carter 那个一问一答的 ask_user 工具，已经被我们自己的实现顶掉了。
/// 留着它们只会让每个工具出现两张卡片。
pub(super) async fn forward_events(
    shell: Arc<AppShell>,
    session_id: String,
    asks: Arc<AskRegistry>,
    mut ask_rx: mpsc::UnboundedReceiver<AskRequest>,
) {
    let mut ask_open = true;

    let send = |shell: &AppShell, event: SessionEvent| {
        shell.emit(AppEvent::Session {
            session_id: session_id.clone(),
            event,
        });
    };

    while ask_open {
        tokio::select! {
            req = ask_rx.recv(), if ask_open => match req {
                Some(AskRequest::Questions { id, questions, reply }) => {
                    // 发送端存住：前端要跨一次进程边界才回得来，而 oneshot 只能发一次。
                    asks.insert(id, questions.clone(), reply);
                    send(&shell, SessionEvent::AskForm { id, questions });
                }
                None => ask_open = false,
            },
        }
    }
}

/// 给工具池里的每个工具套一层记录包装（完整参数与结果 → 界面）。
///
/// 主会话用它（编号取模型给的 `call_id`，见 [`crate::tools::record::Recorded`]）。
/// 抽成自由函数是为了能直接测「一次调用只画一张卡」。
pub(super) fn wrap_recorded(
    bare: carter::tools::ToolRegistry,
    tx: crate::tools::record::ToolTx,
    live: Option<Arc<crate::shell::LiveLog>>,
    running: Option<Arc<crate::tools::control::RunningTools>>,
    activity: Arc<AtomicU32>,
) -> carter::tools::ToolRegistry {
    let seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut out = carter::tools::ToolRegistry::empty();
    for t in bare.tools() {
        out.push(Arc::new(
            crate::tools::record::Recorded::new(
                t.clone(),
                tx.clone(),
                seq.clone(),
                activity.clone(),
            )
            .with_live(live.clone())
            .with_running(running.clone())
            .with_model_call_id(),
        ));
    }
    out
}

/// 工具卡片 → IPC 事件。**同步**：与正文增量走同一条时间线，顺序才不会反过来。
pub(super) fn emit_tool_event(
    shell: Arc<AppShell>,
    session_id: String,
) -> crate::tools::record::ToolTx {
    Arc::new(move |ev| {
        let event = match ev {
            ToolEvent::Started {
                call_id,
                name,
                args,
            } => SessionEvent::ToolStarted {
                call_id,
                name,
                args,
            },
            ToolEvent::Finished {
                call_id,
                ok,
                content,
                elapsed_ms,
            } => SessionEvent::ToolFinished {
                call_id,
                ok,
                content,
                elapsed_ms,
            },
            ToolEvent::Agent { call_id, agents } => SessionEvent::AgentProgress { call_id, agents },
        };
        shell.emit(AppEvent::Session {
            session_id: session_id.clone(),
            event,
        });
    })
}
