//! 连接器的包管理器：优先用 bun，没有就用 npm。
//!
//! ## 为什么不直接 `npm install -g`
//!
//! 三件事叠在一起让 npm 这条路很难走通：
//!
//! * **npm 源慢**。`registry.npmjs.org` 在国内经常几十 KB/s，一个带 postinstall
//!   下载二进制的包能卡十分钟；
//! * **node 不在 GUI 进程的 PATH 里**。从 Finder / 开始菜单启动的进程拿不到
//!   shell 启动文件，`npm` 装的 `lark-cli` 是 `#!/usr/bin/env node`，跑起来直接
//!   127 —— 用户看到的是「装好了但用不了」；
//! * **Windows 上 PowerShell 嵌套引号**。安装命令本身是塞进 `powershell -Command`
//!   的一段脚本，脚本里再出现一层引号就很容易被拆坏。
//!
//! bun 对这三件都有解：单个静态二进制（不需要 node）、`bun install -g` 的全局
//! bin 固定在 `~/.bun/bin`（或者 `<exe 目录>/bun`），装包也快得多。
//!
//! ## 找 bun 的顺序
//!
//! 1. `PATH` 里的 `bun` —— 用户自己装的，版本由他管；
//! 2. `~/.bun/bin/bun` —— `bun` 官方安装脚本的落点，GUI 进程的 PATH 常常没有它；
//! 3. **随安装包分发的那一份**（`<exe 目录>/bun`，打包时由
//!    `scripts/fetch-bun.ts` 从 GitHub Release 取来放进 `externalBin`）。
//!    它排在最后：用户自己那份更新、更贴近他的环境。
//!
//! 全都没有才退回 npm —— 老机器上还是可能只有 npm。

use std::path::{Path, PathBuf};

use crate::net;
use crate::platform;

/// 找到的包管理器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManager {
    /// 可执行文件绝对路径（`bun` 或 `npm`）。
    pub exe: PathBuf,
    /// 是不是 bun。决定要不要给安装命令加 `--trust` 之类的参数。
    pub is_bun: bool,
}

impl PackageManager {
    /// 给界面看的一行：「使用 bun 1.4.2（~/.bun/bin/bun）」之类。
    pub fn label(&self) -> String {
        if self.is_bun {
            format!("bun（{}）", self.exe.display())
        } else {
            format!("npm（{}）", self.exe.display())
        }
    }
}

/// 按优先级找包管理器。找不到 npm 时返回 `None`（调用方要把这个报出来）。
pub fn detect() -> Option<PackageManager> {
    if let Some(exe) = find_bun() {
        return Some(PackageManager { exe, is_bun: true });
    }
    platform::which(npm_name()).map(|exe| PackageManager { exe, is_bun: false })
}

/// bun 的查找（顺序见模块头第 1–3 条）。
pub fn find_bun() -> Option<PathBuf> {
    if let Some(p) = platform::which("bun") {
        return Some(p);
    }
    if let Some(home) = crate::paths::home_dir() {
        let p = home.join(".bun").join("bin").join(bun_name());
        if p.is_file() {
            return Some(p);
        }
    }
    // 随安装包分发的那一份：与主程序同目录（Tauri 的 sidecar 就是放在那儿）。
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let p = dir.join(bun_name());
        if p.is_file() {
            return Some(p);
        }
        // macOS 的 .app 里，sidecar 在 `Contents/MacOS/`。
        let p = dir.join("MacOS").join(bun_name());
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 用找到的包管理器改写一条安装命令。
///
/// 厂商表里的命令统一写成 `npm install -g <pkg>` 或 `bun install -g <pkg>`；
/// 这里把开头的程序名换成**绝对路径** —— GUI 进程的 PATH 里没有 `bun`，直接写
/// `bun` 会「command not found」。Windows 上路径带空格（`C:\Program Files\…`），
/// 要用双引号包住，这在 PowerShell 与 cmd 里都成立。
///
/// 没有 bun 时把 `bun install -g` 退回 `npm install -g`，不让命令直接失效。
pub fn rewrite(command: &str, pm: &PackageManager) -> String {
    let trimmed = command.trim_start();
    let rest = if let Some(r) = trimmed.strip_prefix("bun ") {
        if pm.is_bun {
            Some(r)
        } else {
            return swap_to_npm(trimmed);
        }
    } else if let Some(r) = trimmed.strip_prefix("npm ") {
        if pm.is_bun {
            // 只有全局安装能交给 bun；`npm run` 之类与 bun 语义不同，保持原样。
            if r.starts_with("install -g") || r.starts_with("i -g") {
                Some(r)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    match rest {
        Some(r) => {
            // bun 默认不跑依赖包的 postinstall（`lark-cli` 的二进制正是在
            // postinstall 里下载的），全局装一个连接器 CLI 时显式 `--trust`。
            let args = if pm.is_bun && r.starts_with("install") {
                format!("install --trust{}", &r["install".len()..])
            } else {
                r.to_string()
            };
            format!("\"{}\" {args}", pm.exe.display())
        }
        None => command.to_string(),
    }
}

/// `bun install -g x` → `npm install -g x`（没有 bun 时的退路）。
fn swap_to_npm(command: &str) -> String {
    command
        .strip_prefix("bun ")
        .map(|r| format!("npm {r}"))
        .unwrap_or_else(|| command.to_string())
}

/// 本机没有 bun 时下载一份到 `~/.carter-app/bin/`，返回它的路径。
///
/// 为什么要下载而不是「没有就让用户自己装」：连接器页面上只有一颗「安装 CLI」，
/// 用户在 Windows 上多半连 node 都没有；把 npm 那一套推给他，等于把「装不上」
/// 原样退回给他。下载的是一份静态二进制（`bun` 的官方 Release），约 40 MB，
/// 走与其它下载同一条加速链路（[`net::get_first`]），国内也能下得动。
///
/// 装过一次就一直用；用户自己在 PATH 里装了 bun 的话根本走不到这里。
pub async fn ensure(progress: impl Fn(&str)) -> Result<PathBuf, String> {
    if let Some(p) = find_bun() {
        return Ok(p);
    }
    let (slug, name) = release_asset()?;
    let url = format!("https://github.com/oven-sh/bun/releases/latest/download/{slug}.zip");
    progress(&format!("没有找到 bun，正在下载 {slug}.zip（约 40 MB）…"));

    let settings = crate::app_settings::AppSettings::load();
    let fetched = net::get_first(
        &settings.net_prefs(),
        &[&url],
        std::time::Duration::from_secs(300),
        None,
        None,
    )
    .await
    .map_err(|e| format!("下载 bun 失败：{e}"))?;

    let tmp = std::env::temp_dir().join(format!("carter-bun-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("建临时目录失败：{e}"))?;
    let zip = tmp.join(format!("{slug}.zip"));
    std::fs::write(&zip, &fetched.bytes).map_err(|e| format!("写临时文件失败：{e}"))?;

    progress("正在解压…");
    extract(&zip, &tmp)?;
    let found = find_file(&tmp, name).ok_or("下载的压缩包里没有 bun —— 可能没下完整，重试一次")?;

    let dir = crate::paths::app_home().join("bin");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建目录失败：{e}"))?;
    let dest = dir.join(name);
    // 先拷到临时名再改名：中途失败不会留下一个看起来能用的半截二进制。
    let part = dest.with_extension("part");
    std::fs::copy(&found, &part).map_err(|e| format!("落盘 bun 失败：{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&part, &dest).map_err(|e| format!("落盘 bun 失败：{e}"))?;
    let _ = std::fs::remove_dir_all(&tmp);
    progress(&format!("bun 已就绪：{}", dest.display()));
    Ok(dest)
}

/// 当前平台对应的 bun Release 资产名。
fn release_asset() -> Result<(&'static str, &'static str), String> {
    let (os, arch) = (crate::platform::current_os(), std::env::consts::ARCH);
    let slug = match (os, arch) {
        (crate::platform::Os::Windows, _) => "bun-windows-x64",
        (crate::platform::Os::Macos, "aarch64") => "bun-darwin-aarch64",
        (crate::platform::Os::Macos, _) => "bun-darwin-x64",
        (crate::platform::Os::Linux, "aarch64") => "bun-linux-aarch64",
        (crate::platform::Os::Linux, _) => "bun-linux-x64",
    };
    Ok((slug, bun_name()))
}

/// 解压 zip。优先 `unzip`，退到 `tar -xf`（Windows 10+ 自带的 bsdtar 认 zip，
/// macOS 同理），都不行就报错让用户手动装 bun。
fn extract(zip: &Path, dir: &Path) -> Result<(), String> {
    let attempts: [(&str, Vec<&str>); 2] = [("unzip", vec!["-oq"]), ("tar", vec!["-xf"])];
    let mut last = String::new();
    for (prog, args) in attempts {
        let mut cmd = std::process::Command::new(prog);
        cmd.args(args).arg(zip).current_dir(dir);
        #[cfg(windows)]
        platform::hide_console(&mut cmd);
        match cmd.output() {
            Err(e) => last = format!("{prog}: {e}"),
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => {
                last = format!("{prog}: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
        }
    }
    Err(format!(
        "解压 bun 失败（{last}）。手动装一份 bun 再重试：https://bun.sh"
    ))
}

/// 在目录树里找第一个叫 `name` 的文件。
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(hit) = find_file(&p, name) {
                return Some(hit);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

fn bun_name() -> &'static str {
    if cfg!(windows) { "bun.exe" } else { "bun" }
}

/// npm 在 Windows 上同样不带后缀（`npm.cmd` 由 `platform::which` 按 PATHEXT 找到）。
fn npm_name() -> &'static str {
    "npm"
}

/// 给子进程的额外环境变量。
///
/// * `npm_config_registry`：国内源。只在用户**没显式配过**时才给（看
///   `NPM_CONFIG_REGISTRY` 与 `~/.npmrc`），免得覆盖掉企业内网的私有源；
/// * `BUN_INSTALL_CACHE_DIR`：bun 默认缓存在 `~/.bun/install/cache`，这里不挪。
///
/// 返回的是「要设的键值」，由调用方决定怎么塞进命令。
pub fn env_overrides(use_cn_registry: bool) -> Vec<(&'static str, String)> {
    if !use_cn_registry || user_set_registry() {
        return Vec::new();
    }
    vec![(
        "npm_config_registry",
        "https://registry.npmmirror.com".to_string(),
    )]
}

/// 用户是不是自己配过 registry（环境变量或 `~/.npmrc`）。
fn user_set_registry() -> bool {
    if std::env::var_os("NPM_CONFIG_REGISTRY").is_some()
        || std::env::var_os("npm_config_registry").is_some()
    {
        return true;
    }
    let Some(home) = crate::paths::home_dir() else {
        return false;
    };
    std::fs::read_to_string(home.join(".npmrc"))
        .map(|s| s.lines().any(|l| l.trim_start().starts_with("registry=")))
        .unwrap_or(false)
}

/// 给界面的一句话：这次安装会用哪个包管理器。
pub fn describe() -> String {
    match detect() {
        Some(pm) => pm.label(),
        None => "没找到 bun 或 npm：连接器 CLI 大多需要其中一个".into(),
    }
}

/// 这个路径是不是 bun（测试用）。
pub fn is_bun_path(p: &Path) -> bool {
    p.file_name().is_some_and(|n| n == bun_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bun() -> PackageManager {
        PackageManager {
            exe: PathBuf::from(if cfg!(windows) {
                r"C:\Users\x\.bun\bin\bun.exe"
            } else {
                "/home/x/.bun/bin/bun"
            }),
            is_bun: true,
        }
    }

    fn npm() -> PackageManager {
        PackageManager {
            exe: PathBuf::from("npm"),
            is_bun: false,
        }
    }

    #[test]
    fn npm_installs_become_bun_installs_with_trust() {
        let out = rewrite("npm install -g @larksuite/cli", &bun());
        assert!(out.contains("install --trust -g @larksuite/cli"), "{out}");
        assert!(!out.starts_with("npm"));
        assert!(out.contains(bun_name()));
    }

    #[test]
    fn without_bun_the_npm_command_is_kept() {
        assert_eq!(
            rewrite("npm install -g @larksuite/cli", &npm()),
            "npm install -g @larksuite/cli"
        );
        // 厂商表里写成 bun 的命令，没有 bun 时退回 npm。
        assert_eq!(
            rewrite("bun install -g @larksuite/cli", &npm()),
            "npm install -g @larksuite/cli"
        );
    }

    #[test]
    fn shell_scripts_are_left_alone() {
        let curl = "curl -fsSL https://example.com/i.sh | sh";
        assert_eq!(rewrite(curl, &bun()), curl);
        // `npm run` 与 bun 语义不同，不乱换。
        assert_eq!(rewrite("npm run build", &bun()), "npm run build");
    }
}
