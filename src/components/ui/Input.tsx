// 同 Button：shadcn init 未跑通，手写一个受控输入框的样式壳。

import type { InputHTMLAttributes } from "react";

type Props = InputHTMLAttributes<HTMLInputElement>;

export function Input({ className = "", ...rest }: Props) {
  return (
    <input
      className={`rounded-md border border-slate-700 bg-slate-900/60 px-3 py-1.5 text-sm text-slate-100 placeholder:text-slate-500 outline-none focus:border-sky-600 ${className}`}
      {...rest}
    />
  );
}
