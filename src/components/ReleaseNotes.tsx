// 一版的更新说明：首页提示条展开后与「关于」页的更新卡片共用。
//
// 内容是 Release 正文里由提交记录生成的那一段 Markdown（见
// `scripts/release-notes.ts`），所以只会有小标题、列表和偶尔的链接 —— 不走对话
// 正文那套带图片 / 代码块 / HTML 预览的渲染，那些组件都要一个会话的 cwd。

import { openUrl } from "@tauri-apps/plugin-opener";
import type { ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

/** 链接交给系统浏览器：webview 里直接跳转会把整个应用换成那个网页。 */
function ExternalAnchor({ href, children }: { href?: string; children?: ReactNode }) {
  return (
    <a
      href={href}
      onClick={(e) => {
        e.preventDefault();
        if (href) void openUrl(href).catch(() => {});
      }}
    >
      {children}
    </a>
  );
}

const COMPONENTS = {
  a: ExternalAnchor,
  // 说明来自远端的 Release 正文，图片不加载：既没必要，也不该让一段文字去发请求。
  img: () => null,
};

export function ReleaseNotes({ notes, className = "" }: { notes: string; className?: string }) {
  return (
    <div className={`carter-md text-xs text-ink ${className}`}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={COMPONENTS}>
        {notes}
      </ReactMarkdown>
    </div>
  );
}
