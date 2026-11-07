use crate::prelude::*;

// ---------------------------------------------------------------------------
// 输入框：斜杠命令、@ 文件、图片
// ---------------------------------------------------------------------------

/// `/` 菜单的命令列表：本应用真能执行的内置命令 + 用户自定义命令。
#[tauri::command]
pub(crate) fn list_commands(cwd: Option<String>) -> Vec<carter_app_core::CommandRow> {
    carter_app_core::input::list(&cwd.map(PathBuf::from).unwrap_or_else(default_cwd))
}

/// `@` 菜单的文件/目录列表（相对工作目录的路径，目录带尾随 `/`）。
///
/// `async` 不是为了 await，而是让它跑在异步线程池上：不带 `/` 的查询会
/// 遍历整棵目录树，同步命令会在主线程上做，一敲 `@` 就能把界面卡住。
#[tauri::command]
pub(crate) async fn list_files(
    cwd: Option<String>,
    query: String,
    limit: Option<usize>,
) -> Vec<String> {
    carter_app_core::input::list_files(
        &cwd.map(PathBuf::from).unwrap_or_else(default_cwd),
        &query,
        limit.unwrap_or(50),
    )
}

/// 把粘贴进来的图片存进 carter 的图片库，返回可插进消息文本的 `[img:...]` 引用。
#[tauri::command]
pub(crate) fn save_image(data: String, ext: Option<String>) -> Result<String, String> {
    carter_app_core::images::save_image(&data, ext.as_deref())
}

/// 把一条图片引用读成 data URL 供界面显示。读不到返回 null（文件被清理过等）。
///
/// 与 [`preview_file`] 同理走 `spawn_blocking`：对话正文里每张图都会调它，同步跑在
/// 主线程上会把界面卡住。
#[tauri::command]
pub(crate) async fn read_image(token: String) -> Option<String> {
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
pub(crate) async fn preview_file(
    cwd: Option<String>,
    path: String,
) -> carter_app_core::FilePreview {
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
pub(crate) struct ChangeRules {
    dirs: Vec<String>,
    files: Vec<String>,
}

#[tauri::command]
pub(crate) fn change_rules() -> ChangeRules {
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
pub(crate) async fn file_diff(
    session_id: String,
    since: u32,
    cwd: String,
    path: String,
) -> carter_app_core::FileDiff {
    let fallback = path.clone();
    tokio::task::spawn_blocking(move || {
        carter_app_core::changes::diff_for(&session_id, since, std::path::Path::new(&cwd), &path)
    })
    .await
    .unwrap_or_else(|_| carter_app_core::FileDiff::Unavailable {
        path: fallback,
        reason: "计算 diff 失败".into(),
    })
}
