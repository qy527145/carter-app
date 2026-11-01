//! 点击「本轮变更的文件」时的预览。
//!
//! 只读：打开文件、判断它能不能直接渲染，然后返回一个**前端能直接画**的形状。
//! 路径解析统一在这里做（相对路径按会话 cwd），前端不需要知道「`./a` 到底是哪儿」。
//!
//! ## 为什么分这么多种 kind
//!
//! 一个文件可能是文本、图片、二进制、目录，或在这一轮里被删掉了。把它们都塞进
//! 「一个字符串」里，前端就只能显示一段可能乱码的文本 —— 而用户点开预览最想
//! 确认的是「这个文件现在长什么样」。所以这里把「能渲染的」和「不能渲染的」
//! 分开，前端各自给一个合适的界面（文本 / 图片 / 一句说明 + 系统打开）。
//!
//! ## 上限
//!
//! 文本只读前 256 KB、图片只内联 12 MB。会话里一次改几十个文件时，预览是
//! 按需打开的，但也不能让一次点击把一个几百 MB 的日志整个塞进 IPC。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

/// 文本预览的上限。超出部分截断并明确标注。
const MAX_TEXT: u64 = 256 * 1024;
/// 图片内联的上限。再大就走「用默认应用打开」。
const MAX_IMAGE: u64 = 12 * 1024 * 1024;
/// 判断二进制时只看开头这么多字节。
const SNIFF: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FilePreview {
    /// 文本（按 UTF-8 尽力解码）。
    Text {
        /// 解析后的绝对路径，界面上显示在标题下。
        path: String,
        bytes: u64,
        /// 修改时间（epoch 毫秒），取不到为 `None`。
        mtime: Option<u64>,
        content: String,
        /// 文件比 [`MAX_TEXT`] 大，`content` 只是开头一段。
        truncated: bool,
    },
    /// 图片：整张内联成 data URL。
    Image {
        path: String,
        bytes: u64,
        mtime: Option<u64>,
        data_url: String,
    },
    /// 二进制（含超出内联上限的图片）：不预览，只给大小与系统打开。
    Binary {
        path: String,
        bytes: u64,
        mtime: Option<u64>,
    },
    Dir { path: String },
    /// 不在了：本轮被删掉，或之后被移走。
    Missing { path: String },
    /// 读失败（权限等）。路径成立，只是打不开。
    Error { path: String, message: String },
}

/// 把工具里写的那个路径解析成绝对路径：相对路径按会话 cwd，`~` 展开。
pub fn resolve(cwd: &Path, raw: &str) -> PathBuf {
    let expanded = crate::paths::expand_tilde(raw.trim());
    let p = Path::new(&expanded);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

fn mtime_ms(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

fn image_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        // SVG 是文本，但它在 `<img>` 里只会被渲染成图（不会执行里面的脚本），
        // 所以和别的图片走同一条路 —— 模型画的图不少是 SVG。
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// 读一个文件的预览。**任何失败都落成枚举里的一个变体**，不抛错 ——
/// 用户点的是「看看它现在什么样」，「它不在了」也是一个有效答案。
pub fn preview(cwd: &Path, raw: &str) -> FilePreview {
    let path = resolve(cwd, raw);
    let shown = path.to_string_lossy().to_string();

    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return FilePreview::Missing { path: shown }
        }
        Err(e) => {
            return FilePreview::Error {
                path: shown,
                message: e.to_string(),
            }
        }
    };
    if meta.is_dir() {
        return FilePreview::Dir { path: shown };
    }

    let bytes = meta.len();
    let mtime = mtime_ms(&meta);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    // 图片先处理：它们按扩展名走，内容不需要按文本判定。
    if let Some(mime) = image_mime(&ext) {
        if bytes > MAX_IMAGE {
            return FilePreview::Binary { path: shown, bytes, mtime };
        }
        return match std::fs::read(&path) {
            Ok(raw) => {
                use base64::Engine as _;
                let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
                FilePreview::Image {
                    path: shown,
                    bytes,
                    mtime,
                    data_url: format!("data:{mime};base64,{b64}"),
                }
            }
            Err(e) => FilePreview::Error {
                path: shown,
                message: e.to_string(),
            },
        };
    }

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            return FilePreview::Error {
                path: shown,
                message: e.to_string(),
            }
        }
    };
    let mut buf = Vec::new();
    if let Err(e) = file.take(MAX_TEXT + 1).read_to_end(&mut buf) {
        return FilePreview::Error {
            path: shown,
            message: e.to_string(),
        };
    }
    let truncated = buf.len() as u64 > MAX_TEXT;
    if truncated {
        buf.truncate(MAX_TEXT as usize);
    }

    // NUL 是二进制最可靠的信号；UTF-8 解不开的（GBK 文本等）按有损显示，
    // 总比一句「二进制文件」强 —— 用户至少能看出它是什么。
    if buf[..buf.len().min(SNIFF)].contains(&0) {
        return FilePreview::Binary { path: shown, bytes, mtime };
    }
    FilePreview::Text {
        path: shown,
        bytes,
        mtime,
        content: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "carter-preview-{tag}-{}-{}",
            std::process::id(),
            crate::protocol::now_ms()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn text_is_read_as_text() {
        let dir = tmp_dir("text");
        std::fs::write(dir.join("a.txt"), "line one\nline two\n").unwrap();
        match preview(&dir, "a.txt") {
            FilePreview::Text {
                content,
                truncated,
                bytes,
                ..
            } => {
                assert_eq!(content, "line one\nline two\n");
                assert!(!truncated);
                assert_eq!(bytes, 18);
            }
            other => panic!("应当是文本：{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 相对路径按 cwd 解析，绝对路径原样用。
    #[test]
    fn paths_resolve_against_cwd() {
        let dir = tmp_dir("resolve");
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        match preview(&dir, "a.txt") {
            FilePreview::Text { path, .. } => {
                assert!(Path::new(&path).is_absolute());
                assert!(path.ends_with("a.txt"));
            }
            other => panic!("没解析到文件：{other:?}"),
        }
        let abs = dir.join("a.txt");
        match preview(Path::new("/nonexistent"), abs.to_str().unwrap()) {
            FilePreview::Text { .. } => {}
            other => panic!("绝对路径应当直接读：{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_and_directory_are_reported_not_errors() {
        let dir = tmp_dir("missing");
        assert!(matches!(preview(&dir, "nope.txt"), FilePreview::Missing { .. }));
        assert!(matches!(preview(&dir, "."), FilePreview::Dir { .. }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nul_bytes_mean_binary() {
        let dir = tmp_dir("binary");
        std::fs::write(dir.join("a.bin"), [0u8, 1, 2, 3]).unwrap();
        assert!(matches!(preview(&dir, "a.bin"), FilePreview::Binary { .. }));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn large_text_is_truncated_with_a_flag() {
        let dir = tmp_dir("large");
        let content = "x".repeat((MAX_TEXT + 100) as usize);
        std::fs::write(dir.join("big.txt"), &content).unwrap();
        match preview(&dir, "big.txt") {
            FilePreview::Text {
                content,
                truncated,
                bytes,
                ..
            } => {
                assert!(truncated);
                assert_eq!(content.len() as u64, MAX_TEXT);
                assert_eq!(bytes, MAX_TEXT + 100);
            }
            other => panic!("应当是截断的文本：{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn images_become_data_urls() {
        let dir = tmp_dir("image");
        // 不需要是真的 PNG：这里只测「按扩展名走了图片分支」。
        std::fs::write(dir.join("a.png"), [0x89, b'P', b'N', b'G']).unwrap();
        match preview(&dir, "a.png") {
            FilePreview::Image { data_url, .. } => {
                assert!(data_url.starts_with("data:image/png;base64,"));
            }
            other => panic!("应当是图片：{other:?}"),
        }
        // SVG 也按图片走（模型画的图不少是 SVG，把它当文本显示等于只给源码）。
        std::fs::write(dir.join("b.svg"), "<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
        match preview(&dir, "b.svg") {
            FilePreview::Image { data_url, .. } => {
                assert!(data_url.starts_with("data:image/svg+xml;base64,"));
            }
            other => panic!("SVG 应当也按图片走：{other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
