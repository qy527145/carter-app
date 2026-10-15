//! 应用私有路径。
//!
//! 方案 D8 的数据分家：`~/.carter/` 与 CLI 共享（config / skills / sessions），
//! `~/.carter-app/` 只放本应用独有的东西（原始报文、界面设置）。
//! 这个区分是有意的 —— 放进 `~/.carter/` 的每一项都等于对外承诺「CLI 也认」。

use std::path::PathBuf;

/// `$CARTER_APP_HOME`，否则 `~/.carter-app`。
pub fn app_home() -> PathBuf {
    if let Some(v) = std::env::var_os("CARTER_APP_HOME") {
        return PathBuf::from(v);
    }
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join(".carter-app"))
        .unwrap_or_else(|| PathBuf::from(".carter-app"))
}

/// 界面设置（主题、工具开关、侧栏宽度）。
pub fn settings_path() -> PathBuf {
    app_home().join("settings.json")
}

/// 原始报文仓库的根目录。
pub fn captures_dir() -> PathBuf {
    app_home().join("captures")
}
