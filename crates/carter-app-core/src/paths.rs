//! 应用私有路径。
//!
//! 方案 D8 的数据分家：`~/.carter/` 与 CLI 共享（config / skills / sessions），
//! `~/.carter-app/` 只放本应用独有的东西（原始报文、界面设置）。
//! 这个区分是有意的 —— 放进 `~/.carter/` 的每一项都等于对外承诺「CLI 也认」。

use std::path::{Path, PathBuf};

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
    home_dir()
        .map(|h| h.join(".carter-app"))
        .unwrap_or_else(|| PathBuf::from(".carter-app"))
}

/// 用户 home 目录：Windows `%USERPROFILE%`，其它 `$HOME`。都没有 → `None`。
///
/// 返回 `Option` 而不是空串或 `.`：调用方有的是「拼一个默认子目录」（回落即可），
/// 有的是「给用户一个预设路径」（没有家目录时**应该什么都不给**，而不是给一个
/// 指向当前目录的假路径）。
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    // 另一侧也认一下：Windows 上偶尔只有 HOME（MSYS 环境），unix 上偶尔只有
    // USERPROFILE（交叉编译的容器里）。
    std::env::var_os(key)
        .or_else(|| std::env::var_os(if key == "HOME" { "USERPROFILE" } else { "HOME" }))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// 展开开头的 `~`。只认 `~` 与 `~/…`、`~\…`，不做用户名展开（`~bob` 原样留着）。
///
/// 顺带把 `/` 统一成当前平台的分隔符：用户在 Windows 上手打 `~/.claude/skills`
/// 是最自然的写法，但混排分隔符显示出来很难看。
pub(crate) fn expand_tilde(raw: &str) -> String {
    let rest = match raw.strip_prefix('~') {
        Some("") => {
            return home_dir()
                .map(|h| h.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        Some(r) if r.starts_with('/') || r.starts_with('\\') => r,
        _ => return raw.to_string(),
    };
    let Some(home) = home_dir() else {
        return raw.to_string();
    };
    let home = home.to_string_lossy();
    format!(
        "{}{}",
        home.trim_end_matches(['/', '\\']),
        rest.replace('/', std::path::MAIN_SEPARATOR_STR)
    )
}

/// 把一个用户手填的**扫描根**规范成绝对路径。
///
/// 用在技能页「添加扫描路径」这类地方。与文件工具的 `cwd::resolve` 的差别只有那个
/// `cwd`，而这不是小事：扫描根是**跨会话**的全局设置，没有「当前会话的工作目录」
/// 可以依凭。拿会话 cwd 去拼一个全局设置，路径会随会话变 —— 那是「同一台机器上
/// 换个会话技能就不见了」这类最难查的错。所以这里只认绝对路径（含 `~` 展开后的
/// 绝对路径），相对路径**明确拒绝**，而不是替用户猜一个基准目录。
///
/// 返回的路径已去掉尾部分隔符：`~/.claude/skills/` 与 `~/.claude/skills` 是同一个
/// 目录，但字符串不等，不去掉的话界面上的去重会漏、同一个目录能加两次。
pub fn normalize_root(raw: &str) -> Result<PathBuf, String> {
    let raw = strip_wrapping_quotes(raw.trim()).trim();
    if raw.is_empty() {
        return Err("路径不能为空".into());
    }
    let expanded = expand_tilde(raw);
    let p = Path::new(&expanded);
    if !p.is_absolute() {
        return Err(format!(
            "`{raw}` 不是绝对路径。扫描根是跨会话的全局设置，没有「当前目录」可以依凭，\
             所以必须写绝对路径（可以用 `~` 开头，如 `~/.claude/skills`）"
        ));
    }
    let s = p.to_string_lossy();
    // 全是分隔符的根路径（`/`）去掉尾巴就成空串了，这时保留原样。
    let cleaned = s.trim_end_matches(['/', '\\']);
    let cleaned = if cleaned.is_empty() { s.as_ref() } else { cleaned };
    // Windows 上统一成 `\`：同一个目录的两种写法（`C:/a/b` 与 `C:\a\b`）否则会
    // 在列表里各出现一行，去重也认不出来。
    #[cfg(windows)]
    {
        Ok(PathBuf::from(cleaned.replace('/', "\\")))
    }
    #[cfg(not(windows))]
    {
        Ok(PathBuf::from(cleaned))
    }
}

/// 两个路径是不是同一个目录。
///
/// Windows 的文件系统大小写不敏感，`C:\Users\x\.claude\skills` 与
/// `C:\users\x\.claude\skills` 是同一个目录 —— 按字符串精确比较会让同一个目录
/// 被加两次，而「重复」在界面上与「优先顺序」是两种含义，混起来就没法解释了。
pub fn same_path(a: &str, b: &str) -> bool {
    #[cfg(windows)]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// 剥掉**成对包住整个字符串**的引号。
///
/// Windows 资源管理器的「复制文件地址」给的就是 `"C:\Users\…"` 这种带引号的文本，
/// 用户粘进来时会连着引号一起贴。只剥成对的：路径本身以引号开头又结尾（unix 上
/// 可能）时按原样留着，比无脑 `trim_matches('"')` 安全。
///
/// 技能的 frontmatter 也用同一个规则（`name: "imagegen"` 里那对引号是 YAML 语法，
/// 不是名字的一部分），所以这里的可见性是 `pub(crate)`。
pub(crate) fn strip_wrapping_quotes(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        &s[1..s.len() - 1]
    } else {
        s
    }
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

/// 「这一轮实际改了哪些文件」的存放目录：一个会话一个文件。
///
/// 与 `goals/` 同形同理由。这份数据只服务界面（回答末尾的清单），不参与会话
/// 恢复 —— 会话文件仍是 carter 的 JSONL，我们不改它的记录格式（见 `goal.rs`
/// 模块头：多一种 `RecordKind` 会让整条会话读不出来）。
pub fn changes_dir() -> PathBuf {
    app_home().join("changes")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `~` 开头的扫描根要展开成家目录，并且**丢掉尾部分隔符** ——
    /// 否则 `~/.claude/skills/` 与 `~/.claude/skills` 会是两个不同的字符串，
    /// 同一个目录能被加两次、去重也认不出来。
    #[test]
    fn a_tilde_root_expands_and_loses_its_trailing_separator() {
        let Some(home) = home_dir() else {
            // 没有 USERPROFILE / HOME 的环境（几乎只有容器里才见得到）。
            return;
        };
        let expected = home.join(".claude").join("skills");
        assert_eq!(normalize_root("~/.claude/skills").unwrap(), expected);
        assert_eq!(normalize_root("~/.claude/skills/").unwrap(), expected);
    }

    /// 从资源管理器「复制文件地址」粘过来的路径带引号，要剥掉。
    /// 用临时目录构造（一定是绝对路径），这样测试与平台无关。
    #[test]
    fn quotes_from_copy_as_path_are_stripped() {
        let dir = std::env::temp_dir();
        let plain = dir.to_string_lossy().to_string();
        let quoted = format!("\"{}\"", dir.display());
        assert_eq!(
            normalize_root(&quoted).unwrap(),
            normalize_root(&plain).unwrap(),
            "带引号与不带引号必须规范成同一个路径"
        );
    }

    /// 相对路径拒绝，不替用户猜基准目录 —— 猜出来的路径会随会话变。
    #[test]
    fn relative_and_empty_roots_are_refused() {
        for bad in [".claude/skills", "skills", "~bob/skills", "", "   "] {
            assert!(
                normalize_root(bad).is_err(),
                "`{bad}` 应当被拒绝（不是绝对路径或为空）"
            );
        }
    }

    /// Windows 上大小写不敏感。这条在 unix 上退化成一个恒真断言（路径本来就区分
    /// 大小写），留着是为了让平台差异**写在代码里**而不是留在某个人的脑子里。
    #[test]
    fn same_path_follows_the_platforms_case_rules() {
        assert!(same_path("/a/b", "/a/b"));
        assert!(!same_path("/a/b", "/a/c"));
        #[cfg(windows)]
        assert!(same_path(r"C:\Users\x\.claude", r"c:\users\x\.claude"));
        #[cfg(not(windows))]
        assert!(!same_path("/A/b", "/a/b"));
    }
}
