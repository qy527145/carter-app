// 设置外壳：左侧竖排导航 + 右侧内容。
//
// 竖排而不是横向页签：设置项会越来越多（渠道 / 模型 / 工具 / 技能 / MCP / 会话 /
// 外观 / 关于），横向排到第七个就开始换行或需要横向滚动，而竖排永远只是变长。
// 参考实现（kalo / deepseek-harness）都用这一种。

import { useState } from "react";
import {
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
import { McpPage } from "./McpPage";
import { ModelsPage } from "./ModelsPage";
import { SessionsPage } from "./SessionsPage";
import { SkillsPage } from "./SkillsPage";
import { ToolsPage } from "./ToolsPage";

const TABS = [
  { id: "channels", label: "渠道", icon: Plug },
  { id: "models", label: "模型", icon: Cpu },
  { id: "tools", label: "工具", icon: Wrench },
  { id: "changes", label: "变更", icon: FileDiff },
  { id: "skills", label: "技能", icon: Sparkles },
  { id: "mcp", label: "MCP", icon: Server },
  { id: "sessions", label: "会话", icon: MessagesSquare },
  { id: "appearance", label: "外观", icon: Palette },
  { id: "about", label: "关于", icon: Info },
] as const;

type TabId = (typeof TABS)[number]["id"];

export function SettingsPage() {
  const [tab, setTab] = useState<TabId>("channels");

  return (
    <div className="flex min-h-0 flex-1">
      <nav className="flex w-44 shrink-0 flex-col border-r border-edge bg-card/40">
        <div className="px-4 pb-1 pt-4 text-[11px] font-medium uppercase tracking-wider text-dim">
          设置
        </div>
        <div className="flex flex-col gap-0.5 px-2 py-2">
          {TABS.map((t) => (
            <button
              key={t.id}
              onClick={() => setTab(t.id)}
              className={`flex items-center gap-2 rounded-md px-3 py-1.5 text-left text-sm transition-colors ${
                tab === t.id
                  ? "bg-base font-medium text-ink"
                  : "text-dim hover:bg-base/60 hover:text-ink"
              }`}
            >
              <t.icon size={14} />
              {t.label}
            </button>
          ))}
        </div>
      </nav>

      <div className="min-w-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-2xl px-6 py-6">
          {tab === "channels" && <ChannelsPage />}
          {tab === "models" && <ModelsPage />}
          {tab === "tools" && <ToolsPage />}
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
