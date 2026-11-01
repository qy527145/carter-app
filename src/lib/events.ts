//! 把 Tauri 的事件总线接进 store。
//!
//! 后端把所有会话的事件都广播到同一条通道上，所以这里要按 `session_id` 分流：
//! 属于主对话的走 `applyEvent`，属于侧边聊天（`/btw`）的走 `applySideEvent`，
//! 其余的（同时跑着的别的对话）直接丢掉 —— 混进同一个气泡是最难查的一类 bug。
//!
//! ## 增量按帧合并
//!
//! `text_delta` 是上游 SSE 推一块、这里收一块，快的时候一帧里能到七八条。
//! 每条都单独 set 一次 store 的话，React 要把整段 markdown 重新解析、重新 diff
//! 一遍 DOM —— 一帧做八次，长回答时能明显看出掉帧。
//!
//! 所以增量先攒进缓冲、下一帧一起交（`requestAnimationFrame`）。合并不改变
//! 最终内容，只是把「八次小重排」换成「一次正常重排」；视觉上反而更顺 ——
//! 字是成串落定的，不再一蹦一蹦。
//!
//! 缓冲**必须**在非增量事件到达前刷掉：`text_end` / `tool_started` 的语义依赖
//! 「此前所有增量都已落地」，否则文字会被归到下一个 item 上。

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { EVENT_NAME, type AppEvent, type SessionEvent } from "./api";
import { refreshSessions } from "./sessionOps";
import { useStore } from "../store";

type Chunk = { kind: "text" | "thinking"; text: string };

type Buffer = { chunks: Chunk[]; raf: number; timer: ReturnType<typeof setTimeout> };

/** 按会话 id 存的待合并增量。 */
const buffers = new Map<string, Buffer>();

/**
 * 把缓冲交出去。
 *
 * 归属在这里现查而不是入队时记下：流式期间用户可能正好切走会话或关掉侧边聊天，
 * 那时这批字该丢掉，而不是写进另一个对话。
 */
function flush(sessionId: string): void {
  const buf = buffers.get(sessionId);
  if (!buf) return;
  buffers.delete(sessionId);
  cancelAnimationFrame(buf.raf);
  clearTimeout(buf.timer);

  const state = useStore.getState();
  let apply: ((e: SessionEvent) => void) | null = null;
  if (state.sessionId === sessionId) apply = (e) => useStore.getState().applyEvent(e);
  else if (state.side?.sessionId === sessionId) {
    apply = (e) => useStore.getState().applySideEvent(e);
  }
  if (!apply) return;

  for (const c of buf.chunks) {
    apply(
      c.kind === "text"
        ? { type: "text_delta", text: c.text }
        : { type: "thinking_delta", text: c.text },
    );
  }
}

/** 增量入缓冲。同类的相邻块直接拼字符串，减少 flush 时的 set 次数。 */
function push(sessionId: string, chunk: Chunk): void {
  let buf = buffers.get(sessionId);
  if (!buf) {
    buf = {
      chunks: [],
      raf: 0,
      // rAF 在窗口最小化时会被节流甚至不触发。没有这个兜底，一批字会一直
      // 攒到窗口恢复才出现；流量本身不会丢，但「生成中」的观感会冻住。
      timer: setTimeout(() => flush(sessionId), 250),
    };
    buffers.set(sessionId, buf);
    buf.raf = requestAnimationFrame(() => flush(sessionId));
  }
  const last = buf.chunks[buf.chunks.length - 1];
  if (last && last.kind === chunk.kind) last.text += chunk.text;
  else buf.chunks.push(chunk);
}

/** 订阅 `carter://event`，在整个 App 生命周期内生效。 */
export function useAppEvents() {
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    // listen() 是异步的：清理可能先于它 resolve（React StrictMode 会立刻卸载再挂载）。
    // 用 cancelled 标记，晚到的 unlisten 就地调用，避免留下悬挂的监听器。
    let cancelled = false;

    const onSessionEvent = (sessionId: string, e: SessionEvent) => {
      const state = useStore.getState();
      if (state.sessionId !== null && sessionId === state.sessionId) {
        // 非增量事件先让前面攒的增量落地，顺序才不会被打乱。
        flush(sessionId);
        state.applyEvent(e);
        // 标题是**异步**来的（首轮跑着的时候另一条任务在起名）。它不改对话内容，
        // 只改会话列表里那一行 —— 重拉一次列表，侧栏与顶部标题一起更新。
        if (e.type === "title") void refreshSessions();
      } else if (state.side !== null && sessionId === state.side.sessionId) {
        flush(sessionId);
        state.applySideEvent(e);
      }
      // 两者都不是：别的会话的事件。还没连上会话、或事件属于已关闭的对话时
      // 都会走到这里，静默丢掉即可。
    };

    listen<AppEvent>(EVENT_NAME, ({ payload }) => {
      if (payload.type === "config_changed") {
        // 配置被界面改过并已热重载。设置页据此重拉数据。
        useStore.getState().bumpConfig();
        return;
      }
      if (payload.type === "session") {
        const { session_id: sid, event: e } = payload;
        // 增量进缓冲 —— 只认「确实属于当前主对话或当前侧边聊天」的，别的会话
        // 直接丢，免得缓冲里堆着永远不会被交出去的字。
        if (e.type === "text_delta" || e.type === "thinking_delta") {
          const state = useStore.getState();
          const owned = state.sessionId === sid || state.side?.sessionId === sid;
          if (!owned) return;
          push(sid, { kind: e.type === "text_delta" ? "text" : "thinking", text: e.text });
          return;
        }
        onSessionEvent(sid, e);
        return;
      }
      useStore.getState().setNotice(payload.message);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
      // 卸载（退出应用）时攒着的增量直接丢掉，不再交出去。
      for (const buf of buffers.values()) {
        cancelAnimationFrame(buf.raf);
        clearTimeout(buf.timer);
      }
      buffers.clear();
    };
  }, []);
}