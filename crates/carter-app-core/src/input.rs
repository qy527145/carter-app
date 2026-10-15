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
//! ## `@文件` 展开
//!
//! carter 只在**自定义命令的正文**里展开 `@path`（`commands/expand.rs` 的
//! `inject_files`，私有）。输入框里的 `@path` 在 CLI 里是不展开的，留给模型自己
//! `read_file`。
//!
//! GUI 这边选择展开，因为「添加文件作为上下文」是用户显式要的效果，而让模型先
//! 花一轮去读、还要猜用户指的是哪个文件，是更差的体验。格式与 carter 完全一致
//! （`--- path ---` 包裹），这样两边的行为对得上。

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
const GUI_BUILTINS: &[(&str, &str)] = &[
    ("new", "开启新对话"),
    ("model", "切换模型"),
    ("cwd", "切换工作目录（会新开一个会话）"),
    ("fork", "从当前会话派生一个新会话"),
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

/// 列出工作目录下的文件（相对路径），供 `@` 菜单用。
///
/// 朴素遍历 + 跳过名单，不读 `.gitignore` —— 那要拖进 `ignore` crate 与一套
/// 语义（嵌套 gitignore、全局 ignore、显式 include），而这里只需要「别把
/// node_modules 列出来」。真要精确，用户会自己去敲路径。
pub fn list_files(cwd: &Path, query: &str, limit: usize) -> Vec<String> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut seen = 0usize;

    let mut stack = vec![cwd.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if seen > MAX_WALK || out.len() >= limit {
                break;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
                continue;
            }
            seen += 1;
            let Ok(rel) = path.strip_prefix(cwd) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            // 有查询词就按子串过滤（不区分大小写）；空查询给前 limit 个，
            // 让用户一按 `@` 就有东西可选。
            if q.is_empty() || rel.to_lowercase().contains(&q) {
                out.push(rel);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    // 短的排前面：用户多半在找顶层或浅层的文件。
    out.sort_by_key(|p| (p.matches('/').count(), p.len()));
    out.truncate(limit);
    out
}

/// 把文本里的 `@相对路径` 展开成文件内容块。
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
            Some(block) => {
                out.push_str(&block);
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

/// 读一个文件并渲染成上下文块。不是普通文本文件（图片、目录、二进制）就返回 None。
fn read_for_context(cwd: &Path, token: &str) -> Option<String> {
    let path: PathBuf = if Path::new(token).is_absolute() {
        PathBuf::from(token)
    } else {
        cwd.join(token)
    };
    if path.is_dir() || !path.is_file() {
        return None;
    }
    // 图片交给 `[img:...]` 那条路，不要在这里塞 base64。
    if carter::media::is_image_path(&path) {
        return None;
    }

    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() as usize > MAX_INJECT_BYTES {
        return Some(format!(
            "\n--- {token} ---\n（文件 {} 字节，超过 {} 字节的注入上限，请用 read_file 自己读）\n--- end {token} ---\n",
            meta.len(),
            MAX_INJECT_BYTES
        ));
    }
    // 非 UTF-8（二进制）直接放过，让模型自己决定。
    let content = std::fs::read_to_string(&path).ok()?;
    Some(format!(
        "\n--- {token} ---\n{content}\n--- end {token} ---\n"
    ))
}

/// 文本里有没有被引用到的文件。界面上要据此提示「这条消息带了 N 个文件」。
pub fn count_file_refs(text: &str, cwd: &Path) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut n = 0;
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
            if !token.is_empty() && read_for_context(cwd, &token).is_some() {
                n += 1;
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    n
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
        assert!(!all.iter().any(|p| p.contains("node_modules")), "{all:?}");
        assert!(!all.iter().any(|p| p.contains(".git")), "{all:?}");
        // 浅层的排前面。
        assert_eq!(all[0], "a.rs");

        let filtered = list_files(&d, "b.rs", 50);
        assert_eq!(filtered, vec!["src/b.rs".to_string()]);

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
        for present in ["new", "model", "cwd", "help"] {
            assert!(names.contains(&present), "`/{present}` 应当在菜单里：{names:?}");
        }
        assert!(rows.iter().all(|r| r.kind == "builtin"));

        let _ = std::fs::remove_dir_all(d);
    }
}
