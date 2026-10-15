// toast 渲染层。挂在 App 根部，全应用共用一条通道。

import { X } from "lucide-react";

import { useToastStore, type ToastKind } from "../../lib/toast";

const KIND_CLASS: Record<ToastKind, string> = {
  info: "border-edge bg-card",
  warn: "border-[var(--warn-border)] bg-[var(--warn-bg)]",
  error: "border-[var(--error-border)] bg-[var(--error-bg)]",
};

export function ToastContainer() {
  const toasts = useToastStore((s) => s.toasts);
  const dismiss = useToastStore((s) => s.dismiss);

  if (toasts.length === 0) return null;

  return (
    <div className="pointer-events-none fixed right-4 top-4 z-[60] flex w-80 flex-col gap-2">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`pointer-events-auto flex items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-relaxed shadow-lift ${KIND_CLASS[t.kind]}`}
        >
          <span className="min-w-0 flex-1 break-words">{t.message}</span>
          <button
            onClick={() => dismiss(t.id)}
            aria-label="关闭"
            className="shrink-0 rounded p-0.5 text-dim transition-colors hover:text-ink"
          >
            <X size={12} />
          </button>
        </div>
      ))}
    </div>
  );
}
