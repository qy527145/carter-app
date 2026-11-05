//! 应用自更新：查 Release、挑安装包、下载、把安装包交给系统装。
//!
//! ## 为什么不走 tauri-plugin-updater
//!
//! Release 里放的是三份**安装包**（Windows 的 NSIS `.exe`、macOS 的 `.dmg`、
//! Linux 的 `.deb`），不是 Tauri 更新器要的那种「解开即用」的归档：
//!
//! * 更新器要一份签过名的 `latest.json` + 每个平台一个 `.tar.gz` / `.zip`，
//!   而我们要的正是「用户看到的同一份文件」，不想在 Release 里再养一套影子产物；
//! * 更新器只管下载与替换，macOS 上它认的是 `.app.tar.gz`，对 `.dmg` 无能为力。
//!
//! 所以这里自己下载安装包，再按平台交给系统的方式装上。代价是**没有签名校验**
//! —— 补的是「Release 里只放安装包」这个要求本身。风险面用三条限制压住：
//!
//! 1. 只认本仓库 Releases 里的资产，地址是按仓库名拼出来的，不读任何远程给的
//!    「下哪个文件」的清单；
//! 2. HTTPS 走加速站时前缀也是写死的那几个（见 `net::GITHUB_ACCELS`）；
//! 3. 装之前把文件名、大小都报给用户看过（界面上的确认），不静默安装。
//!
//! ## 各平台怎么装
//!
//! | 平台 | 安装包 | 装的方式 |
//! | ---- | ------ | -------- |
//! | Windows | NSIS `*-setup.exe` | 直接起安装程序（它自带 `/S` 静默，但我们让它正常交互，用户点完再重启本应用） |
//! | macOS | `.dmg` | `hdiutil attach` → 用 `ditto` 覆盖 `/Applications/Carter.app` → 卸载镜像 |
//! | Linux | `.deb` | 在终端模拟器里跑 `sudo dpkg -i`（GUI 进程没有 sudo 密码可问） |
//!
//! macOS 的 `ditto` 跑在**本应用进程内**：覆盖的是磁盘上另一个 `.app` 里的
//! 文件，正在运行的这个进程自己的映像已经映射进内存，不会被换掉。下一次启动
//! 就是新版。

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::app_settings::AppSettings;
use crate::net;
use crate::platform::Os;

/// 仓库坐标。与 `src-tauri/Cargo.toml` 的 `repository` 一致 —— 那里是给 dist 看的，
/// 这里没有更好的共享方式，改的时候两边一起改。
pub const REPO: &str = "qy527145/carter-app";

/// 查 Release 的超时，比普通下载短：启动时顺手查的，不该让用户等。
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
/// 下载安装包的超时。一百多 MB 在慢网下要好几分钟。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// 一次查到的版本信息（给界面看）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// 形如 `v0.1.2`。
    pub tag: String,
    /// 去掉 `v` 的版本号，`0.1.2`。
    pub version: String,
    /// Release 页面地址，界面上「看更新说明」用它。
    pub html_url: String,
    /// 这次要下的安装包文件名（如 `Carter_0.1.2_aarch64.dmg`）。
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
}

/// 当前版本能不能用 `v<tag>` 里的这一版。
pub fn is_newer(current: &str, tag: &str) -> bool {
    match (parse_version(current), parse_version(tag)) {
        (Some(cur), Some(next)) => next > cur,
        // 解析不出来就不要自作主张说「有新版」。
        _ => false,
    }
}

/// 版本号的比较形态：数字段 + 是否预发布。
///
/// 元组本身不能直接比 —— `0.1` 与 `0.1.0` 得当成同一个，短的要补 0，
/// 所以包一层自己实现 [`Ord`]。
#[derive(Debug, PartialEq, Eq)]
struct VersionKey {
    parts: Vec<u64>,
    pre: bool,
}

impl PartialOrd for VersionKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VersionKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let n = self.parts.len().max(other.parts.len());
        let at = |v: &Self, i: usize| v.parts.get(i).copied().unwrap_or(0);
        for i in 0..n {
            match at(self, i).cmp(&at(other, i)) {
                std::cmp::Ordering::Equal => {}
                ord => return ord,
            }
        }
        // 同号时：正式版（`pre = false`）比预发布版大。
        match (self.pre, other.pre) {
            (false, true) => std::cmp::Ordering::Greater,
            (true, false) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Equal,
        }
    }
}

/// `v0.1.2` / `0.1.2` / `0.2.0-beta.1` → 比较形态。
///
/// 预发布的 `-` 后缀按「比同号的正式版小」处理（`0.2.0-beta < 0.2.0`），
/// 这与 semver 一致，也意味着装了正式版的人不会被 beta 提示。
fn parse_version(raw: &str) -> Option<VersionKey> {
    let raw = raw.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = match raw.split_once('-') {
        Some((core, _)) => (core, true),
        None => (raw, false),
    };
    let parts = core
        .split('.')
        .map(|p| p.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    if parts.is_empty() {
        return None;
    }
    Some(VersionKey { parts, pre })
}

/// 这台机器该下哪个安装包。认不出来就是「没有」—— 宁可说「请手动去 Release 下载」，
/// 也别把 `.deb` 下到 mac 上。
pub fn asset_matches_here(name: &str) -> bool {
    matches_here(name, crate::platform::current_os(), std::env::consts::ARCH)
}

/// [`asset_matches_here`] 的实现，平台与架构显式传入，测试里三种平台都能验。
///
/// 架构判断的写法是「**不包含另一种架构的名字**」而不是「包含本架构的名字」：
/// 各平台打包器给 arch 的写法不统一（`_aarch64` / `_aarch64` on mac，deb 里
/// 写 `_amd64` / `_arm64`，NSIS 是 `_x64` 或者干脆不写）。x86_64 那侧的标记
/// 恰好都长得差不多（`x86_64` / `x64` / `amd64`），反过来漏判
/// 等于把错架构的包推给用户。
fn matches_here(name: &str, os: Os, arch: &str) -> bool {
    let foreign_arch = match os {
        // Windows 只有 x86_64 的包，`.exe` 没有架构后缀可言 —— 不做架构判断。
        Os::Windows => return name.ends_with(".exe"),
        _ if arch == "aarch64" => &["x86_64", "x86-64", "x64", "amd64", "intel"][..],
        _ => &["aarch64", "arm64"][..],
    };
    let ext_ok = match os {
        Os::Macos => name.ends_with(".dmg"),
        Os::Linux => name.ends_with(".deb"),
        Os::Windows => unreachable!(),
    };
    ext_ok && !foreign_arch.iter().any(|m| name.contains(m))
}

/// 当前架构名，给界面说明用。
pub fn arch_label() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "Apple Silicon / arm64",
        "x86_64" => "x86_64",
        other => other,
    }
}

/// 查最新 Release。
///
/// `include_prerelease = false` 走 `/releases/latest`（GitHub 自己只返回正式版）；
/// 界面点「检查更新」时一般用这个。带预发布版本的 tag 不会出现在这里 ——
/// [`is_newer`] 也会再把一道关。
pub async fn check(current: &str, settings: &AppSettings) -> Result<ReleaseInfo, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let fetched = net::get_first(&settings.net_prefs(), &[&url], QUERY_TIMEOUT, None, None)
        .await
        .map_err(|e| format!("查更新失败：{e}"))?;

    let release: GhRelease =
        serde_json::from_slice(&fetched.bytes).map_err(|e| format!("Release 信息看不懂：{e}"))?;
    if release.draft || release.prerelease {
        return Err("最新 Release 还是草稿或预发布，暂不提示".into());
    }
    if !is_newer(current, &release.tag_name) {
        return Ok(ReleaseInfo {
            tag: release.tag_name.clone(),
            version: release.tag_name.trim_start_matches('v').to_string(),
            html_url: release.html_url,
            asset_name: String::new(),
            asset_url: String::new(),
            asset_size: 0,
        });
    }
    let asset = release
        .assets
        .into_iter()
        .find(|a| asset_matches_here(&a.name))
        .ok_or_else(|| {
            format!(
                "{} 没有适合这台机器（{}）的安装包，去 Release 页面手动下载",
                release.tag_name,
                arch_label()
            )
        })?;
    Ok(ReleaseInfo {
        tag: release.tag_name.clone(),
        version: release.tag_name.trim_start_matches('v').to_string(),
        html_url: release.html_url,
        asset_name: asset.name,
        asset_url: asset.browser_download_url,
        asset_size: asset.size,
    })
}

/// GitHub API 里用到的字段。只声明用到的，其余忽略。
#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

/// 下载安装包到 `~/.carter-app/updates/`，返回落盘路径。
///
/// 进度通过 `progress(已下载, 总大小)` 回调给界面。取消用 `cancel`（同一个
/// 原子开关，见 tauri 层）。
pub async fn download(
    info: &ReleaseInfo,
    settings: &AppSettings,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    progress: Option<&(dyn Fn(u64, Option<u64>) + Sync)>,
) -> Result<PathBuf, String> {
    let dir = crate::paths::app_home().join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| format!("建更新目录失败：{e}"))?;
    let dest = dir.join(&info.asset_name);

    let fetched = net::get_first(
        &settings.net_prefs(),
        &[&info.asset_url],
        DOWNLOAD_TIMEOUT,
        cancel,
        progress,
    )
    .await?;
    // 先写临时文件再改名：下到一半取消 / 断网时，磁盘上不会留下一个「看起来
    // 下好了」的半截安装包，下次点更新也不会以为已经下完。
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &fetched.bytes).map_err(|e| format!("写安装包失败：{e}"))?;
    std::fs::rename(&tmp, &dest).map_err(|e| format!("落盘安装包失败：{e}"))?;
    Ok(dest)
}

/// 把下载好的安装包交给系统装。返回给界面看的一句话。
///
/// 这一步不会等安装完成（Windows 的安装程序、Linux 的终端都是另起的进程），
/// 界面拿到返回值后建议用户重启应用。
pub fn install(path: &std::path::Path) -> Result<String, String> {
    if !path.is_file() {
        return Err(format!("安装包不见了：{}", path.display()));
    }
    match crate::platform::current_os() {
        Os::Windows => install_windows(path),
        Os::Macos => install_macos(path),
        Os::Linux => install_linux(path),
    }
}

#[cfg(windows)]
fn install_windows(path: &std::path::Path) -> Result<String, String> {
    let mut cmd = std::process::Command::new(path);
    // 安装程序会接管、自己弹窗；这里不等它，用户装完自己重启。
    cmd.spawn().map_err(|e| format!("启动安装程序失败：{e}"))?;
    Ok("安装程序已启动。装完后请重新打开 Carter，旧进程会自动退出。".into())
}

#[cfg(not(windows))]
fn install_windows(_path: &std::path::Path) -> Result<String, String> {
    Err("Windows 安装包只能在 Windows 上安装".into())
}

/// macOS：挂载 → `ditto` 覆盖正在跑的那个 `.app` → 卸载。
///
/// 目标是**当前可执行文件所在的那个 `.app`**，不是写死的 `/Applications` ——
/// 用户可能从别处拖进去的（外置盘、另一个用户目录），覆盖错地方会装出两份
/// 「都是 Carter、一个旧一个新」。从 `target/` 里直接跑的 dev 构建覆盖的是
/// 编译产物，也无所谓（何况那种情况 `current_exe` 里根本没有 `.app`，会走
/// 下面的报错分支让用户手动装）。
#[cfg(target_os = "macos")]
fn install_macos(path: &std::path::Path) -> Result<String, String> {
    let dest = current_app_bundle()
        .ok_or("这个 Carter 不是从 .app 里启动的，没法自动安装 —— 请打开安装包，把 Carter 拖进 Applications")?;

    let mount = std::env::temp_dir().join(format!("carter-update-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&mount);
    let mount = mount.to_string_lossy().to_string();

    let attach = std::process::Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-quiet", "-mountpoint", &mount])
        .arg(path)
        .output()
        .map_err(|e| format!("挂载安装包失败：{e}"))?;
    if !attach.status.success() {
        return Err(format!(
            "挂载安装包失败：{}",
            String::from_utf8_lossy(&attach.stderr).trim()
        ));
    }

    // 不管后面成没成，都要先卸载镜像 —— 留着它，用户下次双击同一个 `.dmg`
    // 会看到「已挂载」而拿不到新窗口。
    let result = {
        let src = std::path::Path::new(&mount).join("Carter.app");
        if src.exists() {
            ditto(&src, &dest)
        } else {
            Err("安装包里没找到 Carter.app".into())
        }
    };
    let _ = std::process::Command::new("hdiutil")
        .args(["detach", "-quiet", &mount])
        .output();
    result?;

    Ok(format!(
        "新版已装进 {}。退出后重新打开 Carter 就是新版。",
        dest.display()
    ))
}

/// 当前可执行文件往上找第一个 `*.app` 目录。
#[cfg(target_os = "macos")]
fn current_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(|p| p.to_path_buf())
}

/// `ditto` 合并式写入：覆盖同名文件、保留资源分支与签名，比 rm + cp 安全
/// （中途失败时不会把整个 app 删成半个）。
#[cfg(target_os = "macos")]
fn ditto(src: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let out = std::process::Command::new("ditto")
        .arg(src)
        .arg(dest)
        .output()
        .map_err(|e| format!("安装失败：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "安装失败（{} 可能没有写权限）：{}",
            dest.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn install_macos(_path: &std::path::Path) -> Result<String, String> {
    Err("macOS 安装包只能在 macOS 上安装".into())
}

/// Linux：在终端里跑 `dpkg -i`。
///
/// 不直接用 `pkexec dpkg -i`：有些桌面没配 polkit 的认证代理，失败时只有一行
/// 无从查起的错误；开终端跑 sudo 卡住的时候用户至少看得到发生了什么。
/// 依次试几个常见的模拟器，一个都没有就退回到「告诉用户手动装」。
#[cfg(target_os = "linux")]
fn install_linux(path: &std::path::Path) -> Result<String, String> {
    // 路径要过一遍 shell，单引号包起来；里面本身有单引号的情况极罕见但还是要处理。
    let quoted = format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"));
    let command = format!("sudo dpkg -i {quoted}; echo; echo '装完了可以关掉这个窗口。'");
    let candidates: [(&str, Vec<&str>); 4] = [
        ("x-terminal-emulator", vec!["-e", "sh", "-c"]),
        ("gnome-terminal", vec!["--", "sh", "-c"]),
        ("konsole", vec!["-e", "sh", "-c"]),
        ("xterm", vec!["-e", "sh", "-c"]),
    ];
    for (term, args) in candidates {
        let mut cmd = std::process::Command::new(term);
        cmd.args(args).arg(&command);
        match cmd.spawn() {
            Ok(_) => return Ok("已在终端里打开安装命令，输入密码后就会装上。".into()),
            Err(_) => continue,
        }
    }
    Err(format!(
        "没找到可用的终端模拟器。请手动安装：sudo dpkg -i {}",
        path.display()
    ))
}

#[cfg(not(target_os = "linux"))]
fn install_linux(_path: &std::path::Path) -> Result<String, String> {
    Err("Linux 安装包只能在 Linux 上安装".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison_handles_padding_and_prerelease() {
        assert!(is_newer("0.1.0", "v0.1.1"));
        assert!(is_newer("0.1.0", "v0.2.0"));
        assert!(is_newer("0.9.9", "v1.0.0"));
        assert!(is_newer("1.0", "v1.0.1"));
        assert!(!is_newer("0.1.1", "v0.1.1"));
        assert!(!is_newer("0.2.0", "v0.1.9"));
        // 预发布比同号正式版小：装了正式版的人不会被 β 版提示。
        assert!(!is_newer("0.2.0", "v0.2.0-beta.1"));
        assert!(is_newer("0.1.0", "v0.2.0-beta.1"));
        // 认不出来的版本号不比，免得乱提示。
        assert!(!is_newer("0.1.0", "nightly"));
    }

    #[test]
    fn picks_an_installer_for_this_platform_and_arch() {
        // Apple Silicon：只要 arm 的 dmg。
        assert!(matches_here(
            "Carter_0.1.0_aarch64.dmg",
            Os::Macos,
            "aarch64"
        ));
        assert!(!matches_here("Carter_0.1.0_x64.dmg", Os::Macos, "aarch64"));

        // 架构判断要认各平台打包器的写法。
        assert!(!matches_here(
            "carter-app_0.1.0_amd64.deb",
            Os::Linux,
            "aarch64"
        ));
        assert!(!matches_here(
            "carter-app_0.1.0_x86_64.deb",
            Os::Linux,
            "aarch64"
        ));
        assert!(matches_here(
            "carter-app_0.1.0_arm64.deb",
            Os::Linux,
            "aarch64"
        ));
        assert!(matches_here(
            "carter-app_0.1.0_amd64.deb",
            Os::Linux,
            "x86_64"
        ));
        assert!(!matches_here(
            "carter-app_0.1.0_arm64.deb",
            Os::Linux,
            "x86_64"
        ));

        // Windows 的安装包没有架构后缀，直接认。
        assert!(matches_here(
            "Carter_0.1.0_x64-setup.exe",
            Os::Windows,
            "x86_64"
        ));

        // 不该把别的平台的安装包下过来。
        assert!(!matches_here(
            "carter-app_0.1.0_amd64.deb",
            Os::Macos,
            "x86_64"
        ));
        assert!(!matches_here("Carter_0.1.0_x64.dmg", Os::Linux, "x86_64"));
        assert!(!matches_here("sha256.sum", Os::Windows, "x86_64"));
    }
}
