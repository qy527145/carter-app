//! 把 Tauri 的事件总线接进 store。
//!
//! 后端把所有会话的事件都广播到同一条通道上，所以这里要按 `session_id` 过滤：
//! 只处理属于当前会话的事件，别的会话（同时跑着的另一个对话）的事件直接丢掉，
//! 否则两个对话的增量会混进同一个气泡。

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { EVENT_NAME, type AppEvent } from "./api";
import { useStore } from "../store";

/** 订阅 `carter://event`，在整个 App 生命周期内生效。 */
export function useAppEvents() {
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    // listen() 是异步的：清理可能先于它 resolve（React StrictMode 会立刻卸载再挂载）。
    // 用 cancelled 标记，晚到的 unlisten 就地调用，避免留下悬挂的监听器。
    let cancelled = false;

    listen<AppEvent>(EVENT_NAME, ({ payload }) => {
      const state = useStore.getState();
      if (payload.type === "config_changed") {
        // 配置被界面改过并已热重载。设置页据此重拉数据。
        state.bumpConfig();
        return;
      }
      if (payload.type === "session") {
        // 还没连上会话、或事件属于别的会话 —— 都不是当前界面该关心的。
        if (state.sessionId === null || payload.session_id !== state.sessionId) return;
        state.applyEvent(payload.event);
      } else {
        state.setNotice(payload.message);
      }
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
