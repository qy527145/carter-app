//! Tauri 外壳：命令层 + 事件桥。
//!
//! 这一层刻意保持很薄 —— 每个命令做三件事：取参数、转给 `carter-app-core`、
//! 把错误转成字符串。真正的状态与编排在 `carter-app-core` 里，那里不依赖 Tauri，
//! 可以脱离 webview 测。

use std::path::PathBuf;
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
    cwd: String,
    history: Vec<MessageView>,
    /// 这条会话现有的长任务目标（`/goal`）。续接时从盘上读回来。
    goal: Option<carter_app_core::GoalView>,
    /// 历史里每一轮实际改过的文件。事件不回放，所以随历史一起带回。
    changes: Vec<carter_app_core::TurnFiles>,
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
    let opts = SessionOptions {
        cwd: cwd.map(PathBuf::from).unwrap_or_else(default_cwd),
        model,
        resume_latest: resume,
        resume_id,
    };
    let started = carter_app_core::start_session(shell.inner().clone(), opts)
        .map_err(|e| e.to_string())?;
    let handle = &started.handle;
    Ok(CreatedSession {
        session_id: handle.id.clone(),
        model: handle.model.load().api_name.clone(),
        cwd: handle.cwd.to_string_lossy().to_string(),
        history: started.history,
        goal: started.goal,
        changes: started.changes,
    })
}

#[derive(Serialize)]
struct CreatedSide {
    session_id: String,
    model: String,
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

/// 列举会话。`all = false` 只看默认目录，`true` 跨所有项目。
#[tauri::command]
fn list_sessions(all: bool) -> Vec<SessionRow> {
    sessions::list(&default_cwd(), all)
}

/// 改标题。走「追加一条 Title 记录」，所以不需要动会话文件其余部分。
#[tauri::command]
fn rename_session(session_id: String, title: String) -> Result<(), String> {
    sessions::rename(&session_id, &title)?;
    Ok(())
}

/// fork 一条会话，返回新会话的 id。
#[tauri::command]
fn fork_session(session_id: String) -> Result<String, String> {
    sessions::fork(&session_id)
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
    sessions::delete(&session_id)
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
    };
    config_edit::save_provider_reload(shell, &draft)
}

/// 全部模型引用 + 从 models.dev 解出来的元数据。单行解析失败只标在该行上。
#[tauri::command]
fn list_models(shell: State<'_, Arc<AppShell>>) -> Vec<carter_app_core::ModelRow> {
    catalog::list(&shell.config.load())
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
fn list_tools(shell: State<'_, Arc<AppShell>>) -> Vec<ToolRow> {
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
    let cwd = carter::config::paths::carter_home();
    let mut rows: Vec<ToolRow> = Vec::new();
    for t in registry.tools() {
        let t = carter_app_core::tools::substitute(t.clone(), &settings, &cwd);
        let name = t.name().to_string();
        let note = match name.as_str() {
            "bash" => Some(carter_app_core::platform::os_label().to_string()),
            // `note` 就是为「这个工具从哪来」准备的 —— 技能多了附加扫描路径之后，
            // 「它到底扫哪几个目录」是用户会问的第一个问题。
            "skill" => Some(match settings.skill_roots.len() {
                0 => "只扫 ~/.carter/skills".to_string(),
                n => format!("~/.carter/skills 与 {n} 条附加扫描路径"),
            }),
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

    // `task` 构造要一个 provider，这里拿不到 —— 单独补一行，标明它不可关。
    // 子 agent 的能力开关该是独立的一项，而不是混在「工具开关」里顺手关掉。
    rows.push(ToolRow {
        name: "task".into(),
        description:
            "派生隔离子 agent 执行子任务。子 agent 有独立上下文与工具池，\
             并遵守同一份工具开关。"
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
    /// 扫描根一览：本地目录排第一，然后是附加路径（顺序 = 优先级）。
    roots: Vec<skills::SkillRootRow>,
    /// Claude Code / Codex 这类默认位置的快捷选项。
    presets: Vec<skills::SkillPreset>,
    /// 被按名字忽略的外部技能。
    ///
    /// 单独回传是因为：一个被忽略的名字可能**再也没有对应的行**（它被同名技能遮蔽，
    /// 或者它所在的那条扫描路径被移除了）。那时候这是用户撤销它的唯一入口。
    ignored_skills: Vec<String>,
}

#[tauri::command]
fn list_skills(shell: State<'_, Arc<AppShell>>) -> SkillListing {
    let settings = AppSettings::load();
    let scan = Scan::from_settings(&settings);
    SkillListing {
        skills: skills::list(&scan),
        globally_enabled: shell.config.load().skills.enabled,
        roots: skills::roots(&scan),
        presets: skills::presets(&scan),
        ignored_skills: settings.ignored_skills.clone(),
    }
}

#[tauri::command]
fn read_skill(name: String) -> Result<String, String> {
    skills::read(&Scan::from_settings(&AppSettings::load()), &name)
}

#[tauri::command]
fn save_skill(shell: State<'_, Arc<AppShell>>, draft: SkillDraft) -> Result<(), String> {
    skills::save(&Scan::from_settings(&AppSettings::load()), &draft)?;
    // 技能目录会进系统提示，所以要让跑着的会话下一轮重算 —— 发个事件叫前端刷新，
    // 会话侧本来每轮都会重读技能目录。
    shell.emit(AppEvent::ConfigChanged);
    Ok(())
}

/// 启停单个技能。本地的搬目录、外部的改忽略名单（分派在 `skills::set_enabled` 里）。
///
/// 回传新的设置：外部技能的开关写的是 `ignored_skills`，而前端 store 里那份设置是
/// 旧的。不回传的话，下一次改主题（`set_app_settings` 是**整体覆盖写入**）会把刚关掉
/// 的外部技能又打开。
/// `external` 是**点的是哪一行**（界面上那个 `readonly`）：本地技能停用后，外部的同名
/// 技能会顶上来，那时两行同名，能区分它们的只有来源 —— 按名字猜会让「点外部那一行」
/// 变成点了没反应（本地目录此刻正好不在启用侧，搬目录那条路会直接返回 Ok）。
#[tauri::command]
fn toggle_skill(
    shell: State<'_, Arc<AppShell>>,
    name: String,
    enabled: bool,
    external: bool,
) -> Result<AppSettings, String> {
    let mut settings = AppSettings::load();
    let scan = Scan::from_settings(&settings);
    if skills::set_enabled(&mut settings, &scan, &name, enabled, external)? {
        settings.save()?;
    }
    shell.emit(AppEvent::ConfigChanged);
    Ok(settings)
}

#[tauri::command]
fn delete_skill(shell: State<'_, Arc<AppShell>>, name: String) -> Result<(), String> {
    skills::delete(&Scan::from_settings(&AppSettings::load()), &name)?;
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

/// `@` 菜单的文件列表（相对工作目录的路径）。
#[tauri::command]
fn list_files(cwd: Option<String>, query: String, limit: Option<usize>) -> Vec<String> {
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
#[tauri::command]
fn read_image(token: String) -> Option<String> {
    carter_app_core::images::read_image(&token)
}

/// 文件预览：点击「本轮变更的文件」里的一行时打开。
///
/// 读的是**现在磁盘上的内容**，不是历史快照 —— 预览要回答的是「它现在什么样」。
/// 返回的是可渲染的形状（文本 / 图片 data URL / 二进制 / 消失 / 读失败），
/// 前端不需要自己判断文件类型。
#[tauri::command]
fn preview_file(cwd: Option<String>, path: String) -> carter_app_core::FilePreview {
    carter_app_core::preview::preview(
        &cwd.map(PathBuf::from).unwrap_or_else(default_cwd),
        &path,
    )
}

/// 「本轮变更的文件」的固定忽略名单，给设置页展示用。
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

/// 某一轮里某个文件的 diff：点开预览时按需算（见 core 的 `changes.rs`）。
///
/// 只有 cwd 在 git 仓库里、且那一轮记下了基线时才有结果；其余情况返回
/// `unavailable` 与一句人话原因。`spawn_blocking`：要起 git 进程，别占着
/// async 的工作线程。
#[tauri::command]
async fn file_diff(
    session_id: String,
    turn: u32,
    cwd: String,
    path: String,
) -> carter_app_core::FileDiff {
    let fallback = path.clone();
    tokio::task::spawn_blocking(move || {
        carter_app_core::changes::diff_for(
            &session_id,
            turn,
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
        .setup(|app| {
            // 配置在启动时读一次。之后每次界面写回都会热重载（`apply_config`）。
            let config = carter::config::Config::load()?;
            let shell = AppShell::new(config);
            app.manage(shell.clone());

            // 事件总线 → webview。一条长驻任务，AppShell 的广播接收端跟着它。
            let handle = app.handle().clone();
            let mut rx = shell.subscribe();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 应用信息与设置
            app_info,
            open_devtools,
            set_app_settings,
            // 会话
            create_session,
            create_side_session,
            send_command,
            close_session,
            list_sessions,
            rename_session,
            fork_session,
            delete_session,
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
