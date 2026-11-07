use crate::prelude::*;

// ---------------------------------------------------------------------------
// 常驻助理
// ---------------------------------------------------------------------------

use carter_app_core::assistant::runtime as assistant;

#[tauri::command]
pub(crate) fn get_assistant(shell: State<'_, Arc<AppShell>>) -> assistant::AssistantView {
    assistant::view(&shell)
}

#[tauri::command]
pub(crate) fn save_assistant_memory(
    shell: State<'_, Arc<AppShell>>,
    id: Option<String>,
    text: String,
    audience: Option<String>,
) -> Result<(), String> {
    assistant::save_memory(shell.inner(), id, text, audience)
}

/// 往期存档里第 `number` 期的原始对话（面板上展开那一期时看）。
#[tauri::command]
pub(crate) fn get_assistant_episode(number: u32) -> Vec<carter_app_core::view::MessageView> {
    use carter_app_core::assistant::memory;
    carter_app_core::view::to_views(&memory::load_archive(&memory::archive_dir(), number))
}

#[tauri::command]
pub(crate) fn save_assistant_settings(
    shell: State<'_, Arc<AppShell>>,
    settings: carter_app_core::assistant::Settings,
) -> Result<(), String> {
    assistant::save_settings(shell.inner(), settings)
}

#[tauri::command]
pub(crate) fn save_assistant_source(
    shell: State<'_, Arc<AppShell>>,
    source: carter_app_core::assistant::Source,
) -> Result<(), String> {
    assistant::save_source(shell.inner(), source)
}

#[tauri::command]
pub(crate) fn delete_assistant_source(
    shell: State<'_, Arc<AppShell>>,
    id: String,
) -> Result<(), String> {
    assistant::delete_source(shell.inner(), &id)
}

/// 「立即检查」：现在就跑一次观察源。返回这次的新行数（第一次是 0 —— 只建基线）。
#[tauri::command]
pub(crate) async fn poll_assistant_source(
    shell: State<'_, Arc<AppShell>>,
    id: String,
) -> Result<u32, String> {
    assistant::poll_now(shell.inner(), &id).await
}

/// 试跑观察源的命令，不比对、不评估 —— 编辑框里「试一下」，让人看到命令到底吐了什么。
#[tauri::command]
pub(crate) async fn test_assistant_command(command: String, cwd: String) -> Result<String, String> {
    carter_app_core::assistant::source::run_command(&command, std::path::Path::new(&cwd)).await
}

#[tauri::command]
pub(crate) async fn assistant_ticket_action(
    shell: State<'_, Arc<AppShell>>,
    ticket_id: String,
    action: assistant::TicketAction,
) -> Result<(), String> {
    assistant::ticket_action(shell.inner(), &ticket_id, action).await
}

#[tauri::command]
pub(crate) async fn approve_assistant_decision(
    shell: State<'_, Arc<AppShell>>,
    decision_id: String,
) -> Result<String, String> {
    assistant::approve(shell.inner(), &decision_id).await
}

/// **必须是 `async fn`**：助理对话还没开时这里要就地开一条（`start_session`
/// 要求 tokio runtime），同步命令所在的主线程没有，会直接报「开不出助理对话」。
#[tauri::command]
pub(crate) async fn new_assistant_episode(shell: State<'_, Arc<AppShell>>) -> Result<(), String> {
    carter_app_core::assistant::chat::new_episode(shell.inner())
}

#[tauri::command]
pub(crate) fn dismiss_assistant_decision(
    shell: State<'_, Arc<AppShell>>,
    decision_id: String,
) -> Result<(), String> {
    assistant::dismiss(shell.inner(), &decision_id)
}

#[tauri::command]
pub(crate) async fn reassess_assistant_decision(
    shell: State<'_, Arc<AppShell>>,
    decision_id: String,
) -> Result<carter_app_core::assistant::Decision, String> {
    assistant::reassess(shell.inner(), &decision_id).await
}

/// 托盘：图标常驻菜单栏，标题显示要人处理的件数。
///
/// 窗口关掉之后应用不退出（见 `run` 里的 `CloseRequested`）—— 助理要接着盯观察源，
/// 托盘是它还活着的唯一证据，也是把窗口叫回来的入口。
pub(crate) const TRAY_ID: &str = "carter-tray";

pub(crate) fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
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

pub(crate) fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 助理的事件 → 系统通知与托盘标题。
pub(crate) fn on_assistant_event(app: &tauri::AppHandle, shell: &AppShell, ev: &AppEvent) {
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
pub(crate) fn list_catalog_models(
    provider_id: String,
) -> Result<Vec<carter_app_core::CatalogEntry>, String> {
    catalog::provider_models(&provider_id)
}

/// 拉一次 models.dev 目录。返回这次是怎么拉到的（哪个源、几条、失败原因）。
#[tauri::command]
pub(crate) async fn refresh_model_catalog() -> Result<catalog::RefreshOutcome, String> {
    catalog::refresh().await
}

/// 某个目录服务商下值得预填的几个模型（同 family 只取最新、跳过弃用与非对话模型）。
#[tauri::command]
pub(crate) fn recommended_catalog_models(
    provider_id: String,
) -> Result<Vec<carter_app_core::CatalogEntry>, String> {
    catalog::recommended_models(&provider_id, 8)
}

/// 跨服务商搜目录条目（手动绑元数据时的搜索框）。
#[tauri::command]
pub(crate) fn search_catalog_models(
    query: String,
) -> Result<Vec<carter_app_core::CatalogHit>, String> {
    catalog::search_models(&query, 40)
}

/// 一批 meta 在目录里查得到什么。查不到的位置是 `null`（不是错误）—— 编辑框据此
/// 把那一行标成「未绑定 / meta 写错了」，而不是等保存时才炸。
#[tauri::command]
pub(crate) fn lookup_metas(metas: Vec<String>) -> Vec<Option<carter_app_core::catalog::MetaInfo>> {
    catalog::meta_infos(metas.iter().map(String::as_str))
}

/// 渠道模板列表。界面「新建渠道」时的那一排按钮。
#[tauri::command]
pub(crate) fn list_provider_templates() -> &'static [carter_app_core::templates::ProviderTemplate] {
    carter_app_core::templates::TEMPLATES
}

/// 这个 base_url 对应 models.dev 里的哪一家？认不出来是 `None`。
///
/// 界面用它决定「模型能不能直接从目录预填」，以及拉上游列表时把哪家当提示。
/// 与真正发请求无关 —— 请求永远走配置里的 base_url。
#[tauri::command]
pub(crate) fn detect_catalog_provider(base_url: String) -> Option<String> {
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
pub(crate) async fn fetch_upstream_models(
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
