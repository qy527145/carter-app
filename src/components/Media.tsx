// 对话里「看得见的东西」：本地图片、内联 HTML。
//
// ## 为什么不能直接用 markdown 的 `<img>`
//
// 助手正文里的 `![](chart.png)` 是按**会话 cwd** 写的相对路径，而界面跑在
// `tauri://localhost` 上 —— webview 会去它自己的根目录找，永远 404。所以本地图片
// 必须经后端读成 data URL（`preview_file`，见 core 的 `preview.rs`），与文件预览
// 走同一条路。远端地址与 data URL 则原样交给 `<img>`。
//
// `[img:<hash>.<ext>]` 是另一种形态：它指向 carter 的内容寻址图片库，由 `read_image`
// 取字节（`read_file` 读图、粘贴进来的图都是这个形态）。
//
// 解析结果按「cwd + src」缓存：流式时正文每几十毫秒重解析一遍，不缓存的话同一张图
// 会被反复读盘、反复 base64，界面会一直闪。
//
// ## HTML 为什么要沙箱
//
// 预览的是**模型生成的任意 HTML**。这里用 `srcdoc` + 不带 `allow-same-origin` 的
// sandbox：脚本能跑（图表库要跑）、能发网络请求，但它是一个不透明源，拿不到父页面的
// DOM / localStorage，也拿不到 Tauri IPC（IPC 只在主框架注入，见 tauri 的
// `for_main_frame_only`）。代价是页面里用到的 localStorage 会抛错 —— 这是刻意的
// 取舍：一段模型输出不该有和主界面同等的权限。
//
// 相对的 `<script src="./chart.js">` 在 `srcdoc` 里没有基准地址，会加载不到；这类页面
// 用「在浏览器中打开」看原文件。图表通常自带 CDN 或全部内联，不受影响。

import {
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
  type ReactElement,
  type ReactNode,
} from "react";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { defaultUrlTransform } from "react-markdown";
import { Code, ExternalLink, LoaderCircle, Maximize2 } from "lucide-react";

import { previewFile, readImage } from "../lib/api";
import { toastError } from "../lib/toast";
import { IconButton } from "./ui/Button";
import { Segmented } from "./ui/Input";
import { Modal } from "./ui/Panel";

// ---------------------------------------------------------------------------
// 地址
// ---------------------------------------------------------------------------

/** 图片库引用走这个私有协议，`<img>` 那边据此去 `read_image` 取字节。 */
const IMG_TOKEN_SCHEME = "carter-img:";

const EXTERNAL = /^(?:https?:|mailto:|tel:)/i;

/** 是不是一个需要按 cwd 解析的本地路径（远端、data URL、图片库引用都不算）。 */
function isLocalPath(src: string): boolean {
  return !EXTERNAL.test(src) && !src.startsWith("data:") && !src.startsWith(IMG_TOKEN_SCHEME);
}

/** 相对路径按会话 cwd 拼成绝对路径；绝对路径与 `~` 原样交给后端展开。 */
function joinPath(cwd: string, p: string): string {
  const s = p.trim();
  if (/^(?:[A-Za-z]:[\\/]|[\\/]|~)/.test(s)) return s;
  const base = cwd.replace(/[\\/]+$/, "");
  return base ? `${base}/${s}` : s;
}

/** markdown 地址里的 `file://` 前缀与百分号编码还原掉 —— 后端要的是裸路径。 */
function cleanLocalSrc(src: string): string {
  let s = src.trim();
  if (s.startsWith("file://")) s = s.slice("file://".length);
  try {
    s = decodeURI(s);
  } catch {
    // 编码坏了就原样用，让后端报真正的原因。
  }
  return s;
}

function baseName(p: string): string {
  return p.split(/[\\/]/).pop() || p;
}

/** 渲染得出来的网页 / 图 —— 工具卡片与链接处理都用这一个判据。 */
export function isHtmlFile(p: string): boolean {
  return /\.(?:html?|svg)$/i.test(p.split(/[?#]/)[0]);
}

/** 单张矢量图：预览走 [`SvgCanvas`]，不套沙箱 iframe。 */
export function isSvgPath(p: string): boolean {
  return /\.svg$/i.test(p.split(/[?#]/)[0]);
}

/** 结果正文里的第一个图片库引用（`read_file` 读图时那串 `[img:…]`）。 */
export function imageTokenIn(text: string): string | null {
  return /\[img:([0-9a-fA-F]+\.[A-Za-z0-9]+)\]/.exec(text)?.[1] ?? null;
}

/**
 * markdown 的 url 白名单。
 *
 * react-markdown 默认只放行 `http/https/ircs/mailto/xmpp` 与相对地址：`carter-img:`
 * 会被清成空串，`data:image/…`（内联图片）与 `C:\x.png` 这种 Windows 绝对路径也会
 * （它们长得像带协议）。这几种放行，其余仍交给默认实现 —— `javascript:` 之类不能
 * 从这里漏过去。
 */
export function transformMarkdownUrl(url: string): string {
  if (url.startsWith(IMG_TOKEN_SCHEME)) return url;
  if (url.startsWith("data:image/")) return url;
  if (/^[A-Za-z]:[\\/]/.test(url) || url.startsWith("file://")) return url;
  return defaultUrlTransform(url);
}

// ---------------------------------------------------------------------------
// 图片
// ---------------------------------------------------------------------------

type Resolved =
  | { kind: "url"; url: string; path: string | null }
  | { kind: "error"; error: string; path: string | null };

/**
 * 解析结果缓存。键是「cwd + src」：同一个相对路径在不同会话里是两张图。
 *
 * 按条目数封顶而不是按字节：一张 12 MB 的图转成 data URL 是十几 MB 的字符串，
 * 不封顶会在长会话里把内存吃光。
 *
 * 条目带时间戳、**过期就重读**：模型经常把重新画过的图写回同一个 `chart.png`，
 * 只按路径缓存的话，界面会一直停在旧图上，而这看起来完全像「它没改」。
 * 已经挂着的组件不会因为过期而重跑（依赖没变），重新挂载时才生效 —— 那正好是
 * 「切走再切回来」这种愿意多读一次的时机。
 */
const CACHE_MAX = 48;
const CACHE_TTL_MS = 15_000;
const CACHE = new Map<string, { value: Resolved; at: number }>();
const INFLIGHT = new Map<string, Promise<Resolved>>();

function cacheKey(cwd: string, src: string): string {
  return `${cwd}\u0000${src}`;
}

function cached(key: string): Resolved | null {
  const hit = CACHE.get(key);
  if (!hit) return null;
  if (Date.now() - hit.at > CACHE_TTL_MS) {
    CACHE.delete(key);
    return null;
  }
  return hit.value;
}

function remember(key: string, value: Resolved): Resolved {
  CACHE.set(key, { value, at: Date.now() });
  if (CACHE.size > CACHE_MAX) {
    const oldest = CACHE.keys().next().value;
    if (oldest !== undefined) CACHE.delete(oldest);
  }
  return value;
}

async function resolveImage(cwd: string, src: string): Promise<Resolved> {
  if (src.startsWith(IMG_TOKEN_SCHEME)) {
    const url = await readImage(src.slice(IMG_TOKEN_SCHEME.length));
    return url
      ? { kind: "url", url, path: null }
      : { kind: "error", error: "图片库里没有这张图", path: null };
  }
  if (!isLocalPath(src)) return { kind: "url", url: src, path: null };

  const p = await previewFile(cwd || null, cleanLocalSrc(src));
  switch (p.kind) {
    case "image":
      return { kind: "url", url: p.data_url, path: p.path };
    case "missing":
      return { kind: "error", error: "文件已经不在了", path: p.path };
    case "error":
      return { kind: "error", error: p.message, path: p.path };
    case "dir":
      return { kind: "error", error: "这是一个目录", path: p.path };
    default:
      return { kind: "error", error: "这不是图片", path: p.path };
  }
}

function useImage(cwd: string, src: string): Resolved | null {
  const key = cacheKey(cwd, src);
  const [state, setState] = useState<Resolved | null>(() => cached(key));

  useEffect(() => {
    const hit = cached(key);
    if (hit) {
      setState(hit);
      return;
    }
    setState(null);
    let cancelled = false;
    // 同一张图可能同时挂在好几处（正文 + 工具卡片）；并发的两次读取合成一次。
    let task = INFLIGHT.get(key);
    if (!task) {
      task = resolveImage(cwd, src).finally(() => INFLIGHT.delete(key));
      INFLIGHT.set(key, task);
    }
    task
      .then((r) => {
        remember(key, r);
        if (!cancelled) setState(r);
      })
      .catch((e) => {
        // 读失败也记下来：同一张坏图不该每次重渲染都再试一遍。
        const r: Resolved = { kind: "error", error: String(e), path: null };
        remember(key, r);
        if (!cancelled) setState(r);
      });
    return () => {
      cancelled = true;
    };
  }, [cwd, src, key]);

  return state;
}

function Loading() {
  return (
    <span className="my-1 inline-flex items-center gap-1.5 rounded border border-dashed border-edge px-2 py-1 text-[11px] text-dim">
      <LoaderCircle size={11} className="animate-spin" />
      读取中…
    </span>
  );
}

/** 读不出来时给一条说清原因的占位，而不是一个破图标。 */
function Unavailable({
  label,
  detail,
  path,
}: {
  label: string;
  detail: string;
  path: string | null;
}) {
  return (
    <span className="my-1 flex max-w-full items-start gap-1.5 rounded border border-dashed border-edge px-2 py-1 text-[11px] leading-relaxed text-dim">
      <Code size={11} className="icon-optical shrink-0" />
      <span className="min-w-0">
        <span className="block truncate text-ink" title={path ?? label}>
          {label}
        </span>
        <span>
          {detail}
          {path && <span className="mono ml-1 break-all">{path}</span>}
        </span>
      </span>
    </span>
  );
}

function Lightbox({
  url,
  alt,
  path,
  onClose,
}: {
  url: string;
  alt: string;
  path: string | null;
  onClose: () => void;
}) {
  return (
    <Modal
      title={alt || "图片"}
      width={980}
      onClose={onClose}
      footer={
        <>
          {path && (
            <button
              onClick={() => openPath(path).catch(toastError)}
              className="inline-flex items-center gap-1 rounded-md bg-ink/5 px-2 py-1 text-xs text-dim transition-colors hover:bg-ink/10 hover:text-ink"
            >
              <ExternalLink size={12} />
              用默认应用打开
            </button>
          )}
          <button
            onClick={onClose}
            className="rounded-md bg-accent px-2 py-1 text-xs font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90"
          >
            关闭
          </button>
        </>
      }
    >
      <img src={url} alt={alt} className="mx-auto block max-h-[76vh] max-w-full object-contain" />
    </Modal>
  );
}

/** 一张可点开放大的图 —— 正文与工具卡片共用。 */
function ZoomableImage({
  url,
  alt,
  path,
  className,
  style,
}: {
  url: string;
  alt: string;
  path: string | null;
  className: string;
  style?: CSSProperties;
}) {
  const [zoom, setZoom] = useState(false);
  return (
    <>
      <img
        src={url}
        alt={alt}
        title={alt || "点击放大"}
        style={style}
        onClick={() => setZoom(true)}
        className={className}
      />
      {zoom && (
        <Lightbox url={url} alt={alt || "图片"} path={path} onClose={() => setZoom(false)} />
      )}
    </>
  );
}

/**
 * 助手正文里的一张图。
 *
 * `src` 三种形态：`carter-img:<hash>.<ext>`（图片库引用）、远端 / data URL、本地路径
 * （按会话 cwd 解析）。
 */
export function MarkdownImage({
  src,
  alt,
  cwd,
}: {
  src: string;
  alt?: string;
  cwd: string;
}) {
  const img = useImage(cwd, src);
  // 没写 src 的 `<img>`（模型偶尔会这么写）不该变成一个「这不是图片」的占位。
  if (!src.trim()) return null;
  if (!img) return <Loading />;
  if (img.kind === "error") {
    return <Unavailable label={alt || baseName(src)} detail={img.error} path={img.path} />;
  }
  return (
    <ZoomableImage
      url={img.url}
      alt={alt ?? ""}
      path={img.path}
      className="my-2 block max-h-[30rem] max-w-full cursor-zoom-in rounded-lg object-contain"
    />
  );
}

/**
 * 工具结果里的图片库引用。
 *
 * 只有 `read_file` 读图会产出 `[img:...]`，其余工具的结果里不会有图片。
 */
export function StoredImage({ token, alt }: { token: string; alt?: string }) {
  const img = useImage("", `${IMG_TOKEN_SCHEME}${token}`);
  if (!img) return <Loading />;
  if (img.kind === "error") {
    return <Unavailable label={alt || token} detail={img.error} path={null} />;
  }
  return (
    <ZoomableImage
      url={img.url}
      alt={alt ?? ""}
      path={null}
      className="my-1 block max-h-64 max-w-full cursor-zoom-in rounded-md object-contain"
    />
  );
}

// ---------------------------------------------------------------------------
// HTML
// ---------------------------------------------------------------------------

/** 沙箱：脚本能跑、能弹窗、能提交表单，但**没有**同源 —— 见文件头。 */
const SANDBOX = "allow-scripts allow-forms allow-popups allow-modals";

/** 这段 HTML 是不是一份完整文档（决定默认落在预览还是源码）。 */
function isHtmlDocument(html: string): boolean {
  const head = html.slice(0, 2048).toLowerCase();
  return /<!doctype|<html[\s>]|<body[\s>]|<svg[\s>]/.test(head);
}

/**
 * 「这段东西拿出来是不是给人看的」。
 *
 * 判据是**它有没有成一个页面的意思**：完整文档、带脚本 / 样式 / 矢量图、或者够长。
 * 一段两行的 `<button>` 是教程里的例子，默认展开成预览会平白多出一块空白；
 * 一份几百字符的 HTML 则几乎只可能是「给我做个页面」。
 */
function preferPreview(html: string): boolean {
  if (isHtmlDocument(html)) return true;
  if (/<script[\s>]|<style[\s>]|<svg[\s>]|<canvas[\s>]/i.test(html)) return true;
  return html.length > 300;
}

/**
 * 一段 HTML 的沙箱窗口。`FilePreview` 也复用这一个 —— 文件预览里的 `.html`
 * 与对话里的代码块要长得一样、用的是同一份沙箱策略。
 */
export function HtmlFrame({
  html,
  height,
  title,
}: {
  html: string;
  height: number;
  title: string;
}): ReactElement {
  return (
    // 白底是刻意的：生成的页面默认假设白纸，跟着应用深色走会把黑字吞掉。
    <div className="overflow-hidden rounded-lg border border-edge bg-white" style={{ height }}>
      <iframe title={title} srcDoc={html} sandbox={SANDBOX} className="size-full border-0" />
    </div>
  );
}

/**
 * `svg` 代码块**不走 iframe**。
 *
 * 沙箱 iframe 有三处不适合画矢量图：
 *
 * - **它不知道自己该多大**。iframe 只能给固定高度（380），而一段 `svg` 的宽高比
 *   是写在 `viewBox` 里的 —— 400×320 的画贴在 900×380 的白框左上角，右边和下面
 *   是一大片空。要贴合就得先解析出比例，那 iframe 的唯一优势也没了。
 * - **它是一整个文档**（HTML 解析器 + 样式 + 脚本引擎）。一段几十行的图从挂载到
 *   画出来要走完整条路径，而 `<img>` 拿到的是一张「图片」。
 * - **它默认能跑脚本**。模型生成的标记里本来就不该有脚本，但 `onload=` 这种属性
 *   只要一个 `<img>` 就会闭嘴 —— 以图片方式加载的 SVG 不执行脚本、不加载外部资源，
 *   比 `allow-scripts` 的沙箱更严。
 *
 * 代价：`<image href="…">`、外部字体在图中不会加载。画一张图用不到这些；真需要
 * 交互的整页 HTML 仍然走 `HtmlFrame`。
 */

/** 从 `<svg>` 开标签里取出宽高比（`viewBox` 优先，其次 width/height）。 */
function svgRatio(svg: string): number {
  const tag = /<svg\b[^>]*>/i.exec(svg)?.[0] ?? "";
  const vb = /viewBox\s*=\s*["']\s*([-\d.]+)[\s,]+([-\d.]+)[\s,]+([-\d.]+)[\s,]+([-\d.]+)/i.exec(tag);
  if (vb) {
    const w = Number(vb[3]);
    const h = Number(vb[4]);
    if (w > 0 && h > 0) return w / h;
  }
  const w = /(?:^|\s)width\s*=\s*["']([\d.]+)(?:px)?["']/i.exec(tag)?.[1];
  const h = /(?:^|\s)height\s*=\s*["']([\d.]+)(?:px)?["']/i.exec(tag)?.[1];
  if (w && h && Number(w) > 0 && Number(h) > 0) return Number(w) / Number(h);
  return 1;
}

/**
 * 套进 data URL。
 *
 * 少了 `xmlns` 的 `<svg>` 作为图片解码时会被当成不认识的外来元素、画出来是空白 ——
 * 而模型写片段时经常省掉它，所以这里补一次（已经有就不动）。
 */
function svgDataUrl(svg: string): string {
  const src = svg.replace(/<svg\b(?![^>]*\bxmlns\s*=)/i, '<svg xmlns="http://www.w3.org/2000/svg"');
  // `#` 在 data URL 里是片段分隔符，不编码的话 `fill="#fff"` 会把后面全截掉。
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(src)}`;
}

/**
 * 一张 SVG 的画布。
 *
 * 容器按图片自身的比例收窄、`mx-auto` 居中，并封顶高度 —— 一张方图不会撑成一条
 * 横带，一张宽图也不会被拉高。宽度 `100%` 只在超过上限时才生效，所以小图保持
 * 原始尺寸、不被放大糊掉。
 */
export function SvgCanvas({
  svg,
  maxHeight = 420,
  className = "",
}: {
  svg: string;
  /** 画布最高的像素数。全屏时给更大的值。 */
  maxHeight?: number;
  className?: string;
}): ReactElement {
  const ratio = useMemo(() => svgRatio(svg), [svg]);
  const src = useMemo(() => svgDataUrl(svg), [svg]);
  const [failed, setFailed] = useState(false);

  if (failed) {
    // 解码不了（标记坏到连图片都画不出来）时退回源码，而不是留一块空白。
    return (
      <div className="max-h-[26rem] overflow-auto">
        <pre className="carter-code mono">{svg}</pre>
      </div>
    );
  }

  return (
    // 白底是刻意的：生成的图按白纸配色，跟着应用深色走会把深色描边吞掉。
    // 外面一层负责居中，里面一层按图收边 —— 小图不该左右各留半屏白。
    <div className={`flex justify-center ${className}`}>
      <div className="inline-flex max-w-full items-center justify-center rounded-lg border border-edge bg-white p-4">
        <img
          src={src}
          alt=""
          style={{ maxHeight, aspectRatio: String(ratio) }}
          className="block h-auto w-auto max-w-full object-contain"
          onError={() => setFailed(true)}
        />
      </div>
    </div>
  );
}

/**
 * 「这段代码块已经写完整了」。
 *
 * 流式期间前半截是半张图，画出来只会闪；但**不必等到整条消息写完** —— 闭合标签
 * 一出现，这一段就不会再变了（后续增量永远接在它后面，重新解析出来还是同一段
 * 字符串），此时挂预览是安全的，而且正是用户等的那个时刻。
 */
function blockComplete(lang: string, src: string): boolean {
  const t = src.trimEnd();
  if (lang === "svg") return /<\/svg>$/i.test(t);
  return /<\/(?:html|body)>$/i.test(t);
}

/**
 * 对话里的 HTML 卡片：预览 / 源码两个页签 + 全屏。
 *
 * 流式期间（`live`）只给源码：`srcDoc` 每来一个增量就整页重载，图表会闪成一片。
 * 等这一段写完了再让人看效果。
 */
export function HtmlArtifact({
  html,
  title,
  absPath,
  live = false,
  truncated = false,
  svg = false,
}: {
  html: string;
  /** 显示用的名字（文件名 / 语言）。 */
  title: string;
  /** 磁盘上的绝对路径。有它才能「在浏览器中打开」。 */
  absPath?: string | null;
  live?: boolean;
  truncated?: boolean;
  /** 内容是 `svg`：预览走 `SvgCanvas`（居中、按自身比例），不走沙箱 iframe。 */
  svg?: boolean;
}) {
  const [tab, setTab] = useState<"preview" | "source">(preferPreview(html) ? "preview" : "source");
  const [full, setFull] = useState(false);
  const mode = live ? "source" : tab;

  return (
    <div className="my-3 overflow-hidden rounded-lg bg-card">
      <div className="flex items-center gap-2 px-2 py-1.5">
        <Code size={12} className="shrink-0 text-tone-violet" />
        <span className="mono min-w-0 flex-1 truncate text-[11px] text-dim" title={title}>
          {title}
        </span>
        {live ? (
          <span className="shrink-0 text-[11px] text-dim">生成中…</span>
        ) : (
          <Segmented
            value={mode}
            onChange={setTab}
            options={[
              { id: "preview", label: "预览" },
              { id: "source", label: "源码" },
            ]}
          />
        )}
        <IconButton title="全屏预览" disabled={live} onClick={() => setFull(true)}>
          <Maximize2 size={12} />
        </IconButton>
        {absPath && (
          <IconButton title="在浏览器中打开" onClick={() => openPath(absPath).catch(toastError)}>
            <ExternalLink size={12} />
          </IconButton>
        )}
      </div>
      {truncated && (
        <div className="px-2 pb-1 text-[11px] text-tone-orange">
          文件较大，这里只读了前面一段，预览可能不完整。
        </div>
      )}
      {mode === "preview" ? (
        svg ? (
          <SvgCanvas svg={html} maxHeight={380} />
        ) : (
          <HtmlFrame html={html} height={380} title={title} />
        )
      ) : (
        // 与正文里的代码块同一套深色样式（`.carter-code`，见 index.css）——
        // 这里可能挂在工具卡片里、不在 `.carter-md` 内，所以走独立入口。
        <div className="max-h-[26rem] overflow-auto">
          <pre className="carter-code mono">{html}</pre>
        </div>
      )}
      {full && (
        <Modal
          title={title}
          width={1020}
          onClose={() => setFull(false)}
          footer={
            <>
              {absPath && (
                <button
                  onClick={() => openPath(absPath).catch(toastError)}
                  className="inline-flex items-center gap-1 rounded-md bg-ink/5 px-2 py-1 text-xs text-dim transition-colors hover:bg-ink/10 hover:text-ink"
                >
                  <ExternalLink size={12} />
                  在浏览器中打开
                </button>
              )}
              <button
                onClick={() => setFull(false)}
                className="rounded-md bg-accent px-2 py-1 text-xs font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90"
              >
                关闭
              </button>
            </>
          }
        >
          {svg ? (
            <SvgCanvas svg={html} maxHeight={Math.round(window.innerHeight * 0.66)} />
          ) : (
            <HtmlFrame html={html} height={Math.round(window.innerHeight * 0.66)} title={title} />
          )}
        </Modal>
      )}
    </div>
  );
}

/** 正文里链接到本地 HTML 文件时的弹窗预览。相对路径按会话 cwd 解析。 */
function HtmlFileModal({ path, cwd, onClose }: { path: string; cwd: string; onClose: () => void }) {
  const [state, setState] = useState<{
    html: string | null;
    abs: string | null;
    note: string | null;
  }>({ html: null, abs: null, note: null });
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    previewFile(cwd || null, cleanLocalSrc(path))
      .then((p) => {
        if (cancelled) return;
        if (p.kind === "text") {
          setState({
            html: p.content,
            abs: p.path,
            note: p.truncated ? "文件较大，这里只读了前面一段，预览可能不完整。" : null,
          });
        } else {
          setState({
            html: null,
            abs: "path" in p ? p.path : null,
            note: p.kind === "missing" ? "文件已经不在了。" : "这不是文本文件，渲染不了。",
          });
        }
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [cwd, path]);

  return (
    <Modal
      title={baseName(path)}
      width={1020}
      onClose={onClose}
      footer={
        <>
          {state.abs && (
            <button
              onClick={() => openPath(state.abs!).catch(toastError)}
              className="inline-flex items-center gap-1 rounded-md bg-ink/5 px-2 py-1 text-xs text-dim transition-colors hover:bg-ink/10 hover:text-ink"
            >
              <ExternalLink size={12} />
              在浏览器中打开
            </button>
          )}
          <button
            onClick={onClose}
            className="rounded-md bg-accent px-2 py-1 text-xs font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90"
          >
            关闭
          </button>
        </>
      }
    >
      {error && <div className="text-xs text-[var(--danger)]">读不了这个文件：{error}</div>}
      {!state.html && !state.note && !error && <Loading />}
      {state.note && <div className="text-xs text-dim">{state.note}</div>}
      {state.html !== null &&
        (isSvgPath(path) ? (
          <SvgCanvas svg={state.html} maxHeight={Math.round(window.innerHeight * 0.62)} />
        ) : (
          <HtmlFrame
            html={state.html}
            height={Math.round(window.innerHeight * 0.62)}
            title={path}
          />
        ))}
    </Modal>
  );
}

/** 工具卡片用：这个路径是不是一份能渲染的网页。 */
export function looksLikeHtml(path: string): boolean {
  return isHtmlFile(path);
}

/**
 * 工具卡片里的「这个 HTML 文件长什么样」。
 *
 * 读的是**现在磁盘上的内容**（与文件预览同一个后端入口）：`write_file` 的 `content`
 * 参数只有一半的时候（比如 `edit_file` 只改了一段）也不至于画出一份残缺的页面。
 */
export function HtmlFilePreview({ path, cwd }: { path: string; cwd: string }) {
  const [state, setState] = useState<{ html: string; abs: string; truncated: boolean } | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    previewFile(cwd || null, path)
      .then((p) => {
        if (cancelled) return;
        if (p.kind === "text") {
          setState({ html: p.content, abs: p.path, truncated: p.truncated });
        } else {
          setError(p.kind === "missing" ? "文件已经不在了" : "这不是文本文件");
        }
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [cwd, path]);

  if (error) return <Unavailable label={baseName(path)} detail={error} path={null} />;
  if (!state) return <Loading />;
  return (
    <HtmlArtifact
      html={state.html}
      title={baseName(path)}
      absPath={state.abs}
      truncated={state.truncated}
      svg={isSvgPath(path)}
    />
  );
}

/**
 * 正文里的链接。
 *
 * 桌面应用里点链接**不能让 webview 跳走**（那会把整个界面换成一张网页，退不回来）。
 * 所以按目标分三路：
 *
 * * 外链 → 系统浏览器；
 * * 本地 `.html` / `.svg` → 就地弹出渲染后的预览 —— 这正是「写一份 HTML 报告给我」的
 *   落地形态；
 * * 别的本地路径 → 交给系统默认程序打开。
 */
export function MarkdownLink({
  href,
  cwd,
  children,
}: {
  href?: string;
  cwd: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const cls = "text-tone-blue underline underline-offset-2";

  if (!href || href.startsWith("#")) return <span>{children}</span>;

  if (EXTERNAL.test(href)) {
    return (
      <a
        href={href}
        className={cls}
        onClick={(e) => {
          e.preventDefault();
          openUrl(href).catch(toastError);
        }}
      >
        {children}
      </a>
    );
  }

  if (isHtmlFile(href)) {
    return (
      <>
        <button
          type="button"
          onClick={() => setOpen(true)}
          title={`预览 ${href}`}
          className={`inline-flex cursor-pointer items-baseline gap-0.5 ${cls}`}
        >
          {children}
          <ExternalLink size={11} className="icon-optical" />
        </button>
        {open && <HtmlFileModal path={href} cwd={cwd} onClose={() => setOpen(false)} />}
      </>
    );
  }

  return (
    <button
      type="button"
      onClick={() => openPath(joinPath(cwd, cleanLocalSrc(href))).catch(toastError)}
      title={`用默认程序打开 ${href}`}
      className={`inline-flex cursor-pointer items-baseline gap-0.5 ${cls}`}
    >
      {children}
      <ExternalLink size={11} className="icon-optical" />
    </button>
  );
}

// ---------------------------------------------------------------------------
// markdown 代码块
// ---------------------------------------------------------------------------

/** 能内联渲染的代码块语言。 */
const RENDERABLE = new Set(["html", "htm", "svg"]);

/** `<code>` 的 children 摊平成一段字符串（hast 已经把转义还原过了）。 */
function textOf(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (typeof node === "object" && "props" in node) {
    return textOf((node as { props?: { children?: ReactNode } }).props?.children);
  }
  return "";
}

/**
 * 代码块的呈现。
 *
 * 普通代码块照旧是深色 `<pre>`（样式在 `.carter-md pre` 里）。`html` / `svg` 这种
 * **能直接看效果的**换成 [`HtmlArtifact`]：写一份图表页出来，用户要的是那张图，
 * 不是那一屏标签。
 *
 * 只看 `<code>` 上的 `language-*` 类名，不猜内容 —— 普通代码块里出现一个 `<div>`
 * 不该变成预览。
 */
export function MarkdownPre({ children, live }: { children?: ReactNode; live: boolean }) {
  const el = children as { props?: { className?: string; children?: ReactNode } } | null;
  const lang = /language-([\w-]+)/.exec(el?.props?.className ?? "")?.[1]?.toLowerCase();
  if (lang && RENDERABLE.has(lang)) {
    const html = textOf(el?.props?.children);
    // 不必等整条消息写完：闭合标签一出现就挂预览（见 `blockComplete`）。这之前
    // 照旧画普通代码块 —— 半张图画出来只会闪。`live={false}` 是给卡片本身的：
    // 它已经稳定，页签与全屏都该能用了。
    if (!live || blockComplete(lang, html)) {
      return <HtmlArtifact html={html} title={lang.toUpperCase()} live={false} svg={lang === "svg"} />;
    }
  }
  return <pre>{children}</pre>;
}