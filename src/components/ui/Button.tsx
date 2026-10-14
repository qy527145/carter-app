// shadcn init 因为下载 shadcn CLI 时的证书校验失败没能跑起来，这里手写一个最小
// 按钮 —— 只保留项目用得到的几个 variant，不引 cva 那一套。

import type { ButtonHTMLAttributes } from "react";

type Variant = "primary" | "ghost" | "danger" | "outline";

type Props = ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant };

const VARIANTS: Record<Variant, string> = {
  primary: "bg-sky-600 hover:bg-sky-500 text-white",
  ghost: "bg-transparent hover:bg-slate-700/60 text-slate-300",
  danger: "bg-red-600 hover:bg-red-500 text-white",
  outline: "border border-slate-600 hover:bg-slate-700/60 text-slate-200",
};

export function Button({ variant = "primary", className = "", ...rest }: Props) {
  return (
    <button
      className={`inline-flex items-center justify-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${VARIANTS[variant]} ${className}`}
      {...rest}
    />
  );
}
