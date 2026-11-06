// 一键更新的前端状态：首页那条提示与「关于」页共用同一份。
//
// 为什么单独抽一个 hook：更新是**全局**的一件事（进度只有一个、同一时刻只允许
// 一次下载），两处各订阅一遍进度事件必然走样 —— 首页条在下载，关于页却显示
// 「下载」，点下去还会报「上一次下载还没结束」。所以状态放这里，两处只读。

import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { applyUpdate, cancelUpdateDownload, UPDATE_PROGRESS, type UpdateProgress } from "./api";
import { toastError } from "./toast";

export type UpdateState =
  | { kind: "idle" }
  | { kind: "downloading"; done: number; total: number | null }
  /** 下载完了，正在交给系统装（macOS / Windows 上这一步很快）。 */
  | { kind: "installing" }
  /** 等本进程退出、由安装脚本拉起新版。界面只剩一句「正在重启」。 */
  | { kind: "restarting" };

const IDLE: UpdateState = { kind: "idle" };

/**
 * 更新进度：只在一个监视者时真正订阅，多个组件共用一份模块级状态。
 *
 * 模块级而不是每个组件一份：两处同时挂载时，第二处订阅拿到的是同一条事件流，
 * 但各自 setState 会先后差一帧 —— 下载进度在首页条与关于页之间来回跳。
 */
let state: UpdateState = IDLE;
const watchers = new Set<(s: UpdateState) => void>();
let unlisten: Promise<() => void> | null = null;

function set(next: UpdateState) {
  state = next;
  for (const w of watchers) w(next);
}

/** 应用启动时订阅一次（见 `events.ts`），进度对两处都可见。 */
export function watchUpdateProgress(): void {
  if (unlisten) return;
  unlisten = listen<UpdateProgress>(UPDATE_PROGRESS, ({ payload }) => {
    if (payload.phase === "downloading") {
      set({
        kind: "downloading",
        done: payload.downloaded ?? 0,
        total: payload.total ?? null,
      });
    } else if (payload.phase === "installing") {
      set({ kind: "installing" });
    } else {
      set({ kind: "restarting" });
    }
  });
}

/** 读当前进度。组件挂载时用它对齐，之后的更新走订阅。 */
export function currentUpdateState(): UpdateState {
  return state;
}

/** 订阅进度。返回退订函数，可直接当 `useEffect` 的清理。 */
export function subscribeUpdate(fn: (s: UpdateState) => void): () => void {
  watchers.add(fn);
  fn(state);
  return () => {
    watchers.delete(fn);
  };
}

/**
 * 一键更新。**正常情况下不会返回** —— 后端装完就让进程退出、拉起新版；
 * 只有失败才走 catch。
 */
export async function startUpdate(): Promise<void> {
  set({ kind: "downloading", done: 0, total: null });
  try {
    await applyUpdate();
  } catch (e) {
    set({ kind: "idle" });
    toastError(e);
  }
}

export async function cancelUpdate(): Promise<void> {
  try {
    await cancelUpdateDownload();
  } catch {
    /* 没有下载在跑 */
  }
  set({ kind: "idle" });
}

/** 组件里用：拿到实时进度。 */
export function useUpdateState(): UpdateState {
  const [s, setS] = useState<UpdateState>(currentUpdateState);
  useEffect(() => subscribeUpdate(setS), []);
  return s;
}

/** 正在更新（下载 / 安装 / 重启），这期间所有更新入口都该锁住。 */
export function updating(s: UpdateState): boolean {
  return s.kind !== "idle";
}

/** 进度百分比；总长未知时返回 `null`（画成不确定的进度条）。 */
export function percent(s: UpdateState): number | null {
  if (s.kind !== "downloading" || !s.total) return null;
  return Math.min(100, Math.round((s.done / s.total) * 100));
}
