use crate::prelude::*;

// ---------------------------------------------------------------------------
// 自更新
//
// 查、下、装都在 `carter_app_core::update` 里（不依赖 Tauri，能单独测）。
// 这里只管三件事：把下载进度推给界面、给一个取消开关、重启进程。
// ---------------------------------------------------------------------------

/// 前端订阅的下载进度事件名。改这里必须同步改 `src/lib/api.ts` 的 `UPDATE_PROGRESS`。
pub(crate) const UPDATE_PROGRESS: &str = "carter://update-progress";

/// 「发现新版本」事件。启动后自动检查查到新版时发一次，前端据此弹一条提示。
pub(crate) const UPDATE_AVAILABLE: &str = "carter://update-available";

/// 正在进行的下载的取消开关。同一时间只允许一次下载 —— 界面上的按钮也只有
/// 一个状态，多开只会让两个任务抢同一个 `.part` 文件。
#[derive(Default)]
pub(crate) struct DownloadState {
    cancel: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
}

/// 查最新版本。**没有新版时** `has_update` 为 false，其余字段是占位。
#[derive(Serialize)]
pub(crate) struct UpdateCheck {
    has_update: bool,
    current: &'static str,
    release: Option<carter_app_core::update::ReleaseInfo>,
}

#[tauri::command]
pub(crate) async fn check_update() -> Result<UpdateCheck, String> {
    let current = env!("CARGO_PKG_VERSION");
    let settings = AppSettings::load();
    let release = carter_app_core::update::check(current, &settings).await?;
    let has_update = !release.asset_name.is_empty();
    Ok(UpdateCheck {
        has_update,
        current,
        release: has_update.then_some(release),
    })
}

/// 取消正在进行的下载。没有下载在跑时什么也不做。
#[tauri::command]
pub(crate) fn cancel_update_download(state: State<'_, DownloadState>) {
    if let Ok(slot) = state.cancel.lock()
        && let Some(flag) = slot.as_ref()
    {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 一键更新：查 → 下 → 装 → 自动重启。
///
/// 界面只调这一个命令，进度走 [`UPDATE_PROGRESS`]。**装完这一条就退出进程** ——
/// macOS / Windows 要替换的是正在运行的程序文件，不退出就换不干净；换完之后
/// 由那段脱离的脚本把新版拉起来（见 `core::update::install` 的说明）。
#[tauri::command]
pub(crate) async fn apply_update(
    app: tauri::AppHandle,
    state: State<'_, DownloadState>,
) -> Result<(), String> {
    let settings = AppSettings::load();
    let release = carter_app_core::update::check(env!("CARGO_PKG_VERSION"), &settings).await?;
    if release.asset_name.is_empty() {
        return Err("已经是最新版本".into());
    }
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut slot = state.cancel.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return Err("上一次更新还没结束".into());
        }
        *slot = Some(cancel.clone());
    }

    let emitter = app.clone();
    let progress = move |done: u64, total: Option<u64>| {
        let _ = emitter.emit(
            UPDATE_PROGRESS,
            serde_json::json!({ "phase": "downloading", "downloaded": done, "total": total }),
        );
    };
    let downloaded =
        carter_app_core::update::download(&release, &settings, Some(&cancel), Some(&progress))
            .await;
    if let Ok(mut slot) = state.cancel.lock() {
        *slot = None;
    }
    let file = downloaded?;

    let _ = app.emit(
        UPDATE_PROGRESS,
        serde_json::json!({ "phase": "installing" }),
    );
    let installed = carter_app_core::update::install(&file)?;
    match installed {
        carter_app_core::update::Installed::WillRestart(_) => {
            // 替换脚本已经在等本进程退出了。
        }
        carter_app_core::update::Installed::Ready(_) => {
            carter_app_core::update::relaunch_after_exit()?;
        }
    }
    let _ = app.emit(
        UPDATE_PROGRESS,
        serde_json::json!({ "phase": "restarting" }),
    );
    // 给前端一点时间把「正在重启」画出来，然后干净地退出 —— 托盘、助理那些
    // 后台任务都得走一遍关闭逻辑，不能直接 `exit`。
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        handle.cleanup_before_exit();
        handle.exit(0);
    });
    Ok(())
}

/// 重启应用。
///
/// **必须先退出再拉起**：本应用有 single-instance 插件，先起新进程的话它会立刻
/// 发现旧进程还在、把参数转过去然后自己退出，紧接着旧进程也退出 —— 界面就没了。
/// 所以起一段脱离的脚本等本进程死掉，再由它拉起可执行文件。
///
/// 不用 `app.restart()`：本应用关窗只是藏进托盘，`restart` 走的是正常的退出
/// 流程，托盘、助理那些后台任务得跟着走一遍关闭逻辑。
#[tauri::command]
pub(crate) fn restart_app(app: tauri::AppHandle) {
    if let Err(e) = carter_app_core::update::relaunch_after_exit() {
        tracing::warn!(error = %e, "重启：拉不起新进程");
    }
    app.cleanup_before_exit();
    app.exit(0);
}
