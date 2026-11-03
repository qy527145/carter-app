// 「我的」与「动态」两屏。
//
// ## 「我的」是默认页
//
// 一个开发成员打开团队页，第一个问题是「我现在该做什么」，不是「整个项目长什么样」。
// 所以默认落在这里：派给我的、能开工的排最前；然后是等前置的、受阻的；最后是
// 跟我有关的新消息。任务图仍然在，换一个标签就到。
//
// ## 「动态」分两档
//
// * **跟我有关** —— @我、广播、派给我、我负责的任务上的评论。平时看这个。
// * **全部** —— 团队里的一切事件（认领、加入、别人之间的讨论）。回头查事用。
//
// 顶部是发广播的输入框：对全员说一句话（「main 冻结到周五」）不该绕去开一条对话。

import { useEffect, useMemo, useState } from "react";
import { Bell, CircleAlert, Inbox, ListChecks, Megaphone, Users } from "lucide-react";

import {
  teamAct,
  teamInbox,
  type TeamEvent,
  type TeamGoal,
  type TeamInboxItem,
  type TeamOverview,
  type TeamTask,
} from "../../lib/api";
import { eventSender, eventVerb, personName, priorityTone, since, statusTone } from "../../lib/teamUi";
import { toastError } from "../../lib/toast";
import { usePeople } from "./people";
import { PersonAvatar } from "./peopleUi";
import { Composer, Mentions, ViaCarter } from "./TaskDrawer";

/** 已读到哪条：存本机。已读回执要是也打成事件，消息流会被淹没（协议第 4 节）。 */
const seenKey = (cwd: string) => `carter.team.seen:${cwd}`;

export function readSeen(cwd: string): number {
  try {
    return Number(localStorage.getItem(seenKey(cwd)) ?? 0) || 0;
  } catch {
    return 0;
  }
}

export function markSeen(cwd: string, at: number) {
  try {
    localStorage.setItem(seenKey(cwd), String(at));
  } catch {
    // 存不下只是下次还显示「新」。
  }
}

/** 收件箱：随全景一起刷新。`nonce` 变了就重拉。 */
export function useInbox(cwd: string, nonce: number, fetch: boolean) {
  const [items, setItems] = useState<TeamInboxItem[]>([]);
  useEffect(() => {
    let alive = true;
    if (!cwd) return;
    teamInbox(cwd, fetch)
      .then((x) => alive && setItems(x))
      .catch(() => alive && setItems([]));
    return () => {
      alive = false;
    };
  }, [cwd, nonce, fetch]);
  return items;
}

const REASON: Record<TeamInboxItem["reason"], string> = {
  mention: "@我",
  broadcast: "广播",
  assigned: "派给我",
  my_task: "我的任务",
};

// ---------------------------------------------------------------------------
// 我的
// ---------------------------------------------------------------------------

export function MineTab({
  data,
  inbox,
  seen,
  onOpenTask,
  onJoin,
}: {
  data: TeamOverview;
  inbox: TeamInboxItem[];
  seen: number;
  onOpenTask: (goal: TeamGoal, task: TeamTask) => void;
  onJoin: () => void;
}) {
  const { people } = usePeople();
  const me = people?.me ?? null;

  const mine = useMemo(() => {
    const out: { goal: TeamGoal; task: TeamTask }[] = [];
    if (!people) return out;
    // 认人：任务里可能是 id（v1）或 `名字 <邮箱>`（v0），按名单统一比。
    const mineIds = new Set<string>();
    const meMember = people.members.find((m) => m.id === me);
    for (const g of data.goals) {
      if (g.status === "done" || g.status === "archived") continue;
      for (const t of g.tasks) {
        if (t.status === "done" || t.status === "dropped" || !t.assignee) continue;
        const a = t.assignee;
        const hit =
          a === me ||
          (meMember &&
            meMember.emails.some((e) => a.toLowerCase().includes(`<${e.toLowerCase()}>`)));
        if (hit && !mineIds.has(t.id)) {
          mineIds.add(t.id);
          out.push({ goal: g, task: t });
        }
      }
    }
    return out;
  }, [data.goals, people, me]);

  // 没人负责、现在就能开工的：给「手上没活」的人一个可以直接领的去处。
  const pool = useMemo(() => {
    const out: { goal: TeamGoal; task: TeamTask }[] = [];
    for (const g of data.goals) {
      if (g.status !== "active") continue;
      for (const t of g.tasks) if (t.ready && !t.assignee && t.children.length === 0) out.push({ goal: g, task: t });
    }
    return out.slice(0, 8);
  }, [data.goals]);

  if (people && !me) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 py-24 text-center text-sm text-dim">
        <Users size={20} />
        <div className="text-ink">先加入这个团队</div>
        <div className="max-w-sm text-xs leading-relaxed">
          加入之后，别人才能把任务派给你、在讨论里 @ 你；这里会列出你名下的任务和找你的消息。
        </div>
        <button
          onClick={onJoin}
          className="mt-2 rounded-lg bg-ink px-3 py-1.5 text-xs font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90"
        >
          加入团队
        </button>
      </div>
    );
  }

  const ready = mine.filter((x) => x.task.ready || x.task.status === "doing");
  const waiting = mine.filter((x) => !x.task.ready && x.task.status === "todo");
  const blocked = mine.filter((x) => x.task.status === "blocked");
  const fresh = inbox.filter((i) => i.at > seen).slice(0, 6);

  return (
    <div className="mx-auto grid max-w-5xl gap-6 p-5 lg:grid-cols-[1fr_20rem]">
      <div className="min-w-0 space-y-5">
        <Group
          icon={<ListChecks size={12} />}
          title="现在能做的"
          empty="手上没有能开工的任务。"
          items={ready}
          onOpen={onOpenTask}
        />
        {waiting.length > 0 && (
          <Group icon={<ListChecks size={12} />} title="等前置完成" items={waiting} onOpen={onOpenTask} />
        )}
        {blocked.length > 0 && (
          <Group icon={<CircleAlert size={12} />} title="受阻" items={blocked} onOpen={onOpenTask} />
        )}
        {mine.length === 0 && pool.length > 0 && (
          <Group
            icon={<ListChecks size={12} />}
            title="没人负责、可以直接领的"
            items={pool}
            onOpen={onOpenTask}
          />
        )}
      </div>

      <div className="min-w-0">
        <div className="mb-2 flex items-center gap-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">
          <Bell size={11} />
          找我的
          {fresh.length > 0 && <span className="text-tone-orange">· {fresh.length} 条新</span>}
        </div>
        {inbox.length === 0 ? (
          <div className="rounded-xl bg-card px-3 py-5 text-center text-[11px] text-dim">
            还没有人找你。
          </div>
        ) : (
          <div className="space-y-1">
            {inbox.slice(0, 12).map((i) => (
              <EventRow key={i.sha} e={i} data={data} reason={REASON[i.reason]} fresh={i.at > seen} compact onOpenTask={onOpenTask} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function Group({
  icon,
  title,
  items,
  empty,
  onOpen,
}: {
  icon: React.ReactNode;
  title: string;
  items: { goal: TeamGoal; task: TeamTask }[];
  empty?: string;
  onOpen: (goal: TeamGoal, task: TeamTask) => void;
}) {
  return (
    <section>
      <div className="mb-2 flex items-center gap-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">
        {icon}
        {title} · {items.length}
      </div>
      {items.length === 0 ? (
        empty && <div className="rounded-xl bg-card px-3 py-5 text-center text-[11px] text-dim">{empty}</div>
      ) : (
        <div className="space-y-1.5">
          {items.map(({ goal, task }) => {
            const tone = statusTone(task.status);
            return (
              <button
                key={task.id}
                onClick={() => onOpen(goal, task)}
                className="flex w-full items-center gap-2 rounded-xl bg-card px-3 py-2 text-left shadow-soft transition-shadow hover:shadow-lift"
              >
                <span
                  className="size-1.5 shrink-0 rounded-full"
                  style={{ background: task.ready ? "var(--tone-green)" : tone.fg }}
                />
                <span className="shrink-0 text-[10px]" style={{ color: priorityTone(task.priority).fg }}>
                  {task.priority}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-xs font-medium">{task.title}</span>
                  <span className="block truncate text-[10px] text-dim">
                    {goal.title}
                    {task.blocked_reason ? ` · ${task.blocked_reason}` : ""}
                    {task.waiting_on.length > 0 ? ` · 等 ${task.waiting_on.length} 个前置` : ""}
                  </span>
                </span>
                <span
                  className="shrink-0 rounded px-1.5 py-[1px] text-[10px]"
                  style={{ background: tone.bg, color: tone.fg }}
                >
                  {task.ready ? "可开工" : task.status_label}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// 动态
// ---------------------------------------------------------------------------

export function LogTab({
  cwd,
  data,
  inbox,
  seen,
  onChanged,
  onOpenTask,
  onNeedJoin,
}: {
  cwd: string;
  data: TeamOverview;
  inbox: TeamInboxItem[];
  seen: number;
  onChanged: () => void;
  onOpenTask: (goal: TeamGoal, task: TeamTask) => void;
  onNeedJoin: () => void;
}) {
  const { people } = usePeople();
  // 名单是异步到的：初值按「还不知道」处理，等名单到了再按是否加入定默认档。
  // 用户手动切过就不再自动改。
  const [scope, setScope] = useState<"mine" | "all" | null>(null);
  const shown = scope ?? (people?.me ? "mine" : "all");
  const reasons = useMemo(() => new Map(inbox.map((i) => [i.sha, REASON[i.reason]])), [inbox]);
  const list: TeamEvent[] = shown === "mine" ? inbox : data.events;

  return (
    <div className="mx-auto max-w-3xl p-5">
      <div className="mb-4 overflow-hidden rounded-xl bg-card shadow-soft">
        <div className="flex items-center gap-1.5 px-3 pt-2.5 text-[11px] font-medium text-dim">
          <Megaphone size={12} />
          对全员说
        </div>
        <Composer
          placeholder="比如：main 冻结到周五，合并请先找我。@ 可以特别提到某人"
          sendLabel="广播"
          onSend={async (text, to) => {
            if (!people?.me) {
              onNeedJoin();
              return false;
            }
            try {
              // 正文里 @ 了人：广播照发，被 @ 的人额外收到一条「提到」，在他的收件箱里置顶。
              await teamAct(cwd, { action: "broadcast", text });
              if (to.length > 0) await teamAct(cwd, { action: "mention", to, text });
              onChanged();
              return true;
            } catch (e) {
              toastError(e);
              return false;
            }
          }}
        />
      </div>

      <div className="mb-2 flex items-center gap-1 rounded-lg bg-card p-0.5 text-[11px]" style={{ width: "fit-content" }}>
        {(
          [
            ["mine", "跟我有关", inbox.length],
            ["all", "全部", data.events.length],
          ] as const
        ).map(([id, label, n]) => (
          <button
            key={id}
            onClick={() => setScope(id)}
            className={`flex items-center gap-1 rounded-md px-2 py-1 transition-colors ${
              shown === id ? "bg-elevated font-medium text-ink shadow-soft" : "text-dim hover:text-ink"
            }`}
          >
            {label}
            <span className="tabular-nums text-dim">{n}</span>
          </button>
        ))}
      </div>

      {list.length === 0 ? (
        <div className="flex flex-col items-center justify-center gap-2 py-16 text-center text-sm text-dim">
          <Inbox size={20} />
          {shown === "mine" ? "还没有人找你。" : "还没有团队动态。"}
          <div className="max-w-sm text-xs">
            指派、认领、评论、@、广播都会在这里留下一条带作者与时间的记录。
          </div>
        </div>
      ) : (
        <div className="space-y-0.5">
          {list.map((e) => (
            <EventRow
              key={e.sha}
              e={e}
              data={data}
              reason={reasons.get(e.sha)}
              fresh={reasons.has(e.sha) && e.at > seen}
              onOpenTask={onOpenTask}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** 一条事件。对象是任务时显示任务标题，点一下打开那个任务。 */
function EventRow({
  e,
  data,
  reason,
  fresh,
  compact = false,
  onOpenTask,
}: {
  e: TeamEvent;
  data: TeamOverview;
  reason?: string;
  fresh?: boolean;
  compact?: boolean;
  onOpenTask: (goal: TeamGoal, task: TeamTask) => void;
}) {
  const { people } = usePeople();
  const who = eventSender(e);
  const target = useMemo(() => {
    if (!e.target) return null;
    for (const g of data.goals) {
      if (g.id === e.target) return { goal: g, task: null as TeamTask | null };
      const t = g.tasks.find((x) => x.id === e.target);
      if (t) return { goal: g, task: t };
    }
    return null;
  }, [data.goals, e.target]);
  const toWhom = e.to.filter((t) => t !== "*").map((t) => personName(people, t));

  return (
    <div
      className={`flex gap-2.5 rounded-lg px-2 py-1.5 ${compact ? "bg-card" : "hover:bg-card"} ${
        fresh ? "ring-1 ring-tone-orange/40" : ""
      }`}
    >
      <PersonAvatar person={who} size={compact ? 18 : 20} />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-baseline gap-1.5 text-[11px]">
          <span className="font-medium">{personName(people, who) || "有人"}</span>
          {e.via === "carter" && <ViaCarter />}
          <span className="text-dim">{eventVerb(e.kind)}</span>
          {toWhom.length > 0 && <span className="text-dim">{toWhom.join("、")}</span>}
          {target && (
            <button
              onClick={() => target.task && onOpenTask(target.goal, target.task)}
              disabled={!target.task}
              title={e.target ?? ""}
              className="max-w-[16rem] truncate text-ink/80 underline-offset-2 hover:underline disabled:no-underline"
            >
              「{target.task?.title ?? target.goal.title}」
            </button>
          )}
          {!target && e.target && <span className="mono text-[10px] text-dim">{e.target}</span>}
          {reason && (
            <span className={`rounded px-1 text-[9px] ${fresh ? "bg-tone-orange-soft text-tone-orange" : "bg-bubble text-dim"}`}>
              {reason}
            </span>
          )}
          <span className="ml-auto text-[10px] text-dim">{since(e.at)}</span>
        </div>
        {e.body && (
          <div
            className={`mt-0.5 whitespace-pre-wrap break-words leading-relaxed text-ink/90 ${
              compact ? "line-clamp-3 text-[11px]" : "text-xs"
            }`}
          >
            <Mentions text={e.body} />
          </div>
        )}
      </div>
    </div>
  );
}
