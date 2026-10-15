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
use carter_app_core::skills::{self, SkillDraft};
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

#[tauri::command]
fn close_session(shell: State<'_, Arc<AppShell>>, session_id: String) -> Result<(), String> {
    shell.close_session(&session_id);
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
fn delete_session(shell: State<'_, Arc<AppShell>>, session_id: String) -> Result<(), String> {
    // 先关会话再删文件。会话任务持有同一份会话文件的 Recorder，不先关掉的话
    // 它下一轮 append 会把文件重新写出来 —— 表现为「删了又回来了」。
    shell.close_session(&session_id);
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

/// 拉一次 models.dev 目录。返回拉到的模型条数。
#[tauri::command]
async fn refresh_model_catalog() -> Result<usize, String> {
    catalog::refresh().await
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
    // ask_user_question 在真的会话里是带发送端注册的；这里只是要看它的说明与 schema，
    // 给一个没人读的通道即可。
    registry.push(Arc::new(carter::agent::AskUserQuestionTool::new(
        tokio::sync::mpsc::unbounded_channel().0,
    )));

    let mut rows: Vec<ToolRow> = registry
        .specs()
        .into_iter()
        .map(|s| {
            let enabled = settings.tool_enabled(&s.name);
            ToolRow {
                enabled,
                toggleable: true,
                note: None,
                name: s.name,
                description: s.description.unwrap_or_default(),
                parameters: s.input_schema,
            }
        })
        .collect();

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
}

#[tauri::command]
fn list_skills(shell: State<'_, Arc<AppShell>>) -> SkillListing {
    SkillListing {
        skills: skills::list(),
        globally_enabled: shell.config.load().skills.enabled,
    }
}

#[tauri::command]
fn read_skill(name: String) -> Result<String, String> {
    skills::read(&name)
}

#[tauri::command]
fn save_skill(shell: State<'_, Arc<AppShell>>, draft: SkillDraft) -> Result<(), String> {
    skills::save(&draft)?;
    // 技能目录会进系统提示，所以要让跑着的会话下一轮重算 —— 发个事件叫前端刷新，
    // 会话侧本来每轮都会重读技能目录。
    shell.emit(AppEvent::ConfigChanged);
    Ok(())
}

#[tauri::command]
fn toggle_skill(shell: State<'_, Arc<AppShell>>, name: String, enabled: bool) -> Result<(), String> {
    skills::set_enabled(&name, enabled)?;
    shell.emit(AppEvent::ConfigChanged);
    Ok(())
}

#[tauri::command]
fn delete_skill(shell: State<'_, Arc<AppShell>>, name: String) -> Result<(), String> {
    skills::delete(&name)?;
    shell.emit(AppEvent::ConfigChanged);
    Ok(())
}

#[tauri::command]
fn set_skills_enabled(shell: State<'_, Arc<AppShell>>, enabled: bool) -> Result<(), String> {
    config_edit::set_skills_enabled_reload(shell.inner(), enabled)
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
    status: Option<u16>,
    ttfb_ms: Option<u128>,
    total_ms: Option<u128>,
    frame_count: usize,
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

#[tauri::command]
fn list_exchanges(shell: State<'_, Arc<AppShell>>, session_id: String) -> Vec<ExchangeSummary> {
    shell
        .capture
        .load(&session_id)
        .into_iter()
        .map(|r| ExchangeSummary {
            id: r.id,
            ts: r.ts,
            turn: r.meta.as_ref().map(|m| m.turn),
            round: r.meta.as_ref().map(|m| m.round),
            // 用 snake_case，与 `ExchangeRecord.meta.purpose` 的 serde 表示保持一致 ——
            // 两处拼法不同会让前端不得不写一个归一化器，那是纯负担。
            purpose: r.meta.as_ref().map(|m| purpose_str(m.purpose)),
            method: r.method,
            url: r.url,
            status: r.status,
            ttfb_ms: r.ttfb_ms,
            total_ms: r.total_ms,
            frame_count: r.stream_frames.len(),
        })
        .collect()
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

// ---------------------------------------------------------------------------
// 共用
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
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
            set_app_settings,
            // 会话
            create_session,
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
            refresh_model_catalog,
            // 工具
            list_tools,
            // 技能
            list_skills,
            read_skill,
            save_skill,
            toggle_skill,
            delete_skill,
            set_skills_enabled,
            // MCP
            list_mcp_servers,
            save_mcp_server,
            delete_mcp_server,
            // 报文检视
            list_exchanges,
            get_exchange,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
