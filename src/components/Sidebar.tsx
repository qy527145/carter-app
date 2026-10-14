// 左侧栏：会话列表 + 新建。
//
// 每条会话都按 id 打开（`create_session({ resumeId })`），不用「只续接最新」
// 那条老路。当前会话自己不可点，避免无谓地重建一次。

import { MessageSquare, Plus } from "lucide-react";

import { useStore } from "../store";
import { Button } from "./ui/Button";

function relTime(seconds: number): string {
  const d = new Date(seconds * 1000);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function Sidebar({
  onNew,
  onOpen,
}: {
  onNew: () => void;
  onOpen: (id: string) => void;
}) {
  const sessions = useStore((s) => s.sessions);
  const sessionId = useStore((s) => s.sessionId);
  const appVersion = useStore((s) => s.appVersion);

  return (
    <aside className="flex w-60 shrink-0 flex-col border-r border-slate-800 bg-slate-900/40">
      <div className="flex items-center gap-2 px-4 py-3">
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-sky-600 text-white">
          <MessageSquare size={16} />
        </div>
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold text-slate-100">Carter</div>
          {appVersion && <div className="text-[11px] text-slate-500">v{appVersion}</div>}
        </div>
      </div>

      <div className="px-3 pb-2">
        <Button onClick={onNew} className="w-full">
          <Plus size={15} />
          新建会话
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        {sessions.length === 0 && (
          <div className="px-2 py-6 text-center text-xs text-slate-600">暂无历史会话</div>
        )}
        {sessions.map((s) => {
          const active = s.id === sessionId;
          return (
            <button
              key={s.id}
              disabled={active}
              onClick={() => onOpen(s.id)}
              title={active ? "当前会话" : "打开这个会话"}
              className={`mb-1 w-full rounded-md px-2.5 py-2 text-left transition-colors ${
                active
                  ? "cursor-default bg-sky-600/20 ring-1 ring-sky-700/60"
                  : "hover:bg-slate-800/70"
              }`}
            >
              <div className="truncate text-sm text-slate-200">
                {s.title || "未命名会话"}
              </div>
              <div className="mt-0.5 flex items-center gap-1.5 text-[11px] text-slate-500">
                <span className="truncate">{s.model}</span>
                <span>·</span>
                <span className="shrink-0">{relTime(s.created_at)}</span>
              </div>
            </button>
          );
        })}
      </div>
    </aside>
  );
}
