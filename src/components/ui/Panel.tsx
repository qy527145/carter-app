// 面板件：模态、卡片、区块、徽章、加载、空态、提示条。
//
// 这些是全应用反复出现的那几个形状，集中在这里是为了让「一个面板长什么样」
// 只有一处定义 —— 以前散在各个页里，同样的卡片在三个文件里是三种写法。

import { useEffect, useRef, type ReactNode } from "react";
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
  const bodyRef = useRef<HTMLDivElement>(null);
  // 按下时是否落在背板上。只有「按下与松开都在背板」才算点背板 ——
  // 在输入框里拖选文字、拖到面板外松手，不该把整个模态关掉。
  const downOnScrim = useRef(false);
  // onClose 往往是内联箭头函数，每次渲染都换引用；用 ref 接住，
  // 免得 effect 每次渲染都重挂一遍监听、把焦点又抢回第一个控件。
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    // 打开时把焦点放进面板：优先 autoFocus 的那个，其次第一个输入控件，
    // 最后是面板本身。关闭时还给打开前的那个元素。
    const prev = document.activeElement as HTMLElement | null;
    const body = bodyRef.current;
    if (body && !body.contains(document.activeElement)) {
      const first =
        body.querySelector<HTMLElement>("[autofocus], [data-autofocus]") ??
        body.querySelector<HTMLElement>("input:not([disabled]), textarea:not([disabled]), select");
      (first ?? body).focus();
    }

    const onKey = (e: KeyboardEvent) => {
      // Escape 关闭。挂在 document 上而不是面板上：用户可能正停在某个输入框里。
      // 只让最上面那一层响应 —— 模态里再弹确认框时，Esc 只关确认框。
      const layers = document.querySelectorAll("[data-modal-layer]");
      if (layers[layers.length - 1] !== body?.parentElement) return;
      if (e.key === "Escape") {
        e.stopPropagation();
        closeRef.current();
        return;
      }
      // Tab 困在面板里：模态是「必须先处理这里」，焦点跑回底下的页面就等于穿帮。
      if (e.key === "Tab" && body) {
        const items = Array.from(
          body.querySelectorAll<HTMLElement>(
            'button:not([disabled]), [href], input:not([disabled]), select, textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
          ),
        ).filter((el) => el.offsetParent !== null);
        if (items.length === 0) return;
        const first = items[0];
        const last = items[items.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      if (prev && prev.isConnected) prev.focus();
    };
  }, []);

  return (
    <div
      data-modal-layer
      className="carter-modal-scrim fixed inset-0 z-40 flex items-center justify-center bg-black/40 backdrop-blur-[2px]"
      onMouseDown={(e) => {
        downOnScrim.current = e.target === e.currentTarget;
      }}
      onClick={(e) => {
        if (downOnScrim.current && e.target === e.currentTarget) onClose();
        downOnScrim.current = false;
      }}
    >
      <div
        ref={bodyRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        style={{ width }}
        // 比 base 亮一档：模态是「浮在最上面」的那一层，去掉边框之后
        // 全靠这个明度差和背板拉开。里面的 Card 比它略暗一档，仍然分得清。
        className="carter-modal-body flex max-h-[85vh] max-w-[92vw] flex-col rounded-xl bg-elevated p-5 shadow-2xl ring-1 ring-edge outline-none"
      >
        <div className="mb-4 flex items-start gap-3">
          <h2 className="min-w-0 flex-1 text-base font-semibold">{title}</h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="关闭"
            className="carter-press -mr-1.5 -mt-1 shrink-0 rounded-md p-1 text-dim hover:bg-ink/5 hover:text-ink"
          >
            <X size={15} />
          </button>
        </div>
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
      <div className="mt-5 flex items-center justify-end gap-2">
        <button
          onClick={onCancel}
          // 破坏性动作默认停在「取消」上：手快按回车不该把东西删了。
          data-autofocus={danger ? true : undefined}
          className="carter-press rounded-md bg-ink/5 px-3 py-1.5 text-sm text-dim outline-none hover:bg-ink/10 hover:text-ink"
        >
          取消
        </button>
        <button
          onClick={onConfirm}
          disabled={busy}
          data-autofocus={danger ? undefined : true}
          className={`carter-press inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium outline-none disabled:opacity-40 ${
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

/**
 * 一张卡片。
 *
 * **没有边框** —— 它和所在的底永远跨一档明度（见 index.css 的色阶说明）。
 * 需要「占位/可添加」语义时才用 `dashed`，那是唯一一个虚线边框仍有意义的场景：
 * 虚线说的是「这里还空着」，不是「这里开始了一块」。给了 `onClick` 就渲染成
 * button（能聚焦、能用键盘触发）。
 */
export function Card({
  children,
  className = "",
  dashed = false,
  onClick,
  ...rest
}: {
  children: ReactNode;
  className?: string;
  dashed?: boolean;
  onClick?: () => void;
  /** 透传 data-* 属性（键盘导航之类按它找元素）。 */
  [data: `data-${string}`]: unknown;
}) {
  const cls = `rounded-xl ${
    dashed ? "border border-dashed border-edge" : "bg-card"
  } px-4 py-3 text-left ${
    onClick
      ? "w-full cursor-pointer transition-colors outline-none hover:bg-ink/[0.03] focus-visible:ring-2 focus-visible:ring-edge-strong"
      : ""
  } ${className}`;

  if (onClick) {
    return (
      <button type="button" onClick={onClick} className={cls} {...rest}>
        {children}
      </button>
    );
  }
  return (
    <div className={cls} {...rest}>
      {children}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 折叠
// ---------------------------------------------------------------------------

/**
 * 可折叠区段。
 *
 * 高度过渡走 CSS Grid 的 `0fr → 1fr`（见 index.css 的 `.carter-collapse`），
 * **不测高、不动画 max-height**。两个附带的好处：
 *
 * - 展开过的内容不再卸载 —— 日志折起来再展开，内部滚动位置还在；
 * - 收起时内层 `visibility: hidden`，里面的按钮不会被 Tab 到。
 *
 * 但**没展开过的内容不挂载**：长会话里几十张工具卡片各带着一大段输出，
 * 全都预先渲染一遍是白烧内存。第一次展开时才建，之后一直留着。
 *
 * 用它替代原生 `<details>`：`<details>` 的展开是瞬变的，而且做不到
 * 「首次展开有动画 + 收起后保留滚动位置」这两条。
 */
export function Collapse({
  open,
  children,
  className = "",
}: {
  open: boolean;
  children: ReactNode;
  className?: string;
}) {
  // 一旦展开过就一直挂着（用 ref 而不是 state：这只是一次性开关，
  // 不该为了它多跑一轮渲染）。
  const mounted = useRef(false);
  if (open) mounted.current = true;

  return (
    <div className={`carter-collapse ${className}`} data-open={open} aria-hidden={!open}>
      <div>{mounted.current ? children : null}</div>
    </div>
  );
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
        className="shrink-0 rounded bg-bubble px-1.5 py-0.5 text-[10px] text-dim"
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
