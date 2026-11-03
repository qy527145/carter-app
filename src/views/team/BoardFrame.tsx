// 看板的显示框：一整页 HTML，来自团队仓库，放进沙箱里渲染。
//
// ## 安全边界
//
// 看板是**从别人的远端拉下来的**任意 HTML，所以它和对话里的 HTML 预览（Media.tsx）
// 一样不能拿到主界面的权限，而且收得更紧：
//
// * `sandbox="allow-scripts"`，**不给** `allow-same-origin`：页面是不透明源，拿不到
//   父页面的 DOM / localStorage，也拿不到 Tauri IPC（IPC 只注入主框架）。
// * 不给 `allow-popups` / `allow-top-navigation` / `allow-forms`：页面开不了新窗口、
//   跳不走主界面、提交不了表单。链接点击由页面里注入的一小段脚本转成 `postMessage`，
//   这里只放行 http(s)，交给系统浏览器打开。
// * 注入一条 CSP：`connect-src 'none'` 封住 fetch / XHR / WebSocket。脚本、样式、
//   图片、字体可以走 https —— 图表库要从 CDN 来。多条 CSP 是**取交集**的，页面自己
//   再写一条只能更严，放不宽。
//
// 页面能看到的只有它自己的内容（而那本来就在仓库里），所以图片地址这类「外带」
// 通道不构成泄露；要防的是它借主界面的身份做事，上面三条就是为此。
//
// ## 主题
//
// 页面在不透明源里，读不到主界面的 CSS 变量。这里把当前主题的一组变量连同
// `color-scheme` 注入进去，切换主题时用 `postMessage` 下发新值，不重载页面
// （重载会丢滚动位置，页面里的脚本状态也会重来）。
//
// ## 高度
//
// 框的高度跟着页面内容走（页面里的脚本用 ResizeObserver 回报），滚动交给外层 ——
// 一页里套一个固定高度的滚动框，等于两层滚动条抢同一个滚轮。

import { useEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";

import { toastError } from "../../lib/toast";

/** 注入进页面的主题变量。名字与 index.css 一致，技能文档里列的也是这一组。 */
const THEME_VARS = [
  "--bg",
  "--bg-card",
  "--bg-elevated",
  "--bubble",
  "--border",
  "--border-strong",
  "--text",
  "--text-dim",
  "--accent",
  "--accent-contrast",
  "--ok",
  "--danger",
  "--warn-bg",
  "--error-bg",
  "--tone-blue",
  "--tone-blue-soft",
  "--tone-green",
  "--tone-green-soft",
  "--tone-orange",
  "--tone-orange-soft",
  "--tone-violet",
  "--tone-violet-soft",
  "--tone-pink",
  "--tone-pink-soft",
  "--font-sans",
  "--font-mono",
];

type Theme = { dark: boolean; vars: Record<string, string> };

function readTheme(): Theme {
  const root = document.documentElement;
  const css = getComputedStyle(root);
  const vars: Record<string, string> = {};
  for (const name of THEME_VARS) vars[name] = css.getPropertyValue(name).trim();
  return { dark: root.classList.contains("dark"), vars };
}

/** 跟着 `<html class="dark">` 变。主题只在这一个地方落地（见 lib/theme.ts）。 */
function useTheme(): Theme {
  const [theme, setTheme] = useState(readTheme);
  useEffect(() => {
    const mo = new MutationObserver(() => setTheme(readTheme()));
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
    return () => mo.disconnect();
  }, []);
  return theme;
}

const CSP = [
  "default-src 'none'",
  "script-src 'unsafe-inline' https:",
  "style-src 'unsafe-inline' https:",
  "img-src data: blob: https:",
  "font-src data: https:",
  "media-src data: blob: https:",
  "connect-src 'none'",
  "form-action 'none'",
  "base-uri 'none'",
].join("; ");

function themeCss({ dark, vars }: Theme): string {
  const decls = Object.entries(vars)
    .filter(([, v]) => v)
    .map(([k, v]) => `${k}:${v}`)
    .join(";");
  return `:root{color-scheme:${dark ? "dark" : "light"};${decls}}`;
}

/**
 * 页面里的那一小段桥：回报高度、转交链接、接收主题。
 *
 * 只认来自父窗口的消息 —— 页面里别的脚本伪造不出 `event.source === parent`。
 */
const BRIDGE = `
(function () {
  var post = function (m) { parent.postMessage(Object.assign({ carterBoard: 1 }, m), "*"); };
  // 量 body 而不是 documentElement：后者的 scrollHeight 至少等于视口高，
  // 框一旦被撑高，内容变短也缩不回来。
  var size = function () {
    var b = document.body;
    if (!b) return;
    var cs = getComputedStyle(b);
    var h = b.getBoundingClientRect().height + parseFloat(cs.marginTop) + parseFloat(cs.marginBottom);
    post({ type: "size", h: h });
  };
  addEventListener("load", function () {
    size();
    new ResizeObserver(size).observe(document.body);
  });
  document.addEventListener("click", function (e) {
    var a = e.target && e.target.closest ? e.target.closest("a[href]") : null;
    if (!a) return;
    var href = a.getAttribute("href") || "";
    if (href.charAt(0) === "#") return;
    e.preventDefault();
    post({ type: "open", url: a.href });
  }, true);
  addEventListener("message", function (e) {
    if (e.source !== parent || !e.data || e.data.carterTheme !== 1) return;
    var el = document.getElementById("carter-theme");
    if (el) el.textContent = e.data.css;
  });
})();
`;

/** 第一个 `<head>` / `<html>` / doctype 之后；都没有就放最前面。与 model.rs 的同名函数一致。 */
function insertInHead(html: string, tag: string): string {
  const lower = html.toLowerCase();
  for (const open of ["<head", "<html", "<!doctype"]) {
    let from = 0;
    for (;;) {
      const s = lower.indexOf(open, from);
      if (s < 0) break;
      const next = lower.charAt(s + open.length);
      // `<header>` 不是 `<head>`。
      if (next === ">" || /\s/.test(next)) {
        const e = lower.indexOf(">", s);
        if (e >= 0) return html.slice(0, e + 1) + tag + html.slice(e + 1);
      }
      from = s + open.length;
    }
  }
  return tag + html;
}

/**
 * 组装 srcdoc。CSP 与主题样式都放在 head 的最前面：CSP 要在任何脚本之前生效；
 * 主题用 `:root` 声明变量、`:where(html)` 给默认底色 —— 优先级是零，页面自己写的
 * 任何样式都压得过它。
 */
function compose(html: string, theme: Theme): string {
  const head =
    `<meta http-equiv="Content-Security-Policy" content="${CSP}">` +
    `<style id="carter-theme">${themeCss(theme)}</style>` +
    `<style>:where(html){background:var(--bg);color:var(--text);font-family:var(--font-sans)}` +
    `:where(body){margin:0;padding:20px}</style>` +
    `<script>${BRIDGE}</script>`;
  return insertInHead(html, head);
}

export function BoardFrame({ html, title }: { html: string; title: string }) {
  const theme = useTheme();
  const frame = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(480);

  // 主题不进 srcdoc 的依赖：切主题走 postMessage，不重载页面。组装时用的是那一刻的
  // 主题，页面每次加载完再补发一次当前值，正文换过之后也不会停在旧主题上。
  const themeNow = useRef(theme);
  themeNow.current = theme;
  const srcDoc = useMemo(() => compose(html, themeNow.current), [html]);

  const pushTheme = () =>
    frame.current?.contentWindow?.postMessage(
      { carterTheme: 1, css: themeCss(themeNow.current) },
      "*",
    );
  useEffect(pushTheme, [theme]);

  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      // 只认自己这个框发来的；页面在不透明源里，`origin` 是 "null"，靠 source 判。
      if (e.source !== frame.current?.contentWindow) return;
      const m = e.data as { carterBoard?: number; type?: string; h?: number; url?: string };
      if (!m || m.carterBoard !== 1) return;
      if (m.type === "size" && typeof m.h === "number" && Number.isFinite(m.h)) {
        // 封顶：一个写坏了的页面不该把框撑到几十万像素。
        setHeight(Math.min(Math.max(Math.ceil(m.h), 240), 20000));
      } else if (m.type === "open" && typeof m.url === "string") {
        if (/^https?:\/\//i.test(m.url)) openUrl(m.url).catch(toastError);
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  return (
    <iframe
      ref={frame}
      title={title}
      srcDoc={srcDoc}
      sandbox="allow-scripts"
      onLoad={pushTheme}
      className="block w-full rounded-xl border-0 bg-base"
      style={{ height }}
    />
  );
}
