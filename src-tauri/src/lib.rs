//! Tauri 外壳：命令层 + 事件桥。
//!
//! 这一层刻意保持很薄 —— 每个命令做三件事：取参数、转给 `carter-app-core`、
//! 把错误转成字符串。真正的状态与编排在 `carter-app-core` 里，那里不依赖 Tauri，
//! 可以脱离 webview 测。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, Manager, State};

use carter_app_core::app_settings::AppSettings;
use carter_app_core::catalog;
use carter_app_core::config_edit::{self, McpDraft, ProviderDraft};
use carter_app_core::sessions::{self, ExportFormat};
use carter_app_core::skills::{self, Scan, SkillDraft};
use carter_app_core::{
    AppEvent, AppShell, Command, MessageView, ProviderDetail, SessionRow, SessionOptions,
};

/// 前端订阅的事件名。
///
/// 字符串字面量散落两边，改名会**静默**破坏订阅（前端不再收到任何事件，也不报错）。
/// 改这里就必须同步改 `src/lib/api.ts` 里的 `EVENT_NAME`。
const CARTER_EVENT: &str = "carter://event";

/// 桌面应用没有「当前工作目录」这个概念 —— 进程的 cwd 取决于从哪启动的，
/// 用户不会预期会话记在 `/` 或 `C:\Windows\System32` 下。默认落在家目录。
/// 日志写进 `~/.carter-app/app.log`。
///
/// 桌面应用没有终端，不装订阅者的话 `tracing::warn!` 全都丢掉 —— 出了「会话被打断」
/// 「工单失败」这类问题，事后无据可查。只记 info 及以上，`CARTER_LOG` 可以改；
/// 每次启动超过 5 MB 就截掉重来，不做轮转。
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let path = carter_app_core::paths::app_home().join("app.log");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(!too_big)
        .write(true)
        .truncate(too_big)
        .open(&path);
    let Ok(file) = file else { return };
    let filter = EnvFilter::try_from_env("CARTER_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,carter_app_core=info,carter=warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .try_init();
}

fn default_cwd() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

// ---------------------------------------------------------------------------
// 应用信息
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    /// 默认工作目录（会话按目录分文件存）。
    default_cwd: String,
    /// 当前配置里的默认模型引用。
    default_model: String,
    /// 配置文件位置。设置页要把这条路径显示出来 —— 有些事只能去文件里做。
    config_path: String,
    config_exists: bool,
    models_cache_exists: bool,
    /// 界面设置随应用信息一起返回，省掉启动时的一次往返（否则主题会闪一下）。
    settings: AppSettings,
}

#[tauri::command]
fn app_info(shell: State<'_, Arc<AppShell>>) -> AppInfo {
    let config = shell.config.load();
    let config_path = config_edit::config_path();
    AppInfo {
        name: "carter-app",
        version: env!("CARGO_PKG_VERSION"),
        default_cwd: default_cwd().to_string_lossy().to_string(),
        default_model: config.agent.model.clone(),
        config_path: config_path.to_string_lossy().to_string(),
        config_exists: config_path.exists(),
        models_cache_exists: carter::config::paths::models_cache_path().exists(),
        settings: AppSettings::load(),
    }
}

/// 开发用：打开 WebView 的开发者工具。
///
/// 应用接管右键菜单之后，默认菜单里那个「检查元素」也没了。前端只在 dev 构建里
/// 挂这个入口（见 `src/components/ui/ContextMenu.tsx`）。
///
/// `WebviewWindow::open_devtools` 这个方法本身只在**开了 `devtools` feature**
/// 的构建里存在（debug 构建默认开，release 不开）。所以这里必须按
/// `debug_assertions` 分叉，否则 `tauri build` 会直接编译不过 —— 不是「调用
/// 没有效果」，而是整个 release 包构建失败。
#[tauri::command]
fn open_devtools(webview: tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        webview.open_devtools();
        Ok(())
    }
    #[cfg(not(debug_assertions))]
    {
        // 正式包里前端根本不会挂这个入口；真被调到也只回一句说明。
        let _ = webview;
        Err("正式包不提供开发者工具".into())
    }
}

// ---------------------------------------------------------------------------
// 界面设置
//
// 只有写入口 —— 读走 `app_info`，它把设置一起带回来（省掉启动时的一次往返，
// 否则主题会在首帧闪一下）。
// ---------------------------------------------------------------------------

#[tauri::command]
fn set_app_settings(settings: AppSettings) -> Result<(), String> {
    settings.save()
}

// ---------------------------------------------------------------------------
// 会话
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CreatedSession {
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
}

/// 进行中那一轮的一条记录（`LiveItem` 的界面形态）。
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LiveView {
    Text { text: String },
    Tool { call_id: String, name: String, args: String, result: Option<String> },
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
                    LiveItem::Tool { call_id, name, args, result } => {
                        LiveView::Tool { call_id, name, args, result }
                    }
                })
                .collect(),
        }
    }
}

/// 新建或续接一个会话，并把已有历史一并带回。
///
/// **必须是 `async fn`。** 它内部要 spawn 会话任务（`tokio::spawn`），而后者要求
/// 在 tokio runtime 上下文里调用 —— 同步的 Tauri 命令跑在主线程上、没有 runtime，
/// 会直接 panic。写成 async 后 Tauri 会把它放进自己的 tokio runtime 执行。
#[tauri::command]
async fn create_session(
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
    let opts = SessionOptions {
        cwd: cwd.map(PathBuf::from).unwrap_or_else(default_cwd),
        model,
        resume_latest: resume,
        resume_id,
        new_id: None,
    };
    let started = carter_app_core::start_session(shell.inner().clone(), opts)
        .map_err(|e| e.to_string())?;
    started.handle.attached.store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(CreatedSession::from_started(started))
}

/// 显示常驻的助理对话：它在后台跑着就接上那一个，不关掉重开 —— 那会打断它正在说的话。
#[tauri::command]
async fn open_assistant_chat(
    shell: State<'_, Arc<AppShell>>,
    model: Option<String>,
) -> Result<CreatedSession, String> {
    let started = carter_app_core::assistant::chat::open(shell.inner(), model)?;
    Ok(CreatedSession::from_started(started))
}

#[derive(Serialize)]
struct CreatedSide {
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
async fn create_side_session(
    shell: State<'_, Arc<AppShell>>,
    parent_session_id: String,
) -> Result<CreatedSide, String> {
    let started =
        carter_app_core::start_side_session(shell.inner().clone(), &parent_session_id)?;
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
fn send_command(
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
async fn close_session(shell: State<'_, Arc<AppShell>>, session_id: String) -> Result<(), String> {
    shell.close_session(&session_id).await;
    Ok(())
}

/// 界面换走了这条会话。正在跑就让它跑完再关，见 `AppShell::detach_session`。
#[tauri::command]
async fn detach_session(shell: State<'_, Arc<AppShell>>, session_id: String) -> Result<(), String> {
    shell.inner().detach_session(&session_id).await;
    Ok(())
}

/// 用户在记忆建议条上点了「记住」。项目级记忆写进会话的工作目录（`cwd`）。
///
/// 不经过会话任务：写一个文件不需要排在正在跑的那一轮后面，而且建议可能在会话
/// 已经关掉之后才被点。
#[tauri::command]
fn accept_memory(
    cwd: String,
    candidate: carter_app_core::memory_suggest::MemoryCandidate,
) -> Result<String, String> {
    carter_app_core::memory_suggest::accept(Path::new(&cwd), &candidate)
}

/// 列举会话。`all = false` 只看 `cwd`（不给就是默认目录），`true` 跨所有项目。
#[tauri::command]
fn list_sessions(all: bool, cwd: Option<String>) -> Vec<SessionRow> {
    let cwd = cwd.map(PathBuf::from).unwrap_or_else(default_cwd);
    sessions::list(&cwd, all)
}

/// 改标题。走「追加一条 Title 记录」，所以不需要动会话文件其余部分。
#[tauri::command]
fn rename_session(session_id: String, title: String) -> Result<(), String> {
    sessions::rename(&session_id, &title)?;
    Ok(())
}

/// fork 一条会话，返回新会话的 id。
///
/// `keep_before` = 只保留第几轮**之前**的历史（不含该轮）；不给 = 整条复制
/// （会话列表里的 fork）。界面上的两处入口：从某条回答之后分叉传 `turn + 1`，
/// 改写某句提问再问传 `turn`（新会话开好后前端会把改写后的句子发进去）。
#[tauri::command]
fn fork_session(session_id: String, keep_before: Option<u32>) -> Result<String, String> {
    sessions::fork(&session_id, keep_before)
}

/// 删除一个会话文件。不可恢复 —— 调用方要先确认。
#[tauri::command]
async fn delete_session(shell: State<'_, Arc<AppShell>>, session_id: String) -> Result<(), String> {
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
async fn delete_project(shell: State<'_, Arc<AppShell>>, cwd: String) -> Result<usize, String> {
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

/// 全文搜索所有会话。
#[tauri::command]
fn search_sessions(query: String) -> Vec<carter_app_core::SearchHit> {
    sessions::search(&query)
}

/// 导出会话到下载目录，返回写出的文件路径。
#[tauri::command]
fn export_session(session_id: String, format: String) -> Result<String, String> {
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

// ---------------------------------------------------------------------------
// 渠道 / 模型 / 协议
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ChannelRow {
    name: String,
    kind: String,
    /// 实际走哪种线协议（kind 可能是别名或兼容写法）。
    protocol: String,
    base_url: String,
    has_api_key: bool,
    /// 会被忽略的服务端工具 —— 该协议没有对应物或名字不认识。
    unsupported_server_tools: Vec<String>,
    /// 该协议**支持**的服务端工具（用来在界面上把支持与不支持并列展示）。
    supported_server_tools: Vec<String>,
    models: Vec<String>,
    is_default: bool,
}

#[tauri::command]
fn list_channels(shell: State<'_, Arc<AppShell>>) -> Vec<ChannelRow> {
    let config = shell.config.load();
    let mut rows: Vec<ChannelRow> = config
        .providers
        .iter()
        .map(|(name, p)| {
            // 不绕 `resolve_provider` —— 那条路要先解析出模型，而模型是从
            // models.dev 缓存里查的；这里只是想看渠道配置，不该依赖它。
            let unsupported = carter_app_core::unsupported_server_tools(p);
            // 「支持哪些」= 声明里没被丢掉的那些。
            let supported: Vec<String> = p
                .server_tools
                .iter()
                .filter(|t| !unsupported.contains(t))
                .cloned()
                .collect();
            let mut models: Vec<String> = p.models.keys().cloned().collect();
            models.sort();
            ChannelRow {
                name: name.clone(),
                kind: p.kind.clone(),
                protocol: p.protocol().to_string(),
                base_url: p.base_url.clone().unwrap_or_default(),
                has_api_key: p.api_key.is_some(),
                unsupported_server_tools: unsupported,
                supported_server_tools: supported,
                models,
                // 默认渠道 = 配置里 `agent.model` 指向的那个 provider。
                is_default: config.agent.model.starts_with(&format!("{name}/")),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// 编辑弹窗用的**原始**渠道配置（`${ENV}` 保持字面量，不回填明文密钥）。
#[tauri::command]
fn get_provider_detail(name: String) -> Result<ProviderDetail, String> {
    config_edit::provider_detail(&name)
}

/// 某个协议能吃下哪些服务端工具。返回 `(生效的, 不生效的)`。
///
/// 编辑弹窗勾选时就要把不生效的标出来，而不是等保存完再让渠道列表报警。
#[tauri::command]
fn check_server_tools(kind: String, tools: Vec<String>) -> (Vec<String>, Vec<String>) {
    carter_app_core::check_server_tools(&kind, &tools)
}

/// 写一份渠道草稿并热重载。返回写回后的详情。
#[tauri::command]
fn save_provider(
    shell: State<'_, Arc<AppShell>>,
    draft: ProviderDraft,
) -> Result<ProviderDetail, String> {
    config_edit::save_provider_reload(shell.inner(), &draft)
}

#[tauri::command]
fn delete_provider(shell: State<'_, Arc<AppShell>>, name: String) -> Result<(), String> {
    config_edit::remove_provider_reload(shell.inner(), &name)
}

#[tauri::command]
fn save_model(
    shell: State<'_, Arc<AppShell>>,
    provider: String,
    model: carter_app_core::ModelDraft,
) -> Result<ProviderDetail, String> {
    let mut detail = config_edit::provider_detail(&provider)?;
    // 按名字 upsert，其余模型原样带走。
    match detail.models.iter_mut().find(|m| m.name == model.name) {
        Some(slot) => {
            slot.meta = model.meta.clone();
            slot.api_name = model.api_name.clone();
            // 草稿里是字符串；详情里存的是原样的 TOML 值。
            slot.default_reasoning = model
                .default_reasoning
                .clone()
                .map(serde_json::Value::String);
        }
        None => detail.models.push(carter_app_core::config_edit::ModelDetail {
            name: model.name.clone(),
            meta: model.meta.clone(),
            api_name: model.api_name.clone(),
            default_reasoning: model
                .default_reasoning
                .clone()
                .map(serde_json::Value::String),
        }),
    }
    save_provider_from_detail(shell.inner(), &detail)
}

#[tauri::command]
fn delete_model(
    shell: State<'_, Arc<AppShell>>,
    provider: String,
    model: String,
) -> Result<ProviderDetail, String> {
    let mut detail = config_edit::provider_detail(&provider)?;
    let before = detail.models.len();
    detail.models.retain(|m| m.name != model);
    if detail.models.len() == before {
        return Err(format!("渠道 `{provider}` 下没有模型 `{model}`"));
    }
    save_provider_from_detail(shell.inner(), &detail)
}

/// 把一份已读出的详情再写回去。
///
/// 注意 `api_key` 传 `None` —— 详情里那个字段是**原始文本**（可能含 `${VAR}`），
/// 但用户没改它就不该重写。`None` 的语义正是「不动」（见 config_edit 的三态约定）。
fn save_provider_from_detail(
    shell: &Arc<AppShell>,
    detail: &ProviderDetail,
) -> Result<ProviderDetail, String> {
    let draft = ProviderDraft {
        name: detail.name.clone(),
        kind: detail.kind.clone(),
        api_key: None,
        base_url: Some(detail.base_url.clone()),
        server_tools: detail.server_tools.clone(),
        server_tool_max_uses: detail.server_tool_max_uses,
        models: detail
            .models
            .iter()
            .map(|m| carter_app_core::ModelDraft {
                name: m.name.clone(),
                meta: m.meta.clone(),
                api_name: m.api_name.clone(),
                // 表形态（`{ budget = 1024 }`）在这里会落成 None = 不动那个键。
                default_reasoning: config_edit::reasoning_to_draft(&m.default_reasoning),
            })
            .collect(),
        rename_from: None,
    };
    config_edit::save_provider_reload(shell, &draft)
}

/// 全部模型引用 + 从 models.dev 解出来的元数据。单行解析失败只标在该行上。
///
/// `async` + `spawn_blocking`：每个模型都要重新解析一遍 models.dev 缓存，整张表
/// 要好几秒，而界面一启动就会调它。同步命令跑在主线程上，这几秒窗口完全没有响应。
#[tauri::command]
async fn list_models(
    shell: State<'_, Arc<AppShell>>,
) -> Result<Vec<carter_app_core::ModelRow>, String> {
    let config = shell.config.load_full();
    tokio::task::spawn_blocking(move || catalog::list(&config))
        .await
        .map_err(|e| format!("读取模型列表中断：{e}"))
}

#[tauri::command]
fn set_default_model(shell: State<'_, Arc<AppShell>>, model_ref: String) -> Result<(), String> {
    config_edit::set_default_model_reload(shell.inner(), &model_ref)
}

/// 设副模型（`agent.fast_model`）：压缩上下文、生成标题用的那个。`None` = 清除，
/// 回落主模型 —— 所以界面上「清除副模型」和「设为副模型」共用一个命令。
#[tauri::command]
fn set_fast_model(
    shell: State<'_, Arc<AppShell>>,
    model_ref: Option<String>,
) -> Result<(), String> {
    config_edit::set_fast_model_reload(shell.inner(), model_ref.as_deref())
}

/// 自动压缩的当前设置（`agent.compact_enabled` / `agent.compact_threshold_ratio`）。
#[derive(serde::Serialize)]
struct CompactionSettings {
    enabled: bool,
    ratio: f64,
}

#[tauri::command]
fn get_compaction(shell: State<'_, Arc<AppShell>>) -> CompactionSettings {
    let cfg = shell.config.load();
    CompactionSettings {
        enabled: cfg.agent.compact_enabled,
        ratio: cfg.agent.compact_threshold_ratio,
    }
}

#[tauri::command]
fn set_compaction(
    shell: State<'_, Arc<AppShell>>,
    enabled: bool,
    ratio: f64,
) -> Result<(), String> {
    config_edit::set_compaction_reload(shell.inner(), enabled, ratio)
}

// ---------------------------------------------------------------------------
// 定时任务
// ---------------------------------------------------------------------------

/// 全部定时任务（停用的排在后面，其余按下次触发时间）。
#[tauri::command]
fn list_schedules(shell: State<'_, Arc<AppShell>>) -> Vec<carter_app_core::schedules::Schedule> {
    shell.schedules.list()
}

/// 设置页里新建 / 编辑一条。
///
/// 时间是「相对现在」还是「绝对」由 `kind` 自己决定，这里只做一件事：**节奏变了**
/// 就重算下一次（否则改完「每天 9:00」到「每天 18:00」，下次触发还停在原来的 9:00）。
#[tauri::command]
fn save_schedule(
    shell: State<'_, Arc<AppShell>>,
    schedule: carter_app_core::schedules::Schedule,
) -> Result<(), String> {
    let old = shell.schedules.get(&schedule.id);
    let mut next = schedule;
    next.name = next.name.trim().to_string();
    next.prompt = next.prompt.trim().to_string();
    if next.name.is_empty() {
        return Err("名字不能为空".into());
    }
    if next.prompt.is_empty() {
        return Err("到点要发的那句话不能为空".into());
    }
    match &old {
        Some(o) if o.kind == next.kind => {}
        _ => next.reschedule(),
    }
    // 编辑一条已经跑完的一次性任务：给它一个新时间，它会重新启用。
    if matches!(
        next.kind,
        carter_app_core::schedules::ScheduleKind::Once { at_ms }
            if at_ms <= carter_app_core::protocol::now_ms()
    ) {
        return Err("一次性任务的时间已经过去了，请改成将来的时间".into());
    }
    shell.schedules.upsert(next)
}

#[tauri::command]
fn delete_schedule(shell: State<'_, Arc<AppShell>>, id: String) -> Result<(), String> {
    shell.schedules.remove(&id).map(|_| ())
}

#[tauri::command]
fn toggle_schedule(
    shell: State<'_, Arc<AppShell>>,
    id: String,
    enabled: bool,
) -> Result<Option<carter_app_core::schedules::Schedule>, String> {
    shell.schedules.set_enabled(&id, enabled)
}

/// 「立即运行」：开一条新会话跑一次，不动它原来的节奏。
#[tauri::command]
async fn run_schedule(shell: State<'_, Arc<AppShell>>, id: String) -> Result<String, String> {
    carter_app_core::schedules::fire_now(shell.inner(), &id).await
}

// ---------------------------------------------------------------------------
// 常驻助理
// ---------------------------------------------------------------------------

use carter_app_core::assistant::runtime as assistant;

#[tauri::command]
fn get_assistant(shell: State<'_, Arc<AppShell>>) -> assistant::AssistantView {
    assistant::view(&shell)
}

#[tauri::command]
fn save_assistant_memory(
    shell: State<'_, Arc<AppShell>>,
    id: Option<String>,
    text: String,
) -> Result<(), String> {
    assistant::save_memory(shell.inner(), id, text)
}

/// 往期存档里第 `number` 期的原始对话（面板上展开那一期时看）。
#[tauri::command]
fn get_assistant_episode(number: u32) -> Vec<carter_app_core::view::MessageView> {
    use carter_app_core::assistant::memory;
    carter_app_core::view::to_views(&memory::load_archive(&memory::archive_dir(), number))
}

#[tauri::command]
fn save_assistant_settings(
    shell: State<'_, Arc<AppShell>>,
    settings: carter_app_core::assistant::Settings,
) -> Result<(), String> {
    assistant::save_settings(shell.inner(), settings)
}

#[tauri::command]
fn save_assistant_source(
    shell: State<'_, Arc<AppShell>>,
    source: carter_app_core::assistant::Source,
) -> Result<(), String> {
    assistant::save_source(shell.inner(), source)
}

#[tauri::command]
fn delete_assistant_source(shell: State<'_, Arc<AppShell>>, id: String) -> Result<(), String> {
    assistant::delete_source(shell.inner(), &id)
}

/// 「立即检查」：现在就跑一次观察源。返回这次的新行数（第一次是 0 —— 只建基线）。
#[tauri::command]
async fn poll_assistant_source(shell: State<'_, Arc<AppShell>>, id: String) -> Result<u32, String> {
    assistant::poll_now(shell.inner(), &id).await
}

/// 试跑观察源的命令，不比对、不评估 —— 编辑框里「试一下」，让人看到命令到底吐了什么。
#[tauri::command]
async fn test_assistant_command(command: String, cwd: String) -> Result<String, String> {
    carter_app_core::assistant::source::run_command(&command, std::path::Path::new(&cwd)).await
}

#[tauri::command]
async fn assistant_ticket_action(
    shell: State<'_, Arc<AppShell>>,
    ticket_id: String,
    action: assistant::TicketAction,
) -> Result<(), String> {
    assistant::ticket_action(shell.inner(), &ticket_id, action).await
}

#[tauri::command]
async fn approve_assistant_decision(
    shell: State<'_, Arc<AppShell>>,
    decision_id: String,
) -> Result<String, String> {
    assistant::approve(shell.inner(), &decision_id).await
}

#[tauri::command]
fn dismiss_assistant_decision(
    shell: State<'_, Arc<AppShell>>,
    decision_id: String,
) -> Result<(), String> {
    assistant::dismiss(shell.inner(), &decision_id)
}

#[tauri::command]
async fn reassess_assistant_decision(
    shell: State<'_, Arc<AppShell>>,
    decision_id: String,
) -> Result<carter_app_core::assistant::Decision, String> {
    assistant::reassess(shell.inner(), &decision_id).await
}

/// 托盘：图标常驻菜单栏，标题显示要人处理的件数。
///
/// 窗口关掉之后应用不退出（见 `run` 里的 `CloseRequested`）—— 助理要接着盯观察源，
/// 托盘是它还活着的唯一证据，也是把窗口叫回来的入口。
const TRAY_ID: &str = "carter-tray";

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let open = MenuItem::with_id(app, "open", "打开 Carter", true, None::<&str>)?;
    let panel = MenuItem::with_id(app, "assistant", "助理面板", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &panel, &sep, &quit])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Carter")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "assistant" => {
                show_main(app);
                if let Some(shell) = app.try_state::<Arc<AppShell>>() {
                    shell.emit(AppEvent::AssistantOpen);
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 助理的事件 → 系统通知与托盘标题。
fn on_assistant_event(app: &tauri::AppHandle, shell: &AppShell, ev: &AppEvent) {
    match ev {
        AppEvent::AssistantAlert { title, body, .. } => {
            use tauri_plugin_notification::NotificationExt;
            let _ = app.notification().builder().title(title).body(body).show();
        }
        AppEvent::AssistantChanged => {
            let n = assistant::attention(&shell.assistant.snapshot());
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let tip = match n {
                    0 => "Carter".to_string(),
                    n => format!("Carter · {n} 件事等你处理"),
                };
                let _ = tray.set_tooltip(Some(tip));
                // 菜单栏上的数字：只在 macOS 上显示得出来，别的平台这一句什么都不做。
                let _ = tray.set_title(if n == 0 { None } else { Some(n.to_string()) });
            }
        }
        _ => {}
    }
}

/// models.dev 里某个服务商下的模型（给渠道编辑框的「从目录选」下拉用）。
#[tauri::command]
fn list_catalog_models(
    provider_id: String,
) -> Result<Vec<carter_app_core::CatalogEntry>, String> {
    catalog::provider_models(&provider_id)
}

/// 拉一次 models.dev 目录。返回拉到的模型条数。
#[tauri::command]
async fn refresh_model_catalog() -> Result<usize, String> {
    catalog::refresh().await
}

/// 某个目录服务商下值得预填的几个模型（同 family 只取最新、跳过弃用与非对话模型）。
#[tauri::command]
fn recommended_catalog_models(
    provider_id: String,
) -> Result<Vec<carter_app_core::CatalogEntry>, String> {
    catalog::recommended_models(&provider_id, 8)
}

/// 跨服务商搜目录条目（手动绑元数据时的搜索框）。
#[tauri::command]
fn search_catalog_models(query: String) -> Result<Vec<carter_app_core::CatalogHit>, String> {
    catalog::search_models(&query, 40)
}

/// 一批 meta 在目录里查得到什么。查不到的位置是 `null`（不是错误）—— 编辑框据此
/// 把那一行标成「未绑定 / meta 写错了」，而不是等保存时才炸。
#[tauri::command]
fn lookup_metas(metas: Vec<String>) -> Vec<Option<carter_app_core::catalog::MetaInfo>> {
    catalog::meta_infos(metas.iter().map(String::as_str))
}

/// 渠道模板列表。界面「新建渠道」时的那一排按钮。
#[tauri::command]
fn list_provider_templates() -> &'static [carter_app_core::templates::ProviderTemplate] {
    carter_app_core::templates::TEMPLATES
}

/// 这个 base_url 对应 models.dev 里的哪一家？认不出来是 `None`。
///
/// 界面用它决定「模型能不能直接从目录预填」，以及拉上游列表时把哪家当提示。
/// 与真正发请求无关 —— 请求永远走配置里的 base_url。
#[tauri::command]
fn detect_catalog_provider(base_url: String) -> Option<String> {
    catalog::detect_provider(&base_url)
}

/// 向渠道的上游要模型清单（`GET {base}/…/models`）并逐条对好 models.dev 元数据。
///
/// 密钥的三态与保存时一致：
/// - `api_key` 非空 → 用它（用户刚敲进去的）；
/// - 空串 + 给了 `provider` → 读配置里那个渠道已存的 key，做 `${ENV}` 展开后用；
/// - 空串 + 没给 `provider`（新建渠道、还没存过）→ 不带鉴权直接问。
///
/// 「编辑已有渠道」是第二种：界面压根拿不到明文密钥（见 `ProviderDetail` 的注释），
/// 但又必须能拉列表 —— 于是让它用磁盘上那份。
#[tauri::command]
async fn fetch_upstream_models(
    kind: String,
    base_url: String,
    api_key: String,
    provider: Option<String>,
) -> Result<carter_app_core::upstream_models::UpstreamModels, String> {
    let key = if !api_key.trim().is_empty() {
        api_key.trim().to_string()
    } else {
        provider
            .as_deref()
            .and_then(|name| config_edit::provider_detail(name).ok())
            .map(|d| d.api_key)
            // 配置里存的是 `${VAR}` 字面量，发出去之前要展开 —— 与 carter 读配置时
            // 同一套规则，否则「填了环境变量引用」的渠道在这里永远 401。
            .map(|raw| carter::config::interpolate(&raw))
            .unwrap_or_default()
    };
    carter_app_core::upstream_models::fetch_matched(&kind, &base_url, &key)
        .await
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ToolRow {
    name: String,
    description: String,
    /// 参数 JSON Schema，供界面展开查看。
    parameters: serde_json::Value,
    enabled: bool,
    /// `false` = 界面上不提供开关（只有 `task` 一例，它承载子 agent 能力）。
    toggleable: bool,
    /// 说明为什么不可关，或这个工具从哪来。
    note: Option<String>,
}

/// 本地会执行的工具清单。
///
/// 这是**本应用**装配出来的那一套（内置 + skill + ask_user + task）；
/// 厂商在服务端执行的工具（web_search 等）不在这里，它们是渠道能力，
/// 见 `list_channels`。MCP 工具也不在这里 —— 它们的名字要连上 server 才知道，
/// 见 `list_mcp_servers`。
#[tauri::command]
fn list_tools(shell: State<'_, Arc<AppShell>>, cwd: Option<String>) -> Vec<ToolRow> {
    let settings = AppSettings::load();
    let skills_on = shell.config.load().skills.enabled;

    let mut registry = carter::tools::ToolRegistry::builtin();
    if skills_on {
        registry.push(Arc::new(carter::skills::SkillTool::new(
            carter::config::paths::skills_dir(),
        )));
    }

    // 每个工具都过一遍 `carter_app_core::tools::substitute` —— 真正装配工具池时用的是
    // **同一个函数**。此前这里手抄了一遍 `bash` 与 `skill` 的构造，两处的说明迟早会漂，
    // 而设置页显示一份与真实行为对不上的文档，比不显示更坏（这是本文件自己的注释里
    // 早已写下的理由，现在才真正落实）。
    //
    // 这里不需要真的能跑：`substitute` 只读构造期参数，不碰 provider、不碰会话。
    // 会话 cwd 只用来让 `skill` 的说明指出项目级目录；没有会话时退回一个占位路径。
    let cwd = cwd
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| carter::config::paths::carter_home());
    let mut rows: Vec<ToolRow> = Vec::new();
    for t in registry.tools() {
        let t = carter_app_core::tools::substitute(t.clone(), &settings, &cwd);
        let name = t.name().to_string();
        let note = match name.as_str() {
            "bash" => Some(carter_app_core::platform::os_label().to_string()),
            // `note` 就是为「这个工具从哪来」准备的 —— 技能多了附加扫描路径之后，
            // 「它到底扫哪几个目录」是用户会问的第一个问题。
            "skill" => {
                let scan = Scan::from_settings_at(&settings, Some(&cwd));
                let extra = match settings.skill_roots.len() {
                    0 => String::new(),
                    n => format!(" 与 {n} 条附加扫描路径"),
                };
                Some(match &scan.project_enabled {
                    Some(p) => format!(
                        "扫 ~/.carter/skills、{} 里的项目技能{extra}",
                        p.display()
                    ),
                    None => format!("扫 ~/.carter/skills{extra}（当前会话的项目里若有 .carter/skills 也会扫）"),
                })
            }
            _ => None,
        };
        rows.push(ToolRow {
            enabled: settings.tool_enabled(&name),
            toggleable: true,
            note,
            description: t.description().to_string(),
            parameters: t.parameters(),
            name,
        });
    }

    // ask_user_question 的 schema 也来自本应用的实现（给一个没人读的通道即可）。
    {
        use carter::tools::Tool as _;
        let a = carter_app_core::tools::ask_user::AskUserTool::new(
            tokio::sync::mpsc::unbounded_channel().0,
            carter::agent::CancelToken::new(),
        );
        rows.push(ToolRow {
            enabled: settings.tool_enabled("ask_user_question"),
            toggleable: true,
            note: None,
            name: a.name().to_string(),
            description: a.description().to_string(),
            parameters: a.parameters(),
        });
    }

    // 后台命令的两个配套工具。它们跟着 `bash` 的 `run_in_background` 一起用，
    // 所以也要出现在开关列表里 —— 用户关掉 bash 时，这两个留着也没有意义，
    // 但那是用户自己的选择，界面上说清楚即可。
    {
        use carter_app_core::tools::background::{BackgroundShells, BashOutputTool, KillShellTool};
        let shells = BackgroundShells::new();
        let extras: Vec<(std::sync::Arc<dyn carter::tools::Tool>, &str)> = vec![
            (
                std::sync::Arc::new(BashOutputTool::new(shells.clone())),
                "读取 `bash` 以后台方式启动的命令的输出。",
            ),
            (
                std::sync::Arc::new(KillShellTool::new(shells)),
                "结束一条后台命令。",
            ),
        ];
        for (t, note) in extras {
            rows.push(ToolRow {
                enabled: settings.tool_enabled(t.name()),
                toggleable: true,
                note: Some(note.to_string()),
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            });
        }
    }

    // 跨会话的三个工具。只展示说明，不会真的执行 —— 发信方 id 给空串即可。
    {
        use carter::tools::Tool as _;
        use carter_app_core::tools::peer::{Kind, PeerTool};
        for (kind, note) in [
            (Kind::List, "列出本机可以发信的其他对话"),
            (Kind::Send, "给另一条对话发一段话，对方空闲时立刻处理、忙时在下次工具返回时读到"),
            (Kind::Read, "旁观另一条对话最近说了什么，不打扰它"),
        ] {
            let t = PeerTool::new(shell.inner().clone(), String::new(), kind);
            rows.push(ToolRow {
                enabled: settings.tool_enabled(t.name()),
                toggleable: true,
                note: Some(note.into()),
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            });
        }
    }

    // 定时任务工具。它要一份任务表 —— 用壳里那份**同一个** `Arc`，而不是新开一个：
    // 设置页与模型改的必须是同一份数据。
    {
        use carter::tools::Tool as _;
        let t = carter_app_core::tools::schedule::ScheduleTool::new(
            shell.schedules.clone(),
            cwd.to_string_lossy().to_string(),
        );
        rows.push(ToolRow {
            enabled: settings.tool_enabled(t.name()),
            toggleable: true,
            note: Some("到点自动新开一条对话".into()),
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters(),
        });
    }

    // `agent` 构造要一个 provider，这里拿不到 —— 单独补一行，标明它不可关。
    // 子 agent 的能力开关该是独立的一项，而不是混在「工具开关」里顺手关掉。
    rows.push(ToolRow {
        name: "agent".into(),
        description:
            "一次派生多个隔离子 agent 并行干活。每个子 agent 有独立上下文与工具池\
             （不含 agent 自身，不会再往下派生），并遵守同一份工具开关。"
                .into(),
        parameters: serde_json::Value::Null,
        enabled: true,
        toggleable: false,
        note: Some("承载子 agent 能力，暂不提供开关".into()),
    });

    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

// ---------------------------------------------------------------------------
// 技能
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SkillListing {
    skills: Vec<carter_app_core::SkillRow>,
    /// 全局开关（`[skills].enabled`）。关掉时整个技能目录都不加载。
    globally_enabled: bool,
    /// 扫描根一览：项目 → 用户 → 附加路径（顺序 = 优先级）。
    roots: Vec<skills::SkillRootRow>,
    /// Claude Code / Codex 这类默认位置的快捷选项。
    presets: Vec<skills::SkillPreset>,
    /// 被按名字忽略的外部技能。
    ///
    /// 单独回传是因为：一个被忽略的名字可能**再也没有对应的行**（它被同名技能遮蔽，
    /// 或者它所在的那条扫描路径被移除了）。那时候这是用户撤销它的唯一入口。
    ignored_skills: Vec<String>,
}

/// 技能命令共用的扫描：会话 cwd 决定项目级根。
///
/// 技能页拿的是**当前会话**的 cwd（见 `SkillsPage`）。没有会话时传 `None`，
/// 那就只有用户级与外部根 —— 与从前一致。
fn skill_scan(settings: &AppSettings, cwd: Option<&str>) -> Scan {
    let cwd = cwd
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    Scan::from_settings_at(settings, cwd.as_deref())
}

#[tauri::command]
fn list_skills(shell: State<'_, Arc<AppShell>>, cwd: Option<String>) -> SkillListing {
    let settings = AppSettings::load();
    let scan = skill_scan(&settings, cwd.as_deref());
    SkillListing {
        skills: skills::list(&scan),
        globally_enabled: shell.config.load().skills.enabled,
        roots: skills::roots(&scan),
        presets: skills::presets(&scan),
        ignored_skills: settings.ignored_skills.clone(),
    }
}

#[tauri::command]
fn read_skill(
    name: String,
    cwd: Option<String>,
    source: Option<String>,
) -> Result<String, String> {
    let scan = skill_scan(&AppSettings::load(), cwd.as_deref());
    skills::read(&scan, &name, source.as_deref())
}

/// 保存技能，返回实际写入的根目录（界面用它说明「存到了项目 / 用户目录」）。
#[tauri::command]
fn save_skill(
    shell: State<'_, Arc<AppShell>>,
    draft: SkillDraft,
    cwd: Option<String>,
) -> Result<String, String> {
    let scan = skill_scan(&AppSettings::load(), cwd.as_deref());
    let dir = skills::save(&scan, &draft)?;
    // 技能目录会进系统提示，所以要让跑着的会话下一轮重算 —— 发个事件叫前端刷新，
    // 会话侧本来每轮都会重读技能目录。
    shell.emit(AppEvent::ConfigChanged);
    Ok(dir.to_string_lossy().to_string())
}

/// 启停单个技能。本地的搬目录、外部的改忽略名单（分派在 `skills::set_enabled` 里）。
///
/// 回传新的设置：外部技能的开关写的是 `ignored_skills`，而前端 store 里那份设置是
/// 旧的。不回传的话，下一次改主题（`set_app_settings` 是**整体覆盖写入**）会把刚关掉
/// 的外部技能又打开。
/// `source` 是**点的是哪一行**（界面上那个 `SkillRow::source`）：同一个名字可能同时在
/// 项目、用户、外部三个根里出现，能区分它们的只有来源 —— 按名字猜会让「点外部那一行」
/// 变成点了没反应（本地目录此刻正好不在启用侧，搬目录那条路会直接返回 Ok）。
/// `None` 只用于从「已忽略」名单里恢复一个名字。
#[tauri::command]
fn toggle_skill(
    shell: State<'_, Arc<AppShell>>,
    name: String,
    enabled: bool,
    source: Option<String>,
    cwd: Option<String>,
) -> Result<AppSettings, String> {
    let mut settings = AppSettings::load();
    let scan = skill_scan(&settings, cwd.as_deref());
    if skills::set_enabled(&mut settings, &scan, &name, enabled, source.as_deref())? {
        settings.save()?;
    }
    shell.emit(AppEvent::ConfigChanged);
    Ok(settings)
}

#[tauri::command]
fn delete_skill(
    shell: State<'_, Arc<AppShell>>,
    name: String,
    source: Option<String>,
    cwd: Option<String>,
) -> Result<(), String> {
    let scan = skill_scan(&AppSettings::load(), cwd.as_deref());
    skills::delete(&scan, &name, source.as_deref())?;
    shell.emit(AppEvent::ConfigChanged);
    Ok(())
}

#[tauri::command]
fn set_skills_enabled(shell: State<'_, Arc<AppShell>>, enabled: bool) -> Result<(), String> {
    config_edit::set_skills_enabled_reload(shell.inner(), enabled)
}

/// 加一条技能扫描路径（Claude Code / Codex 的默认位置，或任意自定义父目录）。
///
/// 存进 `~/.carter-app/settings.json` 而不是 `~/.carter/config.toml`：carter 0.1.4 的
/// `[skills]` 只有一个 `enabled`，写个 CLI 不认的 `paths` 进去等于让配置文件说谎。
///
/// 回传新的设置，理由同 [`toggle_skill`]。
#[tauri::command]
fn add_skill_root(shell: State<'_, Arc<AppShell>>, path: String) -> Result<AppSettings, String> {
    let mut settings = AppSettings::load();
    skills::add_root(&mut settings, &path)?;
    settings.save()?;
    shell.emit(AppEvent::ConfigChanged);
    Ok(settings)
}

#[tauri::command]
fn remove_skill_root(shell: State<'_, Arc<AppShell>>, path: String) -> Result<AppSettings, String> {
    let mut settings = AppSettings::load();
    skills::remove_root(&mut settings, &path)?;
    settings.save()?;
    shell.emit(AppEvent::ConfigChanged);
    Ok(settings)
}

// ---------------------------------------------------------------------------
// IM 连接器
// ---------------------------------------------------------------------------
//
// 每个命令都是「立即返回」，真正干活的那条 `dws` 在后台跑，输出与结果走
// `Connector*` 事件。登录要等最长 15 分钟 —— 一条同步命令挂在那里的话，界面只能
// 干等，连「取消」都点不了（命令还没返回，下一次 invoke 也没法发）。

#[tauri::command]
async fn list_connectors() -> Vec<carter_app_core::ConnectorRow> {
    carter_app_core::connectors::list().await
}

#[tauri::command]
async fn install_connector_cli(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
) -> Result<(), String> {
    carter_app_core::connectors::install_cli(shell.inner(), &vendor).await
}

#[tauri::command]
async fn install_connector_skill(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
) -> Result<(), String> {
    carter_app_core::connectors::install_skill(shell.inner(), &vendor).await
}

/// 扫码登录。`recommend` = 顺带授予服务端推荐权限（官方给 Agent 用的一条路）。
#[tauri::command]
async fn login_connector(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
    recommend: bool,
) -> Result<(), String> {
    carter_app_core::connectors::login(shell.inner(), &vendor, recommend).await
}

/// 取消正在进行的登录。同步命令：它只是杀一个进程，要立刻生效。
#[tauri::command]
fn cancel_connector_login(vendor: String) {
    carter_app_core::connectors::cancel_login(&vendor);
}

#[tauri::command]
async fn logout_connector(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
) -> Result<(), String> {
    carter_app_core::connectors::logout(shell.inner(), &vendor).await
}

/// 配置应用（飞书）：`config init --new`。和登录一样会阻塞到用户在浏览器里完成，
/// 取消走 `cancel_connector_login`。
#[tauri::command]
async fn configure_connector_app(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
) -> Result<(), String> {
    carter_app_core::connectors::configure_app(shell.inner(), &vendor).await
}

/// 添加邮箱账号（邮件）。密码只经环境变量交给 CLI，由它存进系统钥匙串。
#[tauri::command]
async fn add_mail_account(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
    name: String,
    email: String,
    imap_host: String,
    smtp_host: String,
    password: String,
) -> Result<(), String> {
    carter_app_core::connectors::add_mail_account(
        shell.inner(),
        &vendor,
        &name,
        &email,
        &imap_host,
        &smtp_host,
        &password,
    )
    .await
}

#[tauri::command]
async fn remove_mail_account(
    shell: State<'_, Arc<AppShell>>,
    vendor: String,
    name: String,
) -> Result<(), String> {
    carter_app_core::connectors::remove_mail_account(shell.inner(), &vendor, &name).await
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct McpServerRow {
    name: String,
    transport: String,
    /// stdio 给 command，http 给 url。
    target: String,
    enabled: bool,
    args: Vec<String>,
    url: String,
    command: String,
    env: Vec<(String, String)>,
    headers: Vec<(String, String)>,
}

#[tauri::command]
fn list_mcp_servers(shell: State<'_, Arc<AppShell>>) -> Vec<McpServerRow> {
    let config = shell.config.load();
    let mut rows: Vec<McpServerRow> = config
        .mcp
        .servers
        .iter()
        .map(|(name, s)| McpServerRow {
            name: name.clone(),
            transport: s.transport.clone(),
            target: s
                .command
                .clone()
                .or_else(|| s.url.clone())
                .unwrap_or_default(),
            enabled: s.enabled,
            args: s.args.clone(),
            url: s.url.clone().unwrap_or_default(),
            command: s.command.clone().unwrap_or_default(),
            env: sorted_pairs(&s.env),
            headers: sorted_pairs(&s.headers),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// HashMap 的遍历顺序不稳定，界面里得是稳定的。
fn sorted_pairs(m: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = m.iter().map(|(k, val)| (k.clone(), val.clone())).collect();
    v.sort();
    v
}

#[tauri::command]
fn save_mcp_server(shell: State<'_, Arc<AppShell>>, draft: McpDraft) -> Result<(), String> {
    config_edit::save_mcp_server_reload(shell.inner(), &draft)
}

#[tauri::command]
fn delete_mcp_server(shell: State<'_, Arc<AppShell>>, name: String) -> Result<(), String> {
    config_edit::remove_mcp_server_reload(shell.inner(), &name)
}

// ---------------------------------------------------------------------------
// 输入框：斜杠命令、@ 文件、图片
// ---------------------------------------------------------------------------

/// `/` 菜单的命令列表：本应用真能执行的内置命令 + 用户自定义命令。
#[tauri::command]
fn list_commands(cwd: Option<String>) -> Vec<carter_app_core::CommandRow> {
    carter_app_core::input::list(&cwd.map(PathBuf::from).unwrap_or_else(default_cwd))
}

/// `@` 菜单的文件/目录列表（相对工作目录的路径，目录带尾随 `/`）。
///
/// `async` 不是为了 await，而是让它跑在异步线程池上：不带 `/` 的查询会
/// 遍历整棵目录树，同步命令会在主线程上做，一敲 `@` 就能把界面卡住。
#[tauri::command]
async fn list_files(cwd: Option<String>, query: String, limit: Option<usize>) -> Vec<String> {
    carter_app_core::input::list_files(
        &cwd.map(PathBuf::from).unwrap_or_else(default_cwd),
        &query,
        limit.unwrap_or(50),
    )
}

/// 把粘贴进来的图片存进 carter 的图片库，返回可插进消息文本的 `[img:...]` 引用。
#[tauri::command]
fn save_image(data: String, ext: Option<String>) -> Result<String, String> {
    carter_app_core::images::save_image(&data, ext.as_deref())
}

/// 把一条图片引用读成 data URL 供界面显示。读不到返回 null（文件被清理过等）。
///
/// 与 [`preview_file`] 同理走 `spawn_blocking`：对话正文里每张图都会调它，同步跑在
/// 主线程上会把界面卡住。
#[tauri::command]
async fn read_image(token: String) -> Option<String> {
    tokio::task::spawn_blocking(move || carter_app_core::images::read_image(&token))
        .await
        .ok()
        .flatten()
}

/// 文件预览：点击变更条里的一行时打开。
///
/// 读的是**现在磁盘上的内容**，不是历史快照 —— 预览要回答的是「它现在什么样」。
/// 返回的是可渲染的形状（文本 / 图片 data URL / 二进制 / 消失 / 读失败），
/// 前端不需要自己判断文件类型。
///
/// `async` + `spawn_blocking`：对话正文里的图片也走这一条（见 `src/components/Media.tsx`），
/// 而一张图要读盘 + base64（可以到十几 MB）。同步命令跑在主线程上，几张图就能把整个
/// 界面卡住。
#[tauri::command]
async fn preview_file(cwd: Option<String>, path: String) -> carter_app_core::FilePreview {
    let cwd = cwd.map(PathBuf::from).unwrap_or_else(default_cwd);
    let fallback = path.clone();
    tokio::task::spawn_blocking(move || carter_app_core::preview::preview(&cwd, &path))
        .await
        .unwrap_or_else(|e| carter_app_core::FilePreview::Error {
            path: fallback,
            message: format!("读取中断：{e}"),
        })
}

/// 变更条的固定忽略名单，给设置页展示用。
///
/// 从核心拿，不在前端抄一份 —— 抄的那份迟早和真实行为对不上，而「界面上写着会
/// 忽略、实际上没忽略」比不显示更糟（与工具页同一个道理）。
#[derive(serde::Serialize)]
struct ChangeRules {
    dirs: Vec<String>,
    files: Vec<String>,
}

#[tauri::command]
fn change_rules() -> ChangeRules {
    ChangeRules {
        dirs: carter_app_core::changes::builtin_ignored_dirs()
            .iter()
            .map(|s| s.to_string())
            .collect(),
        files: carter_app_core::changes::builtin_ignored_files()
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// 某个文件从第 `since` 轮起累计的 diff：点开预览时按需算（见 core 的 `changes.rs`）。
///
/// 只有 cwd 在 git 仓库里、且那几轮记下了基线时才有结果；其余情况返回
/// `unavailable` 与一句人话原因。`spawn_blocking`：要起 git 进程，别占着
/// async 的工作线程。
#[tauri::command]
async fn file_diff(
    session_id: String,
    since: u32,
    cwd: String,
    path: String,
) -> carter_app_core::FileDiff {
    let fallback = path.clone();
    tokio::task::spawn_blocking(move || {
        carter_app_core::changes::diff_for(
            &session_id,
            since,
            std::path::Path::new(&cwd),
            &path,
        )
    })
    .await
    .unwrap_or_else(|_| carter_app_core::FileDiff::Unavailable {
        path: fallback,
        reason: "计算 diff 失败".into(),
    })
}

// ---------------------------------------------------------------------------
// 报文检视
// ---------------------------------------------------------------------------

/// 报文列表项。只给概览 —— 完整记录里带着几 MB 的 SSE 帧，
/// 列表里全带上会让界面一开就卡。
#[derive(Serialize)]
struct ExchangeSummary {
    id: String,
    ts: u64,
    /// 「第几轮 / 第几次调用 / 干什么用的」，供界面排序与分组。
    turn: Option<u32>,
    round: Option<u32>,
    purpose: Option<String>,
    method: String,
    url: String,
    /// 上游渠道（`[providers.X]` 的 X）与本地模型引用。同一个模型可能配在多个
    /// 渠道上，列表里必须能一眼分出这条走的是哪个。
    channel: Option<String>,
    model: Option<String>,
    status: Option<u16>,
    ttfb_ms: Option<u128>,
    total_ms: Option<u128>,
    frame_count: usize,
    /// 还在进行中（请求已发出、响应没收完）。界面据此显示「进行中」而不是「—」。
    in_flight: bool,
}

fn purpose_str(p: carter::provider::RequestPurpose) -> String {
    use carter::provider::RequestPurpose as P;
    match p {
        P::MainTurn => "main_turn",
        P::Compaction => "compaction",
        P::Title => "title",
        P::Subagent => "subagent",
    }
    .to_string()
}

fn summarize(r: &carter_app_core::ExchangeBrief, in_flight: bool) -> ExchangeSummary {
    ExchangeSummary {
        id: r.id.clone(),
        ts: r.ts,
        turn: r.meta.as_ref().map(|m| m.turn),
        round: r.meta.as_ref().map(|m| m.round),
        // 用 snake_case，与 `ExchangeRecord.meta.purpose` 的 serde 表示保持一致 ——
        // 两处拼法不同会让前端不得不写一个归一化器，那是纯负担。
        purpose: r.meta.as_ref().map(|m| purpose_str(m.purpose)),
        method: r.method.clone(),
        url: r.url.clone(),
        channel: r.channel.clone(),
        model: r.model.clone(),
        status: r.status,
        ttfb_ms: r.ttfb_ms,
        total_ms: r.total_ms,
        frame_count: r.frame_count,
        in_flight,
    }
}

#[tauri::command]
fn list_exchanges(shell: State<'_, Arc<AppShell>>, session_id: String) -> Vec<ExchangeSummary> {
    // 已完成的走摘要缓存（不重复解析几 MB 的帧）、进行中的还在内存里，两者都要。
    // 先取进行中的快照：一条交换恰好在两次读取之间收尾时，它可能同时出现两处 ——
    // 按 id 去重。
    let active = shell.capture.active_briefs(&session_id);
    let active_ids: std::collections::HashSet<&str> =
        active.iter().map(|r| r.id.as_str()).collect();
    let mut list: Vec<ExchangeSummary> = shell
        .capture
        .briefs(&session_id)
        .iter()
        .filter(|r| !active_ids.contains(r.id.as_str()))
        .map(|r| summarize(r, false))
        .collect();
    // 进行中的排在最后：它们是「此刻正在发生」的，位置不该被完成顺序挤走。
    list.extend(active.iter().map(|r| summarize(r, true)));
    list
}

/// 取一条交换的**完整**原始报文。
#[tauri::command]
fn get_exchange(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
    exchange_id: String,
) -> Option<carter_app_core::ExchangeRecord> {
    shell.capture.get(&session_id, &exchange_id)
}

/// 把一条交换**解回 IR**（报文面板「可视化」那一页）。
///
/// 解码放在后端而不是前端：那是三个协议各自的 wire 语法，用 `llm_adapt` 现成的
/// codec 解一遍就够，前端再写一份必然与真实链路走样 —— 而「报文面板显示的」与
/// 「模型实际收到/说出的」不是同一份东西，这个面板就没有意义了。
///
/// 解码失败**不是命令失败**：`InspectExchange` 里带着原因，界面据此降级到「原始」页。
#[tauri::command]
fn inspect_exchange(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
    exchange_id: String,
) -> Option<carter_app_core::inspect::InspectedExchange> {
    shell
        .capture
        .get(&session_id, &exchange_id)
        .map(|rec| carter_app_core::inspect::inspect(&rec))
}

// ---------------------------------------------------------------------------
// 共用
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // 选工作目录 / 选文件用。原生选择器比自己拿 list_files 拼一个强得多。
        .plugin(tauri_plugin_dialog::init())
        // 助理只在工单要人处理时出声，走的就是系统通知。
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            init_logging();
            // 配置在启动时读一次。之后每次界面写回都会热重载（`apply_config`）。
            let config = carter::config::Config::load()?;
            let shell = AppShell::new(config);
            app.manage(shell.clone());

            // GUI 进程拿不到 shell 启动文件里写的 PATH（见 `platform::ensure_user_paths` 的
            // 说明）。官方 CLI 的安装脚本把二进制放进 `~/.local/bin`，不补这一步的话
            // 「终端里能用、应用里说没装」。
            carter_app_core::platform::ensure_user_paths();

            // 定时任务的后台调度。**和窗口无关**：窗口最小化、切走都不影响它，
            // 到点照常开一条新会话。
            //
            // 用 `tauri::async_runtime::spawn` 而不是核心里的 `spawn_scheduler`：
            // `setup` 跑在主线程上，那里没有 tokio reactor，`tokio::spawn` 会直接
            // panic（启动即崩）。Tauri 自己带一个 runtime，这就是把任务送进去的正路。
            tauri::async_runtime::spawn(carter_app_core::schedules::run_scheduler(shell.clone()));

            // 常驻助理：轮询观察源、评估、开工。同样和窗口无关。
            tauri::async_runtime::spawn(assistant::run(shell.clone()));
            if let Err(e) = setup_tray(app) {
                eprintln!("托盘建不起来：{e}");
            }

            // 事件总线 → webview。一条长驻任务，AppShell 的广播接收端跟着它。
            let handle = app.handle().clone();
            let mut rx = shell.subscribe();
            let bus_shell = shell.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            on_assistant_event(&handle, &bus_shell, &ev);
                            let _ = handle.emit(CARTER_EVENT, &ev);
                        }
                        // 慢订阅者被追上时广播会报 Lagged —— 丢的是历史事件，
                        // 后续照常，不该退出循环。
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            let _ = handle.emit(
                                CARTER_EVENT,
                                &AppEvent::Error {
                                    message: format!("事件积压，丢弃了 {n} 条"),
                                },
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            // 启动时把托盘标题对齐一次（上次退出时可能还有等人处理的工单）。
            shell.emit(AppEvent::AssistantChanged);
            Ok(())
        })
        // 关窗 = 收到托盘里，不退出：助理要接着盯观察源。真要退出走托盘菜单的「退出」。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            // 应用信息与设置
            app_info,
            open_devtools,
            set_app_settings,
            // 会话
            create_session,
            detach_session,
            open_assistant_chat,
            create_side_session,
            send_command,
            accept_memory,
            close_session,
            list_sessions,
            rename_session,
            fork_session,
            delete_session,
            delete_project,
            search_sessions,
            export_session,
            // 渠道 / 模型 / 协议
            list_channels,
            get_provider_detail,
            check_server_tools,
            save_provider,
            delete_provider,
            save_model,
            delete_model,
            list_models,
            set_default_model,
            set_fast_model,
            get_compaction,
            set_compaction,
            // 团队目录
            team_status,
            team_initialize,
            team_overview,
            team_board_html,
            team_my_tasks,
            team_commit,
            team_sync,
            team_set_remote,
            team_people,
            team_join,
            team_rename_me,
            team_act,
            team_inbox,
            // 定时任务
            list_schedules,
            save_schedule,
            delete_schedule,
            toggle_schedule,
            run_schedule,
            // 常驻助理
            get_assistant,
            save_assistant_settings,
            save_assistant_memory,
            get_assistant_episode,
            save_assistant_source,
            delete_assistant_source,
            poll_assistant_source,
            test_assistant_command,
            assistant_ticket_action,
            approve_assistant_decision,
            dismiss_assistant_decision,
            reassess_assistant_decision,
            refresh_model_catalog,
            list_catalog_models,
            recommended_catalog_models,
            search_catalog_models,
            lookup_metas,
            list_provider_templates,
            detect_catalog_provider,
            fetch_upstream_models,
            // 工具
            list_tools,
            // 技能
            list_skills,
            read_skill,
            save_skill,
            toggle_skill,
            delete_skill,
            set_skills_enabled,
            add_skill_root,
            remove_skill_root,
            // IM 连接器
            list_connectors,
            install_connector_cli,
            install_connector_skill,
            login_connector,
            cancel_connector_login,
            logout_connector,
            configure_connector_app,
            add_mail_account,
            remove_mail_account,
            // MCP
            list_mcp_servers,
            save_mcp_server,
            delete_mcp_server,
            // 输入框
            list_commands,
            list_files,
            save_image,
            read_image,
            preview_file,
            file_diff,
            change_rules,
            // 报文检视
            list_exchanges,
            get_exchange,
            inspect_exchange,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            // macOS 上点 Dock 图标：窗口藏着的话叫回来。
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_main(app);
            }
            let _ = (app, event);
        });
}

// ---------------------------------------------------------------------------
// 团队目录（目标 / 任务 / 看板）
// ---------------------------------------------------------------------------

/// 这个目录（或它上面某一层）已经初始化过团队目录了吗。
#[tauri::command]
fn team_status(cwd: String) -> Option<TeamStatus> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))?;
    let meta = store.meta().ok()?;
    let goals = store.goals();
    Some(TeamStatus {
        root: store.root().to_string_lossy().to_string(),
        project_root: store.project_root().to_string_lossy().to_string(),
        name: meta.name,
        remote: meta.remote,
        initialized: true,
        goals: goals.items.len(),
        issues: goals.issues.len(),
    })
}

#[derive(Serialize)]
struct TeamStatus {
    root: String,
    project_root: String,
    name: String,
    remote: Option<String>,
    initialized: bool,
    goals: usize,
    issues: usize,
}

/// 初始化团队目录。建骨架 + 首次提交 + 自动建同步观察源。
///
/// 观察源建在这里而不是 `team::init` 里：那个模块刻意不依赖 `AppShell`（它要能在
/// 没有 webview 的情况下测），而建源需要整个外壳。
#[tauri::command]
fn team_initialize(
    shell: State<'_, Arc<AppShell>>,
    project_root: String,
    name: Option<String>,
) -> Result<carter_app_core::InitOutcome, String> {
    let mut outcome = carter_app_core::team::initialize(Path::new(&project_root), name.as_deref())?;
    match carter_app_core::team::init::create_sync_source(
        shell.inner(),
        Path::new(&project_root),
        &outcome.project_root
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("项目"),
    ) {
        Ok(created) => {
            outcome.sync_source = created;
            if !created {
                outcome
                    .warnings
                    .push("没有远端，暂不自动同步。配好 origin 之后再点一次初始化即可补上。".into());
            }
        }
        Err(e) => outcome.warnings.push(format!("同步观察源没建成：{e}")),
    }
    Ok(outcome)
}

/// 目标全景：目标 + 任务树 + 进度 + 看板。界面一次拉完。
#[tauri::command]
fn team_overview(cwd: String) -> Result<TeamOverview, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;

    let mut warnings = Vec::new();
    let goals_loaded = store.goals();
    for (path, why) in &goals_loaded.issues {
        warnings.push(format!("{path}：{why}"));
    }

    let mut goals = Vec::new();
    for goal in goals_loaded.items {
        let view = store.goal_view(&goal.id);
        let tasks = match &view {
            Ok(v) => v
                .tasks
                .iter()
                .map(|t| TaskRow {
                    id: t.task.id.clone(),
                    title: t.task.title.clone(),
                    status: t.task.status.as_str().to_string(),
                    status_label: t.task.status.label().to_string(),
                    priority: t.task.priority.as_str().to_string(),
                    assignee: t.task.assignee.clone(),
                    parent_id: t.task.parent_id.clone(),
                    depends_on: t.task.depends_on.clone(),
                    children: t.children.clone(),
                    waiting_on: t.waiting_on.clone(),
                    progress: t.progress,
                    ready: t.ready,
                    blocked_reason: t.task.blocked_reason.clone(),
                    estimate_hours: t.task.estimate_hours,
                    due_at: t.task.due_at,
                })
                .collect(),
            Err(e) => {
                warnings.push(format!("{}：{e}", goal.id));
                Vec::new()
            }
        };
        goals.push(GoalRow {
            id: goal.id.clone(),
            title: goal.title.clone(),
            description: goal.description.clone(),
            status: goal.status.as_str().to_string(),
            status_label: goal.status.label().to_string(),
            priority: goal.priority.as_str().to_string(),
            owner: goal.owner.clone(),
            due_at: goal.due_at,
            percent: view.as_ref().map(|v| v.counts.percent()).unwrap_or(0),
            counts: view.as_ref().map(|v| v.counts).unwrap_or_default(),
            roots: view.as_ref().map(|v| v.roots.clone()).unwrap_or_default(),
            tasks,
        });
    }

    let boards_loaded = store.boards();
    for (path, why) in &boards_loaded.issues {
        warnings.push(format!("{path}：{why}"));
    }
    let boards = boards_loaded
        .items
        .into_iter()
        .map(|b| BoardRow {
            id: b.id,
            title: b.title,
            goal_id: b.goal_id,
        })
        .collect();

    let meta = store.meta().ok();
    let identity = carter_app_core::team::git::local_identity(store.project_root()).ok();

    Ok(TeamOverview {
        root: store.root().to_string_lossy().to_string(),
        name: meta.as_ref().map(|m| m.name.clone()).unwrap_or_default(),
        // 实时问 git，不读 `team.json` 里那份初始化时的快照 ——
        // 用户之后 `git remote add` 不会回填那个字段。
        remote: carter_app_core::team::git::remote_url(store.project_root())
            .or_else(|| meta.and_then(|m| m.remote)),
        identity: identity.map(|i| i.as_key()),
        goals,
        boards,
        events: carter_app_core::team::events::read(store.project_root(), 60),
        warnings,
    })
}

#[derive(Serialize)]
struct TeamOverview {
    root: String,
    name: String,
    remote: Option<String>,
    identity: Option<String>,
    goals: Vec<GoalRow>,
    boards: Vec<BoardRow>,
    events: Vec<carter_app_core::team::events::Event>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct GoalRow {
    id: String,
    title: String,
    description: String,
    status: String,
    status_label: String,
    priority: String,
    owner: Option<String>,
    due_at: Option<u64>,
    percent: u32,
    counts: carter_app_core::team::Counts,
    roots: Vec<String>,
    tasks: Vec<TaskRow>,
}

#[derive(Serialize)]
struct TaskRow {
    id: String,
    title: String,
    status: String,
    status_label: String,
    priority: String,
    assignee: Option<String>,
    parent_id: Option<String>,
    depends_on: Vec<String>,
    children: Vec<String>,
    waiting_on: Vec<String>,
    progress: u32,
    ready: bool,
    blocked_reason: String,
    estimate_hours: Option<f64>,
    due_at: Option<u64>,
}

/// 全景里只带看板的元信息：正文是一整页 HTML，切到看板页、选中那一块时再取
/// （见 [`team_board_html`]），不必每次刷新全景都把所有页面搬一遍。
#[derive(Serialize)]
struct BoardRow {
    id: String,
    title: String,
    goal_id: Option<String>,
}

/// 一块看板的整页 HTML。
#[tauri::command]
fn team_board_html(cwd: String, board_id: String) -> Result<String, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;
    store.board(&board_id).map(|b| b.html)
}

/// 我名下的未完成任务。消息框下方的待办条用它。
///
/// 单独一条命令而不是让前端从 `team_overview` 里筛：待办条要跟着**每轮结束**刷新
/// （模型可能刚改过状态），而全景包含所有目标的任务，两边的刷新频率不一样。
#[tauri::command]
fn team_my_tasks(cwd: String) -> Vec<TeamTodo> {
    let Some(store) = carter_app_core::team::Store::discover(Path::new(&cwd)) else {
        return Vec::new();
    };
    let Ok(me) = carter_app_core::team::git::local_identity(store.project_root()) else {
        return Vec::new();
    };
    // 认人走名单：任务里存的可能是成员 id（v1）也可能是 `名字 <邮箱>`（v0），
    // 一个人还可能挂着好几台机器的邮箱。
    let roster = carter_app_core::team::members::load_all(&store);
    let key = roster
        .me(&me)
        .map(|m| m.id.clone())
        .unwrap_or_else(|| me.as_key());
    let mut out = Vec::new();
    for goal in store.goals().items {
        if !goal.status.is_live() {
            continue;
        }
        let Ok(view) = store.goal_view(&goal.id) else {
            continue;
        };
        for t in view.tasks {
            if !t.task.assignee.as_deref().is_some_and(|a| roster.same(a, &key))
                || !t.task.status.is_open()
            {
                continue;
            }
            out.push(TeamTodo {
                id: t.task.id.clone(),
                title: t.task.title.clone(),
                status: t.task.status.as_str().to_string(),
                status_label: t.task.status.label().to_string(),
                priority: t.task.priority.as_str().to_string(),
                goal_id: goal.id.clone(),
                goal_title: goal.title.clone(),
                blocked_reason: t.task.blocked_reason.clone(),
                waiting_on: t.waiting_on.len() as u32,
                ready: t.ready,
            });
        }
    }
    // 能开工的排前面，然后按优先级。
    out.sort_by(|a, b| {
        b.ready
            .cmp(&a.ready)
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.title.cmp(&b.title))
    });
    out
}

#[derive(Serialize)]
struct TeamTodo {
    id: String,
    title: String,
    status: String,
    status_label: String,
    priority: String,
    goal_id: String,
    goal_title: String,
    blocked_reason: String,
    waiting_on: u32,
    ready: bool,
}

/// 把团队数据提交成一条 git 提交。
///
/// 单独一条命令而不是每次改都提交：一轮对话里模型可能建三个任务、改两个状态，
/// 那是**一件事**，应该是一条提交。界面在轮末调用它。
#[tauri::command]
fn team_commit(cwd: String, message: String) -> Result<Option<String>, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;
    let root = store.project_root();
    let rel = store
        .root()
        .strip_prefix(root)
        .map_err(|_| "团队目录不在仓库里".to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    carter_app_core::team::git::stage_team(root, &rel)?;
    carter_app_core::team::git::commit_team(root, &rel, message.trim())
}

/// 立刻同步一次（拉远端、快进）。观察源也会定时跑，这里是手动入口。
#[tauri::command]
fn team_sync(cwd: String) -> Result<String, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;
    let root = store.project_root();
    // 「没有远端」不是错误，是一个还没配好的状态。用红色报错去说这件事，
    // 会让人觉得同步坏了；实际是团队仓库本来就只在本地。
    if !carter_app_core::team::git::has_remote(root) {
        return Ok("还没有配远端，团队仓库目前只在本地。点右上角「配远端」填一个仓库地址即可。".into());
    }
    carter_app_core::team::git::sync(root)
}

// ---------------------------------------------------------------------------
// 团队成员与消息（协议见 docs/team-protocol.md）
// ---------------------------------------------------------------------------

/// 团队目录相对仓库根的路径（`/` 分隔）。两边先各自解析符号链接再比：macOS 上
/// `/var` 与 `/private/var` 直接比会对不上。
fn team_rel_of(store: &carter_app_core::team::Store) -> String {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(store.root())
        .strip_prefix(canon(store.project_root()))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".carter".into())
}

fn team_store(cwd: &str) -> Result<carter_app_core::team::Store, String> {
    carter_app_core::team::Store::discover(Path::new(cwd)).ok_or("这个项目还没有初始化团队目录".into())
}

/// 团队里有谁、本机是谁、还有谁提交过代码却没加入。
///
/// 「本机是谁」为空时界面弹「加入团队」—— 第一次在这个团队里需要身份时才问，
/// 不在初始化时问（初始化的人未必是要在这里干活的人）。
#[tauri::command]
fn team_people(cwd: String) -> Result<TeamPeople, String> {
    use carter_app_core::team::{git, members};
    let store = team_store(&cwd)?;
    let roster = members::load_all(&store);
    let identity = git::local_identity(store.project_root()).ok();
    let me = identity
        .as_ref()
        .and_then(|i| roster.me(i))
        .map(|m| m.id.clone());
    let pending = members::git_candidates(store.project_root(), &roster)
        .into_iter()
        .take(20)
        .map(|i| i.as_key())
        .collect();
    Ok(TeamPeople {
        members: roster.members,
        me,
        identity: identity.map(|i| i.as_key()),
        pending,
    })
}

#[derive(Serialize)]
struct TeamPeople {
    members: Vec<carter_app_core::team::members::Member>,
    /// 本机的成员 id。`None` = 还没加入。
    me: Option<String>,
    /// 本机的 git 身份（`名字 <邮箱>`）。没配 git 时为空 —— 那种情况下没法加入。
    identity: Option<String>,
    /// 提交过代码、还没加入的人。指派时可以先写他们的 `名字 <邮箱>`。
    pending: Vec<String>,
}

/// 加入团队。`member_id` 给了 = 「我是这个人，这是我的另一台机器」；否则用 `name` 新建。
#[tauri::command]
fn team_join(
    cwd: String,
    name: Option<String>,
    member_id: Option<String>,
) -> Result<carter_app_core::team::members::Member, String> {
    use carter_app_core::team::members::{Join, join_and_announce};
    let store = team_store(&cwd)?;
    let how = match member_id.filter(|s| !s.trim().is_empty()) {
        Some(id) => Join::Existing { member_id: id },
        None => Join::New {
            name: name.unwrap_or_default(),
        },
    };
    join_and_announce(&store, how, None).map(|(m, _)| m)
}

/// 改自己在这个团队里的称呼。只改本地文件，随下一次提交走。
#[tauri::command]
fn team_rename_me(cwd: String, name: String) -> Result<carter_app_core::team::members::Member, String> {
    use carter_app_core::team::{git, members};
    let store = team_store(&cwd)?;
    let me = git::local_identity(store.project_root())?;
    let roster = members::load(store.root());
    let id = roster
        .me(&me)
        .map(|m| m.id.clone())
        .ok_or("你还没加入这个团队")?;
    let m = members::rename(store.root(), &id, &name)?;
    let root = store.project_root();
    let rel = format!("{}/{}", team_rel_of(&store), members::MEMBERS_DIR);
    let _ = git::stage_team(root, &rel);
    let _ = git::commit_team(root, &rel, &format!("chore(carter): 改称呼为 {}", m.name));
    Ok(m)
}

/// 人在界面上操作团队数据：指派、认领、评论、@、广播……
///
/// 参数与 `team` 工具完全一样，走的也是同一份实现（`TeamTool::for_person`），只是
/// 事件不带 `via=carter`。两条入口共用一份逻辑，才不会出现「界面上指派会通知对方、
/// 让卡特指派却不通知」这种不一致。
///
/// 改了任务文件的动作（指派、认领）在这里**立刻提交**：界面上没有「轮末」，不提交
/// 的话这次改动要等下一轮对话才进 git，别人同步不到。
#[tauri::command]
fn team_act(cwd: String, args: serde_json::Value) -> Result<String, String> {
    use carter_app_core::team::{TeamTool, git};
    let store = team_store(&cwd)?;
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let out = TeamTool::for_person(PathBuf::from(&cwd)).act(&args)?;
    if matches!(
        action.as_str(),
        "assign" | "task_claim" | "task_release" | "task_status" | "task_update"
    ) {
        let root = store.project_root();
        let rel = team_rel_of(&store);
        let _ = git::stage_team(root, &rel);
        let _ = git::commit_team(root, &rel, &format!("chore(carter): {action}"));
    }
    Ok(out)
}

/// 跟本机有关的消息。`fetch = true` 时先拉一次远端事件。
#[tauri::command]
async fn team_inbox(cwd: String, fetch: bool) -> Result<Vec<carter_app_core::team::inbox::InboxItem>, String> {
    // git 调用是阻塞的；拉远端可能要好几秒，别占着 Tauri 的命令线程。
    tauri::async_runtime::spawn_blocking(move || {
        use carter_app_core::team::{events, git, inbox};
        let store = team_store(&cwd)?;
        if fetch {
            let _ = events::fetch(store.project_root());
        }
        let me = git::local_identity(store.project_root())?;
        Ok(inbox::read(&store, &me, 100))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 配远端。界面上那一个输入框走这里 —— 不该让用户为了同步去开终端。
#[tauri::command]
fn team_set_remote(cwd: String, url: String) -> Result<String, String> {
    let store = carter_app_core::team::Store::discover(Path::new(&cwd))
        .ok_or("这个项目还没有初始化团队目录")?;
    let msg = carter_app_core::team::git::set_remote(store.project_root(), &url)?;
    // 配完顺手推一次：不推的话 `fetch` / `merge` 没东西可比，同步仍然是空转，
    // 用户会以为「配了没用」。
    let pushed = carter_app_core::team::git::publish(store.project_root());
    Ok(match pushed {
        Ok(p) => format!("{msg}；{p}"),
        Err(e) => format!("{msg}；但首次推送没成功：{e}"),
    })
}
