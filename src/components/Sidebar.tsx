// 左侧栏：视图切换 + 会话列表 + 主题。
//
// 宽度可拖，拖完存进设置的 `sidebar_width`（应用私有，见 lib/api 的 AppSettings）。
// 会话列表按工作目录（项目）分组折叠，只展开当前目录那组 —— 别的项目的会话
// 一点就能找到，平时又不挤占当前项目的列表。

import { useCallback, useEffect, useRef, useState } from "react";
import {
  ChevronRight,
  Folder,
  MessageSquare,
  Monitor,
  Moon,
  Plus,
  Search,
  Settings as SettingsIcon,
  Sun,
} from "lucide-react";

import { setAppSettings, type AppSettings, type SessionRow, type ThemePref } from "../lib/api";
import { baseName } from "../lib/format";
import { modelSearchText, useModelCatalog } from "../lib/modelLabel";
import { newSession, startInDirectory } from "../lib/sessionOps";
import { groupByProject, SessionList } from "./SessionList";
import { Button, IconButton } from "./ui/Button";
import { Input } from "./ui/Input";
import { useStore } from "../store";

const THEMES: { id: ThemePref; label: string; icon: typeof Sun }[] = [
  { id: "system", label: "跟随系统", icon: Monitor },
  { id: "light", label: "浅色", icon: Sun },
  { id: "dark", label: "深色", icon: Moon },
];

const MIN_W = 200;
const MAX_W = 420;

function ThemeToggle() {
  const settings = useStore((s) => s.settings);
  const setSettings = useStore((s) => s.setSettings);
  if (!settings) return null;

  const pick = (theme: ThemePref) => {
    const next = { ...settings, theme };
    setSettings(next); // 先落界面，主题立刻切
    void setAppSettings(next).catch(() => {
      /* 存不下就只是下次启动回到旧主题，不值得打断用户 */
    });
  };

  return (
    <div className="flex gap-1">
      {THEMES.map((t) => (
        <button
          key={t.id}
          title={t.label}
          aria-label={t.label}
          onClick={() => pick(t.id)}
          className={`flex flex-1 items-center justify-center rounded-md py-1.5 transition-colors ${
            settings.theme === t.id
              ? "bg-ink/10 text-ink"
              : "text-dim hover:bg-ink/5 hover:text-ink"
          }`}
        >
          <t.icon size={13} />
        </button>
      ))}
    </div>
  );
}

/**
 * 按目录分组的会话列表，每组可折叠。
 *
 * 默认只展开当前目录那组；换到别的目录（打开了那边的会话）时跟着改成只展开新的
 * 那组 —— 手动展开过的组不保留，否则开过几次别的项目之后侧栏又会全摊开。
 * 搜索时全部展开：命中项藏在折叠组里等于没搜到。
 */
function ProjectGroups({
  sessions,
  activeId,
  currentCwd,
  searching,
}: {
  sessions: SessionRow[];
  activeId: string | null;
  currentCwd: string;
  searching: boolean;
}) {
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set([currentCwd]));
  useEffect(() => {
    // 空 = 当前会话刚被删掉（store 清了 cwd）或启动首帧还没开会话。这时保持原样，
    // 侧栏该继续展开刚才那组，而不是全部收起。
    if (currentCwd) setExpanded(new Set([currentCwd]));
  }, [currentCwd]);

  const setView = useStore((s) => s.setView);
  const groups = groupByProject(sessions, currentCwd);
  const toggle = (cwd: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(cwd)) next.delete(cwd);
      else next.add(cwd);
      return next;
    });

  return (
    <div className="flex flex-col gap-1">
      {groups.map((g) => {
        const open = searching || expanded.has(g.cwd);
        const current = g.cwd === currentCwd;
        return (
          <div key={g.cwd}>
            {/* 标题行拆成两个按钮：折叠与新建是两件事，按钮也不能套按钮。 */}
            <div className="flex items-center rounded-md transition-colors hover:bg-ink/5">
              <button
                onClick={() => toggle(g.cwd)}
                title={g.cwd}
                className={`flex min-w-0 flex-1 items-center gap-1 px-1.5 py-1 text-left text-[11px] ${
                  current ? "text-ink" : "text-dim"
                }`}
              >
                <ChevronRight
                  size={12}
                  className={`shrink-0 transition-transform ${open ? "rotate-90" : ""}`}
                />
                <Folder size={11} className="shrink-0" />
                <span className={`truncate ${current ? "font-medium" : ""}`}>
                  {baseName(g.cwd) || "未知目录"}
                </span>
                <span className="ml-auto shrink-0 text-dim">{g.sessions.length}</span>
              </button>
              <IconButton
                title={`在 ${g.cwd} 新建对话`}
                aria-label="在此目录新建对话"
                onClick={() => {
                  setView("chat");
                  void startInDirectory(g.cwd);
                }}
                className="p-1"
              >
                <Plus size={12} />
              </IconButton>
            </div>
            {open && (
              <div className="pl-2">
                <SessionList
                  sessions={g.sessions}
                  activeId={activeId}
                  dense
                  groupByTime={false}
                />
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

export function Sidebar() {
  const sessions = useStore((s) => s.sessions);
  const sessionId = useStore((s) => s.sessionId);
  const cwd = useStore((s) => s.cwd);
  const appInfo = useStore((s) => s.appInfo);
  const view = useStore((s) => s.view);
  const setView = useStore((s) => s.setView);
  const settings = useStore((s) => s.settings);
  const setSettings = useStore((s) => s.setSettings);

  const [query, setQuery] = useState("");
  const [width, setWidth] = useState(settings?.sidebar_width ?? 260);
  const dragging = useRef(false);

  // 设置加载晚于首帧，拿到后对齐一次。
  useEffect(() => {
    if (settings?.sidebar_width) setWidth(settings.sidebar_width);
  }, [settings?.sidebar_width]);

  const onDragStart = useCallback((e: React.PointerEvent) => {
    dragging.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
  }, []);

  const onDragMove = useCallback((e: React.PointerEvent) => {
    if (!dragging.current) return;
    setWidth(Math.min(MAX_W, Math.max(MIN_W, e.clientX)));
  }, []);

  const saveWidth = useCallback(
    (w: number) => {
      const s = useStore.getState().settings;
      if (!s) return;
      const next: AppSettings = { ...s, sidebar_width: w };
      setSettings(next);
      // 存不下就只是下次启动回到旧宽度，不值得打断用户。
      void setAppSettings(next).catch(() => {});
    },
    [setSettings],
  );

  const onDragEnd = useCallback(() => {
    if (!dragging.current) return;
    dragging.current = false;
    // 只在松手时落盘 —— 拖动过程中每一帧都写文件没有意义。
    saveWidth(width);
  }, [width, saveWidth]);

  const models = useModelCatalog();
  const q = query.trim().toLowerCase();
  // 模型的三种写法都能搜到（引用 / 展示名 / 实际名）—— 用户记得住的往往不是
  // 配置里那个块名。
  const shown = q
    ? sessions.filter(
        (s) =>
          (s.title ?? "").toLowerCase().includes(q) ||
          modelSearchText(s.model, models).includes(q),
      )
    : sessions;

  return (
    <>
      <aside
        style={{ width }}
        className="flex shrink-0 flex-col border-r border-edge bg-sidebar"
      >
        {/* 品牌行 */}
        <div className="flex items-center gap-2 px-3 pb-2 pt-3">
          <div className="flex size-7 shrink-0 items-center justify-center rounded-lg bg-ink text-[var(--accent-contrast)]">
            <MessageSquare size={14} />
          </div>
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm font-semibold tracking-tight">Carter</div>
            {appInfo && <div className="text-[10px] text-dim">v{appInfo.version}</div>}
          </div>
        </div>

        <div className="px-3 pb-3 pt-1">
          <Button variant="primary" onClick={newSession} className="w-full">
            <Plus size={14} />
            新对话
          </Button>
        </div>

        {/* 常驻，不按会话条数隐藏 —— 会话列表短的时候藏起来省不了多少地方，
            却让人以为「这个应用没有搜索」；而且隐藏期间 query 仍然在生效，
            会出现列表被过滤着、却没有输入框可以清空的状态。 */}
        <div className="px-2 pb-2">
          <div className="relative">
            <Search
              size={12}
              className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-dim"
            />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索会话"
              className="py-1.5 pl-7 text-xs"
            />
          </div>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
          {sessions.length === 0 && (
            <div className="px-2 py-6 text-center text-xs text-dim">暂无历史会话</div>
          )}
          {shown.length === 0 && sessions.length > 0 && (
            <div className="px-2 py-6 text-center text-xs text-dim">没有匹配的会话</div>
          )}
          <ProjectGroups
            sessions={shown}
            activeId={sessionId}
            currentCwd={cwd}
            searching={q !== ""}
          />
        </div>

        {/* 底部：设置 + 主题。设置放在这里而不是顶部导航里 —— 它是「偶尔去一次」
            的地方，占着顶部第一屏的位置不值当；而左下角是这类入口的常规位置。 */}
        <div className="space-y-1 border-t border-edge p-2">
          <button
            onClick={() => setView("settings")}
            className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors ${
              view === "settings" ? "bg-ink/10 font-medium text-ink" : "text-dim hover:bg-ink/5"
            }`}
          >
            <SettingsIcon size={14} />
            设置
          </button>
          <ThemeToggle />
        </div>
      </aside>

      {/* 拖动条。落在 aside 外面，免得被它的 overflow 裁掉。 */}
      <div
        onPointerDown={onDragStart}
        onPointerMove={onDragMove}
        onPointerUp={onDragEnd}
        // 双击复位到默认宽度 —— 拖窄了想调回来时不用瞄着拖。
        onDoubleClick={() => saveWidth(260)}
        className="w-1 shrink-0 cursor-col-resize transition-colors hover:bg-edge-strong"
      />
    </>
  );
}
