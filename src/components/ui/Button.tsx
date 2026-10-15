// 按钮。刻意从简：不引 cva，变体就是一张字符串表。
//
// 两条来自参考实现的纪律：
// 1. **主按钮是黑白的**（`bg-accent`，accent 本身无色），不是某个品牌色。
// 2. **次要按钮的文字是 dim，悬停才到 ink** —— 静止时安静，不靠改边框色做反馈。

import type { ButtonHTMLAttributes, ReactNode } from "react";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md";

type Props = ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: Variant;
  size?: Size;
  children?: ReactNode;
};

const VARIANTS: Record<Variant, string> = {
  primary:
    "bg-accent text-[var(--accent-contrast)] hover:opacity-90 disabled:hover:opacity-40",
  secondary: "border border-edge text-dim hover:text-ink",
  ghost: "text-dim hover:bg-card hover:text-ink",
  // 破坏性动作不填红：红色填充会让每个删除按钮都变成页面上最响的东西。
  danger: "border border-edge text-dim hover:text-[var(--danger)]",
};

const SIZES: Record<Size, string> = {
  sm: "gap-1 rounded-md px-2 py-1 text-xs",
  md: "gap-1.5 rounded-md px-3 py-1.5 text-sm",
};

export function Button({
  variant = "secondary",
  size = "md",
  className = "",
  ...rest
}: Props) {
  return (
    <button
      className={`inline-flex shrink-0 items-center justify-center font-medium transition-colors outline-none disabled:cursor-not-allowed disabled:opacity-40 ${SIZES[size]} ${VARIANTS[variant]} ${className}`}
      {...rest}
    />
  );
}

/** 只有图标的方形按钮。`title` 必填 —— 没有文字就得有可读的名字。 */
export function IconButton({
  className = "",
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      className={`inline-flex shrink-0 items-center justify-center rounded-md p-1.5 text-dim transition-colors outline-none hover:bg-card hover:text-ink disabled:cursor-not-allowed disabled:opacity-40 ${className}`}
      {...rest}
    />
  );
}
