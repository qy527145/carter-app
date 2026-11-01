// 呈现密度的切换药丸：两段都写着名字，实心的那一段就是当前模式。
//
// 之前是一个单按钮、只显示**当前**模式的名字 —— 静止时分不清显示的是现状、
// 还是点一下会切到的目标，用户看不出自己现在在哪一档。两段并列就没有这个歧义：
// 亮着的那一段是「现在」，另一段是「点它会去的地方」。
//
// 放在对话顶栏而不是只放设置页：它是随口对话与排障之间来回切的视角，切一次
// 不该离开对话再翻两层菜单。设置页里用的是同一个组件，两边读写同一份偏好。

import { ListTree, Rows3 } from "lucide-react";

import type { ConversationMode } from "../lib/api";
import { setConversationMode, useConversationMode } from "../lib/conversationMode";

const MODES: {
  id: ConversationMode;
  label: string;
  icon: typeof Rows3;
  hint: string;
}[] = [
  {
    id: "detailed",
    label: "详细",
    icon: ListTree,
    hint: "每一次工具调用各占一张卡片，过程全部摊开",
  },
  {
    id: "compact",
    label: "精简",
    icon: Rows3,
    hint: "思考与工具调用折成一行摘要，只留正文（工具照常执行）",
  },
];

export function ModePill({ className = "" }: { className?: string }) {
  const mode = useConversationMode();
  return (
    <div
      role="group"
      aria-label="对话呈现密度"
      className={`inline-flex shrink-0 items-center rounded-full bg-ink/[0.06] p-0.5 ${className}`}
    >
      {MODES.map((m) => {
        const active = m.id === mode;
        const Icon = m.icon;
        return (
          <button
            key={m.id}
            type="button"
            aria-pressed={active}
            title={`${m.hint}（${active ? "当前模式" : "点一下切过去"}）`}
            onClick={() => setConversationMode(m.id)}
            className={`inline-flex items-center gap-1 rounded-full px-2 py-[3px] text-[11px] transition-colors ${
              // 选中段用「比轨道亮一档」而不是描边：轨道本身已经是比页面深的一层，
              // 再描一圈线就成了两颗按钮，读不出「一个开关的两档」。
              // 阴影要小 —— `shadow-soft` 的扩散是按面板卡的尺寸调的，放在 22px
              // 高的药丸上会糊成一圈。
              active
                ? "bg-elevated font-medium text-ink shadow-[0_1px_2px_rgba(0,0,0,0.06)]"
                : "text-dim hover:text-ink"
            }`}
          >
            <Icon size={11} />
            {m.label}
          </button>
        );
      })}
    </div>
  );
}