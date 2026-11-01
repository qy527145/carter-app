// 表单控件：输入框、多行输入、开关、勾选。
//
// 焦点样式统一是 `.carter-field`（见 index.css）—— 不用 outline 环，也不用
// 把边框整个换成实色：环是「双环」，内圈压深描边、外圈淡淡铺开，靠近时是
// 一圈咬住边缘的暗环。单纯把边框从 7% 透明度跳到实色太跳，会抢走内容本身。

import type {
  InputHTMLAttributes,
  LabelHTMLAttributes,
  ReactNode,
  TextareaHTMLAttributes,
} from "react";

const FIELD_BASE =
  "carter-field w-full rounded-md border border-edge bg-base px-3 py-2 text-sm text-ink " +
  "placeholder:text-dim outline-none transition-[border-color,box-shadow] " +
  "disabled:cursor-not-allowed disabled:opacity-50";

type InputProps = InputHTMLAttributes<HTMLInputElement> & { mono?: boolean };

export function Input({ className = "", mono = false, ...rest }: InputProps) {
  return (
    <input className={`${FIELD_BASE} ${mono ? "mono" : ""} ${className}`} {...rest} />
  );
}

type TextareaProps = TextareaHTMLAttributes<HTMLTextAreaElement> & { mono?: boolean };

export function Textarea({ className = "", mono = false, ...rest }: TextareaProps) {
  return (
    <textarea
      className={`${FIELD_BASE} resize-y leading-relaxed ${mono ? "mono" : ""} ${className}`}
      {...rest}
    />
  );
}

/** 开关。用 button + role=switch 而不是伪装成 checkbox 的 input。 */
export function Switch({
  checked,
  onChange,
  disabled,
  label,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative inline-flex h-[18px] w-[32px] shrink-0 items-center rounded-full transition-colors outline-none disabled:cursor-not-allowed disabled:opacity-40 ${
        checked ? "bg-accent" : "bg-bubble"
      }`}
    >
      {/* 滑块用欠阻尼缓动（到位后微微过冲再落定）—— 这是全应用里少数几个
          「事情办成了」的小反馈，Q 一下不轻浮。开关轨道本身不用，那会显得黏。 */}
      <span
        className={`block h-3 w-3 rounded-full transition-transform duration-200 ease-[cubic-bezier(0.34,1.56,0.64,1)] ${
          checked
            ? "translate-x-[16px] bg-[var(--accent-contrast)]"
            : "translate-x-[2px] bg-dim"
        }`}
      />
    </button>
  );
}

/** 分段选择（主题、transport 这类二三选一）。 */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
}: {
  value: T;
  options: { id: T; label: string }[];
  onChange: (v: T) => void;
}) {
  return (
    <div className="inline-flex gap-1.5">
      {options.map((o) => (
        <button
          key={o.id}
          type="button"
          onClick={() => onChange(o.id)}
          className={`rounded-md px-3 py-1.5 text-sm transition-colors ${
            value === o.id
              ? "bg-ink/10 font-medium text-ink"
              : "text-dim hover:bg-ink/5 hover:text-ink"
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/**
 * 表单里的一行：标签 + 右侧灰色提示 + 控件。
 *
 * 这是配置表单唯一的排版单位。提示放右边而不是下面，是为了让一列标签能对齐 ——
 * 长说明塞在标签下面会把这一行撑高、打断整列的节奏。
 */
export function Field({
  label,
  hint,
  children,
  htmlFor,
}: {
  label: string;
  hint?: ReactNode;
  children: ReactNode;
  htmlFor?: string;
}) {
  return (
    <div className="mb-3">
      <div className="mb-1 flex items-baseline justify-between gap-3">
        <Label htmlFor={htmlFor}>{label}</Label>
        {hint && <span className="text-right text-[11px] text-dim">{hint}</span>}
      </div>
      {children}
    </div>
  );
}

export function Label({ className = "", ...rest }: LabelHTMLAttributes<HTMLLabelElement>) {
  return (
    <label className={`text-xs font-medium text-dim ${className}`} {...rest} />
  );
}
