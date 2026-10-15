// 面板件：模态、卡片、区块、徽章、加载、空态、提示条。
//
// 这些是全应用反复出现的那几个形状，集中在这里是为了让「一个面板长什么样」
// 只有一处定义 —— 以前散在各个页里，同样的卡片在三个文件里是三种写法。

import { useEffect, type ReactNode } from "react";
import { LoaderCircle, X } from "lucide-react";

// ---------------------------------------------------------------------------
// 模态
// ---------------------------------------------------------------------------

export function Modal({
  title,
  onClose,
  children,
  footer,
  width = 520,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  width?: number;
}) {
  // Escape 关闭。挂在 document 上而不是面板上：用户可能正停在某个输入框里。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        // 面板内的点击不该冒泡到背板上去（否则在输入框里选中文字、拖到边缘松手就关了）。
        onClick={(e) => e.stopPropagation()}
        style={{ width }}
        className="flex max-h-[85vh] max-w-[92vw] flex-col rounded-xl border border-edge bg-card p-5 shadow-2xl"
      >
        <h2 className="mb-4 text-base font-semibold">{title}</h2>
        <div className="min-h-0 flex-1 overflow-y-auto pr-1">{children}</div>
        {footer && <div className="mt-4 flex justify-end gap-2">{footer}</div>}
      </div>
    </div>
  );
}

/** 小号确认框。用于不可恢复的动作 —— 一句话 + 两个按钮。 */
export function ConfirmDialog({
  title,
  message,
  confirmLabel = "确认",
  danger = false,
  busy = false,
  onConfirm,
  onCancel,
}: {
  title: string;
  message: ReactNode;
  confirmLabel?: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Modal title={title} onClose={onCancel} width={440}>
      <div className="text-sm leading-relaxed text-dim">{message}</div>
      <div className="mt-4 flex justify-end gap-2">
        <button
          onClick={onCancel}
          className="rounded-md border border-edge px-3 py-1.5 text-sm text-dim transition-colors hover:text-ink"
        >
          取消
        </button>
        <button
          onClick={onConfirm}
          disabled={busy}
          className={`inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition-colors disabled:opacity-40 ${
            danger
              ? "border border-[var(--error-border)] bg-[var(--error-bg)] text-[var(--danger)] hover:opacity-90"
              : "bg-accent text-[var(--accent-contrast)] hover:opacity-90"
          }`}
        >
          {busy && <LoaderCircle size={13} className="animate-spin" />}
          {confirmLabel}
        </button>
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// 卡片 / 区块
// ---------------------------------------------------------------------------

/** 设置页的基本区块：标题 + 内容。 */
export function Section({
  title,
  action,
  children,
  className = "",
}: {
  title?: string;
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={`mb-6 ${className}`}>
      {(title || action) && (
        <div className="mb-3 flex items-center gap-2">
          {title && <h2 className="text-sm font-medium">{title}</h2>}
          {action && <div className="ml-auto">{action}</div>}
        </div>
      )}
      {children}
    </section>
  );
}

/** 一张卡片。给了 `onClick` 就渲染成 button（能聚焦、能用键盘触发）。 */
export function Card({
  children,
  className = "",
  dashed = false,
  onClick,
}: {
  children: ReactNode;
  className?: string;
  dashed?: boolean;
  onClick?: () => void;
}) {
  const cls = `rounded-xl border ${
    dashed ? "border-dashed border-edge" : "border-edge bg-card"
  } px-4 py-3 text-left ${
    onClick ? "w-full transition-colors hover:border-dim" : ""
  } ${className}`;

  if (onClick) {
    return (
      <button type="button" onClick={onClick} className={cls}>
        {children}
      </button>
    );
  }
  return <div className={cls}>{children}</div>;
}

// ---------------------------------------------------------------------------
// 小件
// ---------------------------------------------------------------------------

/**
 * tone → 类名。**必须写成字面量**：`bg-tone-${tone}-soft` 这种拼接 Tailwind 的
 * 扫描器看不见，类不会被生成，颜色会静默失效。
 */
const TONE_CLASS = {
  blue: "bg-tone-blue-soft text-tone-blue",
  green: "bg-tone-green-soft text-tone-green",
  orange: "bg-tone-orange-soft text-tone-orange",
  violet: "bg-tone-violet-soft text-tone-violet",
  pink: "bg-tone-pink-soft text-tone-pink",
} as const;

export function Badge({
  children,
  tone,
  title,
}: {
  children: ReactNode;
  /** 有 tone 时是彩色的状态徽章；不给就是中性的描边 chip。 */
  tone?: keyof typeof TONE_CLASS;
  title?: string;
}) {
  if (!tone) {
    return (
      <span
        title={title}
        className="shrink-0 rounded border border-edge px-1.5 py-0.5 text-[10px] text-dim"
      >
        {children}
      </span>
    );
  }
  return (
    <span
      title={title}
      className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] ${TONE_CLASS[tone]}`}
    >
      {children}
    </span>
  );
}

export function Spinner({ size = 14 }: { size?: number }) {
  return <LoaderCircle size={size} className="animate-spin text-dim" />;
}

/** 一行「加载中…」。 */
export function Loading({ label = "加载中…" }: { label?: string }) {
  return (
    <div className="flex items-center gap-2 py-8 text-sm text-dim">
      <Spinner />
      {label}
    </div>
  );
}

export function EmptyState({
  icon,
  title,
  children,
}: {
  icon?: ReactNode;
  title?: string;
  children?: ReactNode;
}) {
  return (
    <div className="rounded-xl border border-dashed border-edge px-4 py-10 text-center text-sm leading-relaxed text-dim">
      {icon && <div className="mb-2 flex justify-center">{icon}</div>}
      {title && <div className="mb-1 text-ink">{title}</div>}
      {children}
    </div>
  );
}

/** 提示条：错误 / 警告 / 信息。默认不可关闭，需要时给 onClose。 */
export function Banner({
  kind = "info",
  children,
  onClose,
  action,
}: {
  kind?: "info" | "warn" | "error";
  children: ReactNode;
  onClose?: () => void;
  action?: ReactNode;
}) {
  const cls =
    kind === "error"
      ? "border-[var(--error-border)] bg-[var(--error-bg)]"
      : kind === "warn"
        ? "border-[var(--warn-border)] bg-[var(--warn-bg)]"
        : "border-edge bg-card";

  return (
    <div className={`flex items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-relaxed ${cls}`}>
      <div className="min-w-0 flex-1">{children}</div>
      {action}
      {onClose && (
        <button
          onClick={onClose}
          aria-label="关闭"
          className="shrink-0 rounded p-0.5 text-dim transition-colors hover:text-ink"
        >
          <X size={13} />
        </button>
      )}
    </div>
  );
}
