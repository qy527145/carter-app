//! 把 Tauri 的事件总线接进 store。
//!
//! 后端把所有会话的事件都广播到同一条通道上，所以这里要按 `session_id` 分流：
//! 属于主对话的走 `applyEvent`，属于侧边聊天（`/btw`）的走 `applySideEvent`，
//! 其余的（同时跑着的别的对话）直接丢掉 —— 混进同一个气泡是最难查的一类 bug。

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
        if (state.sessionId !== null && payload.session_id === state.sessionId) {
          state.applyEvent(payload.event);
        } else if (state.side !== null && payload.session_id === state.side.sessionId) {
          state.applySideEvent(payload.event);
        }
        // 两者都不是：别的会话的事件。还没连上会话、或事件属于已关闭的对话时
        // 都会走到这里，静默丢掉即可。
        return;
      }
      state.setNotice(payload.message);
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
