//! Tauri 外壳：命令层 + 事件桥。
//!
//! 这一层刻意保持很薄 —— 每个命令做三件事：取参数、转给 `carter-app-core`、
//! 把错误转成字符串。真正的状态与编排在 `carter-app-core` 里，那里不依赖 Tauri，
//! 可以脱离 webview 测。

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, Manager, State};

use carter_app_core::{AppEvent, AppShell, Command, MessageView, SessionOptions};

/// 前端订阅的事件名。
///
/// 字符串字面量散落两边，改名会**静默**破坏订阅（前端不再收到任何事件，也不报错）。
/// 改这里就必须同步改 `src/lib/api.ts` 里的 `EVENT_NAME`。
const CARTER_EVENT: &str = "carter://event";

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    /// 默认工作目录（会话按目录分文件存）。
    default_cwd: String,
    /// 当前配置里的默认模型引用。
    default_model: String,
}

#[derive(Serialize)]
struct CreatedSession {
    session_id: String,
    model: String,
    cwd: String,
    history: Vec<MessageView>,
}

#[derive(Serialize)]
struct SessionSummary {
    id: String,
    title: Option<String>,
    model: String,
    cwd: String,
    created_at: u64,
}

/// 桌面应用没有「当前工作目录」这个概念 —— 进程的 cwd 取决于从哪启动的，
/// 用户不会预期会话记在 `/` 或 `C:\Windows\System32` 下。默认落在家目录。
fn default_cwd() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[tauri::command]
fn app_info(shell: State<'_, Arc<AppShell>>) -> AppInfo {
    let config = shell.config.load();
    AppInfo {
        name: "carter-app",
        version: env!("CARGO_PKG_VERSION"),
        default_cwd: default_cwd().to_string_lossy().to_string(),
        default_model: config.agent.model.clone(),
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

#[tauri::command]
fn list_sessions(cwd: Option<String>) -> Vec<SessionSummary> {
    let cwd = cwd.map(PathBuf::from).unwrap_or_else(default_cwd);
    carter::session::list(&cwd, false)
        .into_iter()
        .map(|e| SessionSummary {
            id: e.meta.id.clone(),
            title: e.meta.title.clone(),
            model: e.meta.model.clone(),
            cwd: e.meta.cwd.clone(),
            created_at: e.meta.created_at,
        })
        .collect()
}

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
fn list_exchanges(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
) -> Vec<ExchangeSummary> {
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
// 管理/检视用的只读查询
//
// 刻意做成只读：改配置要落盘 `config.toml` 再热重载，那是另一件事
// （见方案 M7 的记录）。把「看得到现状」先做出来，价值已经占了大半 ——
// 现在用户想看自己配了哪些渠道、有哪些技能，只能去翻文件。
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SkillRow {
    name: String,
    description: String,
    allowed_tools: Vec<String>,
}

#[tauri::command]
fn list_skills(shell: State<'_, Arc<AppShell>>) -> Vec<SkillRow> {
    if !shell.config.load().skills.enabled {
        return Vec::new();
    }
    carter::skills::discover(&carter::config::paths::skills_dir())
        .into_iter()
        .map(|m| SkillRow {
            name: m.name,
            description: m.description,
            allowed_tools: m.allowed_tools,
        })
        .collect()
}

#[derive(Serialize)]
struct McpServerRow {
    name: String,
    transport: String,
    /// stdio 给 command，http 给 url。
    target: String,
    enabled: bool,
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
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

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
            let mut models: Vec<String> = p.models.keys().cloned().collect();
            models.sort();
            ChannelRow {
                name: name.clone(),
                kind: p.kind.clone(),
                protocol: p.protocol().to_string(),
                base_url: p.base_url.clone().unwrap_or_default(),
                has_api_key: p.api_key.is_some(),
                unsupported_server_tools: unsupported,
                models,
                // 默认渠道 = 配置里 `agent.model` 指向的那个 provider。
                is_default: config
                    .agent
                    .model
                    .starts_with(&format!("{name}/")),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

#[derive(Serialize)]
struct ToolRow {
    name: String,
    description: String,
    /// 参数 JSON Schema，供界面展开查看。
    parameters: serde_json::Value,
}

/// 内置工具清单。这是**本地会执行**的那些；厂商在服务端执行的工具
/// （web_search 等）不在这里，它们是渠道能力，见 `list_channels`。
#[tauri::command]
fn list_tools() -> Vec<ToolRow> {
    carter::tools::ToolRegistry::builtin()
        .specs()
        .into_iter()
        .map(|s| ToolRow {
            name: s.name.clone(),
            description: s.description.clone().unwrap_or_default(),
            parameters: s.input_schema,
        })
        .collect()
}

/// 删除一个会话文件。不可恢复 —— 调用方要先确认。
#[tauri::command]
fn delete_session(
    shell: State<'_, Arc<AppShell>>,
    cwd: Option<String>,
    session_id: String,
) -> Result<(), String> {
    // 先关会话再删文件。会话任务持有同一份会话文件的 Recorder，不先关掉的话
    // 它下一轮 append 会把文件重新写出来 —— 表现为「删了又回来了」。
    shell.close_session(&session_id);
    let cwd = cwd.map(PathBuf::from).unwrap_or_else(default_cwd);
    let entry = carter::session::list(&cwd, false)
        .into_iter()
        .find(|e| e.meta.id == session_id)
        .ok_or_else(|| format!("找不到会话 {session_id}"))?;
    std::fs::remove_file(&entry.path).map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 配置在启动时读一次。改配置需要重启 —— 热重载留到管理页那时再做，
            // 现在多一条热重载链路就多一处「配置改了但没生效」的困惑。
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
            app_info,
            create_session,
            send_command,
            close_session,
            list_sessions,
            list_exchanges,
            get_exchange,
            list_skills,
            list_mcp_servers,
            list_channels,
            list_tools,
            delete_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
