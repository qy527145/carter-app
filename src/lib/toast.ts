// 瞬时提示。命令失败、保存失败这类「此刻发生了一件事」的反馈走这里。
//
// 与 store 里那个 `notice` 的区别：`notice` 是**后端随事件推来的**会话级提示
// （压缩日志、渠道不支持远程工具…），要一直挂着直到用户点掉；toast 是一次命令
// 的结果，3 秒后自己走，不占版面。

import { create } from "zustand";

export type ToastKind = "info" | "warn" | "error";

export type Toast = {
  id: number;
  kind: ToastKind;
  message: string;
};

type ToastStore = {
  toasts: Toast[];
  push: (message: string, kind?: ToastKind) => void;
  dismiss: (id: number) => void;
};

let nextId = 0;

export const useToastStore = create<ToastStore>((set) => ({
  toasts: [],
  push: (message, kind = "info") => {
    const id = nextId++;
    set((s) => ({ toasts: [...s.toasts, { id, kind, message }] }));
    // 3 秒后自己消失。不提供手动延长 —— 需要留住的东西该用 notice 或页面内的
    // 错误块，而不是一个更持久的 toast。
    setTimeout(() => {
      set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) }));
    }, 3000);
  },
  dismiss: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

/** 命令里最常用的那个：把 `unknown` 错误转成一句人话推出去。 */
export function toastError(e: unknown): void {
  const message = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
  useToastStore.getState().push(message || "操作失败", "error");
}

export function toastOk(message: string): void {
  useToastStore.getState().push(message, "info");
}
