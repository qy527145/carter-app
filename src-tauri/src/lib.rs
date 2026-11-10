//! Tauri 外壳：命令层 + 事件桥。
//!
//! 这一层刻意保持很薄 —— 每个命令做三件事：取参数、转给 `carter-app-core`、
//! 把错误转成字符串。真正的状态与编排在 `carter-app-core` 里，那里不依赖 Tauri，
//! 可以脱离 webview 测。

use std::path::PathBuf;

use tauri::{Emitter, Manager};

use carter_app_core::app_settings::AppSettings;
use carter_app_core::{AppEvent, AppShell};

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

/// 各命令文件共用的导入。命令散在 `commands/` 下十几个文件里，每个都重抄一遍
/// 这二十行 `use` 只会让「加一个依赖」要改十几处。
pub(crate) mod prelude {
    pub(crate) use std::path::{Path, PathBuf};
    pub(crate) use std::sync::{Arc, Mutex};

    pub(crate) use serde::Serialize;
    pub(crate) use tauri::{Emitter, Manager, State};

    pub(crate) use carter_app_core::app_settings::AppSettings;
    pub(crate) use carter_app_core::catalog;
    pub(crate) use carter_app_core::config_edit::{self, McpDraft, ProviderDraft};
    pub(crate) use carter_app_core::sessions::{self, ExportFormat};
    pub(crate) use carter_app_core::skills::{self, Scan, SkillDraft};
    pub(crate) use carter_app_core::{
        AppEvent, AppShell, Command, MessageView, ProviderDetail, SessionOptions, SessionRow,
    };

    pub(crate) use crate::default_cwd;
}

mod commands;
use commands::*;

// ---------------------------------------------------------------------------
// 共用
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 从当前目录向上找 `.env`：`tauri dev` / `cargo run` 都能找到仓库根那份。
    // 发给用户的安装包不读它，所以里面的每个变量都必须在代码里有默认值。
    #[cfg(debug_assertions)]
    let _ = dotenvy::dotenv();

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    #[cfg(desktop)]
    {
        use tauri_plugin_window_state::StateFlags;
        // 单实例必须最先注册：重复启动时唤起已有窗口（它可能正藏在托盘里），
        // 而不是再起一个进程 —— 两个助理同时盯观察源会重复接单。
        //
        // 只在 release 里开：它按 identifier 判重，开着的话装好的 Carter 一在跑，
        // `bun run dev` 起来的调试版就会去唤起它然后自己退出。
        #[cfg(not(debug_assertions))]
        {
            builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
                show_main(app);
            }));
        }
        builder = builder
            // 记住位置、尺寸、最大化。**不记可见性**：关窗只是藏进托盘，
            // 记住了的话从托盘退出后下次启动窗口就是隐藏的。
            .plugin(
                tauri_plugin_window_state::Builder::new()
                    .with_state_flags(StateFlags::all() - StateFlags::VISIBLE)
                    .build(),
            );
    }

    builder
        .plugin(tauri_plugin_opener::init())
        // 选工作目录 / 选文件用。原生选择器比自己拿 list_files 拼一个强得多。
        .plugin(tauri_plugin_dialog::init())
        // 助理只在工单要人处理时出声，走的就是系统通知。
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            init_logging();
            // 一个渠道都没有（首次安装）时先建一个空的起步渠道，给设置页一个现成的位置；
            // 还缺的模型、密钥由界面提示去设置里配。
            if let Err(e) = carter_app_core::config_edit::ensure_starter() {
                tracing::warn!(error = %e, "建起步渠道失败");
            }
            // 配置在启动时读一次。之后每次界面写回都会热重载（`apply_config`）。
            let config = carter::config::Config::load()?;
            let shell = AppShell::new(config);
            app.manage(shell.clone());
            app.manage(DownloadState::default());

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
            tauri::async_runtime::spawn(carter_app_core::assistant::runtime::run(shell.clone()));

            // 旧版本建出来的团队观察源要在开跑之前换掉：它们的命令是裸
            // `git pull --ff-only`（只拉不推），节奏也还是旧的。
            carter_app_core::team::init::migrate_sources(&shell);
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

            // 启动后顺手查一次新版本。延后一会儿再查：启动那一瞬要建窗口、
            // 起助理、读配置，抢网络没有意义。查不到（断网、加速站都挂）就
            // 静默算了 —— 用户没在做「检查更新」，不该看到一条报错。
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if !AppSettings::load().auto_check_update {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                let settings = AppSettings::load();
                if let Ok(info) =
                    carter_app_core::update::check(env!("CARGO_PKG_VERSION"), &settings).await
                    && !info.asset_name.is_empty()
                {
                    let _ = handle.emit(UPDATE_AVAILABLE, &info);
                }
            });
            Ok(())
        })
        // 关窗 = 收到托盘里，不退出：助理要接着盯观察源。真要退出走托盘菜单的「退出」。
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            // 应用信息与设置
            app_info,
            open_devtools,
            set_app_settings,
            // 自更新
            check_update,
            apply_update,
            cancel_update_download,
            restart_app,
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
            new_assistant_episode,
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
