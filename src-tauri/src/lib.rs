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
#[tauri::command]
fn create_session(
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
            list_sessions
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
