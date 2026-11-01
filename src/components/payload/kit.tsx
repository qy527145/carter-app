// 报文面板三视图共用的基础件。
//
// 这一层只管「怎么把一段东西摆得能读」，不管它是什么东西 —— 语义拆解在
// `VisualView`，结构化 JSON 在 `FormattedView`，字节在 `RawView`。
//
// 三个视图都按**同一个方向**切分（请求 / 响应），一次只画一侧：
// 看请求时不必在页面上找「哪里开始是响应」，切页签就是一次换向。

import { useState, type ReactNode } from "react";
import { Check, ChevronDown, ChevronRight, Copy } from "lucide-react";

import { copyText } from "../../lib/clipboard";
import { fmtNumber } from "../../lib/format";
import { TOAST_MS } from "../../lib/toast";
import { IconButton } from "../ui/Button";

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/** 解析成值；不是 JSON 就给 `undefined`（由调用方回退到纯文本）。 */
export function parseJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

// ---------------------------------------------------------------------------
// 复制
// ---------------------------------------------------------------------------

/** 复制一条文本。成功后图标变成对勾，和 toast 用同一个时长自己复位。 */
export function CopyButton({
  text,
  title = "复制",
}: {
  text: string;
  title?: string;
}) {
  const [done, setDone] = useState(false);
  return (
    <IconButton
      title={done ? "已复制" : title}
      aria-label={title}
      onClick={async () => {
        if (!(await copyText(text))) return;
        setDone(true);
        setTimeout(() => setDone(false), TOAST_MS);
      }}
    >
      {done ? <Check size={12} /> : <Copy size={12} />}
    </IconButton>
  );
}

// ---------------------------------------------------------------------------
// 方向外壳（请求 / 响应）
// ---------------------------------------------------------------------------

/**
 * 报文的方向。
 *
 * 三个视图都按它切：一次只看一侧，与「请求 / 响应」页签同一套切法。
 * 看请求时不必在页面上找「哪里开始是响应」—— 切页签就是一次换向。
 */
export type Side = "request" | "response";

/**
 * 请求 / 响应这一块的外壳。「可视化」「格式化」「原始」三页共用它 ——
 * 三页说的是同一份东西的三个切面，边界应该长得一模一样。
 *
 * 默认展开但可整块收起：一次交换的请求体常常是几万字（系统提示词 + 工具 +
 * 整段历史），只想回答「模型说了什么」的人不该被它挡在前面。
 */
export function Direction({
  label,
  headline,
  meta,
  copy,
  defaultOpen = true,
  children,
}: {
  label: string;
  headline: string;
  /** 标题右侧的小胶囊：状态、耗时、收尾原因这类一眼要看的东西。 */
  meta?: ReactNode;
  /** 「复制这一整块」能复制的东西（通常是原始 JSON）。 */
  copy?: string;
  defaultOpen?: boolean;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <section className="overflow-hidden rounded-lg bg-card">
      <div className="flex items-center gap-2 px-2.5 py-2">
        <button
          onClick={() => setOpen((v) => !v)}
          className="flex min-w-0 flex-1 items-center gap-2 text-left transition-colors hover:text-ink"
        >
          {open ? (
            <ChevronDown size={14} className="shrink-0 text-dim" />
          ) : (
            <ChevronRight size={14} className="shrink-0 text-dim" />
          )}
          <span className="shrink-0 text-xs font-semibold text-ink">{label}</span>
          <span className="min-w-0 flex-1 truncate mono text-[11px] text-dim" title={headline}>
            {headline}
          </span>
        </button>
        <div className="flex shrink-0 items-center gap-1.5">{meta}</div>
        {copy !== undefined && <CopyButton text={copy} title={`复制${label}体`} />}
      </div>
      {open && <div className="space-y-2 px-2.5 pb-2.5">{children}</div>}
    </section>
  );
}

/** 标题行里的小胶囊。 */
export function Pill({
  children,
  tone,
}: {
  children: ReactNode;
  tone?: "ok" | "warn" | "bad";
}) {
  const cls =
    tone === "bad"
      ? "bg-[var(--error-bg)] text-[var(--danger)]"
      : tone === "warn"
        ? "bg-warn-bg text-tone-orange"
        : tone === "ok"
          ? "bg-tone-green-soft text-tone-green"
          : "bg-base text-dim";
  return <span className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] ${cls}`}>{children}</span>;
}

// ---------------------------------------------------------------------------
// 折叠分区 / 长文本
// ---------------------------------------------------------------------------

/** 可折叠分区。默认全开，长内容（系统提示词、工具清单）默认收起。 */
export function Section({
  title,
  count,
  defaultOpen = true,
  action,
  children,
}: {
  title: string;
  count?: string;
  defaultOpen?: boolean;
  /** 标题右侧的动作（复制、筛选…）。 */
  action?: ReactNode;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className="overflow-hidden rounded-md bg-base">
      <div className="flex items-center gap-2 pr-1">
        <button
          onClick={() => setOpen((v) => !v)}
          className="flex min-w-0 flex-1 items-center gap-2 px-2.5 py-1.5 text-left text-xs font-medium text-ink transition-colors hover:bg-ink/5"
        >
          {open ? (
            <ChevronDown size={13} className="shrink-0 text-dim" />
          ) : (
            <ChevronRight size={13} className="shrink-0 text-dim" />
          )}
          <span className="truncate">{title}</span>
          {count && <span className="shrink-0 font-normal text-dim">{count}</span>}
        </button>
        {action}
      </div>
      {open && <div className="space-y-2 px-2.5 pb-2.5">{children}</div>}
    </div>
  );
}

/**
 * 长文本：先给一截，展开看全部。
 *
 * **始终给出真实字数** —— 截断了却不说，读的人会以为"就这么多"。
 */
export function LongText({
  text,
  limit = 800,
  className = "",
}: {
  text: string;
  limit?: number;
  className?: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const tooLong = text.length > limit;
  return (
    <div className="space-y-1">
      <pre
        className={`max-h-72 overflow-auto whitespace-pre-wrap break-words rounded bg-base p-2 mono text-[11px] leading-relaxed text-ink ${className}`}
      >
        {expanded || !tooLong ? text : `${text.slice(0, limit)}…`}
      </pre>
      {tooLong && (
        <button
          onClick={() => setExpanded((v) => !v)}
          className="text-[11px] text-dim underline transition-colors hover:text-ink"
        >
          {expanded ? "收起" : `展开全部（共 ${fmtNumber(text.length)} 字）`}
        </button>
      )}
    </div>
  );
}

/**
 * 一段正文：能当 JSON 解析就美化，否则**原样**显示。
 *
 * 非 JSON 的正文是真实存在的（网关的 HTML 错误页、`data: [DONE]` 这种控制帧），
 * 把它们藏起来或强行格式化，都会把"上游到底回了什么"这件事搞丢。
 */
export function JsonOrRaw({ text, label }: { text: string; label?: string }) {
  const parsed = parseJson(text);
  const pretty =
    parsed === undefined ? null : typeof parsed === "string" ? parsed : JSON.stringify(parsed, null, 2);
  return (
    <div className="space-y-1">
      {!pretty && label && <div className="text-[11px] text-dim">{label}</div>}
      <pre className="max-h-[50vh] overflow-auto whitespace-break-spaces rounded bg-base p-2 mono text-[11px] leading-relaxed text-ink">
        {pretty ?? (text.trim() ? text : "（空）")}
      </pre>
    </div>
  );
}

/** 键少值短的信息：一行一个胶囊，比表格省地方。 */
export function Chips({ items }: { items: { key: string; value: string }[] }) {
  if (items.length === 0) return <div className="text-xs text-dim">（无）</div>;
  return (
    <div className="flex flex-wrap gap-1.5">
      {items.map(({ key, value }) => (
        <span
          key={key}
          className="rounded bg-base px-1.5 py-0.5 mono text-[11px]"
          title={`${key}=${value}`}
        >
          <span className="text-dim">{key}=</span>
          <span className="text-ink">{value}</span>
        </span>
      ))}
    </div>
  );
}

/**
 * 头表。敏感头已在后端脱敏（`authorization` → `***`），这里不做二次处理。
 *
 * 只给一份可复制的整表 —— 逐条复制头很少是想要的，要拷就是拷一整块。
 */
export function HeadersTable({ headers }: { headers: [string, string][] }) {
  if (headers.length === 0) return <div className="text-xs text-dim">（无）</div>;
  const text = headers.map(([k, v]) => `${k}: ${v}`).join("\n");
  return (
    <div className="space-y-1">
      <div className="flex justify-end">
        <CopyButton text={text} title="复制全部请求头 / 响应头" />
      </div>
      <div className="overflow-hidden rounded border border-edge-strong/60">
        <table className="w-full border-collapse text-xs">
          <tbody>
            {headers.map(([k, v], i) => (
              <tr key={`${k}-${i}`} className={i % 2 ? "bg-base" : ""}>
                <td className="w-40 border-r border-edge px-2 py-1 align-top mono text-dim">
                  {k}
                </td>
                <td className="break-all px-2 py-1 mono text-ink">{v}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}