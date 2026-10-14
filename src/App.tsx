// 布局与启动：左侧会话栏 + 右侧对话 + 底部状态栏。
//
// 单窗口、单会话（一次只跑一个对话）。启动时续接最近一个会话；从侧栏点历史会话
// 则按 id 打开（`resumeId`）。换会话一律走「关掉当前的 → 新建/打开」这条路。

import { useCallback, useEffect } from "react";

import { ChatView } from "./components/ChatView";
import { Inspector } from "./components/Inspector";
import { Settings } from "./components/Settings";
import { Sidebar } from "./components/Sidebar";
import { StatusBar } from "./components/StatusBar";
import { appInfo, closeSession, createSession, listSessions } from "./lib/api";
import { useAppEvents } from "./lib/events";
import { useStore } from "./store";

export default function App() {
  useAppEvents();

  // 启动：读应用信息 + 会话列表，然后续接最近一个（没有就新建）。
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const info = await appInfo();
        if (cancelled) return;
        useStore.getState().setAppInfo(info);

        const sessions = await listSessions(null);
        if (cancelled) return;
        useStore.getState().setSessions(sessions);

        const created = await createSession({ resume: sessions.length > 0 });
        if (cancelled) return;
        useStore.getState().beginSession(created);
      } catch (e) {
        if (!cancelled) useStore.getState().setNotice(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // 会话任务的存活跟着句柄走：换会话前先把旧的关掉，免得它继续占着后台任务。
  // 换会话 = 关掉当前 + 新建/打开。给 resumeId 就按 id 打开历史会话。
  const replaceSession = useCallback(
    async (args: { resume: boolean; resumeId?: string | null }) => {
      const prev = useStore.getState().sessionId;
      useStore.getState().resetSession();
      if (prev) {
        // 会话可能已经自己结束了，关不掉不算错。
        try {
          await closeSession(prev);
        } catch {
          /* ignore */
        }
      }
      try {
        const created = await createSession(args);
        // 按 id 打开时，后端找不到该 id 不会报错，而是**静默新建** —— 返回值会是
        // 另一个 id。据此提示，免得「点了个旧会话却得到一个空白会话」被当成成功。
        if (args.resumeId && created.session_id !== args.resumeId) {
          useStore.getState().setNotice("该会话已不存在，已新建会话");
        }
        useStore.getState().beginSession(created);
        useStore.getState().setSessions(await listSessions(null));
      } catch (e) {
        useStore.getState().setNotice(String(e));
      }
    },
    [],
  );

  // 新建 / 打开会话都是「对话」这个视图里的动作，顺手切回去。
  const newSession = useCallback(() => {
    useStore.getState().setView("chat");
    void replaceSession({ resume: false });
  }, [replaceSession]);
  const openSession = useCallback(
    (id: string) => {
      useStore.getState().setView("chat");
      void replaceSession({ resume: false, resumeId: id });
    },
    [replaceSession],
  );

  const view = useStore((s) => s.view);

  return (
    <div className="flex h-full bg-slate-950 text-slate-100">
      <Sidebar onNew={newSession} onOpen={openSession} />
      <main className="flex min-w-0 flex-1 flex-col">
        {view === "chat" ? (
          <>
            <ChatView />
            <StatusBar />
          </>
        ) : (
          <Settings />
        )}
      </main>
      <Inspector />
    </div>
  );
}
