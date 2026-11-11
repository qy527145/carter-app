use crate::prelude::*;

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct CreatedSession {
    session_id: String,
    model: String,
    /// 模型引用 `渠道/显示名`（配置里的写法）。界面上的模型标签用它 ——
    /// `model` 是真正发给上游的名字，同一个名字可能挂在多个渠道上。
    model_ref: String,
    cwd: String,
    history: Vec<MessageView>,
    /// 这条会话现有的长任务目标（`/goal`）。续接时从盘上读回来。
    goal: Option<carter_app_core::GoalView>,
    /// 历史里每一轮实际改过的文件。事件不回放，所以随历史一起带回。
    changes: Vec<carter_app_core::TurnFiles>,
    /// 变更条的统计起点（点过「清空」之后只汇总这一轮及之后的）。
    changes_since: u32,
    /// 历史里每一轮用户打的原文（展开之前）。界面拿它回填「编辑」框。
    prompts: Vec<carter_app_core::prompts::TurnPrompt>,
    /// 历史里每一轮的起止时刻。界面在消息旁边显示时间（见 `timeline.rs`）。
    times: Vec<carter_app_core::timeline::TurnTime>,
    /// 接上的是一条正在跑的会话：界面要显示成「生成中」。
    running: bool,
    /// 正在跑的那一轮已经产出的正文与工具调用，界面先把它们画出来。
    live: Vec<LiveView>,
    /// 这一轮正在等用户回答的问题：错过了 `ask_form` 事件时靠它把表单补出来。
    pending_ask: Option<carter_app_core::protocol::PendingAsk>,
    /// 这条会话现在的 todo 列表。`todo_updated` 事件不回放，切走再接上时靠它补回来。
    todos: Vec<carter_app_core::TodoView>,
    /// 这一轮里已经跑完的工具。工具事件不回放，接回时靠它把卡片上的结果补齐
    /// （否则最后一张卡会一直停在「运行中」）。
    finished_tools: Vec<carter_app_core::ToolUsageView>,
    /// 这条会话此刻还在跑的后台命令。事件不回放，切走再接上时靠它把「还有后台
    /// 任务在跑」那条提醒补回来。
    background_shells: Vec<carter_app_core::protocol::BackgroundShellView>,
}

/// 进行中那一轮的一条记录（`LiveItem` 的界面形态）。
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LiveView {
    Text {
        text: String,
    },
    /// 这一轮中途用户插的话（原文）。
    User {
        text: String,
    },
    Tool {
        call_id: String,
        name: String,
        args: String,
        result: Option<String>,
    },
}

impl CreatedSession {
    fn from_started(started: carter_app_core::StartedSession) -> Self {
        use carter_app_core::shell::LiveItem;
        let handle = &started.handle;
        Self {
            session_id: handle.id.clone(),
            model: handle.model.load().api_name.clone(),
            model_ref: handle.model.load().key.clone(),
            cwd: handle.cwd.to_string_lossy().to_string(),
            history: started.history,
            goal: started.goal,
            changes: started.changes,
            changes_since: started.changes_since,
            prompts: started.prompts,
            times: started.times,
            running: started.running,
            live: started
                .live
                .into_iter()
                .map(|x| match x {
                    LiveItem::Text(text) => LiveView::Text { text },
                    LiveItem::User(text) => LiveView::User { text },
                    LiveItem::Tool {
                        call_id,
                        name,
                        args,
                        result,
                    } => LiveView::Tool {
                        call_id,
                        name,
                        args,
                        result,
                    },
                })
                .collect(),
            pending_ask: started.pending_ask,
            todos: started.todos,
            finished_tools: started.finished_tools,
            background_shells: started.background_shells,
        }
    }
}

/// 新建或续接一个会话，并把已有历史一并带回。
///
/// **必须是 `async fn`。** 它内部要 spawn 会话任务（`tokio::spawn`），而后者要求
/// 在 tokio runtime 上下文里调用 —— 同步的 Tauri 命令跑在主线程上、没有 runtime，
/// 会直接 panic。写成 async 后 Tauri 会把它放进自己的 tokio runtime 执行。
#[tauri::command]
pub(crate) async fn create_session(
    shell: State<'_, Arc<AppShell>>,
    cwd: Option<String>,
    model: Option<String>,
    resume: bool,
    resume_id: Option<String>,
) -> Result<CreatedSession, String> {
    // 按 id 打开一条**还在后台跑**的会话：接上它，不关掉重开。
    //
    // 这类会话是助理的工单、定时任务开的那条，或者页面重载前界面连着的那条。
    // 关掉重开会打断它正在跑的命令（关会话必须先让工具停下才能放掉会话文件），
    // 而用户点开它只是想看一眼 —— 接上就够了，正在跑的那一轮照常往下走。
    if let Some(started) = resume_id
        .as_deref()
        .and_then(|id| carter_app_core::session::attach_session(&shell, id))
    {
        return Ok(CreatedSession::from_started(started));
    }
    // 目标会话刚好在收尾（工单那一轮跑完、助理正在关它）：不等的话这里会直接
    // 报「还在收尾，请稍后再打开」—— 从用户视角看就是「点了没反应，再点一次才好」。
    // 最多等几秒；超时仍然照常往下走，让 `start_session` 给出那句明确的错误。
    if let Some(id) = resume_id.as_deref() {
        for _ in 0..50 {
            if !shell.is_closing(id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Some(started) = carter_app_core::session::attach_session(&shell, id) {
                return Ok(CreatedSession::from_started(started));
            }
        }
    }
    let opts = SessionOptions {
        cwd: cwd.map(PathBuf::from).unwrap_or_else(default_cwd),
        model,
        resume_latest: resume,
        resume_id,
        new_id: None,
        title: None,
    };
    let started =
        carter_app_core::start_session(shell.inner().clone(), opts).map_err(|e| e.to_string())?;
    started
        .handle
        .attached
        .store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(CreatedSession::from_started(started))
}

/// 显示常驻的助理对话：它在后台跑着就接上那一个，不关掉重开 —— 那会打断它正在说的话。
#[tauri::command]
pub(crate) async fn open_assistant_chat(
    shell: State<'_, Arc<AppShell>>,
    model: Option<String>,
) -> Result<CreatedSession, String> {
    let started = carter_app_core::assistant::chat::open(shell.inner(), model)?;
    Ok(CreatedSession::from_started(started))
}

#[derive(Serialize)]
pub(crate) struct CreatedSide {
    session_id: String,
    model: String,
    /// 见 [`CreatedSession::model_ref`]。
    model_ref: String,
    cwd: String,
    /// 继承下来的历史（打开那一刻的快照）。界面把它折叠在顶部，让用户知道
    /// 「这条会话看得见主对话说到哪了」。
    inherited: Vec<MessageView>,
}

/// 开一个侧边聊天（`/btw`）：继承主会话上下文、不落盘、关掉即丢。
///
/// **也必须是 `async fn`**：理由同 `create_session`。
#[tauri::command]
pub(crate) async fn create_side_session(
    shell: State<'_, Arc<AppShell>>,
    parent_session_id: String,
) -> Result<CreatedSide, String> {
    let started = carter_app_core::start_side_session(shell.inner().clone(), &parent_session_id)?;
    Ok(CreatedSide {
        session_id: started.handle.id.clone(),
        model: started.model,
        model_ref: started.handle.model.load().key.clone(),
        cwd: started.cwd.to_string_lossy().to_string(),
        inherited: started.inherited,
    })
}

/// 往会话投递一条命令（发消息 / 取消 / 换模型 / 作答）。
#[tauri::command]
pub(crate) fn send_command(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
    command: Command,
) -> Result<(), String> {
    if shell.send(&session_id, command) {
        Ok(())
    } else {
        // 会话已经结束（窗口关掉重开、任务退出）—— 让前端知道要重建会话，
        // 而不是把消息发进黑洞。
        Err(format!("会话 {session_id} 已结束"))
    }
}

/// 关闭会话，**等任务真的退出了才返回**。
///
/// 前端换会话的顺序是「关旧的 → 开新的」。不等旧任务退出就按 id 重开同一条
/// 会话的话，会有两个 runner 同时往一个会话文件追加（见 `AppShell::close_session`）。
#[tauri::command]
pub(crate) async fn close_session(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
) -> Result<(), String> {
    shell.close_session(&session_id).await;
    Ok(())
}

/// 界面换走了这条会话。正在跑就让它跑完再关，见 `AppShell::detach_session`。
#[tauri::command]
pub(crate) async fn detach_session(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
) -> Result<(), String> {
    shell.inner().detach_session(&session_id).await;
    Ok(())
}

/// 用户在记忆建议条上点了「记住」。项目级记忆写进会话的工作目录（`cwd`）。
///
/// 不经过会话任务：写一个文件不需要排在正在跑的那一轮后面，而且建议可能在会话
/// 已经关掉之后才被点。
#[tauri::command]
pub(crate) fn accept_memory(
    cwd: String,
    candidate: carter_app_core::memory_suggest::MemoryCandidate,
) -> Result<String, String> {
    carter_app_core::memory_suggest::accept(Path::new(&cwd), &candidate)
}

/// 列举会话。`all = false` 只看 `cwd`（不给就是默认目录），`true` 跨所有项目。
///
/// `async` 不是为了 await：它要把每个会话文件读一遍折叠出元信息，会话一多就是几十上百
/// 毫秒。同步命令跑在主线程上，这段时间里界面上的动画全停（换会话时正好有一段飞入
/// 动效在走，表现就是「点下去卡一下」）。
#[tauri::command]
pub(crate) async fn list_sessions(all: bool, cwd: Option<String>) -> Vec<SessionRow> {
    let cwd = cwd.map(PathBuf::from).unwrap_or_else(default_cwd);
    sessions::list(&cwd, all)
}

/// 此刻正在跑一轮的会话 id。侧栏据此给每条在跑的会话亮「生成中」的点。
#[tauri::command]
pub(crate) fn busy_sessions(shell: State<'_, Arc<AppShell>>) -> Vec<String> {
    shell.busy_sessions()
}

/// 改标题。走「追加一条 Title 记录」，所以不需要动会话文件其余部分。
#[tauri::command]
pub(crate) fn rename_session(session_id: String, title: String) -> Result<(), String> {
    sessions::rename(&session_id, &title)?;
    Ok(())
}

/// fork 一条会话，返回新会话的 id。
///
/// `keep_before` = 只保留第几轮**之前**的历史（不含该轮）；不给 = 整条复制
/// （会话列表里的 fork）。界面上的两处入口：从某条回答之后分叉传 `turn + 1`，
/// 改写某句提问再问传 `turn`（新会话开好后前端会把改写后的句子发进去）。
#[tauri::command]
pub(crate) fn fork_session(session_id: String, keep_before: Option<u32>) -> Result<String, String> {
    sessions::fork(&session_id, keep_before)
}

/// 删除一个会话文件。不可恢复 —— 调用方要先确认。
#[tauri::command]
pub(crate) async fn delete_session(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
) -> Result<(), String> {
    // 先关会话再删文件。会话任务持有同一份会话文件的 Recorder，不先关掉的话
    // 它下一轮 append 会把文件重新写出来 —— 表现为「删了又回来了」。
    shell.close_session(&session_id).await;
    // 目标与变更记录也一并清掉：那两个目录都是按会话 id 分文件的，不删就会
    // 跟着历史会话一直长。
    carter_app_core::goal::remove(&session_id);
    carter_app_core::changes::remove(&session_id);
    carter_app_core::prompts::remove(&session_id);
    carter_app_core::timeline::remove(&session_id);
    sessions::delete(&session_id)
}

/// 删除某个目录（项目）下的**全部**会话。不可恢复 —— 调用方要先确认。
///
/// 只删会话记录，不碰项目目录本身。逐条走与 `delete_session` 相同的收尾：先关掉
/// 还在跑的（否则它下一轮 append 会把文件写回来），再清目标 / 变更 / 原文记录。
/// 返回删掉的条数。
#[tauri::command]
pub(crate) async fn delete_project(
    shell: State<'_, Arc<AppShell>>,
    cwd: String,
) -> Result<usize, String> {
    // 助理对话住在家目录下，但不算这个项目的会话：侧栏的条数里没有它，也不该被连带删掉。
    let mut ids = sessions::ids_in(&cwd);
    ids.retain(|id| id != carter_app_core::assistant::CHAT_SESSION_ID);
    for id in &ids {
        shell.close_session(id).await;
        carter_app_core::goal::remove(id);
        carter_app_core::changes::remove(id);
        carter_app_core::prompts::remove(id);
        carter_app_core::timeline::remove(id);
        sessions::delete(id)?;
    }
    Ok(ids.len())
}

/// 全文搜索所有会话。`async` 理由同 `list_sessions`：要读遍全部会话文件。
#[tauri::command]
pub(crate) async fn search_sessions(query: String) -> Vec<carter_app_core::SearchHit> {
    sessions::search(&query)
}

/// 导出会话到下载目录，返回写出的文件路径。
#[tauri::command]
pub(crate) fn export_session(session_id: String, format: String) -> Result<String, String> {
    let fmt = match format.as_str() {
        "json" => ExportFormat::Json,
        "markdown" | "md" => ExportFormat::Markdown,
        other => return Err(format!("不支持的导出格式 `{other}`")),
    };
    let (filename, content) = sessions::export(&session_id, fmt)?;

    let home = default_cwd();
    let downloads = home.join("Downloads");
    let dir = if downloads.is_dir() { downloads } else { home };
    let path = dir.join(filename);
    std::fs::write(&path, content).map_err(|e| format!("写 {} 失败：{e}", path.display()))?;
    Ok(path.to_string_lossy().to_string())
}
