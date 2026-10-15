// 下拉选择。
//
// 刻意不用原生 `<select>`：它在 Windows 上没法去掉自己的原生外观，是整个界面里
// 唯一一个「怎么调都和输入框不像一套」的控件。手写一个按钮 + 绝对定位面板，
// 代价是得自己做外部点击与 Escape 关闭。

import { useEffect, useRef, useState } from "react";
import { Check, ChevronDown } from "lucide-react";

export type Option = {
  value: string;
  /** 显示文本。省略时用 value。 */
  label?: string;
  /** 次要说明，显示在右侧。 */
  hint?: string;
};

export function Select({
  value,
  options,
  onChange,
  placeholder = "请选择",
  disabled,
  className = "",
}: {
  value: string;
  options: Option[];
  onChange: (v: string) => void;
  placeholder?: string;
  disabled?: boolean;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  // 点外面关掉。用 pointerdown 而不是 click：拖选文本后松手落在外面时
  // click 不会触发，菜单会赖着不走。
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const current = options.find((o) => o.value === value);

  return (
    <div ref={wrapRef} className={`relative ${className}`}>
      <button
        type="button"
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-2 rounded-md border border-edge bg-base px-3 py-2 text-left text-sm text-ink outline-none transition-colors hover:border-dim focus:border-dim disabled:cursor-not-allowed disabled:opacity-50"
      >
        <span className={`min-w-0 flex-1 truncate ${current ? "" : "text-dim"}`}>
          {current?.label ?? current?.value ?? placeholder}
        </span>
        <ChevronDown size={14} className="shrink-0 text-dim" />
      </button>

      {open && (
        <div className="absolute left-0 top-[calc(100%+4px)] z-50 max-h-72 w-full overflow-y-auto rounded-lg border border-edge bg-card py-1 shadow-lift">
          {options.map((o) => (
            <button
              key={o.value}
              type="button"
              onClick={() => {
                onChange(o.value);
                setOpen(false);
              }}
              className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-xs text-ink hover:bg-base"
            >
              <span className="w-3 shrink-0 text-accent">
                {o.value === value && <Check size={12} />}
              </span>
              <span className="min-w-0 flex-1 truncate">{o.label ?? o.value}</span>
              {o.hint && (
                <span className="shrink-0 font-mono text-[10px] text-dim">{o.hint}</span>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
