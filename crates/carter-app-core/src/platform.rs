//! 运行平台与 shell 探测 —— 复用 carter 的那一份，本模块只多一个 GUI 专属的
//! [`ensure_user_paths`]。
//!
//! ## 为什么这里几乎是空的
//!
//! shell 探测、`CREATE_NO_WINDOW`、bash 工具描述都是「基础能力」，carter 里已经有一份
//! 完整的（CLI 自己也要用）。早先 app 抄了一份，两份逐行相同 —— 改一边忘一边就会
//! 出现「CLI 能跑、app 却 spawn 不到 shell」这种只在某个平台复现的问题。
//!
//! 现在统一指向 `carter::platform`，`bash_tool_description` 也就自动带上了 carter
//! 每一轮对描述的更新（`bash` 的 `run_in_background`、`cd` 保留、看图指引等）。
//!
//! ## 只有 `ensure_user_paths` 留在 app
//!
//! 它服务的是**GUI 进程**的处境：从 Finder / 开始菜单启动时不读 shell 启动文件，
//! PATH 里没有 `/usr/local/bin`、`~/.local/bin`，「我终端里明明能跑 `dws`」在应用里
//! 就是不认。CLI 从终端启动，天然没有这个问题，所以这个函数只属于宿主侧。

pub use carter::platform::{
    Os, Shell, ShellKind, bash_tool_description, bash_tool_description_leaked, current_os,
    detect_shell, os_label, which,
};

// `hide_console` 系列只在 Windows 上存在（那里才有 `CREATE_NO_WINDOW`）。调用点
// 本来就都包在 `#[cfg(windows)]` 里，这里跟着门一次，非 Windows 上不必凭空造一个
// 空函数 —— 那会让「忘了加 cfg」的调用点静默通过。
#[cfg(windows)]
pub use carter::platform::{CREATE_NO_WINDOW, hide_console, hide_console_async};

use std::path::PathBuf;

/// 把用户级可执行目录补进本进程的 `PATH`。
///
/// ## 为什么需要
///
/// 从 Finder / 开始菜单启动的 GUI 进程**不读 shell 启动文件**，拿到的 PATH 是系统
/// 给的默认值：没有 `/usr/local/bin`、没有 `~/.local/bin`。而官方 CLI 的安装脚本
/// 恰恰把二进制放进 `~/.local/bin`（钉钉的 `dws` 就是这样）。结果就是「我在终端里
/// 明明能跑 `dws`，这个应用却说没装」。
///
/// 补的是**用户级**目录，不是把登录 shell 的 PATH 整个搬过来：后者要起一个 shell、
/// 要解析 rc 文件，慢且不可预测（rc 里可能有一堆交互式的东西）。
///
/// 幂等 —— 装完 CLI 之后再调一次，把新出现的目录补上。
pub fn ensure_user_paths() {
    let Some(home) = crate::paths::home_dir() else {
        return;
    };
    let mut dirs = vec![home.join(".local").join("bin")];
    if cfg!(windows) {
        // npm / bun 的全局 bin 在 Windows 上也常常不在这份 PATH 里。
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(PathBuf::from(appdata).join("npm"));
        }
    }
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths: Vec<PathBuf> = std::env::split_paths(&current).collect();
    let mut added = false;
    for d in dirs {
        if d.is_dir() && !paths.contains(&d) {
            paths.push(d);
            added = true;
        }
    }
    if added && let Ok(joined) = std::env::join_paths(&paths) {
        // SAFETY（语义上）：`set_var` 在多线程下与并发读 PATH 有数据竞争。
        // 这里只在启动早期、或用户点了「安装」之后的命令层调用，那时没有
        // 别的线程在读 PATH 做决策；而且这是整个应用唯一写 PATH 的地方。
        unsafe { std::env::set_var("PATH", joined) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复用来的那几项要真的能用 —— 这条挡的是「再导出写漏了某个名字」
    /// 之外更隐蔽的一种：`carter::platform` 里探测到的 shell 在本机 spawn 不起来。
    #[test]
    fn the_detected_shell_is_actually_spawnable() {
        let s = detect_shell();
        let ok = if s.program.is_absolute() {
            // 绝对路径必须真的在。
            s.program.is_file()
        } else {
            // 裸名字必须能从 PATH 解析出来。
            which(&s.program.to_string_lossy()).is_some()
        };
        assert!(
            ok,
            "探测到的 shell `{}` 不存在，spawn 时会报 program not found",
            s.program.display()
        );
    }

    /// 描述必须点出当前是哪一个 shell。这是 shell 探测存在的理由：
    /// 模型看不到我们的进程环境，只能从描述里知道该写 `ls` 还是 `dir`。
    #[test]
    fn the_description_names_the_platform_and_the_shell() {
        let d = bash_tool_description();
        assert!(d.contains(os_label()), "描述里要写明是哪个系统：{d}");
        assert!(
            d.contains("echo $VAR") || d.contains("%VAR%"),
            "描述里要写清环境变量的写法：{d}"
        );
        assert!(
            d.contains("cmd 的内建命令不可用") || d.contains("不是 bash"),
            "描述里要写清哪些命令不存在：{d}"
        );
    }

    /// app 复用的必须就是 carter 那一份，而不是又抄了一遍 —— 两份漂开正是
    /// 这个模块当初要解决的问题。
    #[test]
    fn the_description_is_the_one_carter_ships() {
        assert_eq!(
            bash_tool_description(),
            carter::platform::bash_tool_description()
        );
    }
}
