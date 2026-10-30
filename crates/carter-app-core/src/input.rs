//! 输入框里的那些「打字触发」的东西：斜杠命令、`@` 文件引用。
//!
//! ## 斜杠命令分两类
//!
//! 1. **内置命令**：carter 的 `commands::BUILTINS` 是**元数据表**（名字 + 说明），
//!    dispatch 行为在各自的宿主里（TUI 在 `tui/`，这里在本模块）。所以本模块只
//!    暴露**本应用真的做得到**的那些 —— 列表里出现一个点了没反应的命令，比不显示
//!    它更糟。`/compact`、`/rewind` 这类需要 carter 内部状态的，直接不出现在菜单里。
//!
//! 2. **自定义命令**：`~/.carter/commands/*.md` 与 `.carter/commands/*.md`，
//!    带 frontmatter 的 Markdown。carter 的 `commands::discover` 找出来、
//!    `commands::expand` 把正文展开成一段 prompt（会注入 `@文件` 内容与 `!shell`
//!    输出）。这两个函数直接用，不重写 —— 与 CLI 共用同一套语义。
//!
//! ## `@文件` / `@目录` 展开
//!
//! carter 只在**自定义命令的正文**里展开 `@path`（`commands/expand.rs` 的
//! `inject_files`，私有）。输入框里的 `@path` 在 CLI 里是不展开的，留给模型自己
//! `read_file`。
//!
//! GUI 这边选择展开，因为「添加文件作为上下文」是用户显式要的效果，而让模型先
//! 花一轮去读、还要猜用户指的是哪个文件，是更差的体验。文件格式与 carter 完全一致
//! （`--- path ---` 包裹），这样两边的行为对得上。
//!
//! 目录是 GUI 独有的能力（carter 的 `inject_files` 只认文件）：`@src/utils`
//! 会把目录里能读的文本文件按 `--- 相对路径 ---` 逐个附上，总预算与单文件一致
//! （`MAX_INJECT_BYTES`）。放不下的文件列个名字，让模型自己 `read_file` ——
//! 目录可以很大，全塞进上下文会把这一轮直接撑爆。
//!
//! ## 多级 `@`
//!
//! 菜单里的路径是**相对 cwd 的完整路径**，目录以 `/` 结尾。选中目录时插入的是
//! `@src/utils/`（不带尾随空格），菜单会继续停在这个目录里，用户可以一层层往下选；
//! 选中文件才补空格、结束这次引用。查询词就是对整条相对路径做子串匹配，所以
//! `@src/co` 直接筛出 `src/` 下的匹配项。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use carter::commands::{self, SlashCommand};

/// 一条能出现在 `/` 菜单里的命令。
#[derive(Debug, Clone, Serialize)]
pub struct CommandRow {
    /// 不带斜杠的名字。
    pub name: String,
    pub description: String,
    /// 参数提示（自定义命令的 `argument-hint`）。
    pub argument_hint: Option<String>,
    /// `builtin` = 由本应用执行；`custom` = 展开成 prompt 发给模型。
    pub kind: &'static str,
}

/// 菜单里出现的内置命令。
///
/// **只列本应用真能做到的。** carter 的 `BUILTINS` 有 15 个，其中
/// `/compact` `/context` `/cost` `/rewind` `/skills` `/mcp` `/resume` 这些要么需要
/// carter 的内部状态、要么在 GUI 里已经有等价入口（用量在底栏、技能与 MCP 在设置页），
/// 这里就不列了 —— 列了却按不动，用户会以为坏了。
///
/// 表里的每一项在前端 `Composer.tsx::runBuiltin` 都必须有落点（那边是真正执行的地方）。
const GUI_BUILTINS: &[(&str, &str)] = &[
    ("new", "开启新对话"),
    ("model", "切换模型"),
    ("cwd", "切换工作目录（会新开一个会话）"),
    ("fork", "从当前会话派生一个新会话"),
    ("btw", "开一个不影响主对话的侧边聊天"),
    ("goal", "设定/查看长任务目标，模型会自己接着干"),
    ("export", "把当前会话导出到下载目录"),
    ("settings", "打开设置"),
    ("help", "显示可用命令"),
];

pub fn list(cwd: &Path) -> Vec<CommandRow> {
    let mut rows: Vec<CommandRow> = GUI_BUILTINS
        .iter()
        .map(|(name, description)| CommandRow {
            name: (*name).to_string(),
            description: (*description).to_string(),
            argument_hint: None,
            kind: "builtin",
        })
        .collect();

    let mut custom: Vec<CommandRow> = commands::discover(cwd)
        .into_iter()
        .map(|c| CommandRow {
            name: c.name.clone(),
            description: c.description.clone().unwrap_or_default(),
            argument_hint: c.argument_hint.clone(),
            kind: "custom",
        })
        .collect();
    custom.sort_by(|a, b| a.name.cmp(&b.name));
    rows.extend(custom);
    rows
}

/// 把一条自定义命令 + 参数展开成要发给模型的 prompt。
pub fn expand_custom(cwd: &Path, name: &str, args: &str) -> Result<String, String> {
    let cmd: SlashCommand = commands::discover(cwd)
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("找不到命令 /{name}"))?;
    Ok(commands::expand(&cmd, args, cwd))
}

// ---------------------------------------------------------------------------
// @ 文件引用
// ---------------------------------------------------------------------------

/// 一次最多注入多少字节的文件内容。超过就只给路径，让模型自己去读 ——
/// 往上下文里塞一个几百 KB 的文件会把这一轮的 token 直接打满。
const MAX_INJECT_BYTES: usize = 128 * 1024;

/// 目录遍历的上限。`@` 菜单是即时反馈的，遍历几万个文件会卡住输入。
const MAX_WALK: usize = 20_000;

/// 不值得列出来的目录。与 carter 的 `glob` 工具保持同一份直觉。
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
    "vendor",
];

/// 列出工作目录下的文件和目录（相对路径），供 `@` 菜单用。
///
/// 目录路径以 `/` 结尾，这样菜单能把「还能往下走」和「已经到文件了」区分开，
/// 选中时的插入行为也不同（见 `Composer.tsx`）。
///
/// 广度优先 + 跳过名单，不读 `.gitignore` —— 那要拖进 `ignore` crate 与一套
/// 语义（嵌套 gitignore、全局 ignore、显式 include），而这里只需要「别把
/// node_modules 列出来」。广度优先是为了让 `limit` 截断时留下的是**浅层**结果：
/// 用户一敲 `@`，最先看到的应该是当前这一层有什么，而不是某个角落里碰巧
/// 先被 `read_dir` 返回的深文件。
pub fn list_files(cwd: &Path, query: &str, limit: usize) -> Vec<String> {
    let q = normalized_query(query);
    let mut out: Vec<String> = Vec::new();
    let mut seen = 0usize;

    let mut queue = std::collections::VecDeque::from([cwd.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        if seen > MAX_WALK || out.len() >= limit {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if seen > MAX_WALK || out.len() >= limit {
                break;
            }
            seen += 1;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(ft) = entry.file_type() else { continue };
            let Ok(rel) = path.strip_prefix(cwd) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if ft.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                    continue;
                }
                let rel = format!("{rel}/");
                // 用户已经打出 `@src/` 时，`src/` 自己再列一遍没有意义 —— 那会
                // 让「回车选第一个」原地打转。跳过它，第一项就是目录里的子项。
                let already_inside = q.ends_with('/')
                    && q.trim_end_matches('/') == rel.trim_end_matches('/').to_lowercase();
                // 有查询词就按子串过滤（不区分大小写）；空查询给前 limit 个，
                // 让用户一按 `@` 就有东西可选。
                if !already_inside && (q.is_empty() || rel.to_lowercase().contains(&q)) {
                    out.push(rel);
                }
                queue.push_back(path);
                continue;
            }
            if q.is_empty() || rel.to_lowercase().contains(&q) {
                out.push(rel);
            }
        }
    }

    // 浅的排前面；同一层里目录排文件前面（继续往下走比直接选文件更常见）。
    out.sort_by_key(|p| {
        let is_file = !p.ends_with('/');
        (depth_of(p), is_file, p.len())
    });
    out.truncate(limit);
    out
}

/// 查询词归一化：去空白、统一分隔符、去掉开头的 `./`。
fn normalized_query(query: &str) -> String {
    let q = query.trim().replace('\\', "/").to_lowercase();
    q.strip_prefix("./").unwrap_or(&q).to_string()
}

/// 相对路径的层级深度（目录的尾随 `/` 不算一层）。
fn depth_of(rel: &str) -> usize {
    rel.trim_end_matches('/').matches('/').count()
}

/// 把文本里的 `@相对路径` 展开成内容块。路径可以是文件，也可以是目录。
///
/// 规则与 carter 的 `commands/expand.rs::inject_files` 一致：`@` 必须在词首
/// （行首或空白之后），路径读到下一个空白，能读到文件才替换。读不到就原样留着 ——
/// 用户可能就是想打一个 `@`，或者那个文件是图片（图片走 `[img:...]` 那条路）。
///
/// 一个非图片文件在**没被显式引用**时不该被展开，所以这里只认 `@`。图片路径则由
/// carter 的 `inline_user_attachments` 在 provider 边界处理，两者互不冲突。
pub fn expand_file_refs(text: &str, cwd: &Path) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        let at_boundary = i == 0 || chars[i - 1].is_whitespace();
        if c != '@' || !at_boundary {
            out.push(c);
            i += 1;
            continue;
        }

        let start = i + 1;
        let mut j = start;
        while j < chars.len() && !chars[j].is_whitespace() {
            j += 1;
        }
        let token: String = chars[start..j].iter().collect();

        if token.is_empty() {
            out.push(c);
            i += 1;
            continue;
        }

        match read_for_context(cwd, &token) {
            Some(reference) => {
                out.push_str(&reference.text);
                i = j;
            }
            None => {
                // 读不到就原样留着。**不报错** —— 用户可能只是打了个 @ 提到某人，
                // 或者路径写错了但还没写完。
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// 一次引用展开的结果。
struct Reference {
    /// 要拼进消息的上下文块。
    text: String,
    /// 这次引用实际带进来的文件数。目录引用可能带出多个，界面据此提示「N 个文件」。
    files: usize,
}

/// 读一个文件或目录并渲染成上下文块。
///
/// 图片、不存在的路径、读不了的二进制返回 None（文件引用原样留着）；
/// 目录返回 Some，内容是递归展开的文本文件。
fn read_for_context(cwd: &Path, token: &str) -> Option<Reference> {
    let path: PathBuf = if Path::new(token).is_absolute() {
        PathBuf::from(token)
    } else {
        cwd.join(token)
    };
    let meta = std::fs::metadata(&path).ok()?;

    if meta.is_dir() {
        // 不展开文件系统根：`@/` 几乎一定是手滑，把根目录下的杂项塞进上下文
        // 没有意义，还会占满预算。
        let _parent = path.parent()?;
        return Some(dir_for_context(&path, token));
    }
    if !meta.is_file() {
        return None;
    }
    // 图片交给 `[img:...]` 那条路，不要在这里塞 base64。
    if carter::media::is_image_path(&path) {
        return None;
    }

    if meta.len() as usize > MAX_INJECT_BYTES {
        return Some(Reference {
            text: format!(
                "\n--- {token} ---\n（文件 {} 字节，超过 {} 字节的注入上限，请用 read_file 自己读）\n--- end {token} ---\n",
                meta.len(),
                MAX_INJECT_BYTES
            ),
            files: 1,
        });
    }
    // 非 UTF-8（二进制）直接放过，让模型自己决定。
    let content = std::fs::read_to_string(&path).ok()?;
    Some(Reference {
        text: format!("\n--- {token} ---\n{content}\n--- end {token} ---\n"),
        files: 1,
    })
}

/// 把目录递归展开成上下文块：能读的文本文件逐个用 `--- 相对路径 ---` 包起来，
/// 共享 [`MAX_INJECT_BYTES`] 的总预算。放不下的（太大、二进制、图片）只列名字，
/// 让模型按需 `read_file`。
fn dir_for_context(dir: &Path, token: &str) -> Reference {
    // `@src/`（菜单插入的形态）在标题里去掉尾随斜杠，读起来更像路径。
    let label = token.trim_end_matches('/');

    let mut paths: Vec<PathBuf> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0usize;
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            if seen > MAX_WALK {
                break;
            }
            seen += 1;
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                    continue;
                }
                stack.push(entry.path());
                continue;
            }
            if ft.is_file() {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();

    let mut out = format!("\n--- {label}（目录，{} 个文件）---\n", paths.len());
    if paths.is_empty() {
        out.push_str("（空目录）\n");
    }
    let mut budget = MAX_INJECT_BYTES;
    let mut injected = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    for path in &paths {
        let rel = path
            .strip_prefix(dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if carter::media::is_image_path(path) {
            skipped.push(rel);
            continue;
        }
        let Ok(meta) = std::fs::metadata(path) else {
            skipped.push(rel);
            continue;
        };
        let size = meta.len() as usize;
        if size > budget {
            skipped.push(rel);
            continue;
        }
        // 非 UTF-8（二进制）读不出来，也进 skipped。
        let Ok(content) = std::fs::read_to_string(path) else {
            skipped.push(rel);
            continue;
        };
        out.push_str(&format!("--- {rel} ---\n{content}\n--- end {rel} ---\n"));
        budget -= size;
        injected += 1;
    }
    if !skipped.is_empty() {
        let shown: Vec<&str> = skipped.iter().take(40).map(String::as_str).collect();
        out.push_str(&format!(
            "（{} 个文件未注入：{}{}；需要的话用 read_file 自己读）\n",
            skipped.len(),
            shown.join(", "),
            if skipped.len() > shown.len() { " 等" } else { "" }
        ));
    }
    out.push_str(&format!("--- end {label} ---\n"));

    // 空目录 / 全是二进制时也计一次：引用本身是成立的，界面上不该悄悄没反应。
    Reference {
        text: out,
        files: injected.max(1),
    }
}

/// 文本里有没有被引用到的文件。界面上要据此提示「这条消息带了 N 个文件」。
/// 目录引用按它实际展开的文件数计。
pub fn count_file_refs(text: &str, cwd: &Path) -> usize {
    split_refs(text)
        .into_iter()
        .filter_map(|token| read_for_context(cwd, &token))
        .map(|r| r.files)
        .sum()
}

/// 文本里所有 `@token` 的 token（不含 `@`）。词首规则与 [`expand_file_refs`] 一致。
fn split_refs(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let at_boundary = i == 0 || chars[i - 1].is_whitespace();
        if chars[i] == '@' && at_boundary {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            let token: String = chars[start..j].iter().collect();
            if !token.is_empty() {
                out.push(token);
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    out
}

/// 供图标 / 提示用的文件种类（按扩展名粗分）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Code,
    Doc,
    Data,
    Other,
}

pub fn kind_of(path: &str) -> FileKind {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "h" | "cpp"
        | "hpp" | "rb" | "php" | "swift" | "kt" | "cs" | "sh" | "ps1" | "toml" | "yaml"
        | "yml" | "json" | "html" | "css" | "sql" => FileKind::Code,
        "md" | "txt" | "rst" | "adoc" => FileKind::Doc,
        "csv" | "tsv" | "xlsx" | "parquet" => FileKind::Data,
        _ => FileKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "carter-app-cmd-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn file_refs_are_expanded_with_the_same_framing_as_carter() {
        let d = tmp_dir("expand");
        std::fs::write(d.join("main.rs"), "fn main() {}").unwrap();

        let out = expand_file_refs("看看 @main.rs 有什么问题", &d);
        assert!(out.contains("--- main.rs ---"), "框架要与 carter 一致：{out}");
        assert!(out.contains("fn main() {}"));
        assert!(out.contains("--- end main.rs ---"));
        assert!(out.starts_with("看看 "), "引用之外的文本要原样保留");

        let _ = std::fs::remove_dir_all(d);
    }

    /// 读不到的文件原样留着 —— 不能因为用户打了一半路径就把整个 `@` 吞掉。
    #[test]
    fn an_unreadable_ref_is_left_alone() {
        let d = tmp_dir("missing");
        let src = "对比 @不存在.rs 和 @./也不存在.rs";
        assert_eq!(expand_file_refs(src, &d), src);
        let _ = std::fs::remove_dir_all(d);
    }

    /// 邮箱里的 `@` 不该被当成引用。
    #[test]
    fn an_at_sign_inside_a_word_is_not_a_reference() {
        let d = tmp_dir("email");
        std::fs::write(d.join("example.com"), "x").unwrap();
        // `a@example.com` 里的 @ 前面不是空白，不展开。
        let src = "发到 a@example.com";
        assert_eq!(expand_file_refs(src, &d), src);
        let _ = std::fs::remove_dir_all(d);
    }

    /// 图片不在这里注入 —— 它走 `[img:...]` 那条路，塞 base64 进上下文是灾难。
    #[test]
    fn image_refs_are_left_to_the_media_pipeline() {
        let d = tmp_dir("img");
        std::fs::write(d.join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let src = "看下 @shot.png";
        assert_eq!(expand_file_refs(src, &d), src);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn oversized_files_get_a_pointer_instead_of_their_contents() {
        let d = tmp_dir("big");
        std::fs::write(d.join("big.txt"), "x".repeat(MAX_INJECT_BYTES + 1)).unwrap();
        let out = expand_file_refs("@big.txt", &d);
        assert!(out.contains("read_file"), "要引导模型自己去读：{out}");
        assert!(out.len() < 1000, "不能把超大内容注进去");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn listing_skips_noise_directories() {
        let d = tmp_dir("walk");
        std::fs::write(d.join("a.rs"), "").unwrap();
        std::fs::create_dir_all(d.join("node_modules/pkg")).unwrap();
        std::fs::write(d.join("node_modules/pkg/index.js"), "").unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git/config"), "").unwrap();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/b.rs"), "").unwrap();

        let all = list_files(&d, "", 50);
        assert!(all.contains(&"a.rs".to_string()));
        assert!(all.contains(&"src/b.rs".to_string()));
        // 目录也在列表里，带尾随 `/`。
        assert!(all.contains(&"src/".to_string()));
        assert!(!all.iter().any(|p| p.contains("node_modules")), "{all:?}");
        assert!(!all.iter().any(|p| p.contains(".git")), "{all:?}");
        // 同层的目录排文件前面，浅层排深层前面。
        assert_eq!(all[0], "src/", "{all:?}");
        assert_eq!(all[1], "a.rs", "{all:?}");

        // 查询词按整条相对路径匹配，所以 `src/` 能筛出这一层的子项。
        let filtered = list_files(&d, "b.rs", 50);
        assert_eq!(filtered, vec!["src/b.rs".to_string()]);
        // 打出 `src/` 后第一项应当是目录里的内容，而不是 `src/` 本身。
        let subtree = list_files(&d, "src/", 50);
        assert_eq!(subtree, vec!["src/b.rs".to_string()], "{subtree:?}");
        // 不带到尾斜杠时，目录本身仍可选（选中后会插入 `@src/`）。
        let named = list_files(&d, "src", 50);
        assert_eq!(
            named,
            vec!["src/".to_string(), "src/b.rs".to_string()],
            "{named:?}"
        );

        let _ = std::fs::remove_dir_all(d);
    }

    /// 引用目录时把里面能读的文本文件递归附上，目录本身不再被当成「读不到」。
    #[test]
    fn a_directory_ref_injects_its_files_recursively() {
        let d = tmp_dir("dirref");
        std::fs::create_dir_all(d.join("src/nested/deep")).unwrap();
        std::fs::write(d.join("src/a.rs"), "fn a() {}").unwrap();
        std::fs::write(d.join("src/nested/b.rs"), "fn b() {}").unwrap();
        std::fs::write(d.join("src/nested/deep/c.rs"), "fn c() {}").unwrap();
        std::fs::create_dir_all(d.join("src/node_modules/pkg")).unwrap();
        std::fs::write(d.join("src/node_modules/pkg/i.js"), "nope").unwrap();

        let out = expand_file_refs("看看 @src 里有什么", &d);
        assert!(out.contains("--- src（目录，3 个文件）---"), "{out}");
        assert!(out.contains("--- a.rs ---\nfn a() {}"), "{out}");
        assert!(out.contains("--- nested/b.rs ---\nfn b() {}"), "{out}");
        assert!(out.contains("--- nested/deep/c.rs ---\nfn c() {}"), "{out}");
        assert!(!out.contains("node_modules"), "跳过名单里的目录不展开：{out}");
        // 引用之外的文本原样保留。
        assert!(out.starts_with("看看 "), "{out}");

        // 菜单插入的形态带尾随斜杠，展开行为要一致。
        let slash = expand_file_refs("@src/", &d);
        assert_eq!(slash, expand_file_refs("@src", &d));

        // 展开计数按实际文件数算。
        assert_eq!(count_file_refs("@src", &d), 3);
        assert_eq!(count_file_refs("@src @src/a.rs", &d), 4);

        let _ = std::fs::remove_dir_all(d);
    }

    /// 目录太大时只注入预算内的文件，剩下的列名字让模型自己读。
    #[test]
    fn a_large_directory_stops_at_the_budget_and_lists_the_rest() {
        let d = tmp_dir("dirbig");
        std::fs::create_dir_all(d.join("many")).unwrap();
        // 每个 4 KB，40 个 = 160 KB > 128 KB 预算。
        for i in 0..40 {
            std::fs::write(d.join(format!("many/f{i:02}.txt")), "x".repeat(4096)).unwrap();
        }

        let out = expand_file_refs("@many", &d);
        assert!(out.contains("个文件未注入"), "{out}");
        assert!(out.contains("read_file"), "{out}");
        assert!(
            out.len() < MAX_INJECT_BYTES + 20 * 1024,
            "总长度要被预算挡住，实际 {}",
            out.len()
        );

        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_slash_menu_only_lists_things_this_app_can_actually_do() {
        let d = tmp_dir("cmds");
        let rows = list(&d);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();

        // carter 的 BUILTINS 里有，但本应用做不到 / 已有等价入口 —— 不该出现。
        for absent in ["compact", "rewind", "resume", "cost", "context", "quit"] {
            assert!(!names.contains(&absent), "`/{absent}` 不该出现在菜单里：{names:?}");
        }
        // 真的能执行的要在。
        for present in ["new", "model", "cwd", "help", "goal", "btw"] {
            assert!(names.contains(&present), "`/{present}` 应当在菜单里：{names:?}");
        }
        assert!(rows.iter().all(|r| r.kind == "builtin"));

        let _ = std::fs::remove_dir_all(d);
    }
}
