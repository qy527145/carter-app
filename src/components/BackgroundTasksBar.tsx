// 「还有后台任务在跑」的提醒条。
//
// ## 为什么需要它
//
// 后台命令（`bash` 的 `run_in_background`）的设计就是「别卡住这一轮」：模型交代完
// 就可以收工，用户照常聊别的。于是最常见的一幕是 —— **前台这一轮已经结束**，界面上
// 风平浪静，而命令还在跑（起服务、跑长构建、`tail -f`）。等它跑完，系统会自己唤醒
// 会话接着做；在那之前，用户没有任何办法从界面上知道「还有事没完」。
//
// ## 长什么样
//
// 与「变更 N 个文件」那条同一个骨架：一行 `bg-card` 圆角条，展开才多说几句。
// 它常驻在输入框上方，不能是一块占满宽度的彩色横幅 —— 那样每一轮都在抢注意力，
// 几天之后人只会学会无视它。真正要一直在的只有三样东西：**在跑**（蓝点）、
// **几条**、**多久**；命令原文收在展开里。
//
// ## 为什么是整表快照
//
// 状态来自后端的 `background_shells` 事件，给的是「此刻还在跑的那些」。增量在这里
// 没有意义：漏掉一条「结束了」只会让提醒条一直挂着，而它的全部价值就是「还挂着 = 还没完」。

import { ChevronRight, Terminal } from "lucide-react";
import { useEffect, useState } from "react";
import { fmtDuration } from "../lib/format";
import { useStore } from "../store";
import { Collapse } from "./ui/Panel";

export function BackgroundTasksBar() {
  const shells = useStore((s) => s.backgroundShells);
  const streaming = useStore((s) => s.streaming);
  const [open, setOpen] = useState(false);
  const [now, setNow] = useState(() => Date.now());

  // 秒表只在有任务时走：一条都没有时开着定时器纯属白跑。
  useEffect(() => {
    if (shells.length === 0) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [shells.length]);

  // 全跑完就顺手收起来 —— 下次再有任务时从收起态开始，不会莫名摊开一屏。
  useEffect(() => {
    if (shells.length === 0) setOpen(false);
  }, [shells.length]);

  if (shells.length === 0) return null;

  const elapsed = (startedAt: number) => Math.max(0, now - startedAt);
  const longest = Math.max(...shells.map((s) => elapsed(s.started_at)));
  // 折起来时也要能看出「在跑什么」，否则只剩一个数字，等于什么都没说。
  const preview = shells.map((s) => s.command).join(" · ");

  return (
    <div className="shrink-0 px-4 pt-2">
      {/* 前台这一轮还在跑时它只是背景信息；这一轮结束了、它成了界面上唯一还在动的
          东西，用一圈淡蓝提醒一下 —— 别让人以为对话已经收工了。 */}
      <div
        className={`mx-auto max-w-3xl rounded-xl bg-card px-3 py-1.5 transition-shadow ${
          streaming ? "" : "ring-1 ring-tone-blue/25"
        }`}
      >
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          title={open ? "收起后台任务" : "展开后台任务"}
          className="flex w-full min-w-0 items-center gap-2 py-0.5 text-left"
        >
          <ChevronRight
            size={12}
            className={`shrink-0 text-dim transition-transform ${open ? "rotate-90" : ""}`}
          />
          <span className="size-1.5 shrink-0 animate-pulse rounded-full bg-tone-blue" />
          <span className="shrink-0 text-xs text-ink">{shells.length} 个后台任务运行中</span>
          <span className="shrink-0 tabular-nums text-[11px] text-dim">
            {shells.length > 1 ? "最长" : "已运行"} {fmtDuration(longest)}
          </span>
          {!open && (
            <span className="mono min-w-0 flex-1 truncate text-[11px] text-dim/70">{preview}</span>
          )}
        </button>
        <Collapse open={open}>
          <div className="space-y-1 pb-1 pt-1.5 pl-5">
            {shells.map((s) => (
              <div key={s.id} className="flex min-w-0 items-center gap-2 text-[11px]">
                <Terminal size={11} className="shrink-0 text-dim" />
                <code className="mono min-w-0 flex-1 truncate text-ink/90" title={s.command}>
                  {s.command}
                </code>
                <span className="shrink-0 tabular-nums text-dim">
                  {fmtDuration(elapsed(s.started_at))}
                </span>
              </div>
            ))}
            <div className="pt-0.5 text-[11px] text-dim/70">跑完会自动唤醒这条对话接着做。</div>
          </div>
        </Collapse>
      </div>
    </div>
  );
}
