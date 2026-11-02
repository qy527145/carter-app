//! IM 连接器：把外部 IM（现在只有钉钉，将来飞书 / 企业微信 …）收成一个统一入口。
//!
//! ## 为什么是「skill + 官方 CLI」
//!
//! 连接 IM 需要的东西（通讯录、日历、待办、群消息、审批 …）都在厂商自己的服务端，
//! 本应用不该重写一遍协议，也不该自己保存一堆 AppKey / AppSecret。厂商官方都出了
//! 「CLI + 配套 skill」这一套（钉钉是 `dws`）：CLI 负责鉴权与 HTTP，skill 告诉模型
//! 怎么用。本模块要做的只是三件事 ——
//!
//! 1. **装**：把官方 CLI 与它的 skill 装到本机能用的位置；
//! 2. **登**：跑官方 CLI 的 OAuth 扫码登录，把浏览器拉起来；
//! 3. **看**：把 CLI / skill / 登录态摊在设置页上，出问题能一眼看出是哪一层。
//!
//! 界面上只有一张卡片，用户不需要知道底下是两条独立的东西 —— 但那三层状态必须
//! **分别**显示。只知道「连不上」时，用户要么重装 CLI、要么重新登录、要么去技能页
//! 把停用的技能打开，猜错的每一次都是一轮无效操作。
//!
//! ## 为什么 skill 要自己搬
//!
//! `dws skill setup` 只认它自己那张「Agent 名录」（`~/.agents/skills`、`~/.claude/skills`
//! …），没有「装到任意目录」的选项。而本应用扫描的是 `~/.carter/skills`。于是这里用
//! 一个**临时的 `HOME`** 让 dws 把 skill 铺到临时目录，再把那一份搬进本应用的技能目录。
//! 好处是一次 `skill setup` 不碰用户机器上任何别的 Agent 目录 —— 装一个连接器不该顺手
//! 往 Claude Code / Codex 的地盘里塞东西。
//!
//! ## 登录为什么不用 dws 自己拉浏览器
//!
//! `dws auth login` 默认会自己 `open` 一个浏览器，然后把「请手动访问: <url>」打在
//! stderr 里。桌面应用里这两件事都要接过来：浏览器由前端用 opener 插件打开（用户看得见
//! 是从哪儿弹出的），URL 也留在界面上 —— 浏览器没起来时，那颗「打开授权页」按钮是唯一
//! 的出路。所以这里一律带 `--no-browser`，自己解析出地址再广播出去。
//!
//! `--yes` 同理：GUI 里没人能回答 TUI 的确认框，加上它才是**非交互**的那条路。
//!
//! ## 进程为什么要单独一组
//!
//! 登录会等最久 15 分钟（组织没开 CLI 权限时 dws 还会等管理员审批）。用户点了「取消」
//! 就得真的停下 —— 不然 `dws` 还在后台等着回调，下一次点「登录」会和它抢同一个
//! 127.0.0.1 端口。所以子进程 `setsid` 自成一组，取消时整组杀掉。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc;

use crate::app_settings::AppSettings;
use crate::platform;
use crate::protocol::AppEvent;
use crate::shell::AppShell;

// ---------------------------------------------------------------------------
// 厂商表
// ---------------------------------------------------------------------------

/// 一个可接入的 IM 厂商。
///
/// 字段都是静态的：加飞书就是往 [`VENDORS`] 里再加一项，外层的命令、事件、界面
/// 全都不用动。这是「统一入口」这个说法能成立的原因 —— 入口只有一份。
pub struct Vendor {
    /// 稳定标识，事件与命令里都按它认人。
    pub id: &'static str,
    pub name: &'static str,
    /// 卡片上那句说明：接进来之后模型能干什么。
    pub blurb: &'static str,
    /// 官方 CLI 的可执行文件名（在 PATH 与常见安装目录里找）。
    pub cli: &'static str,
    /// 官方 skill 的目录名。装到 `~/.carter/skills/<这个名字>`。
    pub skill: &'static str,
    /// 安装 CLI 的 shell 命令（macOS / Linux）。
    pub install_unix: &'static str,
    /// 安装 CLI 的 PowerShell 命令（Windows）。
    pub install_windows: &'static str,
    /// 官方仓库地址，界面上的「文档」按钮用它。
    pub docs: &'static str,
}

/// 支持的连接器。顺序 = 界面上的顺序。
pub static VENDORS: &[Vendor] = &[Vendor {
    id: "dingtalk",
    name: "钉钉",
    blurb: "日历、待办、通讯录、群消息、审批、钉钉文档与云盘。底层是钉钉官方的 dws CLI 与配套 skill。",
    cli: "dws",
    skill: "dws",
    // 安装脚本默认落在 `~/.local/bin`；`DWS_NO_SKILLS=1` 让它只装二进制 ——
    // skill 由本模块单独搬进本应用的技能目录（见模块头），不往别的 Agent 目录里铺。
    install_unix: "curl -fsSL https://raw.githubusercontent.com/DingTalk-Real-AI/dingtalk-workspace-cli/main/scripts/install.sh | DWS_NO_SKILLS=1 sh",
    install_windows: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"$env:DWS_NO_SKILLS='1'; irm https://raw.githubusercontent.com/DingTalk-Real-AI/dingtalk-workspace-cli/main/scripts/install.ps1 | iex\"",
    docs: "https://github.com/DingTalk-Real-AI/dingtalk-workspace-cli",
}];

pub fn vendor(id: &str) -> Option<&'static Vendor> {
    VENDORS.iter().find(|v| v.id == id)
}

// ---------------------------------------------------------------------------
// 对前端的快照
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorRow {
    pub id: String,
    pub name: String,
    pub blurb: String,
    pub docs: String,
    /// 手动安装 CLI 的命令（界面上「装不上」时给人看的那一条）。
    pub install_hint: String,
    pub cli: CliState,
    pub skill: SkillState,
    pub auth: AuthState,
    /// 三层都齐了（CLI 装了、skill 装了、登录态有效）。界面上的绿点看它。
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CliState {
    pub name: String,
    /// 找到的可执行文件绝对路径。`None` = 没装（或装了但不在找得到的地方）。
    pub path: Option<String>,
    /// `dws version` 报的版本号。
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillState {
    pub name: String,
    /// 技能目录（`~/.carter/skills/dws`）。
    pub dir: String,
    pub installed: bool,
    /// 在 `skills.disabled/` 里：装了但被停用。技能页的开关能把它打开。
    pub disabled: bool,
    /// `SKILL.md` frontmatter 里写的 CLI 版本要求。只用来提示，不做版本比较。
    pub requires_cli: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AuthState {
    pub logged_in: bool,
    pub user_name: Option<String>,
    pub corp_name: Option<String>,
    pub corp_id: Option<String>,
    /// access token 过期时间（RFC3339）。刷新是 CLI 自己做的，这里只是给人看。
    pub expires_at: Option<String>,
    /// 没登录 / 有问题时的一句话。
    pub message: Option<String>,
    /// 常见原因的下一步提示。
    pub hint: Option<String>,
}

/// 一轮扫描：CLI / skill / 登录态。多个厂商并发跑，谁慢不拖累谁。
pub async fn list() -> Vec<ConnectorRow> {
    let rows = VENDORS.iter().map(row);
    futures::future::join_all(rows).await
}

async fn row(v: &'static Vendor) -> ConnectorRow {
    let cli_path = cli_path(v);
    let cli = match &cli_path {
        None => CliState {
            name: v.cli.to_string(),
            path: None,
            version: None,
        },
        Some(p) => CliState {
            name: v.cli.to_string(),
            path: Some(p.to_string_lossy().to_string()),
            version: cli_version(p).await,
        },
    };
    let skill = skill_state(v);
    let auth = match &cli_path {
        Some(p) => auth_state(p).await,
        None => AuthState {
            message: Some(format!("还没有安装 {} CLI", v.cli)),
            hint: Some("点「安装 CLI」，装完就能登录。".into()),
            ..Default::default()
        },
    };
    let ready = cli.path.is_some() && skill.installed && auth.logged_in;
    ConnectorRow {
        id: v.id.to_string(),
        name: v.name.to_string(),
        blurb: v.blurb.to_string(),
        docs: v.docs.to_string(),
        install_hint: install_command(v),
        cli,
        skill,
        auth,
        ready,
    }
}

// ---------------------------------------------------------------------------
// 三层状态的探测
// ---------------------------------------------------------------------------

/// CLI 在哪儿。
///
/// 先看 PATH，再看用户级 bin 目录。第二档不是多余的：从 Finder / 开始菜单启动的
/// GUI 进程拿不到 shell 启动文件里写的 PATH，而安装脚本恰恰把二进制放进
/// `~/.local/bin` —— 只查 PATH 的话，「装完重启应用」之后仍然显示「没装」。
pub fn cli_path(v: &Vendor) -> Option<PathBuf> {
    if let Some(p) = platform::which(v.cli) {
        return Some(p);
    }
    let exe = exe_name(v.cli);
    user_bin_dirs()
        .into_iter()
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// 用户级可执行目录（安装脚本的默认落点）。与 [`platform::ensure_user_paths`] 同源。
pub fn user_bin_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = crate::paths::home_dir() {
        v.push(home.join(".local").join("bin"));
    }
    v
}

async fn cli_version(cli: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(cli);
    cmd.args(["version", "-f", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    platform::hide_console_async(&mut cmd);
    let out = tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .ok()?
        .ok()?;
    let raw = String::from_utf8_lossy(&out.stdout);
    // 老版本不认 `-f json`，吐的是 `dws version v1.0.46 (...)`
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("version")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

/// 技能在哪儿、是不是生效的。
///
/// 不只认 `~/.carter/skills/<name>`：用户可能已经用 `dws skill setup` 把它装进了
/// `~/.claude/skills/dws`，再在技能页把那个目录加成扫描路径（那是并集里的一份，
/// 模型确实加载得到）。那种情况下报「还没装」是错的，用户会照着按钮再装一份，
/// 然后同一份技能在列表里出现两行、谁遮谁说不清。
///
/// 所以顺序是：本应用的技能目录（启用 → 停用）→ 用户配置里的外部扫描根。
/// 装到别处的那一份只显示「在哪」，不给「重装」暗示 —— 覆盖别人的目录不是本页的事。
fn skill_state(v: &Vendor) -> SkillState {
    let dir = carter::config::paths::skills_dir().join(v.skill);
    let disabled_dir = dir.with_file_name(format!("{}.disabled", v.skill));
    if dir.join("SKILL.md").is_file() {
        return skill_at(v, &dir, true, false);
    }
    if disabled_dir.join("SKILL.md").is_file() {
        return skill_at(v, &disabled_dir, false, true);
    }
    for root in AppSettings::load().skill_root_paths() {
        let p = root.join(v.skill);
        if p.join("SKILL.md").is_file() {
            return skill_at(v, &p, true, false);
        }
    }
    skill_at(v, &dir, false, false)
}

fn skill_at(v: &Vendor, dir: &Path, installed: bool, disabled: bool) -> SkillState {
    SkillState {
        name: v.skill.to_string(),
        dir: dir.to_string_lossy().to_string(),
        installed,
        disabled,
        requires_cli: if installed {
            frontmatter_value(&dir.join("SKILL.md"), "cli_version")
        } else {
            None
        },
    }
}

/// 从 `SKILL.md` 的 frontmatter 里读一个键。读不到就是 `None`（不是错误）。
fn frontmatter_value(path: &Path, key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let mut lines = raw.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let prefix = format!("{key}:");
    lines
        .take_while(|l| l.trim() != "---")
        .find_map(|l| l.strip_prefix(&prefix))
        .map(|v| v.trim().trim_matches('"').to_string())
}

/// `dws auth status` 的 JSON。字段名与官方实现一致（snake_case）。
#[derive(Debug, Default, Deserialize)]
struct RawStatus {
    #[serde(default)]
    authenticated: bool,
    #[serde(default)]
    token_valid: bool,
    #[serde(default)]
    refresh_token_valid: bool,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    corp_id: Option<String>,
    #[serde(default)]
    corp_name: Option<String>,
    #[serde(default)]
    user_name: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    hint: Option<String>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

async fn auth_state(cli: &Path) -> AuthState {
    let mut cmd = tokio::process::Command::new(cli);
    cmd.args(["auth", "status", "-f", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    platform::hide_console_async(&mut cmd);
    // 状态查询会顺手刷新过一次期的 access token（一次网络往返）。给足时间，
    // 但别让它无限期挂着 —— 设置页打开时整个列表都在等它。
    let out = match tokio::time::timeout(Duration::from_secs(25), cmd.output()).await {
        Err(_) => {
            return AuthState {
                message: Some("检查登录态超时".into()),
                hint: Some("网络不通，或钉钉服务端暂时没有响应，稍后再试。".into()),
                ..Default::default()
            };
        }
        Ok(Err(e)) => {
            return AuthState {
                message: Some(format!("跑不起来：{e}")),
                ..Default::default()
            };
        }
        Ok(Ok(o)) => o,
    };
    let raw = String::from_utf8_lossy(&out.stdout);
    let Ok(st) = serde_json::from_str::<RawStatus>(&raw) else {
        let err = String::from_utf8_lossy(&out.stderr);
        return AuthState {
            message: Some(format!(
                "看不懂 `{} auth status` 的输出",
                cli.file_name().unwrap_or_default().to_string_lossy()
            )),
            hint: first_line(&err).or_else(|| first_line(&raw)),
            ..Default::default()
        };
    };

    if !st.authenticated {
        // `error.message` 比 `message` 具体（"未登录" 之外还带着原因）。
        let msg = st
            .error
            .as_ref()
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .map(|s| s.to_string())
            .or(st.message);
        return AuthState {
            message: msg,
            hint: st.hint,
            ..Default::default()
        };
    }

    // 已登录，但 token 可能已经不能刷新了 —— 那时候「显示已登录、用起来全部报错」
    // 是最难查的状态，得在界面上提前说出来。
    let mut message = None;
    let mut hint = None;
    if !st.refresh_token_valid {
        message = Some("登录态快过期了（refresh token 无效）".into());
        hint = Some("重新登录一次即可。".into());
    } else if !st.token_valid {
        message = Some("access token 已过期，下次调用会自动刷新".into());
    }
    AuthState {
        logged_in: true,
        user_name: st.user_name,
        corp_name: st.corp_name,
        corp_id: st.corp_id,
        expires_at: st.expires_at,
        message,
        hint,
    }
}

fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

// ---------------------------------------------------------------------------
// 安装 CLI
// ---------------------------------------------------------------------------

pub fn install_command(v: &Vendor) -> String {
    if cfg!(windows) {
        v.install_windows.to_string()
    } else {
        v.install_unix.to_string()
    }
}

/// 跑官方安装脚本。输出实时推给界面（下载几十 MB，静默几分钟是最难受的）。
pub async fn install_cli(shell: &AppShell, id: &str) -> Result<(), String> {
    let Some(v) = vendor(id) else {
        return Err(format!("没有 `{id}` 这个连接器"));
    };
    let mut cmd = tokio::process::Command::new(install_shell_program());
    cmd.arg(install_shell_flag()).arg(install_command(v));
    let status = run_streamed(shell, v, cmd, Duration::from_secs(600), false).await?;
    if !status.success() {
        return Err(format!("安装失败（退出码 {:?}）", status.code()));
    }
    // 装好的目录可能不在本进程的 PATH 里（它是启动时补的），再补一次，
    // 这样装完立刻就能找到 —— 不需要让用户重启应用。
    platform::ensure_user_paths();
    // 让界面重拉 —— 卡片上的 CLI 一栏要当场变绿。
    shell.emit(AppEvent::ConnectorDone {
        vendor: v.id.to_string(),
        ok: true,
        message: Some("CLI 安装完成".into()),
    });
    Ok(())
}

fn install_shell_program() -> &'static str {
    #[cfg(windows)]
    {
        "powershell"
    }
    #[cfg(not(windows))]
    {
        "sh"
    }
}

fn install_shell_flag() -> &'static str {
    #[cfg(windows)]
    {
        "-Command"
    }
    #[cfg(not(windows))]
    {
        "-c"
    }
}

// ---------------------------------------------------------------------------
// 安装技能
// ---------------------------------------------------------------------------

/// 用临时 `HOME` 跑 `dws skill setup`，再把铺出来的那份搬进 `~/.carter/skills`。
///
/// 为什么要临时 `HOME`：`skill setup` 的目标只有它那张 Agent 名录，最小侵入的做法是
/// 让它在空目录里把 skill 展开（`--target agents` → `<tmp>/.agents/skills/dws`），
/// 然后自己搬到本应用的扫描根。整个过程中用户机器上别的 Agent 目录一个都不会被写。
pub async fn install_skill(shell: &AppShell, id: &str) -> Result<(), String> {
    let Some(v) = vendor(id) else {
        return Err(format!("没有 `{id}` 这个连接器"));
    };
    let cli = cli_path(v).ok_or_else(|| format!("还没装 {} CLI，先装 CLI", v.cli))?;
    let dst = carter::config::paths::skills_dir().join(v.skill);
    fetch_skill(shell, v, &cli, &dst).await?;

    shell.emit(AppEvent::ConnectorDone {
        vendor: v.id.to_string(),
        ok: true,
        message: Some("技能已装进 ~/.carter/skills（会话里立刻可用）".into()),
    });
    Ok(())
}

/// 装技能里**跟 UI 无关**的那一半：临时目录里跑 `skill setup`，再把产物搬进 `dst`。
///
/// 从 [`install_skill`] 里拆出来是为了能真跑一遍（`AppShell` 要一整套配置与会话表）。
async fn fetch_skill(
    shell: &AppShell,
    v: &Vendor,
    cli: &Path,
    dst: &Path,
) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!(
        "carter-connector-{}-{}-{}",
        v.id,
        std::process::id(),
        crate::protocol::now_ms()
    ));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("建临时目录失败：{e}"))?;

    let mut cmd = tokio::process::Command::new(cli);
    cmd.args(["skill", "setup", "--mode", "mono", "--target", "agents", "--yes"])
        // 临时 HOME 同时收住技能目录与 CLI 自己的状态目录（`~/.dws`），
        // 免得一次「装技能」动到用户真正的登录态与别的 Agent 目录。
        .env("HOME", &tmp)
        .env("USERPROFILE", &tmp)
        .env("DWS_CONFIG_DIR", tmp.join(".dws"));
    let status = run_streamed(shell, v, cmd, Duration::from_secs(180), false).await;

    let result = match status {
        Err(e) => Err(e),
        Ok(st) if !st.success() => Err(format!(
            "`{} skill setup` 失败（退出码 {:?}）",
            v.cli,
            st.code()
        )),
        Ok(_) => copy_skill_dir(&tmp.join(".agents").join("skills").join(v.skill), dst),
    };
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// 整目录替换。旧目录先挪开再删 —— 直接 `remove_dir_all` 再拷的话，中途失败
/// 会留下一个「装了但缺文件」的技能目录，比没装更难查。
fn copy_skill_dir(src: &Path, dst: &Path) -> Result<(), String> {
    if !src.join("SKILL.md").is_file() {
        return Err(format!("`skill setup` 没有产出技能：{} 里找不到 SKILL.md", src.display()));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建技能目录失败：{e}"))?;
    }
    let old = dst.with_extension("old");
    if old.exists() {
        std::fs::remove_dir_all(&old).map_err(|e| format!("清理旧备份失败：{e}"))?;
    }
    if dst.exists() {
        std::fs::rename(dst, &old).map_err(|e| format!("挪开旧技能失败：{e}"))?;
    }
    copy_tree(src, dst).inspect_err(|_| {
        // 拷贝失败就把旧的放回去，别让用户失去原本那一份。
        let _ = std::fs::remove_dir_all(dst);
        let _ = std::fs::rename(&old, dst);
    })?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("建目录失败：{e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("读 {} 失败：{e}", src.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        if ty.is_dir() {
            copy_tree(&from, &to)?;
        } else if ty.is_symlink() {
            // skill 包里没有符号链接，真有就按内容拷一份，别在别的机器上指空。
            let target = std::fs::canonicalize(&from).map_err(|e| e.to_string())?;
            if target.is_dir() {
                copy_tree(&target, &to)?;
            } else {
                std::fs::copy(&target, &to).map_err(|e| format!("复制失败：{e}"))?;
            }
        } else {
            std::fs::copy(&from, &to).map_err(|e| format!("复制失败：{e}"))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 登录 / 退出
// ---------------------------------------------------------------------------

/// 正在跑的登录进程（vendor → 进程组 id）。
///
/// 放在模块级而不是 `AppShell` 里：登录是**进程级**的事，一个进程同时只该有一个
/// 钉钉登录在听回调（不然两个 `dws` 会抢同一个 127.0.0.1 端口）。也正因为它是
/// 进程级的，模块级的表比挂在某个 shell 实例上更贴切。
///
/// 安装 / 退出这类短命令**不登记**：它们不该挡着登录，也不该被「取消登录」误杀。
static LOGINS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);

/// 用户点过「取消」的登录。`cancel_login` 只负责杀进程，这条标记负责把
/// 「被杀」和「自己失败」在返回时区分开 —— 前者要报「已取消」，后者要报真正的原因。
static LOGIN_CANCELLED: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

fn logins() -> std::sync::MutexGuard<'static, Option<HashMap<String, u32>>> {
    LOGINS.lock().unwrap_or_else(|e| e.into_inner())
}

fn cancelled_set() -> std::sync::MutexGuard<'static, Option<std::collections::HashSet<String>>> {
    LOGIN_CANCELLED.lock().unwrap_or_else(|e| e.into_inner())
}

/// 扫码登录。成功返回时 token 已经落盘，界面可以直接重拉状态。
///
/// `recommend` = 顺带授予服务端推荐权限（`--recommend`，官方给 Agent 用的一条路）。
/// 不勾的话第一次调某些工具会各自弹一次授权页 —— 所以默认勾上，但**让用户看见**这个勾。
pub async fn login(shell: &AppShell, id: &str, recommend: bool) -> Result<(), String> {
    let Some(v) = vendor(id) else {
        return Err(format!("没有 `{id}` 这个连接器"));
    };
    let cli = cli_path(v).ok_or_else(|| format!("还没装 {} CLI", v.cli))?;
    {
        let guard = logins();
        if guard.as_ref().is_some_and(|m| m.contains_key(v.id)) {
            return Err("已经在登录中，先取消上一次再试。".into());
        }
    }
    cancelled_set()
        .get_or_insert_with(Default::default)
        .remove(v.id);

    let mut cmd = tokio::process::Command::new(&cli);
    cmd.args(["auth", "login", "--no-browser", "--yes", "-f", "json"]);
    if recommend {
        cmd.arg("--recommend");
    }
    let status = run_streamed(shell, v, cmd, Duration::from_secs(15 * 60), true).await;

    logins().get_or_insert_with(HashMap::new).remove(v.id);
    let was_cancelled = cancelled_set()
        .as_mut()
        .map(|s| s.remove(v.id))
        .unwrap_or(false);
    let status = status?;
    if was_cancelled {
        return Err("已取消登录".into());
    }
    if !status.success() {
        // 组织没开 CLI 权限时 `dws` 的提示已经在日志里了，别用一句笼统的话盖掉它。
        return Err("登录没有完成，看上面对话框里的提示。".into());
    }
    shell.emit(AppEvent::ConnectorDone {
        vendor: v.id.to_string(),
        ok: true,
        message: Some("登录成功".into()),
    });
    Ok(())
}

/// 取消正在进行的登录。没在登录时什么也不做（幂等）。
pub fn cancel_login(id: &str) {
    let pid = logins().as_ref().and_then(|m| m.get(id).copied());
    let Some(pid) = pid else { return };
    cancelled_set().get_or_insert_with(Default::default).insert(id.to_string());
    crate::tools::background::kill_process_tree(pid);
}

/// 退出登录（清本机 token）。所有组织一起清 —— 界面上是一次「退出」，不该留半截。
pub async fn logout(shell: &AppShell, id: &str) -> Result<(), String> {
    let Some(v) = vendor(id) else {
        return Err(format!("没有 `{id}` 这个连接器"));
    };
    let cli = cli_path(v).ok_or_else(|| format!("还没装 {} CLI", v.cli))?;
    let mut cmd = tokio::process::Command::new(&cli);
    cmd.args(["auth", "logout", "--yes", "-f", "json"]);
    let status = run_streamed(shell, v, cmd, Duration::from_secs(60), false).await?;
    if !status.success() {
        return Err("退出登录失败".into());
    }
    shell.emit(AppEvent::ConnectorDone {
        vendor: v.id.to_string(),
        ok: true,
        message: Some("已退出登录".into()),
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// 跑一个会说话的进程
// ---------------------------------------------------------------------------

/// 起进程、把 stdout / stderr 逐行转成事件、等它退出。
///
/// 为什么要流式而不是 `output()`：安装要下载几十 MB、登录要等人扫码，中间几分钟
/// 一句话都不说的话，用户只会以为按钮没反应。每一行都推给界面，弹窗里那个日志框
/// 就同时是「正在干活」的证据和出问题时的诊断材料。
///
/// `register_login` = 这次是登录：进程登记进 [`LOGINS`]，可以被
/// [`cancel_login`] 找到并杀掉。安装 / 退出不登记（没人会去取消它们，也不该
/// 被「取消登录」误杀）。
async fn run_streamed(
    shell: &AppShell,
    v: &Vendor,
    mut cmd: tokio::process::Command,
    timeout: Duration,
    register_login: bool,
) -> Result<std::process::ExitStatus, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    platform::hide_console_async(&mut cmd);
    // 独立进程组：取消时能整组杀干净（`dws` 起的子进程也不会漏）。
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("启动 {} 失败：{e}", v.cli))?;
    if register_login {
        if let Some(pid) = child.id() {
            logins()
                .get_or_insert_with(HashMap::new)
                .insert(v.id.to_string(), pid);
        }
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    pump(child.stdout.take(), tx.clone());
    pump(child.stderr.take(), tx);

    let mut seen_url = false;
    let mut status: Option<std::process::ExitStatus> = None;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    // 先把输出读完。两个管都关掉之后（`rx` 返回 `None`）就不再 select —— 否则
    // 已关闭的 receiver 会立刻返回 `None`，转成一个忙等循环。
    loop {
        tokio::select! {
            line = rx.recv() => {
                let Some(line) = line else { break };
                if let Some(url) = auth_url(&line) {
                    if !seen_url {
                        seen_url = true;
                        shell.emit(AppEvent::ConnectorAuthUrl {
                            vendor: v.id.to_string(),
                            url,
                        });
                    }
                }
                shell.emit(AppEvent::ConnectorOutput {
                    vendor: v.id.to_string(),
                    line,
                });
            }
            // 进程退了之后不再重复轮询它：`wait` 对已回收的子进程会立刻返回，
            // 配合一个还没关的输出管就转成忙等。输出读完那一路会 break。
            st = child.wait(), if status.is_none() => {
                status = Some(st.map_err(|e| format!("等待 {} 退出失败：{e}", v.cli))?);
            }
            _ = &mut deadline => {
                if let Some(pid) = child.id() {
                    crate::tools::background::kill_process_tree(pid);
                }
                return Err(format!("超时（{} 秒）", timeout.as_secs()));
            }
        }
    }

    // 输出已经收干，剩下就是等它退出（正常情况下已经退了）。
    let status = match status {
        Some(s) => s,
        None => tokio::select! {
            st = child.wait() => st.map_err(|e| format!("等待 {} 退出失败：{e}", v.cli))?,
            _ = &mut deadline => {
                if let Some(pid) = child.id() {
                    crate::tools::background::kill_process_tree(pid);
                }
                return Err(format!("超时（{} 秒）", timeout.as_secs()));
            }
        },
    };
    Ok(status)
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(
    r: Option<R>,
    tx: mpsc::UnboundedSender<String>,
) {
    let Some(r) = r else { return };
    tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
}

/// 从一行输出里认出授权页地址。
///
/// `dws` 的提示是「如果浏览器未自动打开，请手动访问:\n  https://login.dingtalk.com/...」，
/// 地址单独一行。按「整行就是一个 http(s) 地址」认，比猜措辞稳。
fn auth_url(line: &str) -> Option<String> {
    let t = line.trim();
    if (t.starts_with("https://") || t.starts_with("http://")) && !t.contains(char::is_whitespace) {
        Some(t.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_authorization_url_is_recognized_on_its_own_line() {
        assert_eq!(
            auth_url("  https://login.dingtalk.com/oauth2/auth?client_id=x&state=y"),
            Some("https://login.dingtalk.com/oauth2/auth?client_id=x&state=y".into())
        );
        // 前后带着说明文字的行不是地址。
        assert_eq!(auth_url("请访问 https://example.com/cb 完成授权"), None);
        assert_eq!(auth_url("⏳ 等待授权中..."), None);
        assert_eq!(auth_url(""), None);
    }

    #[test]
    fn the_frontmatter_reader_only_looks_inside_the_header() {
        let dir = std::env::temp_dir().join(format!("carter-fm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("SKILL.md");
        std::fs::write(
            &p,
            "---\nname: dws\ndescription: x\ncli_version: \">=1.0.15\"\n---\n\ncli_version: not this one\n",
        )
        .unwrap();
        assert_eq!(frontmatter_value(&p, "cli_version"), Some(">=1.0.15".into()));
        assert_eq!(frontmatter_value(&p, "missing"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_skill_directory_is_copied_whole() {
        let root = std::env::temp_dir().join(format!("carter-copy-{}", std::process::id()));
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(src.join("references")).unwrap();
        std::fs::write(src.join("SKILL.md"), "---\nname: dws\n---\n").unwrap();
        std::fs::write(src.join("references").join("a.md"), "x").unwrap();
        copy_skill_dir(&src, &dst).unwrap();
        assert!(dst.join("SKILL.md").is_file());
        assert!(dst.join("references").join("a.md").is_file());
        // 再装一遍：整目录替换，旧的多余文件不该留着。
        std::fs::write(src.join("references").join("b.md"), "y").unwrap();
        copy_skill_dir(&src, &dst).unwrap();
        assert!(dst.join("references").join("b.md").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_skill_source_is_an_error_and_leaves_the_old_one_alone() {
        let root = std::env::temp_dir().join(format!("carter-copy-miss-{}", std::process::id()));
        let src = root.join("src");
        let dst = root.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("SKILL.md"), "old").unwrap();
        assert!(copy_skill_dir(&src, &dst).is_err());
        assert_eq!(std::fs::read_to_string(dst.join("SKILL.md")).unwrap(), "old");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_vendor_has_the_fields_the_page_needs() {
        for v in VENDORS {
            assert!(!v.id.is_empty());
            assert!(!v.name.is_empty());
            assert!(!v.cli.is_empty());
            assert!(!v.skill.is_empty());
            assert!(v.install_unix.contains(v.cli) || v.install_unix.contains("install"));
            assert!(v.install_windows.contains("install"));
        }
    }
}

#[cfg(test)]
mod skill_fetch_against_the_real_cli {
    use super::*;

    /// 真跑一遍「临时 HOME → skill setup → 搬进技能目录」。
    ///
    /// 这一段是本模块最容易出错的地方（临时 HOME、`--target agents` 的落点、整目录
    /// 替换），而且只有装上真的 CLI 才验证得了。所以标 `#[ignore]`：默认不进 CI，
    /// 手动 `cargo test -p carter-app-core -- --ignored skill_fetch` 跑。
    #[tokio::test]
    #[ignore]
    async fn it_installs_the_skill_into_the_target_directory() {
        let v = vendor("dingtalk").unwrap();
        let cli = cli_path(v).expect("本机没装 dws");
        let shell = AppShell::new(carter::config::Config::load().unwrap());
        let dst = std::env::temp_dir().join(format!(
            "carter-skill-target-{}-{}",
            std::process::id(),
            crate::protocol::now_ms()
        ));
        fetch_skill(&shell, v, &cli, &dst).await.expect("装技能失败");
        assert!(dst.join("SKILL.md").is_file());
        // 技能包自带的参考资料也要在 —— 只有 SKILL.md 是装了一半。
        assert!(dst.join("references").is_dir());
        let _ = std::fs::remove_dir_all(&dst);
    }
}
