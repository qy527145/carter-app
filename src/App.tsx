// 布局与启动：左侧栏 + 主区（对话 / 设置）+ 报文抽屉 + toast。
//
// 单窗口、单会话（一次只跑一个对话）。启动时续接最近一个会话；从侧栏点历史会话
// 则按 id 打开。换会话一律走「关掉当前的 → 新建/打开」这条路（见 lib/sessionOps）。

import { useEffect } from "react";

import { ChatView } from "./components/ChatView";
import { Inspector } from "./components/Inspector";
import { SidePanel } from "./components/SidePanel";
import { Sidebar } from "./components/Sidebar";
import { StatusBar } from "./components/StatusBar";
import { ToastContainer } from "./components/ui/Toast";
import { SettingsPage } from "./views/settings/SettingsPage";
import { appInfo, createSession, listSessions } from "./lib/api";
import { useAppEvents } from "./lib/events";
import { applyTheme } from "./lib/theme";
import { toastError } from "./lib/toast";
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

        const sessions = await listSessions(false);
        if (cancelled) return;
        useStore.getState().setSessions(sessions);

        const created = await createSession({ resume: sessions.length > 0 });
        if (cancelled) return;
        useStore.getState().beginSession(created);
      } catch (e) {
        if (!cancelled) {
          useStore.getState().setNotice(String(e));
          toastError(e);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // 主题：跟着设置走。`system` 档还要听系统偏好变化，所以 effect 返回清理函数。
  const theme = useStore((s) => s.settings?.theme);
  useEffect(() => {
    if (!theme) return;
    return applyTheme(theme);
  }, [theme]);

  const view = useStore((s) => s.view);

  return (
    // 高度锚在**视口**上（`h-screen`），不靠 html/body/#root 的 `height:100%` 链。
    // 这一层是全部滚动容器的根：它一旦比视口高，对话区就会把输入框顶出屏幕外
    // （表现是「输入框不见了」，而页面上看不出任何错误）。
    // `overflow-hidden` 是第二道保险。
    <div className="flex h-screen overflow-hidden bg-base text-ink">
      <Sidebar />
      {/* min-h-0 同理：不加的话 flex 子项的自动最小高度是内容高度，
          内容一长就把这一列撑开，里面的滚动容器就永远不滚了。 */}
      <main className="flex min-h-0 min-w-0 flex-1 flex-col">
        {view === "chat" ? (
          <>
            <ChatView />
            <StatusBar />
          </>
        ) : (
          <SettingsPage />
        )}
      </main>
      <SidePanel />
      <Inspector />
      <ToastContainer />
    </div>
  );
}
