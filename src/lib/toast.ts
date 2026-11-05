// 瞬时提示。命令失败、保存失败这类「此刻发生了一件事」的反馈走这里。
//
// 与 store 里那个 `notice` 的区别：`notice` 是**后端随事件推来的**会话级提示
// （压缩日志、渠道不支持远程工具…），要一直挂着直到用户点掉；toast 是一次命令
// 的结果，几秒后自己走，不占版面。

import { create } from "zustand";

export type ToastKind = "info" | "warn" | "error";

export type Toast = {
  id: number;
  kind: ToastKind;
  message: string;
  /** 离场动画进行中。先标记、动画放完再真正移除。 */
  leaving?: boolean;
  /** 同一句话短时间内重复推送时的次数 —— 合并成一条带 ×N，而不是叠一摞。 */
  count: number;
  /** 可选的「点一下做点什么」按钮（如「重启更新」让用户**点**让于自动重启）。 */
  action?: { label: string; run: () => void };
};

type ToastStore = {
  toasts: Toast[];
  push: (message: string, kind?: ToastKind, action?: Toast["action"]) => void;
  dismiss: (id: number) => void;
  /** 鼠标停在 toast 上时暂停全部计时：正读着的那条不该在眼前消失。 */
  pause: () => void;
  resume: () => void;
};

let nextId = 0;

/**
 * toast 自己消失的时长。
 *
 * 「已复制」在按钮上变成对勾、然后自己复位的时长也用它：同一个应用里两种
 * 「反馈自己消失」的节奏，用户会觉得哪里不对但说不清。
 */
export const TOAST_MS = 2500;
/** 错误多留一会儿：通常要读完才知道下一步怎么做。 */
const ERROR_MS = 5000;
/** 离场动画时长，与 index.css 的 `.carter-toast` 过渡一致。 */
const LEAVE_MS = 150;
/** 同时最多挂几条。多了最老的先走 —— 一摞 toast 盖住右上角比丢一条更糟。 */
const MAX_TOASTS = 4;

// 每条 toast 的剩余时长与计时器。放在 store 外：它们不参与渲染。
const timers = new Map<number, { left: number; started: number; handle: number | null }>();
let paused = false;

function arm(id: number) {
  const t = timers.get(id);
  if (!t || paused) return;
  t.started = Date.now();
  t.handle = window.setTimeout(() => useToastStore.getState().dismiss(id), t.left);
}

export const useToastStore = create<ToastStore>((set, get) => ({
  toasts: [],
  push: (message, kind = "info", action) => {
    // 带按钮的不自己走：那是「等你做一件事」，2.5 秒后消失等于没提示。
    const ms = action ? Number.POSITIVE_INFINITY : kind === "error" ? ERROR_MS : TOAST_MS;
    // 同一句话还挂着：计数 +1、重新计时，不再叠一条。
    const same = get().toasts.find((t) => !t.leaving && t.message === message && t.kind === kind);
    if (same) {
      const t = timers.get(same.id);
      if (t?.handle) clearTimeout(t.handle);
      timers.set(same.id, { left: ms, started: Date.now(), handle: null });
      arm(same.id);
      set((s) => ({
        toasts: s.toasts.map((x) => (x.id === same.id ? { ...x, count: x.count + 1 } : x)),
      }));
      return;
    }
    const id = nextId++;
    set((s) => ({ toasts: [...s.toasts, { id, kind, message, count: 1, action }] }));
    timers.set(id, { left: ms, started: Date.now(), handle: null });
    arm(id);
    const live = get().toasts.filter((t) => !t.leaving);
    if (live.length > MAX_TOASTS) get().dismiss(live[0].id);
  },
  dismiss: (id) => {
    const t = timers.get(id);
    if (t?.handle) clearTimeout(t.handle);
    timers.delete(id);
    if (!get().toasts.some((x) => x.id === id && !x.leaving)) return;
    set((s) => ({ toasts: s.toasts.map((x) => (x.id === id ? { ...x, leaving: true } : x)) }));
    setTimeout(() => set((s) => ({ toasts: s.toasts.filter((x) => x.id !== id) })), LEAVE_MS);
  },
  pause: () => {
    if (paused) return;
    paused = true;
    const now = Date.now();
    for (const t of timers.values()) {
      if (t.handle) clearTimeout(t.handle);
      t.handle = null;
      t.left = Math.max(800, t.left - (now - t.started));
    }
  },
  resume: () => {
    if (!paused) return;
    paused = false;
    for (const id of timers.keys()) arm(id);
  },
}));

/** 命令里最常用的那个：把 `unknown` 错误转成一句人话推出去。 */
export function toastError(e: unknown): void {
  const message = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
  useToastStore.getState().push(message || "操作失败", "error");
}

export function toastOk(message: string): void {
  useToastStore.getState().push(message, "info");
}

/** 带一个按钮的提示（e.g. 「新版本已就绪」+「重启更新」）。不自动消失。 */
export function toastAction(
  message: string,
  action: { label: string; run: () => void },
  kind: ToastKind = "info",
): void {
  useToastStore.getState().push(message, kind, action);
}
