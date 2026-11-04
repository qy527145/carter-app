// 「交给卡特」—— 团队页里最高频的动作。
//
// ## 为什么单独做成一个组件
//
// 它原来有五处入口，各自写得不一样：图层节点与看板卡是 `hidden group-hover:flex`，
// 清单与时间线是悬停才出现的图标，抽屉里是一块灰底灰字的通栏按钮。于是「这件事
// 交给卡特」在哪都要先找到它，而且每处的形状都不同 —— 找不到的地方就退化成
// 开抽屉、开对话、自己复述一遍任务。
//
// ## 常驻但低调，而不是悬停才出现
//
// 「每张卡常驻一个按钮，整屏就是一片按钮」这个担心是真的，但解法不是把它藏起来：
// 藏起来的东西对没悬停过的人等于不存在，而这是这一页的主语。折中是**低对比度的
// 常驻图标**：位置固定在每张卡的同一处（右下角），扫一眼就能记住；悬停时它拓成
// 「交给卡特」并浮出一层底。整屏看过去是一片很淡的星点，不是一片按钮。
//
// 展开用 `max-width` 过渡而不是 `hidden` → `inline`：后者动画不了，鼠标划过时
// 文字是硬蹦出来的。宽度从 0 长到实际宽度，读起来是「一个字从图标后面滑出来」。
//
// ## 为什么要自己 stopPropagation
//
// 它长在卡片里，而卡片整块可点（打开任务抽屉）。不拦一下，点「交给卡特」会先在
// 新对话里摆好卡片、紧接着又把抽屉推上来 —— 两件事同时发生，人不知道该看哪。
//
// ## 为什么是星星
//
// 「卡特」在这一页的其它入口（问问卡特、让卡特拆解、让卡特改这块看板）都用
// `Sparkles`。同一个动作在五处用同一个符号，人才不用每处重新认一遍。

import { Sparkles } from "lucide-react";
import type { MouseEvent } from "react";

/**
 * 交办按钮。
 *
 * `quiet` 给卡片用（常驻图标，悬停展开文字）；`full` 给抽屉与空状态用
 * （通栏，页面上主要的那个动作）。
 */
export function HandoffButton({
  onClick,
  variant = "quiet",
  hint = "开一条新对话，把这件事交给卡特",
  shortcut,
  className = "",
}: {
  onClick: () => void;
  variant?: "quiet" | "full";
  hint?: string;
  /** 给了就在 `full` 变体的按钮后面缀上按键提示（如 `⌘↵`）。 */
  shortcut?: string;
  className?: string;
}) {
  function fire(e: MouseEvent<HTMLButtonElement>) {
    // 卡片整块可点，这一下不该同时打开任务抽屉。
    e.stopPropagation();
    onClick();
  }

  if (variant === "full") {
    return (
      <button
        type="button"
        onClick={fire}
        title={shortcut ? `${hint}（${shortcut}）` : hint}
        className={`carter-press flex w-full items-center justify-center gap-1.5 rounded-lg bg-ink py-2 text-xs font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90 ${className}`}
      >
        <Sparkles size={12} />
        交给卡特去做
        {shortcut && (
          <span className="ml-1 rounded bg-white/15 px-1 py-px font-mono text-[9px] tracking-tight opacity-80">
            {shortcut}
          </span>
        )}
      </button>
    );
  }

  return (
    <button
      type="button"
      onClick={fire}
      title={hint}
      aria-label="交给卡特"
      // `relative z-10`：卡片上压着一层铺满的透明按钮（负责「整块可点」），
      // 不抬起来这一下会被它接走，点「交给卡特」变成打开抽屉。
      // `pointer-events-auto`：它多半长在 `pointer-events-none` 的文字层里，
      // 那一层是为了让文字别接走点击，但按钮自己得把指针要回来。
      className={`carter-press group/hand pointer-events-auto relative z-10 -my-1 -mr-1 flex h-6 shrink-0 cursor-pointer items-center overflow-hidden rounded-md pl-1 pr-1 text-dim/70 transition-colors hover:bg-ink/[0.07] hover:pr-2 hover:text-ink ${className}`}
    >
      <Sparkles size={11} className="shrink-0" />
      <span className="inline-block max-w-0 overflow-hidden whitespace-nowrap text-[10px] font-medium opacity-0 transition-[max-width,opacity,margin] duration-200 ease-out group-hover/hand:ml-1 group-hover/hand:max-w-[5rem] group-hover/hand:opacity-100 group-focus-visible/hand:ml-1 group-focus-visible/hand:max-w-[5rem] group-focus-visible/hand:opacity-100">
        交给卡特
      </span>
    </button>
  );
}
