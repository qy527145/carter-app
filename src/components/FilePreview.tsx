// 文件预览弹窗：点击「本轮变更的文件」里的一行时打开。
//
// 两页：
// * **本轮改动** —— 后端用轮首 git 基线算出的 unified diff（见 core 的
//   `changes.rs`）。只有 cwd 在 git 仓库里、且那一轮记下了基线时才有；
//   没有就只显示当前内容，并说明为什么。
// * **当前内容** —— 现在磁盘上的样子（见 core 的 `preview.rs`）。
//
// 文本 / 图片 / 二进制 / 已经没了这些情况后端已经分好类，这里只负责按 `kind` 画。

import { useEffect, useMemo, useState } from "react";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  Check,
  Copy,
  ExternalLink,
  FileText,
  FolderOpen,
  LoaderCircle,
  TriangleAlert,
} from "lucide-react";

import {
  fileDiff,
  previewFile,
  type FileDiff,
  type FilePreview as Preview,
} from "../lib/api";
import { copyText } from "../lib/clipboard";
import { fmtBytes, relTime } from "../lib/format";
import { toastError, TOAST_MS } from "../lib/toast";
import { useStore } from "../store";
import { HtmlFrame, isHtmlFile } from "./Media";
import { Button } from "./ui/Button";
import { Segmented } from "./ui/Input";
import { Modal } from "./ui/Panel";

/** 路径的末段，用作弹窗标题。 */
function baseName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function Meta({ preview }: { preview: Preview }) {
  const bytes = "bytes" in preview ? preview.bytes : null;
  const mtime = "mtime" in preview ? preview.mtime : null;
  const bits = [bytes === null ? null : fmtBytes(bytes), mtime ? `修改于 ${relTime(mtime)}` : null];
  const text = bits.filter(Boolean).join(" · ");
  if (!text) return null;
  return <div className="mb-2 text-[11px] text-dim">{text}</div>;
}

function Notice({ kind, children }: { kind?: "warn" | "error"; children: React.ReactNode }) {
  const tone =
    kind === "error"
      ? "border-[var(--error-border)] bg-[var(--error-bg)]"
      : "border-[var(--warn-border)] bg-[var(--warn-bg)]";
  const icon = kind === "error" ? "text-[var(--danger)]" : "text-tone-orange";
  return (
    <div className={`mb-2 flex items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-relaxed ${tone}`}>
      <TriangleAlert size={13} className={`icon-optical shrink-0 ${icon}`} />
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

/** 文本正文：左侧行号 + 右侧原文。两者在同一个滚动容器里，横向滚动也同步。 */
function Code({ content }: { content: string }) {
  const numbers = useMemo(() => {
    const n = content === "" ? 0 : content.split("\n").length;
    return Array.from({ length: n }, (_, i) => i + 1).join("\n");
  }, [content]);

  return (
    <div className="max-h-[58vh] overflow-auto rounded-lg bg-card">
      <div className="flex min-w-full">
        <pre className="mono shrink-0 select-none border-r border-edge px-2 py-2 text-right text-[11px] leading-[1.65] text-dim/60">
          {numbers}
        </pre>
        <pre className="mono min-w-0 flex-1 whitespace-pre px-3 py-2 text-[11px] leading-[1.65] text-ink">
          {content}
        </pre>
      </div>
    </div>
  );
}

/** unified diff：按行着色。`+++` / `---` 是文件头，不算增删。 */
function DiffCode({ text }: { text: string }) {
  const lines = useMemo(() => text.split("\n"), [text]);
  return (
    <div className="max-h-[58vh] overflow-auto rounded-lg bg-card">
      <pre className="mono min-w-full whitespace-pre py-2 text-[11px] leading-[1.65]">
        {lines.map((line, i) => {
          const cls =
            line.startsWith("+++") || line.startsWith("---")
              ? "text-dim"
              : line.startsWith("+")
                ? "bg-[var(--diff-add-bg)] text-ink"
                : line.startsWith("-")
                  ? "bg-[var(--diff-del-bg)] text-ink"
                  : line.startsWith("@@")
                    ? "text-tone-blue"
                    : "text-dim";
          return (
            <span key={i} className={`block px-3 ${cls}`}>
              {line || " "}
            </span>
          );
        })}
      </pre>
    </div>
  );
}

export function FilePreviewModal({
  path,
  cwd,
  turn,
  onClose,
}: {
  path: string;
  cwd: string;
  /** 这一轮的用户输入序号；没有就只显示当前内容。 */
  turn?: number;
  onClose: () => void;
}) {
  const sessionId = useStore((s) => s.sessionId);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [diff, setDiff] = useState<FileDiff | null>(null);
  const [tab, setTab] = useState<"diff" | "file">("diff");
  // 网页文件在「当前内容」下再分预览 / 源码两页。默认渲染 —— 点开一个 `.html`
  // 想看的通常不是那屏标签。（`HtmlFrame` 的沙箱说明见 `Media.tsx`。）
  const [htmlTab, setHtmlTab] = useState<"render" | "source">("render");
  // 「复制内容」按一下就变对勾，2.5 秒后自己复位。比弹一条 toast 克制 ——
  // 反馈发生在手指落下的那个点上，不用把视线挪到右上角。
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const t = setTimeout(() => setCopied(false), TOAST_MS);
    return () => clearTimeout(t);
  }, [copied]);

  useEffect(() => {
    let cancelled = false;
    setPreview(null);
    setError(null);
    // 换一个文件回到「预览」：上一个是网页、换成一个网页之后该是渲染而不是源码。
    setHtmlTab("render");
    previewFile(cwd || null, path)
      .then((r) => {
        if (!cancelled) setPreview(r);
      })
      .catch((e) => {
        if (!cancelled) setError(typeof e === "string" ? e : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [cwd, path]);

  useEffect(() => {
    setDiff(null);
    setTab("diff");
    if (turn === undefined || !sessionId) {
      setDiff({ kind: "unavailable", path, reason: "这一轮没有 diff 基线" });
      return;
    }
    let cancelled = false;
    fileDiff(sessionId, turn, cwd, path)
      .then((d) => {
        if (!cancelled) setDiff(d);
      })
      .catch((e) => {
        if (!cancelled) {
          setDiff({
            kind: "unavailable",
            path,
            reason: typeof e === "string" ? e : String(e),
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [sessionId, turn, cwd, path]);

  // 没有可用 diff 时不要停在空页面上 —— 直接落到「当前内容」。
  useEffect(() => {
    if (diff?.kind === "unavailable") setTab("file");
  }, [diff]);

  const absolute = preview?.path ?? path;
  const exists =
    preview !== null &&
    preview.kind !== "missing" &&
    preview.kind !== "error" &&
    preview.kind !== "dir";
  const diffUsable = diff?.kind === "diff" || diff?.kind === "empty";
  const copyable =
    tab === "diff" && diff?.kind === "diff"
      ? diff.text
      : preview?.kind === "text"
        ? preview.content
        : null;

  return (
    <Modal
      title={baseName(absolute)}
      onClose={onClose}
      width={780}
      footer={
        <>
          {copyable && (
            <Button
              size="sm"
              onClick={async () => {
                // 成功才打勾；失败时 copyText 自己会 toast。
                if (await copyText(copyable, "已复制")) setCopied(true);
              }}
            >
              {copied ? (
                <Check size={12} className="carter-pop text-[var(--ok)]" />
              ) : (
                <Copy size={12} />
              )}
              {copied ? "已复制" : "复制内容"}
            </Button>
          )}
          {exists && (
            <Button size="sm" onClick={() => revealItemInDir(absolute).catch(toastError)}>
              <FolderOpen size={12} />
              在文件夹中显示
            </Button>
          )}
          {exists && (
            <Button size="sm" onClick={() => openPath(absolute).catch(toastError)}>
              <ExternalLink size={12} />
              用默认应用打开
            </Button>
          )}
          <Button size="sm" variant="primary" onClick={onClose}>
            关闭
          </Button>
        </>
      }
    >
      <div className="mono mb-2 break-all text-[11px] text-dim">{absolute}</div>

      {diffUsable && (
        <div className="mb-3">
          <Segmented
            value={tab}
            onChange={setTab}
            options={[
              { id: "diff", label: "本轮改动" },
              { id: "file", label: "当前内容" },
            ]}
          />
        </div>
      )}

      {/* 不可用时在「当前内容」上方说明原因，而不是让用户以为这一轮没动过。 */}
      {diff?.kind === "unavailable" && (
        <div className="mb-2 text-[11px] text-dim">本轮改动：{diff.reason}</div>
      )}

      {error && <Notice kind="error">{error}</Notice>}

      {!preview && !error && (
        <div className="flex items-center gap-2 py-8 text-sm text-dim">
          <LoaderCircle size={14} className="animate-spin" />
          正在读取…
        </div>
      )}

      {tab === "diff" && diff === null && (
        <div className="flex items-center gap-2 py-8 text-sm text-dim">
          <LoaderCircle size={14} className="animate-spin" />
          正在计算本轮改动…
        </div>
      )}

      {tab === "diff" && diff?.kind === "diff" && (
        <>
          {diff.truncated && <Notice>改动较大，这里只显示前面一部分。</Notice>}
          <DiffCode text={diff.text} />
        </>
      )}

      {tab === "diff" && diff?.kind === "empty" && (
        <div className="flex items-center gap-2 rounded-lg bg-card px-3 py-4 text-sm text-dim">
          这一轮相对轮首没有差异（可能改完又改回去了）。
        </div>
      )}

      {tab === "file" && (
        <>
          {preview?.kind === "text" && (
            <>
              <Meta preview={preview} />
              {preview.truncated && <Notice>文件较大，这里只显示开头 256 KB。</Notice>}
              {/* 网页文件默认给渲染后的样子 —— 点开一个 `.html` 想看的通常不是源码。
                  源码仍然要能翻，所以是两个页签而不是二选一。 */}
              {isHtmlFile(path) ? (
                <div className="mb-3">
                  <Segmented
                    value={htmlTab}
                    onChange={setHtmlTab}
                    options={[
                      { id: "render", label: "预览" },
                      { id: "source", label: "源码" },
                    ]}
                  />
                </div>
              ) : null}
              {isHtmlFile(path) && htmlTab === "render" ? (
                <HtmlFrame
                  html={preview.content}
                  height={Math.round(window.innerHeight * 0.5)}
                  title={baseName(absolute)}
                />
              ) : (
                <Code content={preview.content} />
              )}
            </>
          )}

          {preview?.kind === "image" && (
            <>
              <Meta preview={preview} />
              <div className="flex justify-center rounded-lg bg-card p-3">
                <img
                  src={preview.data_url}
                  alt={baseName(preview.path)}
                  className="max-h-[58vh] max-w-full rounded object-contain"
                />
              </div>
            </>
          )}

          {preview?.kind === "binary" && (
            <>
              <Meta preview={preview} />
              <div className="flex items-center gap-2 rounded-lg bg-card px-3 py-4 text-sm text-dim">
                <FileText size={15} />
                这是二进制文件，没法在这里预览。
              </div>
            </>
          )}

          {preview?.kind === "dir" && (
            <div className="flex items-center gap-2 rounded-lg bg-card px-3 py-4 text-sm text-dim">
              <FolderOpen size={15} />
              这是一个目录。
            </div>
          )}

          {preview?.kind === "missing" && (
            <Notice kind="warn">文件已经不存在了 —— 可能在本轮或之后被删除 / 移走。</Notice>
          )}

          {preview?.kind === "error" && (
            <Notice kind="error">读不了这个文件：{preview.message}</Notice>
          )}
        </>
      )}
    </Modal>
  );
}
