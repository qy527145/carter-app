use crate::prelude::*;

// ---------------------------------------------------------------------------
// 应用信息
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct AppInfo {
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
    /// models.dev 缓存是哪次、从哪拉的；从没记录过时是 `None`。
    models_meta: Option<catalog::CacheMeta>,
    /// 编进安装包的目录快照是哪天的。
    models_snapshot_date: &'static str,
}

#[tauri::command]
pub(crate) fn app_info(shell: State<'_, Arc<AppShell>>) -> AppInfo {
    let config = shell.config.load();
    let meta = catalog::cache_meta();
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
        models_meta: meta,
        models_snapshot_date: catalog::builtin_snapshot_date(),
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
pub(crate) fn open_devtools(webview: tauri::WebviewWindow) -> Result<(), String> {
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
pub(crate) fn set_app_settings(settings: AppSettings) -> Result<(), String> {
    settings.save()
}
