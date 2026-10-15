//! 图片：粘贴进输入框的图，以及把它渲染回界面。
//!
//! ## 复用 carter 的那一套，不另起炉灶
//!
//! carter 已经有完整的图片管线（`carter::media`）：按 sha256 内容寻址存
//! `~/.carter/images/<hash>.<ext>`，消息文本里只放 `[img:<hash>.<ext>]` 引用，
//! 发请求时由 provider 边界展开成多模态块。三个好处正合 GUI 的需要：
//!
//! 1. **会话 JSONL 保持纯文本** —— 粘贴一张截图不会把会话文件撑大。
//! 2. **重复粘贴自动去重** —— 同一张图两次进来是同一个 hash。
//! 3. **入库前自动降采样**到长边 1568px，控住多模态 token 成本。
//!
//! 所以本模块只做两件事：把前端传来的字节送进 `media::put_bytes`，以及读出字节
//! 给界面显示。图片真正**发给模型**的那一步不需要我们做 —— 引用 token 留在文本里，
//! provider 层自己会认。

use base64::Engine;

use carter::media::{self, ImageRef};

/// 界面里能显示的最大尺寸（字节）。再大就不读了 —— 那是几 MB 的图，
/// 转成 data URL 只会让 webview 卡住，而它本来也只是个缩略图。
const MAX_VIEW_BYTES: u64 = 12 * 1024 * 1024;

/// 把粘贴进来的图片存进 carter 的图片库，返回可以直接插进消息文本的引用 token。
pub fn save_image(base64_data: &str, ext_hint: Option<&str>) -> Result<String, String> {
    // 前端传的是 data URL 也可能只是裸 base64，两种都收。
    let payload = base64_data
        .split_once(";base64,")
        .map(|(_, d)| d)
        .unwrap_or(base64_data);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("图片数据不是合法的 base64：{e}"))?;
    if bytes.is_empty() {
        return Err("图片是空的".into());
    }

    let img = media::put_bytes(&bytes, ext_hint).map_err(|e| format!("写入图片库失败：{e}"))?;
    Ok(img.token())
}

/// 把一条图片引用读成 data URL，供界面显示。
///
/// `token` 可以是完整的 `[img:<hash>.<ext>]`，也可以只是 `<hash>.<ext>` ——
/// 前者来自消息文本，后者来自历史里的 `BlockView::Image`。
pub fn read_image(token: &str) -> Option<String> {
    let body = token
        .trim()
        .strip_prefix("[img:")
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(token.trim());
    let (hash, ext) = body.rsplit_once('.')?;
    let img = ImageRef {
        hash: hash.to_string(),
        ext: ext.to_ascii_lowercase(),
    };

    let path = img.path();
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_VIEW_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:{};base64,{b64}", img.mime()))
}

/// 一段文本里有没有图片引用。输入框要据此提示「这条消息带了 N 张图」。
pub fn count_images(text: &str) -> usize {
    media::parse_segments(text)
        .iter()
        .filter(|s| matches!(s, media::Segment::Image(_)))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// base64 解码 + 入库 + token 形态；再读回来要能显示。
    ///
    /// 用一个临时 `CARTER_HOME` —— 绝不碰用户真实的 `~/.carter/images`。
    #[test]
    fn pasting_an_image_stores_it_and_reads_back_as_a_data_url() {
        // 与 config_edit / app_settings 那几条测试共用一把锁：都在改进程级的
        // `CARTER_HOME`，各用各的锁等于没锁。
        let _g = crate::paths::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let dir = std::env::temp_dir().join(format!("carter-app-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: 本模块内只有这一条测试改环境变量，且上面已串行化。
        unsafe { std::env::set_var("CARTER_HOME", &dir) };

        // 最小的合法 PNG（1x1 透明）。用真图而不是随机字节，
        // 这样还能顺带走过 carter 的解码/降采样分支。
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);

        let token = save_image(&b64, Some("png")).unwrap();
        assert!(token.starts_with("[img:"), "token 形态不对：{token}");
        assert!(token.ends_with(".png]"), "token 形态不对：{token}");

        // 内容寻址：同一张图再来一次是同一个 token（去重）。
        assert_eq!(save_image(&b64, Some("png")).unwrap(), token);

        let url = read_image(&token).expect("应当读得回来");
        assert!(url.starts_with("data:image/png;base64,"), "{url}");

        // 裸 `hash.ext` 形态也要认（历史里的 BlockView::Image 只带这两样）。
        let bare = token
            .trim_start_matches("[img:")
            .trim_end_matches(']')
            .to_string();
        assert!(read_image(&bare).is_some(), "裸 hash.ext 形态应当也能读");

        // 文本里数得出来。
        assert_eq!(count_images(&format!("看这个 {token}")), 1);
        assert_eq!(count_images("没有图片"), 0);

        unsafe { std::env::remove_var("CARTER_HOME") };
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn garbage_input_is_rejected_with_a_readable_message() {
        assert!(save_image("这不是 base64！！！", None).is_err());
        assert!(save_image("", None).is_err());
        assert!(read_image("[img:不存在的hash.png]").is_none());
    }
}
