// 会话的增删改开 —— 从 App.tsx 里抽出来，因为现在侧栏与「会话」设置页都要用，
// 而这两处各自实现一遍必然会走样（比如一处换了会话却忘了刷列表）。
//
// 直接操作 zustand store 而不是做成 hook：这些是「事件处理函数」而不是渲染逻辑，
// 不需要订阅。`useStore.getState()` 在任何地方都拿得到同一份状态。

import * as api from "./api";
import type { GoalCommand } from "./api";
import { useStore, withCurrent } from "../store";
import { toastError, toastOk } from "./toast";

/**
 * 重拉会话列表。侧栏与设置页共用同一份。
 *
 * 拉回来之后补一次 `withCurrent`：刚开的会话在磁盘上还没有文件（carter 的 Recorder
 * 惰性建文件），不补的话「点了新对话，左边什么都没多」。
 */
export async function refreshSessions(): Promise<void> {
  try {
    const list = await api.listSessions(false);
    const s = useStore.getState();
    s.setSessions(withCurrent(list, { id: s.sessionId, model: s.model, cwd: s.cwd }));
  } catch (e) {
    toastError(e);
  }
}

/**
 * 换会话 = 关掉当前 + 新建/打开。
 *
 * 会话任务的存活跟着句柄走：不先关掉旧的，它会继续占着后台任务，
 * 而且旧的 `Recorder` 还攥着文件。
 */
async function replaceSession(args: { resume: boolean; resumeId?: string | null }) {
  const store = useStore.getState();
  const prev = store.sessionId;
  store.resetSession();
  if (prev) {
    // 会话可能已经自己结束了，关不掉不算错。
    try {
      await api.closeSession(prev);
    } catch {
      /* ignore */
    }
  }
  try {
    const created = await api.createSession(args);
    // 按 id 打开时，后端找不到该 id 不会报错，而是**静默新建** —— 返回值会是
    // 另一个 id。据此提示，免得「点了个旧会话却得到一个空白会话」被当成成功。
    if (args.resumeId && created.session_id !== args.resumeId) {
      useStore.getState().setNotice("该会话已不存在，已新建会话");
    }
    useStore.getState().beginSession(created);
    await refreshSessions();
  } catch (e) {
    toastError(e);
  }
}

/** 新建会话并切到对话视图。 */
export function newSession(): void {
  useStore.getState().setView("chat");
  void replaceSession({ resume: false });
}

/** 打开一条历史会话。 */
export function openSession(id: string): void {
  useStore.getState().setView("chat");
  void replaceSession({ resume: false, resumeId: id });
}

export async function rename(id: string, title: string): Promise<void> {
  try {
    await api.renameSession(id, title);
    await refreshSessions();
  } catch (e) {
    toastError(e);
  }
}

/**
 * fork 一条会话并打开它。
 *
 * carter 的 `fork` 会写出一个**自包含**的新文件（种子历史 + `forked_from` 血缘），
 * 所以这里只要把新 id 打开即可，不用管历史怎么搬。
 */
export async function fork(id: string): Promise<void> {
  try {
    const newId = await api.forkSession(id);
    await refreshSessions();
    useStore.getState().setView("chat");
    await replaceSession({ resume: false, resumeId: newId });
  } catch (e) {
    toastError(e);
  }
}

/**
 * 删除一条会话。不可恢复 —— 调用方必须先确认。
 *
 * 删掉的正好是当前打开的会话时，界面里也得关掉它，否则界面还停在一条已经不存在的
 * 对话上，继续发消息会重建出一个同名新会话，看起来像「删了没生效」。
 */
export async function remove(id: string): Promise<void> {
  const store = useStore.getState();
  try {
    await api.deleteSession(id);
    if (id === store.sessionId) {
      try {
        await api.closeSession(id);
      } catch {
        /* 会话可能已经自己结束了 */
      }
      useStore.getState().resetSession();
      useStore.getState().setNotice("当前会话已删除，请点「新对话」继续。");
    }
    await refreshSessions();
  } catch (e) {
    toastError(e);
  }
}

/** 导出到下载目录，成功后把路径告诉用户 —— 否则他得自己去猜文件在哪。 */
export async function export_(id: string, format: "markdown" | "json"): Promise<void> {
  try {
    const path = await api.exportSession(id, format);
    toastOk(`已导出到 ${path}`);
  } catch (e) {
    toastError(e);
  }
}

/**
 * 在指定目录里新开一个会话。
 *
 * 换工作目录必然是**新会话**，不是就地切换：carter 的会话按 `projects/<cwd 的键>/`
 * 分文件存，换目录在磁盘上就是另一条对话。把这件事说清楚，别让用户以为只是换了个标签。
 */
export async function startInDirectory(cwd: string): Promise<void> {
  const store = useStore.getState();
  const prev = store.sessionId;
  try {
    const created = await api.createSession({ cwd, resume: false });
    // 新会话建好之后再关旧的：万一新建失败，用户手上还留着原来那条对话。
    if (prev) {
      try {
        await api.closeSession(prev);
      } catch {
        /* 已经自己结束了 */
      }
    }
    store.beginSession(created);
    await refreshSessions();
    toastOk(`已在 ${cwd} 新开会话`);
  } catch (e) {
    toastError(e);
  }
}

// ---------------------------------------------------------------------------
// 长任务目标（/goal）
// ---------------------------------------------------------------------------

/**
 * 投递一个 `/goal` 动作。
 *
 * **不走 `send`**：目标不是发给模型的内容，它由会话任务自己处理（改状态、落盘、
 * 再决定要不要自动继续）。走 `send` 的话它会变成一条用户消息进历史。
 */
export function goal(sessionId: string, action: GoalCommand): void {
  api.sendCommand(sessionId, { type: "goal", action }).catch((e) => {
    // 会话已经结束这类失败要说出来 —— 静默失败会让用户以为目标设上了。
    toastError(e);
  });
}

// ---------------------------------------------------------------------------
// 侧边聊天（/btw）
// ---------------------------------------------------------------------------

/**
 * 开一个侧边聊天，可选地立刻问第一句。
 *
 * 后端起的是一条**不落盘**的会话（继承当前对话的历史作为参考，关掉即丢），
 * 事件按 `session_id` 分到 `side` 上（见 `lib/events.ts`）。
 *
 * 已经开着的话就把话发给它 —— 抽屉只有一个，重开一个会把手上那条对话丢掉。
 */
export async function openSideChat(text?: string): Promise<void> {
  const store = useStore.getState();
  const parent = store.sessionId;
  if (!parent) {
    toastError("会话还没就绪 —— 点「新对话」开一条再试");
    return;
  }
  const ask = text?.trim();
  const existing = store.side;
  if (existing) {
    if (ask) {
      useStore.getState().addSideUserTurn(ask);
      await send(existing.sessionId, ask);
    }
    return;
  }
  try {
    const side = await api.createSideSession(parent);
    // 先落地再发消息：事件按 session_id 路由，`side` 还是 null 的话，
    // 这一轮的增量会被当成「别人的会话」丢掉。
    useStore.getState().beginSide(side);
    if (ask) {
      useStore.getState().addSideUserTurn(ask);
      await send(side.session_id, ask);
    }
  } catch (e) {
    toastError(e);
  }
}

/** 关掉侧边聊天。后端那条会话不落盘，关掉之后连痕迹都不该留。 */
export async function closeSideChat(): Promise<void> {
  const side = useStore.getState().side;
  useStore.getState().closeSide();
  if (!side) return;
  try {
    await api.closeSession(side.sessionId);
  } catch {
    /* 已经自己结束了 */
  }
}

/** 往会话投递一条命令。发消息的路径只有这一处，方便统一处理「会话已死」。 */
export async function send(sessionId: string, text: string): Promise<void> {
  await api.sendCommand(sessionId, { type: "send_prompt", text });
}

export async function cancel(sessionId: string): Promise<void> {
  await api.sendCommand(sessionId, { type: "cancel" });
}
