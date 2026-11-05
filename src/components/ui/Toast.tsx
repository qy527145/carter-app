// toast 渲染层。挂在 App 根部，全应用共用一条通道。

import { CircleAlert, CircleCheck, TriangleAlert, X } from "lucide-react";
import type { ReactNode } from "react";

import { type ToastKind, useToastStore } from "../../lib/toast";

const KIND_CLASS: Record<ToastKind, string> = {
  // info 不是「没有状态」，它是底色上浮起来的一层：给底色而不是描边，
  // 和全站去边框的方向一致。内衬一圈 ring 是给深色主题的 —— 那里阴影托不起来。
  info: "bg-elevated ring-1 ring-edge",
  warn: "border border-[var(--warn-border)] bg-[var(--warn-bg)]",
  error: "border border-[var(--error-border)] bg-[var(--error-bg)]",
};

const KIND_ICON: Record<ToastKind, ReactNode> = {
  info: <CircleCheck size={13} className="text-[var(--ok)]" />,
  warn: <TriangleAlert size={13} className="text-tone-orange" />,
  error: <CircleAlert size={13} className="text-[var(--danger)]" />,
};

export function ToastContainer() {
  const toasts = useToastStore((s) => s.toasts);
  const dismiss = useToastStore((s) => s.dismiss);
  const pause = useToastStore((s) => s.pause);
  const resume = useToastStore((s) => s.resume);

  if (toasts.length === 0) return null;

  return (
    <div
      role="status"
      aria-live="polite"
      onMouseEnter={pause}
      onMouseLeave={resume}
      className="pointer-events-none fixed right-4 top-4 z-[60] flex w-80 flex-col gap-2"
    >
      {toasts.map((t) => (
        <div
          key={t.id}
          data-leaving={t.leaving ? "true" : undefined}
          className={`carter-toast group pointer-events-auto flex items-start gap-2 rounded-lg px-3 py-2 text-xs leading-relaxed shadow-lift ${KIND_CLASS[t.kind]}`}
        >
          <span className="icon-optical mt-px shrink-0">{KIND_ICON[t.kind]}</span>
          <span className="min-w-0 flex-1 break-words">
            {t.message}
            {t.count > 1 && <span className="ml-1.5 text-dim">×{t.count}</span>}
            {t.action && (
              <button
                onClick={() => {
                  const run = t.action?.run;
                  dismiss(t.id);
                  run?.();
                }}
                className="carter-press mt-1.5 block rounded-md bg-ink px-2 py-0.5 text-[11px] font-medium text-base outline-none hover:opacity-85"
              >
                {t.action.label}
              </button>
            )}
          </span>
          <button
            onClick={() => dismiss(t.id)}
            aria-label="关闭"
            className="shrink-0 rounded p-0.5 text-dim opacity-60 transition-opacity hover:text-ink group-hover:opacity-100"
          >
            <X size={12} />
          </button>
        </div>
      ))}
    </div>
  );
}
