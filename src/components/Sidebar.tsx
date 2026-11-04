// 左侧栏：视图切换 + 会话列表 + 主题。
//
// 宽度可拖，拖完存进设置的 `sidebar_width`（应用私有，见 lib/api 的 AppSettings）。
// 会话列表按工作目录（项目）分组折叠，只展开当前目录那组 —— 别的项目的会话
// 一点就能找到，平时又不挤占当前项目的列表。

import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ChevronRight,
  Copy,
  Folder,
  FolderOpen,
  ListCollapse,
  ListTree,
  MessageSquare,
  Monitor,
  Moon,
  MoreHorizontal,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Radar,
  Search,
  Settings as SettingsIcon,
  Sun,
  Target,
  Trash,
  X,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

import { type AppSettings, type SessionRow, setAppSettings, type ThemePref } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { baseName } from "../lib/format";
import { modelSearchText, useModelCatalog } from "../lib/modelLabel";
import {
  ASSISTANT_CHAT_ID,
  newSession,
  openAssistantChat,
  removeProject,
  startInDirectory,
} from "../lib/sessionOps";
import { toastError } from "../lib/toast";
import { useStore } from "../store";
import { groupByProject, SessionList } from "./SessionList";
import { Button, IconButton } from "./ui/Button";
import { type MenuEntry, useContextMenu } from "./ui/ContextMenu";
import { Input } from "./ui/Input";
import { Collapse, ConfirmDialog } from "./ui/Panel";

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
  const idx = Math.max(
    0,
    THEMES.findIndex((t) => t.id === settings.theme),
  );

  return (
    <div
      role="radiogroup"
      aria-label="主题"
      className="relative flex rounded-md bg-ink/[0.04] p-0.5"
    >
      {/* 选中块是一块滑过去的底，而不是三个按钮各自变色 —— 切换时眼睛跟着它走。 */}
      <span
        aria-hidden
        style={{ transform: `translateX(${idx * 100}%)` }}
        className="absolute inset-y-0.5 left-0.5 w-[calc((100%-4px)/3)] rounded-[5px] bg-elevated shadow-soft transition-transform duration-200 ease-[var(--ease-out)]"
      />
      {THEMES.map((t) => (
        <button
          key={t.id}
          role="radio"
          aria-checked={settings.theme === t.id}
          title={t.label}
          aria-label={t.label}
          onClick={() => pick(t.id)}
          className={`carter-press relative flex flex-1 items-center justify-center rounded-[5px] py-1 ${
            settings.theme === t.id ? "text-ink" : "text-dim hover:text-ink"
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
  query,
}: {
  sessions: SessionRow[];
  activeId: string | null;
  currentCwd: string;
  query: string;
}) {
  const searching = query !== "";
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set([currentCwd]));
  useEffect(() => {
    // 空 = 当前会话刚被删掉（store 清了 cwd）或启动首帧还没开会话。这时保持原样，
    // 侧栏该继续展开刚才那组，而不是全部收起。
    if (currentCwd) setExpanded(new Set([currentCwd]));
  }, [currentCwd]);

  const setView = useStore((s) => s.setView);
  const setTeamCwd = useStore((s) => s.setTeamCwd);
  const home = useStore((s) => s.appInfo?.default_cwd ?? "");
  const menu = useContextMenu();
  // 待确认删除的那一组。
  const [deleting, setDeleting] = useState<{ cwd: string; count: number } | null>(null);
  const groups = groupByProject(sessions);
  const toggle = (cwd: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(cwd)) next.delete(cwd);
      else next.add(cwd);
      return next;
    });

  const projectMenu = (cwd: string, count: number, gone: boolean): MenuEntry[] => {
    const open = expanded.has(cwd);
    const entries: MenuEntry[] = [
      {
        label: "在此目录新建对话",
        icon: <Plus size={12} />,
        disabled: gone,
        onSelect: () => {
          setView("chat");
          void startInDirectory(cwd);
        },
      },
    ];
    if (cwd !== home && !gone) {
      entries.push({
        label: "目标与任务",
        icon: <Target size={12} />,
        onSelect: () => {
          setTeamCwd(cwd);
          setView("team");
        },
      });
    }
    entries.push(
      "separator",
      {
        label: "在访达中显示",
        icon: <FolderOpen size={12} />,
        disabled: gone,
        onSelect: () => void revealItemInDir(cwd).catch(toastError),
      },
      {
        label: "复制路径",
        icon: <Copy size={12} />,
        onSelect: () => void copyText(cwd, "已复制路径"),
      },
      "separator",
      {
        label: open ? "折叠" : "展开",
        icon: <ChevronRight size={12} className={open ? "rotate-90" : ""} />,
        onSelect: () => toggle(cwd),
      },
      {
        label: "只展开这一组",
        icon: <ListCollapse size={12} />,
        onSelect: () => setExpanded(new Set([cwd])),
      },
      {
        label: "全部展开",
        icon: <ListTree size={12} />,
        onSelect: () => setExpanded(new Set(groups.map((g) => g.cwd))),
      },
      "separator",
      {
        label: `删除全部 ${count} 条会话…`,
        icon: <Trash size={12} />,
        danger: true,
        onSelect: () => setDeleting({ cwd, count }),
      },
    );
    return entries;
  };

  return (
    <div data-session-scope className="flex flex-col gap-0.5">
      {groups.map((g) => {
        const open = searching || expanded.has(g.cwd);
        const current = g.cwd === currentCwd;
        // 组内各条记的是同一个目录，取哪条都一样。
        const gone = !g.sessions[0]?.cwd_exists;
        // 折起来的组里藏着当前会话：标题行上给个点，免得「我刚才那条去哪了」。
        const hidesActive = !open && g.sessions.some((x) => x.id === activeId);
        return (
          <div key={g.cwd}>
            {/* 标题行拆成两个按钮：折叠与新建是两件事，按钮也不能套按钮。 */}
            <div
              onContextMenu={(e) => {
                e.preventDefault();
                menu.open(
                  { x: e.clientX, y: e.clientY },
                  projectMenu(g.cwd, g.sessions.length, gone),
                );
              }}
              className="group/project sticky top-0 z-[1] flex items-center rounded-md bg-sidebar transition-colors hover:bg-[color-mix(in_srgb,var(--text)_4.5%,var(--bg-sidebar))]"
            >
              <button
                onClick={() => toggle(g.cwd)}
                aria-expanded={open}
                title={gone ? `${g.cwd}（目录已不存在）` : g.cwd}
                className={`flex min-w-0 flex-1 items-center gap-1 px-1.5 py-1 text-left text-[11px] outline-none ${
                  current ? "text-ink" : "text-dim"
                }`}
              >
                <ChevronRight
                  size={12}
                  className={`shrink-0 transition-transform duration-200 ease-[var(--ease-out)] ${open ? "rotate-90" : ""}`}
                />
                <Folder size={11} className="shrink-0" />
                <span
                  className={`truncate ${current ? "font-medium" : ""} ${gone ? "line-through opacity-60" : ""}`}
                >
                  {baseName(g.cwd) || "未知目录"}
                </span>
                {hidesActive && (
                  <span
                    title="当前会话在这一组里"
                    className="size-1 shrink-0 rounded-full bg-ink"
                  />
                )}
                <span className="ml-auto shrink-0 tabular-nums text-dim/70 group-hover/project:hidden">
                  {g.sessions.length}
                </span>
              </button>
              {/* 悬停才露出来：静止时标题行只有名字和条数。 */}
              <div className="hidden items-center group-hover/project:flex group-focus-within/project:flex">
                {g.cwd !== home && !gone && (
                  <IconButton
                    title={`查看 ${baseName(g.cwd)} 的目标与任务`}
                    aria-label="查看此项目的目标与任务"
                    onClick={() => {
                      setTeamCwd(g.cwd);
                      setView("team");
                    }}
                    className="p-1"
                  >
                    <Target size={12} />
                  </IconButton>
                )}
                <IconButton
                  title="更多"
                  aria-label="项目操作"
                  onClick={(e) => {
                    const r = e.currentTarget.getBoundingClientRect();
                    menu.open(
                      { x: r.right, y: r.bottom + 4, align: "end" },
                      projectMenu(g.cwd, g.sessions.length, gone),
                    );
                  }}
                  className="p-1"
                >
                  <MoreHorizontal size={12} />
                </IconButton>
              </div>
              <IconButton
                // 目录没了就别让点：后端也会拒，但先在这里说清楚比点完弹错误强。
                disabled={gone}
                title={gone ? "目录已不存在，无法在这里新建对话" : `在 ${g.cwd} 新建对话`}
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
            <Collapse open={open}>
              <div className="pb-1 pl-2 pt-0.5">
                <SessionList
                  sessions={g.sessions}
                  activeId={activeId}
                  dense
                  groupByTime={false}
                  query={query}
                />
              </div>
            </Collapse>
          </div>
        );
      })}

      {deleting && (
        <ConfirmDialog
          title="删除项目的全部会话"
          danger
          confirmLabel={`删除 ${deleting.count} 条会话`}
          message={
            <>
              将删除 <span className="mono text-ink">{deleting.cwd}</span> 下的全部 {deleting.count}{" "}
              条会话，包括它们的对话历史。
              <br />
              项目目录本身和里面的文件不受影响；会话记录会从磁盘上直接删掉，
              <strong className="font-medium">不可恢复</strong>。
            </>
          }
          onCancel={() => setDeleting(null)}
          onConfirm={() => {
            const cwd = deleting.cwd;
            setDeleting(null);
            void removeProject(cwd);
          }}
        />
      )}
    </div>
  );
}

/**
 * 常驻助理的入口：放在「新对话」下面，点开就是和它的对话（面板从对话顶部进）。
 * 角标是要人处理的件数 —— 健康的时候什么都不显示（原型的「面板只显示要人处理的」）；
 * 在跑的工单只用一个小点表示「它在干活」。
 */
function AssistantEntry() {
  const view = useStore((s) => s.view);
  const sessionId = useStore((s) => s.sessionId);
  const a = useStore((s) => s.assistant);
  const active = view === "assistant" || (view === "chat" && sessionId === ASSISTANT_CHAT_ID);
  const attention = a?.attention ?? 0;
  const working = a?.tickets.some((t) => t.status === "working") ?? false;
  return (
    <button
      onClick={() => void openAssistantChat()}
      className={`${NAV_ITEM} ${active ? navState(true) : "text-ink hover:bg-ink/[0.045]"}`}
    >
      <Radar size={14} className="text-tone-blue" />
      卡特助理
      {a?.settings.paused && <span className="text-[10px] text-dim">已暂停</span>}
      <span className="ml-auto flex items-center gap-1.5">
        {working && (
          <span title="有工单在跑" className="size-1.5 animate-pulse rounded-full bg-tone-blue" />
        )}
        {attention > 0 && (
          <span className="min-w-4 rounded-full bg-tone-orange px-1 text-center text-[10px] font-medium leading-4 text-white">
            {attention}
          </span>
        )}
      </span>
    </button>
  );
}

/**
 * 团队页入口。与「助理」并列，而不是塞进设置 ——
 * 这里是每天要看的地方（我该做什么、现在卡在哪），不是偶尔改一次配置的地方。
 */
function TeamEntry() {
  const setView = useStore((s) => s.setView);
  const view = useStore((s) => s.view);
  // 显示正在看哪个项目：入口本身就回答「点进去会看到谁的团队」。
  const teamCwd = useStore((s) => s.teamCwd);
  return (
    <button onClick={() => setView("team")} className={`${NAV_ITEM} ${navState(view === "team")}`}>
      <Target size={14} className={view === "team" ? "text-tone-violet" : ""} />
      目标与任务
      {teamCwd && (
        <span className="ml-auto min-w-0 truncate text-[10px] font-normal text-dim" title={teamCwd}>
          {baseName(teamCwd)}
        </span>
      )}
    </button>
  );
}

/** 侧栏里的一个导航入口（设置 / 目标与任务）。选中态与悬停态在全栏统一。 */
export const NAV_ITEM =
  "carter-press flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm outline-none";
export const navState = (active: boolean) =>
  active ? "bg-ink/[0.08] font-medium text-ink" : "text-dim hover:bg-ink/[0.045] hover:text-ink";

const IS_MAC = navigator.platform.toLowerCase().includes("mac");
const MOD = IS_MAC ? "⌘" : "Ctrl+";

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
  const [resizing, setResizing] = useState(false);
  // 收起侧栏：只是这一次运行里的状态，不落盘 —— 下次打开应用侧栏还在，
  // 不会出现「侧栏没了、不知道怎么调出来」。
  const [collapsed, setCollapsed] = useState(false);
  const searchRef = useRef<HTMLInputElement>(null);
  const dragging = useRef(false);

  // 设置加载晚于首帧，拿到后对齐一次。
  useEffect(() => {
    if (settings?.sidebar_width) setWidth(settings.sidebar_width);
  }, [settings?.sidebar_width]);

  // 全局快捷键：⌘K 搜会话（侧栏收着就先展开）、⌘B 收展侧栏、⌘N 新对话、⌘, 设置。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = IS_MAC ? e.metaKey : e.ctrlKey;
      if (!mod || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k === "k") {
        e.preventDefault();
        setCollapsed(false);
        // 侧栏开着时**同步**聚焦：按下 ⌘K 紧接着打字，延一帧的话头几个字会落到别处。
        // 收着的时候 input 还是 inert，只能等展开那一帧再聚焦。
        const focus = () => {
          searchRef.current?.focus();
          searchRef.current?.select();
        };
        focus();
        if (document.activeElement !== searchRef.current) requestAnimationFrame(focus);
      } else if (k === "b" && !e.shiftKey) {
        e.preventDefault();
        setCollapsed((c) => !c);
      } else if (k === "n" && !e.shiftKey) {
        e.preventDefault();
        newSession();
      } else if (k === ",") {
        e.preventDefault();
        setView("settings");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setView]);

  const onDragStart = useCallback((e: React.PointerEvent) => {
    dragging.current = true;
    setResizing(true);
    e.currentTarget.setPointerCapture(e.pointerId);
  }, []);

  const onDragMove = useCallback((e: React.PointerEvent) => {
    if (!dragging.current) return;
    // 往左拖过最小宽度一大截 = 想把它收起来（与 VS Code 的手势一致）。
    if (e.clientX < MIN_W / 2) {
      setCollapsed(true);
      return;
    }
    setCollapsed(false);
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
    setResizing(false);
    // 只在松手时落盘 —— 拖动过程中每一帧都写文件没有意义。
    saveWidth(width);
  }, [width, saveWidth]);

  const models = useModelCatalog();
  const q = query.trim().toLowerCase();
  // 模型的三种写法都能搜到（引用 / 展示名 / 实际名）—— 用户记得住的往往不是
  // 配置里那个块名。
  // 助理对话有自己的入口，不混在项目的会话列表里。
  const listed = sessions.filter((s) => s.id !== ASSISTANT_CHAT_ID);
  const shown = q
    ? listed.filter(
        (s) =>
          (s.title ?? "").toLowerCase().includes(q) ||
          modelSearchText(s.model, models).includes(q) ||
          s.cwd.toLowerCase().includes(q),
      )
    : listed;

  return (
    <>
      <aside
        // 收起时宽度过渡到 0，内容按原宽度排好、只是被裁掉 —— 不会在动画途中
        // 一路挤压、换行。拖动期间关掉过渡，否则边线会跟不上手。
        style={{ width: collapsed ? 0 : width }}
        aria-hidden={collapsed}
        inert={collapsed}
        className={`carter-sidebar flex shrink-0 flex-col overflow-hidden bg-sidebar [--row-bg:var(--bg-sidebar)] ${
          collapsed ? "" : "border-r border-edge"
        } ${resizing ? "" : "transition-[width] duration-200 ease-[var(--ease-out)]"}`}
      >
        <div style={{ width }} className="flex min-h-0 flex-1 flex-col">
          {/* 品牌行 */}
          <div className="flex items-center gap-2 px-3 pb-2 pt-3">
            <div className="flex size-7 shrink-0 items-center justify-center rounded-lg bg-ink text-[var(--accent-contrast)]">
              <MessageSquare size={14} />
            </div>
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm font-semibold tracking-tight">Carter</div>
              {appInfo && <div className="text-[10px] text-dim">v{appInfo.version}</div>}
            </div>
            <IconButton
              title={`收起侧栏（${MOD}B）`}
              aria-label="收起侧栏"
              onClick={() => setCollapsed(true)}
            >
              <PanelLeftClose size={14} />
            </IconButton>
          </div>

          <div className="px-3 pb-3 pt-1">
            <Button
              variant="primary"
              onClick={newSession}
              title={`新对话（${MOD}N）`}
              className="group/new w-full"
            >
              <Plus size={14} />
              新对话
              <kbd className="ml-auto font-sans text-[10px] font-normal opacity-0 transition-opacity group-hover/new:opacity-50">
                {MOD}N
              </kbd>
            </Button>
            <div className="mt-1.5 space-y-0.5">
              <AssistantEntry />
              <TeamEntry />
            </div>
          </div>

          {/* 常驻，不按会话条数隐藏 —— 会话列表短的时候藏起来省不了多少地方，
            却让人以为「这个应用没有搜索」；而且隐藏期间 query 仍然在生效，
            会出现列表被过滤着、却没有输入框可以清空的状态。 */}
          <div className="px-2 pb-2">
            <div className="group/search relative">
              <Search
                size={12}
                className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-dim"
              />
              <Input
                ref={searchRef}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Escape") {
                    // 第一下清空，第二下（已经空了）把焦点还给页面。
                    if (query) setQuery("");
                    else e.currentTarget.blur();
                  } else if (e.key === "ArrowDown" || e.key === "Enter") {
                    // 从搜索框直接走进结果：↓ 落到第一条，↵ 直接打开第一条。
                    const first = document.querySelector<HTMLElement>("aside [data-session-row]");
                    if (!first) return;
                    e.preventDefault();
                    if (e.key === "Enter") first.click();
                    else first.focus();
                  }
                }}
                placeholder="搜索会话"
                className="py-1.5 pl-7 pr-12 text-xs"
              />
              {query ? (
                <button
                  onClick={() => {
                    setQuery("");
                    searchRef.current?.focus();
                  }}
                  aria-label="清空搜索"
                  className="carter-press absolute right-1.5 top-1/2 -translate-y-1/2 rounded p-0.5 text-dim hover:text-ink"
                >
                  <X size={12} />
                </button>
              ) : (
                <kbd className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 font-sans text-[10px] text-dim/60 group-focus-within/search:opacity-0">
                  {MOD}K
                </kbd>
              )}
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
            {listed.length === 0 && (
              <div className="px-2 py-8 text-center text-xs leading-relaxed text-dim">
                还没有历史会话
                <div className="mt-1 text-dim/70">发出第一句话后，会话会出现在这里</div>
              </div>
            )}
            {shown.length === 0 && listed.length > 0 && (
              <div className="px-2 py-8 text-center text-xs text-dim">
                没有匹配「{query.trim()}」的会话
                <button
                  onClick={() => setView("settings")}
                  className="mt-1.5 block w-full text-dim/80 underline-offset-2 hover:text-ink hover:underline"
                >
                  去「设置 · 会话」搜全文
                </button>
              </div>
            )}
            <ProjectGroups sessions={shown} activeId={sessionId} currentCwd={cwd} query={q} />
          </div>

          {/* 底部：设置 + 主题。设置放在这里而不是顶部导航里 —— 它是「偶尔去一次」
            的地方，占着顶部第一屏的位置不值当；而左下角是这类入口的常规位置。 */}
          <div className="space-y-1.5 border-t border-edge p-2">
            <button
              onClick={() => setView("settings")}
              title={`设置（${MOD},）`}
              className={`${NAV_ITEM} ${navState(view === "settings")}`}
            >
              <SettingsIcon size={14} />
              设置
            </button>
            <ThemeToggle />
          </div>
        </div>
      </aside>

      {/* 拖动条。落在 aside 外面，免得被它的 overflow 裁掉。
          命中区 6px 宽、视觉线 1px：好抓，又不显得粗。 */}
      {collapsed ? (
        // 收起后留一条窄轨：最常用的几个入口仍然一下就能点到，
        // 不必为了开个新对话先把整个侧栏拉出来。
        <nav className="flex w-11 shrink-0 flex-col items-center gap-1 border-r border-edge bg-sidebar py-3">
          <IconButton
            onClick={() => setCollapsed(false)}
            title={`展开侧栏（${MOD}B）`}
            aria-label="展开侧栏"
          >
            <PanelLeftOpen size={15} />
          </IconButton>
          <div className="my-1 h-px w-5 bg-edge" />
          <IconButton onClick={newSession} title={`新对话（${MOD}N）`} aria-label="新对话">
            <Plus size={15} />
          </IconButton>
          <IconButton
            onClick={() => void openAssistantChat()}
            title="卡特助理"
            aria-label="卡特助理"
          >
            <Radar size={15} />
          </IconButton>
          <IconButton
            onClick={() => setView("team")}
            title="目标与任务"
            aria-label="目标与任务"
            className={view === "team" ? "bg-ink/[0.08] text-ink" : ""}
          >
            <Target size={15} />
          </IconButton>
          <IconButton
            onClick={() => {
              setCollapsed(false);
              requestAnimationFrame(() => searchRef.current?.focus());
            }}
            title={`搜索会话（${MOD}K）`}
            aria-label="搜索会话"
          >
            <Search size={15} />
          </IconButton>
          <IconButton
            onClick={() => setView("settings")}
            title={`设置（${MOD},）`}
            aria-label="设置"
            className={`mt-auto ${view === "settings" ? "bg-ink/[0.08] text-ink" : ""}`}
          >
            <SettingsIcon size={15} />
          </IconButton>
        </nav>
      ) : (
        <div
          role="separator"
          aria-orientation="vertical"
          aria-label="调整侧栏宽度"
          onPointerDown={onDragStart}
          onPointerMove={onDragMove}
          onPointerUp={onDragEnd}
          onPointerCancel={onDragEnd}
          // 双击复位到默认宽度 —— 拖窄了想调回来时不用瞄着拖。
          onDoubleClick={() => {
            setWidth(260);
            saveWidth(260);
          }}
          title="拖动调整宽度 · 双击复位"
          className="group/resize relative z-10 -ml-[3px] w-1.5 shrink-0 cursor-col-resize"
        >
          <span
            className={`absolute inset-y-0 left-1/2 w-0.5 -translate-x-1/2 transition-colors duration-150 ${
              resizing ? "bg-tone-blue" : "group-hover/resize:bg-edge-strong"
            }`}
          />
        </div>
      )}
    </>
  );
}
