//! 「这一轮实际改了哪些文件、改了哪些片段」—— 靠**观察文件系统**，不靠解析工具参数。
//!
//! ## 为什么不是解析命令
//!
//! 模型改文件的路子不止 `write_file` / `edit_file`：`printf … >> f`、`cat > f <<EOF`、
//! `sed -i`、`python -c`、`node -e`、MCP 工具……一长串。解析 shell 命令只能覆盖
//! 其中一部分，而且每加一种写法就要加一条规则，猜错的代价还是把 `/dev/null`
//! 或变量名当成文件列给用户。
//!
//! 这里换成另一条路：每一轮开始前对会话的 cwd 挂一个文件系统监听，轮末把这一轮
//! 真正发生变更的路径收回来。不管是谁写的、用什么写的，只要落在 cwd 下面就能看见。
//!
//! ## 忽略规则：三层
//!
//! 1. **内置目录**（[`IGNORED_DIRS`]）：`.git`、`node_modules`、`target` 这类
//!    几乎总是构建产物的目录。固定规则，不需要配置。
//! 2. **`.gitignore`**：cwd 在 git 仓库里时，轮末按 `.gitignore` 过一遍 ——
//!    `.gitignore` / `.git/info/exclude` / 全局 excludes 一并生效，连嵌套的
//!    `.gitignore` 也算在内。装了 git 就交给 `git check-ignore`，省得和 git 对不上；
//!    没装 git 则退回进程内的 `ignore` crate（见 [`git_ignored_in_process`]）。
//! 3. **用户额外规则**（设置里的 `changes_ignore`）：每条一个 glob，相对 cwd。
//!
//! ## 「改了哪些片段」：按需用 git 算
//!
//! 监听只知道「变了」，不知道「之前是什么」。要给出 diff 就必须有轮首基线：
//!
//! * cwd 在 git 仓库里时，轮首用 `git stash create` 给**当前工作区**拍一张快照
//!   （它只写对象、不动工作区，也不进 stash 列表），没有本地改动时退回 HEAD。
//!   轮末用 `git diff <基线> -- <path>` 就能得到这一轮真正改了什么，包括那些
//!   轮首本来就脏的文件。
//! * 不在 git 仓库里时没有基线，只能列文件、看当前内容 —— 这不是偷懒：没有基线
//!   就无法还原「改动前」，而为一个普通目录做全量内容快照的代价远大于收益。
//!
//! diff 在用户点开预览时才计算（见 [`diff_for`]），事件与落盘只带路径与基线 SHA。
//!
//! ## 边界
//!
//! * 只覆盖**会话 cwd 之下**。写到 `/tmp`、cwd 之外的路径看不见 —— 那部分由前端
//!   从成功的 `write_file` / `edit_file` 参数补上。
//! * 删掉的文件也算：它确实变了。改名会同时出现旧路径与新路径。
//!
//! ## 为什么按轮存盘
//!
//! 事件是转瞬即逝的，而历史回看要能重新画出同一张清单。会话文件是 carter 的
//! JSONL，**不能**往里加自己的记录类型（`RecordKind` 是封闭枚举，多一种会让整条
//! 会话读不出来）。所以按 `goal.rs` 的形状落在 `~/.carter-app/changes/<id>.json`。

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use notify::{EventKind, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

use crate::paths;

/// 收事件前等一小会儿。
///
/// 工具进程退出与文件系统事件送达是两条独立的路径：命令返回时，最后几次
/// `Modify` 可能还在内核/监听线程里。等这一下（体感上察觉不到）比漏掉最后一个
/// 写入好 —— 那往往正是 `printf … >> f` 里真正的那次写。
const SETTLE: Duration = Duration::from_millis(150);

/// 一次 diff 的上限。超过就截断并明确标注 —— 生成出来的大文件不该把 IPC 撑爆。
const MAX_DIFF: usize = 512 * 1024;

/// git 的空树对象。空仓库（还没有 HEAD）拿它当基线，于是所有文件都是「新增」。
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// 不看的目录名。构建产物、依赖、版本库内部文件与缓存不是「用户关心的改动」。
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cache",
    // 会话文件与报文抓包的落点。cwd 是家目录时它们每次对话都在变，
    // 不排除的话清单里会混进 `sessions/xxx.jsonl` 和 captures。
    ".carter",
    ".carter-app",
    // macOS 的家目录里最大的一坨非源码目录（缓存、容器、应用数据）。
    "Library",
];

const IGNORED_FILES: &[&str] = &[".DS_Store"];

/// 内置忽略的目录名。设置页要把这份名单原样展示出来 —— 用户不该靠翻源码
/// 才知道为什么某个文件没出现。**这里是唯一来源**，设置页不许再抄一份。
pub fn builtin_ignored_dirs() -> &'static [&'static str] {
    IGNORED_DIRS
}

/// 内置忽略的文件名（同 [`builtin_ignored_dirs`]）。
pub fn builtin_ignored_files() -> &'static [&'static str] {
    IGNORED_FILES
}

fn ignored_name(name: &str) -> bool {
    IGNORED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
        || IGNORED_FILES.iter().any(|f| f.eq_ignore_ascii_case(name))
}

// ---------------------------------------------------------------------------
// 用户额外忽略规则
// ---------------------------------------------------------------------------

/// 设置页里写的那组 glob。
///
/// 含 `/` 的按相对路径匹配（`dist/**`）；不含 `/` 的按**任意层级的名字**匹配
/// （`*.log`、`__generated__`）—— 后者是用户最自然的写法，不该逼他补 `**/`。
#[derive(Debug, Default)]
pub struct IgnoreRules {
    paths: GlobSet,
    names: GlobSet,
}

impl IgnoreRules {
    pub fn new(patterns: &[String]) -> Self {
        let mut paths = GlobSetBuilder::new();
        let mut names = GlobSetBuilder::new();
        for raw in patterns {
            let p = raw.trim();
            if p.is_empty() || p.starts_with('#') {
                continue;
            }
            // `dist/` 与 `dist` 是同一个意思：目录名本身。
            let p = p.trim_end_matches('/');
            if p.is_empty() {
                continue;
            }
            // `literal_separator`：与 .gitignore 一样，`*` 不跨 `/`，跨层级要写 `**`。
            let Ok(glob) = GlobBuilder::new(p).literal_separator(true).build() else {
                tracing::warn!("忽略规则 `{raw}` 不是合法 glob，已跳过");
                continue;
            };
            if p.contains('/') {
                paths.add(glob);
            } else {
                names.add(glob);
            }
        }
        Self {
            paths: paths.build().unwrap_or_default(),
            names: names.build().unwrap_or_default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.names.is_empty()
    }

    /// `rel` 是相对 cwd 的 `/` 分隔路径。
    pub fn matches(&self, rel: &str) -> bool {
        if self.paths.is_match(rel) {
            return true;
        }
        if self.names.is_empty() {
            return false;
        }
        rel.split('/').any(|part| self.names.is_match(part))
    }
}

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// 事件路径 → cwd 下的相对路径；不关心的返回 `None`。
///
/// 目录本身不记：新建一个目录会产生一条目录事件，随后里面每个文件还会各来一条，
/// 把目录也列出来只是重复。
///
/// `roots` 是同一个根的几种写法（见 [`root_variants`]）—— 事件路径用哪一种都得认。
/// 比较走字符串并统一成 `/`，因为 Windows 上事件路径与 `canonicalize` 的结果可能
/// 一个带 `\\?\` 前缀、一个不带。
fn relative(roots: &[PathBuf], path: &Path) -> Option<PathBuf> {
    let p = path.to_string_lossy().replace('\\', "/");
    for root in roots {
        let root = root.to_string_lossy().replace('\\', "/");
        let root = root.trim_end_matches('/');
        if root.is_empty() {
            continue;
        }
        let Some(head) = p.get(..root.len()) else {
            continue;
        };
        let hit = if cfg!(windows) {
            head.eq_ignore_ascii_case(root)
        } else {
            head == root
        };
        if !hit {
            continue;
        }
        let rest = &p[root.len()..];
        if !rest.is_empty() && !rest.starts_with('/') {
            continue; // `/work/proj2` 不是 `/work/proj` 的子路径
        }
        let rel = rest.trim_start_matches('/');
        if rel.is_empty() {
            return None;
        }
        if rel.split('/').any(ignored_name) {
            return None;
        }
        // `Remove` 之后路径已经不存在，`is_dir()` 为 false —— 被删掉的目录会当成
        // 文件记一条。这种误报很少（模型删文件比删目录多），比反向漏报可控。
        if path.is_dir() {
            return None;
        }
        return Some(PathBuf::from(rel));
    }
    None
}

/// 一个根目录在事件里可能出现的几种写法。
///
/// * macOS 的 FSEvents 回报**规范化后**的路径（`/tmp` 是 `/private/tmp` 的符号
///   链接），所以要把 canonical 形式也放进来；
/// * Windows 的 `canonicalize` 会加 `\\?\` 前缀，而事件不带 —— 两种都试。
fn root_variants(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    if let Ok(c) = root.canonicalize() {
        if !out.contains(&c) {
            out.push(c.clone());
        }
        #[cfg(windows)]
        if let Some(s) = c.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
            let v = PathBuf::from(s);
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

/// 本机有没有可用的 `git`。
///
/// 每轮开头要跑 2~3 条 git，轮末还要一条。没装 git 时这些调用不是「慢」，而是
/// 每次都白起一个必然失败的进程（`CreateProcessW` 找不到文件就返回）。探一次
/// 结果缓存住，之后直接短路 —— 桌面应用里 `git` 有没有装不会中途变。
///
/// 只看 PATH 与 PATHEXT，不试执行：执行一次的代价远大于这个判断，而且探到的
/// 路径可用与否在真起进程时还会再报一次错。
fn git_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| crate::platform::which("git").is_some())
}

/// `cwd` 在不在一个 git 仓库里（含子目录）。
///
/// 纯文件系统判断，不起进程。它挡在 [`git_baseline`] 前面有两个好处：
/// 非仓库目录（家目录是最常见的 cwd）每轮省掉一条必然失败的 `git rev-parse`；
/// `finish` 里也靠它决定要不要按 `.gitignore` 过滤。
///
/// `.git` 可能是目录（普通仓库）也可能是文件（worktree / submodule），所以用
/// `exists()` 而不是 `is_dir()`。
fn in_git_repo(cwd: &Path) -> bool {
    cwd.ancestors().any(|d| d.join(".git").exists())
}

/// 跑一条 git 命令，成功则返回 stdout（去掉尾部空白）。
fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    if !git_available() {
        return None;
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    // 轮首这几条 git 是**发消息那一刻**跑的：装好的 release 包没有控制台，
    // 不带上这个 flag 就会连闪几个黑框。
    #[cfg(windows)]
    crate::platform::hide_console(&mut cmd);
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 轮首基线：当前工作区的一个提交 SHA；不是 git 仓库 / git 不可用时为 `None`。
///
/// `git stash create` 会**写对象**（进 `.git/objects`，之后由 gc 回收），但不动
/// 工作区、不动索引、不进 `stash list`。这是拿到「当前工作区快照」而不打扰用户
/// 的最轻方式；自己把每个脏文件复制一份反而是更重的活。
fn git_baseline(cwd: &Path) -> Option<String> {
    // 不是仓库就别起进程了 —— 这条判断对「家目录当 cwd」这种最常见的用法
    // 每轮都省一次 `git rev-parse`。
    if !in_git_repo(cwd) || !git_available() {
        return None;
    }
    if git(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return None;
    }
    if let Some(sha) = git(cwd, &["stash", "create"]).filter(|s| !s.is_empty()) {
        return Some(sha);
    }
    // 工作区干净（或 stash create 不支持这种仓库）：基线就是 HEAD。
    match git(cwd, &["rev-parse", "--verify", "HEAD"]) {
        Some(head) => Some(head),
        // 空仓库：还没有 HEAD。拿空树当基线，所有文件都算新增。
        None => Some(EMPTY_TREE.to_string()),
    }
}

/// 一批路径里哪些被 `.gitignore`（含嵌套 / 全局 excludes）忽略。
///
/// 一次进程问完，而不是每个文件问一次 —— 一次轮末多 20ms 可以接受，几百次
/// 进程启动不行。
fn git_ignored(cwd: &Path, files: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    if files.is_empty() {
        return out;
    }
    // 没装 git 时用进程内的 `ignore` crate 顶上 —— 语义（嵌套 .gitignore、
    // 取反规则、全局 excludes）由它保证，与 grep 工具用的是同一套。
    if !git_available() {
        return git_ignored_in_process(cwd, files);
    }
    let mut input = Vec::new();
    for f in files {
        input.extend_from_slice(f.as_bytes());
        input.push(0);
    }

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    crate::platform::hide_console(&mut cmd);
    let Ok(mut child) = cmd.spawn() else {
        return out;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&input);
    }
    let Ok(result) = child.wait_with_output() else {
        return out;
    };
    // 退出码 1 = 一个都没被忽略，不是错误。
    for raw in result.stdout.split(|b| *b == 0) {
        if !raw.is_empty() {
            out.insert(String::from_utf8_lossy(raw).into_owned());
        }
    }
    out
}

/// 没装 git 时的 `.gitignore` 过滤：用进程内的 `ignore` crate 自己算。
///
/// 与 grep 工具用的是同一套引擎，所以嵌套 `.gitignore`、`!` 取反、目录规则的
/// 语义与 rg 一致。全局 excludes（`core.excludesFile` / `~/.config/git/ignore`）
/// 也照 git 的查找顺序找一遍。
///
/// 与 `git check-ignore` 的差别只有一处已知的：某个目录被排除后，git 不允许
/// 用更深的 `.gitignore` 把里面的文件重新包含回来，这里与 `ignore` crate 的
/// `matched_path_or_any_parents` 行为一致，会允许。变更清单只是给用户看的提示，
/// 这一点偏差比「没装 git 就完全不过滤」小得多。
fn git_ignored_in_process(cwd: &Path, files: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    // 每个目录的 `.gitignore` 只解析一次：一次轮末常常有几十个文件落在同一批目录下。
    let mut cache: HashMap<PathBuf, Option<ignore::gitignore::Gitignore>> = HashMap::new();
    let global = global_gitignore(cwd);

    for f in files {
        let abs = cwd.join(f);
        let mut ignored = false;
        // 由深到浅：越靠近文件的 `.gitignore` 优先级越高，第一个给出结论的说了算。
        let mut dir = abs.parent();
        while let Some(d) = dir {
            if !d.starts_with(cwd) {
                break;
            }
            let matcher = cache.entry(d.to_path_buf()).or_insert_with(|| {
                let path = d.join(".gitignore");
                path.is_file()
                    .then(|| ignore::gitignore::Gitignore::new(&path).0)
            });
            if let Some(m) = matcher {
                match m.matched_path_or_any_parents(&abs, false) {
                    ignore::Match::Ignore(_) => {
                        ignored = true;
                        break;
                    }
                    // 被更深层的 `!` 规则取反了：就地结束，不再往上问。
                    ignore::Match::Whitelist(_) => break,
                    ignore::Match::None => {}
                }
            }
            if d == cwd {
                break;
            }
            dir = d.parent();
        }

        // 全局 excludes 优先级最低，前面都没结论时才算数。
        if !ignored && let Some(g) = &global {
            ignored = g.matched_path_or_any_parents(&abs, false).is_ignore();
        }
        if ignored {
            out.insert(f.clone());
        }
    }
    out
}

/// 全局 gitignore（`core.excludesFile`，默认 `~/.config/git/ignore`），以 `cwd`
/// 为根构建。
///
/// 必须按 `cwd` 建：`ignore` 的 matcher 是按「相对根」匹配的，拿
/// `Gitignore::new(排除文件)` 得到的 matcher 根本在别的目录（`~/.config/git`），
/// 与要匹配的路径没有公共前缀时 `matched_path_or_any_parents` 会 **panic**。
fn global_gitignore(cwd: &Path) -> Option<ignore::gitignore::Gitignore> {
    let path = ignore::gitignore::gitconfig_excludes_path()?;
    if !path.is_file() {
        return None;
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(cwd);
    builder.add(&path);
    builder.build().ok()
}

// ---------------------------------------------------------------------------
// 一轮的变更收集器
// ---------------------------------------------------------------------------

/// 一轮监听收工后的结果。
pub struct FinishedChanges {
    pub files: Vec<String>,
    /// 轮首的 git 基线（见 [`git_baseline`]）。
    pub baseline: Option<String>,
}

/// 一轮的变更收集器。建的时候开始听，[`ChangeWatch::finish`] 收尾。
pub struct ChangeWatch {
    seen: Arc<Mutex<BTreeSet<PathBuf>>>,
    cwd: PathBuf,
    rules: IgnoreRules,
    baseline: Option<String>,
    /// 必须活着，drop 掉就停止监听了。
    _watcher: notify::RecommendedWatcher,
}

impl ChangeWatch {
    /// 对 `root` 建递归监听。建不起来（权限、路径没了、平台不支持）就返回 `None` ——
    /// 「列不出文件」不该让这一轮跑不下去。
    pub fn start(root: &Path, ignore: &[String]) -> Option<Self> {
        let seen = Arc::new(Mutex::new(BTreeSet::new()));
        let sink = seen.clone();
        let event_roots = root_variants(root);

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            // 只关心「内容/名字变了」。Access 是读、Any 是平台认不出的批量事件，
            // 都不该出现在变更清单里。
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
                _ => return,
            }
            let mut set = match sink.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            for p in event.paths {
                if let Some(rel) = relative(&event_roots, &p) {
                    set.insert(rel);
                }
            }
        })
        .ok()?;

        if watcher.watch(root, RecursiveMode::Recursive).is_err() {
            return None;
        }
        Some(Self {
            seen,
            cwd: root.to_path_buf(),
            rules: IgnoreRules::new(ignore),
            baseline: git_baseline(root),
            _watcher: watcher,
        })
    }

    /// 收工：等一下还在路上的事件，再按用户规则与 `.gitignore` 过滤。
    ///
    /// 返回的是**相对 cwd** 的路径，统一用 `/` 分隔 —— 前端要拿它跟
    /// `write_file` 参数里的路径对齐，两种分隔符混着会对不上。
    pub async fn finish(self) -> FinishedChanges {
        tokio::time::sleep(SETTLE).await;
        let set = match self.seen.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let mut files: Vec<String> = set
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect();

        if !self.rules.is_empty() {
            files.retain(|p| !self.rules.matches(p));
        }
        // `.gitignore` 只在 git 仓库里算。轮末一次算完，比每个事件问一次便宜。
        // 装了 git 就交给 `git check-ignore`（语义最全）；没装则退回进程内的
        // `ignore` crate —— 否则「没装 git」会把 `*.log`、`dist/` 全塞进清单。
        if !files.is_empty() && in_git_repo(&self.cwd) {
            let ignored = git_ignored(&self.cwd, &files);
            files.retain(|p| !ignored.contains(p));
        }

        FinishedChanges {
            files,
            baseline: self.baseline,
        }
    }
}

// ---------------------------------------------------------------------------
// diff（按需计算）
// ---------------------------------------------------------------------------

/// 点开某个文件时「这一轮改了什么」。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileDiff {
    /// 可显示的 unified diff。
    Diff {
        path: String,
        text: String,
        /// 超过 [`MAX_DIFF`]，`text` 只是开头一段。
        truncated: bool,
    },
    /// 与轮首基线一致（改完又改回去了）。
    Empty { path: String },
    /// 做不出 diff：不是 git 仓库、没有基线、二进制、文件不存在……
    Unavailable { path: String, reason: String },
}

fn cap_diff(mut text: String) -> (String, bool) {
    if text.len() <= MAX_DIFF {
        return (text, false);
    }
    let mut end = MAX_DIFF;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

/// 新增文件的 diff：没有旧内容可比，整段都是 `+` 行。
fn new_file_diff(cwd: &Path, path: &str) -> FileDiff {
    let shown = path.to_string();
    match crate::preview::preview(cwd, path) {
        crate::preview::FilePreview::Text { content, .. } => {
            let (content, truncated) = cap_diff(content);
            let mut out = format!("--- /dev/null\n+++ b/{path}\n");
            let body: Vec<String> = content.lines().map(|l| format!("+{l}")).collect();
            out.push_str(&format!("@@ -0,0 +1,{} @@\n", body.len()));
            for line in &body {
                out.push_str(line);
                out.push('\n');
            }
            FileDiff::Diff {
                path: shown,
                text: out,
                truncated,
            }
        }
        crate::preview::FilePreview::Binary { .. } | crate::preview::FilePreview::Image { .. } => {
            FileDiff::Unavailable {
                path: shown,
                reason: "二进制文件没有文本 diff".into(),
            }
        }
        crate::preview::FilePreview::Missing { .. } => FileDiff::Unavailable {
            path: shown,
            reason: "文件已经不存在了".into(),
        },
        other => FileDiff::Unavailable {
            path: shown,
            reason: format!("读不了这个文件：{other:?}"),
        },
    }
}

/// 算某一轮里某个文件的 diff。**按需**调用（用户点开预览时）。
pub fn diff_for(session_id: &str, turn: u32, cwd: &Path, raw_path: &str) -> FileDiff {
    let path = raw_path.trim().replace('\\', "/");
    let Some(record) = load(session_id).into_iter().find(|t| t.turn == turn) else {
        return FileDiff::Unavailable {
            path,
            reason: "没有这一轮的变更记录".into(),
        };
    };
    let Some(baseline) = record.baseline else {
        return FileDiff::Unavailable {
            path,
            reason: "这一轮没有 git 基线（工作目录不在 git 仓库里，或 git 不可用）".into(),
        };
    };
    if Path::new(&path).is_absolute() {
        return FileDiff::Unavailable {
            path,
            reason: "这个文件不在会话工作目录里，算不了本轮 diff".into(),
        };
    }

    // 未被 git 跟踪的文件（含新文件）不会出现在 `git diff <基线>` 里，
    // 单独按「整段新增」处理。
    let tracked = git(cwd, &["ls-files", "--error-unmatch", "--", &path]).is_some();
    if !tracked {
        return new_file_diff(cwd, &path);
    }

    let out = match git(
        cwd,
        &["diff", "--no-color", "--unified=3", &baseline, "--", &path],
    ) {
        Some(s) => s,
        None => {
            return FileDiff::Unavailable {
                path,
                reason: "git diff 执行失败（文件可能在仓库之外）".into(),
            }
        }
    };
    if out.trim().is_empty() {
        return FileDiff::Empty { path };
    }
    if out.lines().any(|l| l.starts_with("Binary files ")) {
        return FileDiff::Unavailable {
            path,
            reason: "二进制文件没有文本 diff".into(),
        };
    }
    let (text, truncated) = cap_diff(out);
    FileDiff::Diff {
        path,
        text,
        truncated,
    }
}

// ---------------------------------------------------------------------------
// 按会话持久化
// ---------------------------------------------------------------------------

/// 一轮的变更记录（持久化与 IPC 用同一形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFiles {
    pub turn: u32,
    pub files: Vec<String>,
    /// 轮首的 git 基线。没有基线（非 git）时为空，界面上就不提供 diff 页。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    turns: BTreeMap<u32, StoredTurn>,
}

/// 每一轮的落盘形状。`Plain` 是早先只有路径数组的版本 —— 读到它就当没有基线，
/// 历史清单照常显示，只是点开没有 diff。
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredTurn {
    Plain(Vec<String>),
    Full {
        files: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        baseline: Option<String>,
    },
}

impl StoredTurn {
    fn into_turn(self, turn: u32) -> TurnFiles {
        match self {
            StoredTurn::Plain(files) => TurnFiles {
                turn,
                files,
                baseline: None,
            },
            StoredTurn::Full { files, baseline } => TurnFiles {
                turn,
                files,
                baseline,
            },
        }
    }

    fn from_parts(files: &[String], baseline: Option<&str>) -> Self {
        StoredTurn::Full {
            files: files.to_vec(),
            baseline: baseline.map(str::to_string),
        }
    }
}

/// 会话 id → 文件名。与 `goal.rs::file_for` 同一条白名单：拼路径的地方不信任输入。
fn file_for(session_id: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    paths::changes_dir().join(format!("{safe}.json"))
}

/// 读一条会话的全部按轮记录。文件不存在或坏了就是空 —— 它只服务界面，
/// 不该让会话起不来。
pub fn load(session_id: &str) -> Vec<TurnFiles> {
    load_from(&file_for(session_id))
}

fn load_from(path: &Path) -> Vec<TurnFiles> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(stored) = serde_json::from_str::<Stored>(&raw) else {
        return Vec::new();
    };
    stored
        .turns
        .into_iter()
        .map(|(turn, t)| t.into_turn(turn))
        .collect()
}

/// 记下某一轮的变更。空清单不落盘（没有条目就是「没变」）。
pub fn record(
    session_id: &str,
    turn: u32,
    files: &[String],
    baseline: Option<&str>,
) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    record_to(&file_for(session_id), turn, files, baseline)
}

fn record_to(path: &Path, turn: u32, files: &[String], baseline: Option<&str>) -> Result<(), String> {
    let mut stored = match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str::<Stored>(&raw).unwrap_or_default(),
        Err(_) => Stored::default(),
    };
    stored
        .turns
        .insert(turn, StoredTurn::from_parts(files, baseline));
    write_stored(path, &stored)
}

/// 会话被删时一并清掉。不删的话这个目录会跟着历史会话一直长。
pub fn remove(session_id: &str) -> bool {
    std::fs::remove_file(file_for(session_id)).is_ok()
}

/// 重试：把第 `turn` 轮及之后的变更记录一起丢掉。
///
/// 不带的话，回退之后重跑出来的那一轮会**覆盖**自己那条记录（同一个轮次号），
/// 但更后面那些轮次的记录会留下来 —— 界面上它们挂在已经不存在了的回答下面。
pub fn retract(session_id: &str, turn: u32) -> Result<(), String> {
    retract_from(&file_for(session_id), turn)
}

/// fork 出来的新会话继承变更记录。`keep_before` = 只留轮次号小于它的那些。
///
/// 不复制的话，新会话里那些保留下来的历史回答会**一个文件清单都没有** ——
/// 而分叉恰恰发生在用户刚看到「本轮改了这几个文件」的时候，那行突然消失最像丢数据。
pub fn copy(from: &str, to: &str, keep_before: Option<u32>) -> Result<(), String> {
    copy_between(&file_for(from), &file_for(to), keep_before)
}

fn copy_between(from: &Path, to: &Path, keep_before: Option<u32>) -> Result<(), String> {
    let mut stored = match std::fs::read_to_string(from) {
        Ok(raw) => serde_json::from_str::<Stored>(&raw).unwrap_or_default(),
        Err(_) => return Ok(()),
    };
    if let Some(cut) = keep_before {
        stored.turns.split_off(&cut);
    }
    if stored.turns.is_empty() {
        return Ok(());
    }
    write_stored(to, &stored)
}

fn write_stored(path: &Path, stored: &Stored) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let json = serde_json::to_string(stored).map_err(|e| e.to_string())?;
    // 先写临时文件再改名：中途断电不能留下半截 JSON（与 `goal.rs::save_to` 同理）。
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("改名到 {} 失败：{e}", path.display()))
}

fn retract_from(path: &Path, turn: u32) -> Result<(), String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let Ok(mut stored) = serde_json::from_str::<Stored>(&raw) else {
        return Ok(());
    };
    if stored.turns.split_off(&turn).is_empty() {
        return Ok(());
    }
    write_stored(path, &stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "carter-changes-{tag}-{}-{}",
            std::process::id(),
            crate::protocol::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 事件路径要么落在 cwd 下、要么被忽略名单挡掉 —— 这条规则决定了清单里会不会
    /// 混进 `sessions/*.jsonl` 这种每次对话都在变的应用私有文件。
    #[test]
    fn relative_paths_skip_ignored_dirs_and_the_root_itself() {
        let roots = vec![PathBuf::from("/work/proj")];
        assert_eq!(
            relative(&roots, Path::new("/work/proj/src/main.rs")),
            Some(PathBuf::from("src/main.rs"))
        );
        assert_eq!(relative(&roots, Path::new("/work/proj")), None);
        // 前缀相同但不是子目录。
        assert_eq!(relative(&roots, Path::new("/work/proj2/x.rs")), None);
        assert_eq!(
            relative(&roots, Path::new("/work/proj/node_modules/x/index.js")),
            None
        );
        assert_eq!(relative(&roots, Path::new("/work/proj/.git/index")), None);
        assert_eq!(relative(&roots, Path::new("/work/other/file")), None);
    }

    /// macOS 的符号链接（`/tmp` → `/private/tmp`）让事件路径与 cwd 的写法不同；
    /// 几种写法都要能对上同一个根。
    #[test]
    fn any_root_spelling_matches() {
        let roots = vec![PathBuf::from("/tmp/x"), PathBuf::from("/private/tmp/x")];
        assert_eq!(
            relative(&roots, Path::new("/private/tmp/x/a.txt")),
            Some(PathBuf::from("a.txt"))
        );
        assert_eq!(
            relative(&roots, Path::new("/tmp/x/a.txt")),
            Some(PathBuf::from("a.txt"))
        );
    }

    /// 目录本身不出现在清单里（里面的文件会各自出现）。
    #[test]
    fn directories_are_not_listed() {
        let dir = tmp_dir("dir");
        let roots = vec![dir.clone()];
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(relative(&roots, &sub), None);
        let file = sub.join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(relative(&roots, &file), Some(PathBuf::from("sub/a.txt")));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 监听器要能看见真实写入，并且返回相对路径。事件是异步的，所以等一会儿
    /// 再看 —— 这是测试，不是运行时路径。
    #[tokio::test]
    async fn watch_reports_written_files() {
        let dir = tmp_dir("watch");
        let watch = ChangeWatch::start(&dir, &[]).expect("监听器建不起来");
        std::fs::write(dir.join("a.txt"), "one").unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("node_modules/skip.js"), "x").unwrap();

        // `finish` 有 150ms 的收尾等待；文件事件通常远早于此。
        let done = watch.finish().await;
        assert_eq!(done.files, vec!["a.txt".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn user_rules_match_paths_and_names() {
        let rules = IgnoreRules::new(&[
            "*.log".into(),
            "dist/**".into(),
            "__generated__".into(),
            "  ".into(),
            "# 注释".into(),
            "sub/*.tmp".into(),
        ]);
        assert!(rules.matches("a.log"));
        assert!(rules.matches("deep/dir/b.log"), "不含 / 的规则要匹配任意层级");
        assert!(rules.matches("dist/app.js"));
        assert!(rules.matches("deep/__generated__/x.rs"));
        assert!(rules.matches("sub/a.tmp"));
        assert!(!rules.matches("sub/deep/a.tmp"), "含 / 的规则按相对路径匹配");
        assert!(!rules.matches("src/main.rs"));
    }

    #[test]
    fn records_roundtrip_per_turn() {
        let dir = tmp_dir("store");
        let path = dir.join("s.json");
        record_to(
            &path,
            2,
            &["b.txt".to_string(), "a.txt".to_string()],
            Some("deadbeef"),
        )
        .unwrap();
        record_to(&path, 5, &["c.txt".to_string()], None).unwrap();
        // 临时文件不能留在原地。
        assert!(!path.with_extension("json.tmp").exists());

        let got = load_from(&path);
        assert_eq!(
            got,
            vec![
                TurnFiles {
                    turn: 2,
                    files: vec!["b.txt".into(), "a.txt".into()],
                    baseline: Some("deadbeef".into()),
                },
                TurnFiles {
                    turn: 5,
                    files: vec!["c.txt".into()],
                    baseline: None,
                },
            ]
        );

        // 旧格式（只有路径数组）仍然读得出来，只是没有基线。
        std::fs::write(&path, r#"{"turns":{"1":["old.txt"]}}"#).unwrap();
        assert_eq!(
            load_from(&path),
            vec![TurnFiles {
                turn: 1,
                files: vec!["old.txt".into()],
                baseline: None,
            }]
        );

        // 坏文件按「没有记录」处理，不 panic。
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_from(&path).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 重试回退：第 N 轮及之后的记录一起丢掉，之前的留着。
    ///
    /// 留着的部分是对的，没有它的话，被回退掉的回答在下一次打开会话时又会把
    /// 文件清单挂回来 —— 挂在一段已经不存在的对话下面。
    #[test]
    fn retracting_a_turn_drops_it_and_everything_after() {
        let dir = tmp_dir("retract");
        let path = dir.join("s.json");
        record_to(&path, 1, &["a.txt".to_string()], None).unwrap();
        record_to(&path, 2, &["b.txt".to_string()], None).unwrap();
        record_to(&path, 3, &["c.txt".to_string()], None).unwrap();

        retract_from(&path, 2).unwrap();
        let turns: Vec<u32> = load_from(&path).iter().map(|t| t.turn).collect();
        assert_eq!(turns, vec![1]);

        // 没有文件 / 已经是空的：不报错（重试本身不该被它挡下来）。
        assert!(retract_from(&dir.join("nope.json"), 1).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 有 git 时：轮首基线 + 轮末 diff 能还原这一轮的改动。
    /// 没装 git 或 git 不在 PATH 的环境直接跳过。
    #[test]
    fn git_baseline_gives_a_turn_diff() {
        let dir = tmp_dir("git");
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !run(&["init", "-q"]) || !run(&["config", "user.email", "t@example.com"]) {
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        assert!(run(&["add", "a.txt"]) && run(&["commit", "-qm", "init"]));

        let baseline = git_baseline(&dir).expect("应当拿到基线");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        let diff = diff_against(&dir, &baseline, "a.txt");
        assert!(diff.contains("+two"), "diff 里应当有这一轮加的那行：{diff}");
        assert!(!diff.contains("+one"), "基线里已有的行不该算改动：{diff}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `.gitignore` 的过滤交给 git：嵌套规则、全局 excludes 都不用自己实现。
    #[test]
    fn gitignore_filters_changed_files() {
        let dir = tmp_dir("gitignore");
        let ok = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !ok(&["init", "-q"]) {
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        std::fs::write(dir.join(".gitignore"), "*.log\nbuild/\n").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        let files = vec![
            "a.log".to_string(),
            "src/b.log".to_string(),
            "build/x.js".to_string(),
            "keep.txt".to_string(),
        ];
        let ignored = git_ignored(&dir, &files);
        assert!(ignored.contains("a.log"));
        assert!(ignored.contains("src/b.log"));
        assert!(ignored.contains("build/x.js"));
        assert!(!ignored.contains("keep.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 没装 git 时，`.gitignore` 的过滤由进程内的 `ignore` crate 顶上。
    ///
    /// 这条守的是「装了 git 的机器」与「没装的机器」在变更清单上不该有差别：
    /// 之前没有 git 会把 `*.log`、`build/` 全列出来。
    #[test]
    fn in_process_gitignore_matches_what_git_would_ignore() {
        let dir = tmp_dir("gigno");
        std::fs::write(dir.join(".gitignore"), "*.log\nbuild/\n!keep.log\n").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("src/.gitignore"), "*.tmp\n").unwrap();

        let files = vec![
            "a.log".to_string(),
            "src/b.log".to_string(),
            "build/x.js".to_string(),
            "src/c.tmp".to_string(),
            "keep.log".to_string(),
            "src/main.rs".to_string(),
        ];
        let ignored = git_ignored_in_process(&dir, &files);

        assert!(ignored.contains("a.log"), "顶层的 *.log 要被忽略：{ignored:?}");
        assert!(ignored.contains("src/b.log"), "不含 / 的规则要匹配任意层级");
        assert!(ignored.contains("build/x.js"), "被忽略目录里的文件也算");
        assert!(ignored.contains("src/c.tmp"), "嵌套 .gitignore 要生效");
        assert!(!ignored.contains("keep.log"), "`!` 取反规则要生效");
        assert!(!ignored.contains("src/main.rs"));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `in_git_repo` 是挡住无谓 git 进程的那道闸，得认得出子目录与 worktree
    /// （`.git` 是文件而不是目录）。
    #[test]
    fn in_git_repo_recognises_subdirectories_and_worktrees() {
        let dir = tmp_dir("repodetect");
        let sub = dir.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(!in_git_repo(&sub), "没有 .git 就该是 false");

        // worktree / submodule 的 `.git` 是文件。
        std::fs::write(dir.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert!(in_git_repo(&sub), "子目录要能从祖先里找到 .git");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 新增文件没有旧内容可比：整段按 `+` 行给出。
    #[test]
    fn untracked_file_becomes_a_new_file_diff() {
        let dir = tmp_dir("newfile");
        std::fs::write(dir.join("new.txt"), "hello\nworld\n").unwrap();
        match new_file_diff(&dir, "new.txt") {
            FileDiff::Diff { text, .. } => {
                assert!(text.starts_with("--- /dev/null"));
                assert!(text.contains("+hello"));
                assert!(text.contains("+world"));
            }
            other => panic!("应当是新增文件的 diff：{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 从落盘记录到 `diff_for` 的整条路：基线写进 changes 文件，点开时能算出来。
    #[test]
    fn diff_for_uses_the_persisted_baseline() {
        let _g = crate::paths::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tmp_dir("apphome");
        let prev = std::env::var_os("CARTER_APP_HOME");
        // SAFETY: 与其它改进程级环境变量的测试共用 paths::ENV_LOCK。
        unsafe { std::env::set_var("CARTER_APP_HOME", &home) };

        let out = (|| {
            let repo = tmp_dir("diffrepo");
            let ok = |args: &[&str]| {
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            };
            if !ok(&["init", "-q"]) || !ok(&["config", "user.email", "t@example.com"]) {
                return None;
            }
            ok(&["config", "user.name", "t"]);
            std::fs::write(repo.join("a.txt"), "one\n").unwrap();
            ok(&["add", "a.txt"]);
            ok(&["commit", "-qm", "init"]);

            let baseline = git_baseline(&repo).expect("基线");
            std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
            std::fs::write(repo.join("b.txt"), "new file\n").unwrap();
            record(
                "sess-diff",
                7,
                &["a.txt".to_string(), "b.txt".to_string()],
                Some(&baseline),
            )
            .unwrap();

            let tracked = diff_for("sess-diff", 7, &repo, "a.txt");
            let untracked = diff_for("sess-diff", 7, &repo, "b.txt");
            // 没有基线的那一轮（非 git 目录）明说原因，而不是假装没改。
            record("sess-diff", 8, &["c.txt".to_string()], None).unwrap();
            // 两个基线不同的轮次共用一个会话文件 —— 第 7 轮的记录不能被覆盖。
            let no_base = diff_for("sess-diff", 8, &repo, "c.txt");

            let result = (tracked, untracked, no_base);
            std::fs::remove_dir_all(&repo).ok();
            Some(result)
        })();

        match prev {
            Some(v) => unsafe { std::env::set_var("CARTER_APP_HOME", v) },
            None => unsafe { std::env::remove_var("CARTER_APP_HOME") },
        }
        let _ = std::fs::remove_dir_all(&home);

        let Some((tracked, untracked, no_base)) = out else {
            return; // 没装 git，跳过
        };
        match tracked {
            FileDiff::Diff { text, .. } => {
                assert!(text.contains("+two"), "应当有本轮新增的行：{text}");
                assert!(!text.contains("+one"), "基线里已有的行不该算改动：{text}");
            }
            other => panic!("应当是 diff：{other:?}"),
        }
        match untracked {
            FileDiff::Diff { text, .. } => {
                assert!(text.starts_with("--- /dev/null"));
                assert!(text.contains("+new file"));
            }
            other => panic!("未跟踪文件应当是整段新增：{other:?}"),
        }
        assert!(matches!(no_base, FileDiff::Unavailable { .. }));
    }

    /// 测试辅助：直接跑 `git diff`，绕开落盘记录。
    fn diff_against(cwd: &Path, baseline: &str, path: &str) -> String {
        git(
            cwd,
            &["diff", "--no-color", "--unified=3", baseline, "--", path],
        )
        .unwrap_or_default()
    }
}
