// 复制到剪贴板。
//
// 只走浏览器 API，不引 Tauri 的 clipboard 插件：这里唯一的用途是「复制一段文字」，
// 而 `navigator.clipboard` 在 WKWebView 的两种构建（dev 的 http://localhost 与
// 打包后的 tauri://localhost）里都算安全上下文。仍保留 execCommand 兜底：
// 某些 WebView 版本在窗口失焦后会拒绝 clipboard API，而菜单项点击本身是用户手势。

import { toastError, toastOk } from "./toast";

function legacyCopy(text: string): boolean {
  const ta = document.createElement("textarea");
  ta.value = text;
  // 不能 display:none —— 那选不中，execCommand 会失败。
  ta.style.position = "fixed";
  ta.style.top = "0";
  ta.style.left = "0";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  const ok = document.execCommand("copy");
  ta.remove();
  return ok;
}

export async function copyText(text: string, okMessage = "已复制"): Promise<void> {
  try {
    if (navigator.clipboard?.writeText) {
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        // 权限/焦点问题 → 回退到 execCommand，还是不行才算失败。
        if (!legacyCopy(text)) throw new Error("复制失败");
      }
    } else if (!legacyCopy(text)) {
      throw new Error("复制失败");
    }
    toastOk(okMessage);
  } catch (e) {
    toastError(e);
  }
}
