// 设置外壳：左侧竖排导航 + 右侧内容。
//
// 竖排而不是横向页签：设置项会越来越多（渠道 / 模型 / 工具 / 技能 / MCP / 会话 /
// 外观 / 关于），横向排到第七个就开始换行或需要横向滚动，而竖排永远只是变长。
// 参考实现（kalo / deepseek-harness）都用这一种。
//
// 导航列用 `bg-sidebar`（整套色阶里最深的一档），选中项用 `bg-ink/10` 提亮 ——
// 明暗两个主题下「选中 = 比周围亮」的关系一致，不需要分隔线。

import { useEffect, useRef, useState } from "react";
import {
  ChevronLeft,
  Clock,
  Cable,
  Cpu,
  FileDiff,
  Info,
  MessagesSquare,
  Palette,
  Plug,
  Server,
  Sparkles,
  Wrench,
} from "lucide-react";

import { AboutPage } from "./AboutPage";
import { ChangesPage } from "./ChangesPage";
import { AppearancePage } from "./AppearancePage";
import { ChannelsPage } from "./ChannelsPage";
import { ConnectorsPage } from "./ConnectorsPage";
import { McpPage } from "./McpPage";
import { ModelsPage } from "./ModelsPage";
import { SchedulesPage } from "./SchedulesPage";
import { SessionsPage } from "./SessionsPage";
import { SkillsPage } from "./SkillsPage";
import { ToolsPage } from "./ToolsPage";
import { useStore } from "../../store";

// 分组：按「多久来一次」排 —— 接入（第一次配好就很少动）、能力（时常调）、
// 数据（翻记录）、应用本身。组标题让十一项不再是一长串平铺。
const GROUPS = [
  {
    label: "接入",
    tabs: [
      { id: "channels", label: "渠道", icon: Plug },
      { id: "models", label: "模型", icon: Cpu },
      { id: "connectors", label: "连接器", icon: Cable },
      { id: "mcp", label: "MCP", icon: Server },
    ],
  },
  {
    label: "能力",
    tabs: [
      { id: "tools", label: "工具", icon: Wrench },
      { id: "skills", label: "技能", icon: Sparkles },
      { id: "schedules", label: "定时任务", icon: Clock },
    ],
  },
  {
    label: "记录",
    tabs: [
      { id: "sessions", label: "会话", icon: MessagesSquare },
      { id: "changes", label: "变更", icon: FileDiff },
    ],
  },
  {
    label: "应用",
    tabs: [
      { id: "appearance", label: "外观", icon: Palette },
      { id: "about", label: "关于", icon: Info },
    ],
  },
] as const;

type TabId = (typeof GROUPS)[number]["tabs"][number]["id"];
const ALL: TabId[] = GROUPS.flatMap((g) => g.tabs.map((t) => t.id));
const TAB_KEY = "carter.settings.tab";

/** 记住上次停在哪一页：改配置往往是来回几趟，每次都从「渠道」重新点进去很烦。 */
function readTab(): TabId {
  const saved = localStorage.getItem(TAB_KEY) as TabId | null;
  return saved && ALL.includes(saved) ? saved : "channels";
}

export function SettingsPage() {
  const [tab, setTabState] = useState<TabId>(readTab);
  const scrollRef = useRef<HTMLDivElement>(null);
  const navRef = useRef<HTMLDivElement>(null);
  const setView = useStore((s) => s.setView);

  const setTab = (id: TabId) => {
    setTabState(id);
    localStorage.setItem(TAB_KEY, id);
    // 换页回到顶部：停在上一页滚到的位置，新页会从中间某处开始，很迷惑。
    scrollRef.current?.scrollTo({ top: 0 });
  };

  // Esc 退出设置回到对话 —— 设置是「去一下就回来」的地方。
  // 输入框、模态里的 Esc 各有其主，不抢。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t?.closest("input, textarea, select, [contenteditable], [role=menu]")) return;
      if (document.querySelector("[data-modal-layer]")) return;
      setView("chat");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setView]);

  // 导航里的方向键：↑↓ 换页，焦点跟着走（roving tabindex，整列只占一个 Tab 停靠点）。
  const onNavKey = (e: React.KeyboardEvent) => {
    const i = ALL.indexOf(tab);
    let next = -1;
    if (e.key === "ArrowDown") next = (i + 1) % ALL.length;
    else if (e.key === "ArrowUp") next = (i - 1 + ALL.length) % ALL.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = ALL.length - 1;
    if (next < 0) return;
    e.preventDefault();
    setTab(ALL[next]);
    navRef.current?.querySelector<HTMLElement>(`[data-tab="${ALL[next]}"]`)?.focus();
  };

  return (
    <div className="flex min-h-0 flex-1">
      <nav className="flex w-48 shrink-0 flex-col overflow-y-auto border-r border-edge bg-sidebar">
        <div className="flex items-center gap-1 px-3 pb-1 pt-3">
          <button
            onClick={() => setView("chat")}
            title="回到对话（Esc）"
            className="carter-press flex items-center gap-1 rounded-md px-1.5 py-1 text-xs text-dim outline-none hover:bg-ink/[0.045] hover:text-ink"
          >
            <ChevronLeft size={13} />
            返回
          </button>
        </div>
        <div className="px-4 pb-3 pt-1 text-[16px] font-semibold tracking-tight">设置</div>
        <div
          ref={navRef}
          role="tablist"
          aria-orientation="vertical"
          onKeyDown={onNavKey}
          className="flex flex-col px-2 pb-4"
        >
          {GROUPS.map((g, gi) => (
            <div key={g.label} className={gi > 0 ? "mt-3" : ""}>
              <div className="px-3 pb-1 text-[10px] font-medium tracking-wider text-dim/70">
                {g.label}
              </div>
              <div className="flex flex-col gap-px">
                {g.tabs.map((t) => {
                  const on = tab === t.id;
                  return (
                    <button
                      key={t.id}
                      data-tab={t.id}
                      role="tab"
                      aria-selected={on}
                      tabIndex={on ? 0 : -1}
                      onClick={() => setTab(t.id)}
                      className={`carter-press relative flex items-center gap-2 rounded-md px-3 py-1.5 text-left text-sm ${
                        on
                          ? "bg-ink/[0.08] font-medium text-ink"
                          : "text-dim hover:bg-ink/[0.045] hover:text-ink"
                      }`}
                    >
                      <t.icon size={14} className={on ? "text-ink" : ""} />
                      {t.label}
                    </button>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      </nav>

      <div ref={scrollRef} className="min-w-0 flex-1 overflow-y-auto [scrollbar-gutter:stable]">
        {/* key 让换页时重挂这一层，`.carter-enter` 才会重放 —— 页面是「换了一块」，
            不是原地变内容。只淡入 + 4px，快到不挡手。 */}
        <div key={tab} role="tabpanel" className="carter-enter carter-settings mx-auto max-w-2xl px-8 py-8">
          {tab === "channels" && <ChannelsPage />}
          {tab === "connectors" && <ConnectorsPage />}
          {tab === "models" && <ModelsPage />}
          {tab === "tools" && <ToolsPage />}
          {tab === "schedules" && <SchedulesPage />}
          {tab === "changes" && <ChangesPage />}
          {tab === "skills" && <SkillsPage />}
          {tab === "mcp" && <McpPage />}
          {tab === "sessions" && <SessionsPage />}
          {tab === "appearance" && <AppearancePage />}
          {tab === "about" && <AboutPage />}
        </div>
      </div>
    </div>
  );
}
