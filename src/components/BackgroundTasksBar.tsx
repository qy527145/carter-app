// 「还有后台任务在跑」的提醒条。
//
// ## 为什么需要它
//
// 后台命令（`bash` 的 `run_in_background`）与后台子 agent（`task` 的
// `run_in_background`）的设计都是「别卡住这一轮」：模型交代完就可以收工，用户照常
// 聊别的。于是最常见的一幕是 —— **前台这一轮已经结束**，界面上风平浪静，而命令 /
// 子 agent 还在跑（起服务、跑长构建、几个并行的调研）。等它们跑完，系统会自己唤醒
// 会话接着做；在那之前，用户没有任何办法从界面上知道「还有事没完」。
//
// ## 长什么样
//
// 与「变更 N 个文件」那条同一个骨架：一行 `bg-card` 圆角条，展开才多说几句。
// 它常驻在输入框上方，不能是一块占满宽度的彩色横幅 —— 那样每一轮都在抢注意力，
// 几天之后人只会学会无视它。真正要一直在的只有三样东西：**在跑**（蓝点）、
// **几条**、**多久**；命令原文、子 agent 的行收在展开里。
//
// 子 agent 组展开后是一行一个子 agent，再展开能看到它自己的每一次工具调用 ——
// 与对话里那张 `task` 卡片是**同一个组件**（`AgentRows`），两处长得一样。
//
// ## 为什么是整表快照
//
// 状态来自后端的 `background_shells` / `background_agents` 事件，给的是「此刻还在跑的
// 那些」。增量在这里没有意义：漏掉一条「结束了」只会让提醒条一直挂着，而它的全部
// 价值就是「还挂着 = 还没完」。

import { ChevronRight, Square, Terminal, Users } from "lucide-react";
import { useEffect, useState } from "react";
import { type BackgroundAgentsView, type BackgroundShellView, sendCommand } from "../lib/api";
import { fmtDuration } from "../lib/format";
import { toastError } from "../lib/toast";
import { useStore } from "../store";
import { AgentRows } from "./toolcards/ToolCard";
import { Collapse } from "./ui/Panel";

/** 怎么进的后台。模型起的最常见，不标；另外两种标出来，免得人以为是模型自作主张。 */
const ORIGIN_LABEL: Record<BackgroundShellView["origin"], string | null> = {
  model: null,
  user: "手动转入",
  timeout: "超时转入",
};

export function BackgroundTasksBar() {
  const shells = useStore((s) => s.backgroundShells);
  const groups = useStore((s) => s.backgroundAgents);
  const streaming = useStore((s) => s.streaming);
  const sessionId = useStore((s) => s.sessionId);
  const cwd = useStore((s) => s.cwd);
  // 按过「终止」的那几条：等后端快照把它们摘掉之前，按钮先变灰，别让人连点。
  const [killing, setKilling] = useState<Set<string>>(new Set());

  const kill = (
    id: string,
    cmd: { type: "kill_shell"; shell_id: string } | { type: "kill_agents"; task_id: string },
  ) => {
    if (!sessionId) return;
    setKilling((s) => new Set(s).add(id));
    sendCommand(sessionId, cmd).catch((e) => {
      setKilling((s) => {
        const n = new Set(s);
        n.delete(id);
        return n;
      });
      toastError(e);
    });
  };
  const [open, setOpen] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const count = shells.length + groups.length;

  // 秒表只在有任务时走：一条都没有时开着定时器纯属白跑。
  useEffect(() => {
    if (count === 0) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [count]);

  // 全跑完就顺手收起来 —— 下次再有任务时从收起态开始，不会莫名摊开一屏。
  useEffect(() => {
    if (count === 0) setOpen(false);
  }, [count]);

  if (count === 0) return null;

  const elapsed = (startedAt: number) => Math.max(0, now - startedAt);
  const longest = Math.max(
    ...shells.map((s) => elapsed(s.started_at)),
    ...groups.map((g) => elapsed(g.started_at)),
  );
  // 折起来时也要能看出「在跑什么」，否则只剩一个数字，等于什么都没说。
  const preview = [
    ...shells.map((s) => s.command),
    ...groups.map((g) => g.agents.map((a) => a.description).join("、")),
  ].join(" · ");

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
          <span className="shrink-0 text-xs text-ink">{count} 个后台任务运行中</span>
          <span className="shrink-0 tabular-nums text-[11px] text-dim">
            {count > 1 ? "最长" : "已运行"} {fmtDuration(longest)}
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
                {ORIGIN_LABEL[s.origin] && (
                  <span className="shrink-0 rounded bg-bubble px-1 py-px text-[10px] text-dim">
                    {ORIGIN_LABEL[s.origin]}
                  </span>
                )}
                <span className="shrink-0 tabular-nums text-dim">
                  {fmtDuration(elapsed(s.started_at))}
                </span>
                <KillButton
                  disabled={killing.has(s.id)}
                  title="结束这条命令（连同子进程）。模型会收到「被用户终止」的通知"
                  onClick={() => kill(s.id, { type: "kill_shell", shell_id: s.id })}
                />
              </div>
            ))}
            {groups.map((g) => (
              <AgentGroupRow
                key={g.id}
                group={g}
                cwd={cwd}
                elapsed={fmtDuration(elapsed(g.started_at))}
                killing={killing.has(g.id)}
                onKill={() => kill(g.id, { type: "kill_agents", task_id: g.id })}
              />
            ))}
            <div className="pt-0.5 text-[11px] text-dim/70">
              跑完会自动唤醒这条对话接着做；手动终止也会告诉它。
            </div>
          </div>
        </Collapse>
      </div>
    </div>
  );
}

/**
 * 一组后台子 agent：一行摘要，展开是每个子 agent 各一行（再展开是它的工具调用）。
 * 自己记展开状态 —— 几组同时跑时各开各的。
 */
function AgentGroupRow({
  group: g,
  cwd,
  elapsed,
  killing,
  onKill,
}: {
  group: BackgroundAgentsView;
  cwd: string;
  elapsed: string;
  killing: boolean;
  onKill: () => void;
}) {
  const [open, setOpen] = useState(false);
  const running = g.agents.filter((a) => a.status === "running").length;
  const names = g.agents.map((a) => a.description).join("、");
  return (
    <div className="min-w-0">
      <div className="flex min-w-0 items-center gap-2 text-[11px]">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
        >
          <ChevronRight
            size={11}
            className={`shrink-0 text-dim transition-transform ${open ? "rotate-90" : ""}`}
          />
          <Users size={11} className="shrink-0 text-tone-violet" />
          <span className="shrink-0 text-ink/90">
            {running}/{g.agents.length} 个子 agent 在跑
          </span>
          <span className="min-w-0 flex-1 truncate text-dim/70" title={names}>
            {names}
          </span>
        </button>
        <span className="shrink-0 tabular-nums text-dim">{elapsed}</span>
        <KillButton
          disabled={killing}
          title="终止这组子 agent。模型会收到「被用户终止」的通知，已有的半截结论也会带上"
          onClick={onKill}
        />
      </div>
      <Collapse open={open}>
        <div className="pt-1.5 pl-4">
          <AgentRows agents={g.agents} cwd={cwd} />
        </div>
      </Collapse>
    </div>
  );
}

function KillButton({
  disabled,
  title,
  onClick,
}: {
  disabled: boolean;
  title: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      className="inline-flex shrink-0 items-center gap-1 rounded bg-bubble px-1.5 py-0.5 text-[11px] text-dim transition-colors hover:text-[var(--danger)] disabled:opacity-40"
    >
      <Square size={9} />
      终止
    </button>
  );
}
