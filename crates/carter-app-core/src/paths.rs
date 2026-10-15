//! 应用私有路径。
//!
//! 方案 D8 的数据分家：`~/.carter/` 与 CLI 共享（config / skills / sessions），
//! `~/.carter-app/` 只放本应用独有的东西（原始报文、界面设置）。
//! 这个区分是有意的 —— 放进 `~/.carter/` 的每一项都等于对外承诺「CLI 也认」。

use std::path::PathBuf;

/// 改环境变量的测试共用的一把锁。
///
/// `CARTER_HOME` / `CARTER_APP_HOME` 是**进程级**的，而 cargo 并行跑测试 ——
/// 两个测试各用各的 `Mutex` 等于没锁，会互相把对方的临时目录顶掉（表现为
/// 「配置读出来是默认值」这种莫名其妙的失败）。所以必须是**同一把**。
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// 长任务目标（`/goal`）的存放目录：一个会话一个文件。
///
/// 与 `captures/` 同样的形状，理由也一样：会话 id 本身就是索引键，而且主会话与
/// 侧边会话可能同时活着 —— 共用一个 `goals.json` 就得处理「读改写」的互相覆盖。
pub fn goals_dir() -> PathBuf {
    app_home().join("goals")
}
