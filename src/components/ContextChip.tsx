// 预制上下文的卡片：输入框里和用户气泡里是同一枚，点开能看完整内容。
//
// 一次可以摆好几枚（「我的任务」里勾选几件事一起交给卡特）。多枚时排成一列而不是
// 挤成一行：标题要被读完整才有意义，截断成一排「任务 · 修复…」等于什么都没说。

import { useState } from "react";
import { ChevronDown, Kanban, ListChecks, Target, X } from "lucide-react";

import { CONTEXT_LABEL, type PromptContext } from "../lib/promptContext";
import { Collapse } from "./ui/Panel";

const LOOK = {
  task: { icon: ListChecks, tone: "bg-tone-blue-soft text-tone-blue" },
  goal: { icon: Target, tone: "bg-tone-violet-soft text-tone-violet" },
  board: { icon: Kanban, tone: "bg-tone-green-soft text-tone-green" },
} as const;

export function ContextChip({
  context,
  onRemove,
}: {
  context: PromptContext;
  /** 给了才显示移除按钮：发出去的那枚不能再改。 */
  onRemove?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const { icon: Icon, tone } = LOOK[context.kind];
  return (
    <div className="min-w-0 max-w-full">
      <div
        data-context-chip
        className="inline-flex max-w-full items-center rounded-lg bg-ink/[0.05] text-xs text-ink"
      >
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          title={open ? "收起" : "看看卡特会收到什么"}
          className="carter-press flex min-w-0 items-center gap-1.5 rounded-lg py-1 pl-1 pr-2 text-left"
        >
          <span className={`flex size-5 shrink-0 items-center justify-center rounded-md ${tone}`}>
            <Icon size={12} />
          </span>
          <span className="shrink-0 text-dim">{CONTEXT_LABEL[context.kind]}</span>
          <span className="min-w-0 truncate font-medium">{context.title}</span>
          <ChevronDown
            size={12}
            className={`shrink-0 text-dim transition-transform duration-200 ${open ? "rotate-180" : ""}`}
          />
        </button>
        {onRemove && (
          <button
            type="button"
            onClick={onRemove}
            title="移除"
            aria-label="移除这段上下文"
            className="mr-1 shrink-0 rounded p-0.5 text-dim transition-colors hover:bg-ink/10 hover:text-ink"
          >
            <X size={12} />
          </button>
        )}
      </div>
      <Collapse open={open}>
        <div className="mt-1.5 max-h-40 overflow-y-auto whitespace-pre-wrap break-words rounded-lg bg-ink/[0.03] px-2.5 py-2 text-left text-[11px] leading-relaxed text-dim">
          {context.body}
        </div>
      </Collapse>
    </div>
  );
}

/**
 * 一列卡片。
 *
 * 只有一枚时保持行内（它跟在一句话后面也读得通）；多枚时每枚独占一行，并在这一组
 * 左侧压一条竖线 —— 竖线是「它们是一伙的」，比给每枚卡片描边轻得多。
 */
export function ContextChips({
  contexts,
  onRemove,
  className = "",
}: {
  contexts: PromptContext[];
  /** 给了才显示移除按钮：发出去的那一叠不能再改。 */
  onRemove?: (index: number) => void;
  className?: string;
}) {
  if (contexts.length === 0) return null;
  if (contexts.length === 1) {
    return (
      <ContextChip context={contexts[0]} onRemove={onRemove ? () => onRemove(0) : undefined} />
    );
  }
  return (
    <div className={`flex flex-col gap-1 border-l-2 border-edge-strong/40 pl-2 ${className}`}>
      {contexts.map((c, i) => (
        <ContextChip key={`${c.kind}:${c.title}:${i}`} context={c} onRemove={onRemove ? () => onRemove(i) : undefined} />
      ))}
    </div>
  );
}
