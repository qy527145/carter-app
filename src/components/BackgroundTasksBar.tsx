// 「还有后台任务在跑」的提醒条。
//
// ## 为什么需要它
//
// 后台命令（`bash` 的 `run_in_background`）的设计就是「别卡住这一轮」：模型交代完
// 就可以收工，用户照常聊别的。于是最常见的一幕是 —— **前台这一轮已经结束**，界面上
// 风平浪静，而命令还在跑（起服务、跑长构建、`tail -f`）。等它跑完，系统会自己唤醒
// 会话接着做；在那之前，用户没有任何办法从界面上知道「还有事没完」。
//
// 所以这一条常驻在输入框上方，只要还有命令在跑就显示，跑完了自动消失。它有两个
// 必须说清的信息：**有几条**、**已经跑了多久** —— 前者回答「我能不能走开」，后者
// 回答「是不是卡死了」。
//
// ## 为什么是整表快照
//
// 状态来自后端的 `background_shells` 事件，给的是「此刻还在跑的那些」。增量在这里
// 没有意义：漏掉一条「结束了」只会让提醒条一直挂着，而它的全部价值就是「还挂着 = 还没完」。
//
// ## 位置
//
// 贴在输入框正上方：用户准备打字时一定会看到。放到顶栏或侧栏，正在读长回答的人
// 根本不会往那儿看。

import { ChevronDown, LoaderCircle, Terminal } from "lucide-react";
import { useEffect, useState } from "react";
import { fmtDuration } from "../lib/format";
import { useStore } from "../store";
import { Collapse } from "./ui/Panel";

export function BackgroundTasksBar() {
  const shells = useStore((s) => s.backgroundShells);
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
  const oldest = Math.min(...shells.map((s) => elapsed(s.started_at)));

  return (
    <div className="shrink-0 px-4 pt-2">
      <div className="mx-auto overflow-hidden rounded-xl bg-tone-blue-soft ring-1 ring-tone-blue/20">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          title={open ? "收起后台任务" : "查看后台任务"}
          className="carter-press flex w-full items-center gap-2.5 px-3 py-2 text-left"
        >
          <LoaderCircle size={14} className="shrink-0 animate-spin text-tone-blue" />
          <span className="min-w-0 flex-1">
            <span className="flex flex-wrap items-baseline gap-x-2">
              <span className="text-xs font-medium text-ink">
                {shells.length} 个后台任务正在运行
              </span>
              <span className="text-[11px] tabular-nums text-dim">最长 {fmtDuration(oldest)}</span>
            </span>
            <span className="mt-0.5 block truncate text-[11px] text-dim">
              跑完会自动唤醒这条对话接着做。
            </span>
          </span>
          <ChevronDown
            size={13}
            className={`shrink-0 text-dim transition-transform duration-200 ${
              open ? "rotate-180" : ""
            }`}
          />
        </button>
        <Collapse open={open}>
          <div className="space-y-1 px-3 pb-2 pl-9">
            {shells.map((s) => (
              <div key={s.id} className="flex items-baseline gap-2 text-[11px]">
                <Terminal size={11} className="shrink-0 self-center text-tone-blue" />
                <code className="min-w-0 flex-1 truncate font-mono text-ink/90" title={s.command}>
                  {s.command}
                </code>
                <span className="shrink-0 tabular-nums text-dim">
                  {fmtDuration(elapsed(s.started_at))}
                </span>
              </div>
            ))}
          </div>
        </Collapse>
      </div>
    </div>
  );
}
