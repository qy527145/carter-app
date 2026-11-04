// 任务抽屉：点任务卡打开。看要求、改负责人、在这件事上说话。
//
// ## 为什么评论框放在这里
//
// 讨论总是关于「某件事」的。把输入框放在任务旁边，说出去的话自动挂在这个任务上，
// 收件人（负责人、被 @ 的人）也就自然确定了 —— 不用先选对象再写字。
//
// ## @ 的解析在发送前做
//
// 协议规定收件人只看 `to=`，不从正文里猜（昵称会改、会重名）。所以这里在发送时把
// 正文里的 `@昵称` 解析成成员 id 填进 `to`。用户看到的是 `@小张`，发出去的是 id。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AtSign, Send, Sparkles, UserRound, X } from "lucide-react";

import {
  teamAct,
  type TeamEvent,
  type TeamGoal,
  type TeamTask,
} from "../../lib/api";
import {
  eventSender,
  eventVerb,
  mentionsIn,
  personName,
  priorityTone,
  since,
  statusTone, taskHandoff,
} from "../../lib/teamUi";
import { toastError, toastOk } from "../../lib/toast";
import { usePeople } from "./people";
import { HandoffButton } from "./HandoffButton";
import { PersonAvatar, PersonPicker } from "./peopleUi";

export function TaskDrawer({
  cwd,
  task,
  goal,
  events,
  onClose,
  onChanged,
  onAsk,
  onNeedJoin,
}: {
  cwd: string;
  task: TeamTask;
  goal: TeamGoal;
  /** 全部事件，这里自己筛出这个任务上的。 */
  events: TeamEvent[];
  onClose: () => void;
  /** 改了东西（指派、评论）之后让团队页重读。 */
  onChanged: () => void;
  onAsk: (draft?: string) => void;
  /** 还没加入时点了需要身份的操作。 */
  onNeedJoin: () => void;
}) {
  const { people } = usePeople();
  const [picker, setPicker] = useState<{ x: number; y: number } | null>(null);
  const tone = statusTone(task.status);
  const closed = task.status === "done" || task.status === "dropped";

  // ⌘/Ctrl+Enter 直接交给卡特。
  //
  // 抽屉里最常做的两件事是「说一句」和「交给卡特」。前者有输入框、回车即发；
  // 后者原来要点底部那块通栏按钮，鼠标得一路移到左下角。给它一个键，看完任务
  // 直接敲出去 —— 而右下角的讨论框里 ⌘↵ 仍然归它自己（发送），两者不抢。
  const handoff = useCallback(() => {
    onAsk(taskHandoff(goal, task));
  }, [onAsk, goal, task]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // 正在输入时 Escape 先交给输入框（清 @ 补全），不关抽屉。
      if (e.key === "Escape" && !(e.target instanceof HTMLTextAreaElement)) onClose();
      if (
        e.key === "Enter" &&
        (e.metaKey || e.ctrlKey) &&
        !closed &&
        // 焦点在讨论框里时那是「发送这句话」，不能在这里截走。
        !(e.target instanceof HTMLTextAreaElement)
      ) {
        e.preventDefault();
        handoff();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, handoff, closed]);

  const thread = useMemo(
    () => events.filter((e) => e.target === task.id).slice().reverse(),
    [events, task.id],
  );

  async function act(args: Parameters<typeof teamAct>[1], ok?: string) {
    if (!people?.me) {
      onNeedJoin();
      return;
    }
    try {
      const out = await teamAct(cwd, args);
      toastOk(ok ?? out);
      onChanged();
    } catch (e) {
      toastError(e);
    }
  }

  return (
    <div className="fixed inset-0 z-30 flex justify-end" onClick={onClose}>
      <aside
        onClick={(e) => e.stopPropagation()}
        className="carter-drawer flex h-full w-[26rem] max-w-[92vw] flex-col border-l border-edge bg-elevated shadow-2xl"
      >
        <header className="flex items-start gap-2 border-b border-edge px-4 py-3">
          <div className="min-w-0 flex-1">
            <div className="mb-1 flex flex-wrap items-center gap-1.5 text-[10px]">
              <span className="rounded px-1.5 py-[1px] font-medium" style={{ background: tone.bg, color: tone.fg }}>
                {task.status_label}
              </span>
              <span style={{ color: priorityTone(task.priority).fg }}>{task.priority}</span>
              {task.ready && <span className="text-tone-green">可开工</span>}
              <span className="truncate text-dim">· {goal.title}</span>
            </div>
            <h3 className="text-sm font-semibold leading-snug">{task.title}</h3>
            <div className="mono mt-0.5 text-[10px] text-dim">{task.id}</div>
          </div>
          <button
            onClick={onClose}
            title="关闭（Esc）"
            className="rounded-md p-1 text-dim transition-colors hover:bg-ink/5 hover:text-ink"
          >
            <X size={14} />
          </button>
        </header>

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          {/* 负责人：这一页最常改的东西，放在最上面。 */}
          <section className="mb-4">
            <div className="mb-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">负责人</div>
            <div className="flex items-center gap-2">
              {task.assignee ? (
                <span className="flex items-center gap-1.5 text-xs">
                  <PersonAvatar person={task.assignee} size={20} />
                  {personName(people, task.assignee)}
                  {people?.me && task.assignee === people.me && <span className="text-[10px] text-dim">（我）</span>}
                </span>
              ) : (
                <span className="text-xs text-dim">还没人负责</span>
              )}
              {!closed && (
                <button
                  onClick={(e) => {
                    const r = e.currentTarget.getBoundingClientRect();
                    setPicker({ x: r.left, y: r.bottom + 4 });
                  }}
                  className="ml-auto flex items-center gap-1 rounded-md bg-ink/5 px-2 py-1 text-[11px] text-dim transition-colors hover:bg-ink/10 hover:text-ink"
                >
                  <UserRound size={11} />
                  {task.assignee ? "换人" : "指派"}
                </button>
              )}
              {!closed && task.assignee && people?.me === task.assignee && (
                <button
                  onClick={() => void act({ action: "task_release", task_id: task.id })}
                  className="rounded-md px-2 py-1 text-[11px] text-dim transition-colors hover:bg-ink/5 hover:text-ink"
                >
                  放手
                </button>
              )}
            </div>
            <PersonPicker
              open={picker !== null}
              anchor={picker}
              onClose={() => setPicker(null)}
              onClaim={
                task.assignee && people?.me !== task.assignee
                  ? undefined
                  : () => void act({ action: "task_claim", task_id: task.id })
              }
              onPick={(m) => void act({ action: "assign", task_id: task.id, assignee: m.id })}
            />
          </section>

          {task.blocked_reason && (
            <section className="mb-4 rounded-lg bg-tone-orange-soft px-2.5 py-2 text-[11px] text-tone-orange">
              受阻：{task.blocked_reason}
            </section>
          )}

          {task.waiting_on.length > 0 && (
            <section className="mb-4 text-[11px] text-dim">还在等 {task.waiting_on.length} 个前置任务完成。</section>
          )}

          {/* 抽屉里最主要的动作。原来是一块灰底灰字的通栏 —— 看上去像禁用态。 */}
          {!closed && (
            <HandoffButton
              variant="full"
              shortcut="⌘↵"
              className="mb-4"
              onClick={handoff}
            />
          )}

          <section>
            <div className="mb-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">
              讨论 · {thread.length}
            </div>
            {thread.length === 0 && (
              <div className="rounded-lg bg-card px-3 py-4 text-center text-[11px] text-dim">
                还没有人在这件事上说过话。
              </div>
            )}
            <div className="space-y-2.5">
              {thread.map((e) => (
                <ThreadItem key={e.sha} event={e} />
              ))}
            </div>
          </section>
        </div>

        <Composer
          placeholder={
            task.assignee && task.assignee !== people?.me
              ? `对 ${personName(people, task.assignee)} 说点什么，@ 提到别人`
              : "在这件事上说点什么，@ 提到别人"
          }
          onSend={async (text, to) => {
            if (!people?.me) {
              onNeedJoin();
              return false;
            }
            // 负责人不是我的话，评论默认也通知他 —— 在他的任务上说话，他该知道。
            const recipients = new Set(to);
            if (task.assignee?.startsWith("m-") && task.assignee !== people.me) recipients.add(task.assignee);
            try {
              await teamAct(cwd, { action: "comment", task_id: task.id, text, to: [...recipients] });
              onChanged();
              return true;
            } catch (e) {
              toastError(e);
              return false;
            }
          }}
        />
      </aside>
    </div>
  );
}

function ThreadItem({ event: e }: { event: TeamEvent }) {
  const { people } = usePeople();
  const who = eventSender(e);
  const isText = e.body.trim().length > 0;
  return (
    <div className="flex gap-2">
      <PersonAvatar person={who} size={20} />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-baseline gap-1.5 text-[11px]">
          <span className="font-medium">{personName(people, who) || "有人"}</span>
          {e.via === "carter" && <ViaCarter />}
          {e.kind !== "comment" && <span className="text-dim">{eventVerb(e.kind)}</span>}
          {(e.kind === "assign" || e.kind === "handoff") && e.to[0] && (
            <span className="text-dim">给 {personName(people, e.to[0])}</span>
          )}
          <span className="ml-auto text-[10px] text-dim">{since(e.at)}</span>
        </div>
        {isText && (
          <div className="mt-0.5 whitespace-pre-wrap break-words text-xs leading-relaxed text-ink/90">
            <Mentions text={e.body} />
          </div>
        )}
      </div>
    </div>
  );
}

/** 「经卡特」：这条是 agent 代发的。人写的和 agent 写的要一眼能分开。 */
export function ViaCarter() {
  return (
    <span
      title="由卡特代发"
      className="flex items-center gap-0.5 rounded bg-tone-violet-soft px-1 text-[9px] text-tone-violet"
    >
      <Sparkles size={8} />
      经卡特
    </span>
  );
}

/** 把正文里的 `@昵称` 高亮出来。 */
export function Mentions({ text }: { text: string }) {
  const { people } = usePeople();
  const names = (people?.members ?? []).map((m) => m.name).sort((a, b) => b.length - a.length);
  if (names.length === 0) return <>{text}</>;
  const esc = names.map((n) => n.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  const re = new RegExp(`(@(?:${esc.join("|")}))`, "g");
  return (
    <>
      {text.split(re).map((part, i) =>
        re.test(part) && part.startsWith("@") ? (
          <span key={i} className="rounded bg-tone-blue-soft px-0.5 text-tone-blue">
            {part}
          </span>
        ) : (
          <span key={i}>{part}</span>
        ),
      )}
    </>
  );
}

/**
 * 带 @ 补全的输入框。⌘↵ / Ctrl+↵ 发送，↵ 换行 —— 评论常常不止一行。
 *
 * `onSend` 返回 `true` 才清空：发送失败时保留原文，不让人重打一遍。
 */
export function Composer({
  placeholder,
  onSend,
  autoFocus = false,
  sendLabel = "发送",
}: {
  placeholder: string;
  onSend: (text: string, to: string[]) => Promise<boolean>;
  autoFocus?: boolean;
  sendLabel?: string;
}) {
  const { people } = usePeople();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [query, setQuery] = useState<string | null>(null);
  const [pick, setPick] = useState(0);
  const ta = useRef<HTMLTextAreaElement>(null);

  const matches = useMemo(() => {
    if (query === null || !people) return [];
    const q = query.toLowerCase();
    return people.members.filter((m) => m.id !== people.me && m.name.toLowerCase().includes(q)).slice(0, 6);
  }, [people, query]);

  // 光标前面是 `@xxx`（中间没有空白）时进入补全。
  function track(value: string, caret: number) {
    const before = value.slice(0, caret);
    const m = /@([^\s@]*)$/.exec(before);
    setQuery(m ? m[1] : null);
    setPick(0);
  }

  function complete(name: string) {
    const el = ta.current;
    if (!el) return;
    const caret = el.selectionStart;
    const before = text.slice(0, caret).replace(/@([^\s@]*)$/, `@${name} `);
    const next = before + text.slice(caret);
    setText(next);
    setQuery(null);
    requestAnimationFrame(() => {
      el.focus();
      el.setSelectionRange(before.length, before.length);
    });
  }

  async function send() {
    const body = text.trim();
    if (!body || busy) return;
    setBusy(true);
    const ok = await onSend(body, mentionsIn(body, people));
    setBusy(false);
    if (ok) setText("");
  }

  return (
    <div className="relative border-t border-edge p-3">
      {matches.length > 0 && (
        <div className="absolute bottom-full left-3 mb-1 w-56 rounded-lg bg-elevated py-1 shadow-lift">
          {matches.map((m, i) => (
            <button
              key={m.id}
              onMouseDown={(e) => {
                e.preventDefault();
                complete(m.name);
              }}
              className={`flex w-full items-center gap-2 px-2.5 py-1 text-left text-xs ${
                i === pick ? "bg-ink/5" : ""
              }`}
            >
              <PersonAvatar person={m.id} size={16} />
              {m.name}
            </button>
          ))}
        </div>
      )}
      <textarea
        ref={ta}
        autoFocus={autoFocus}
        value={text}
        rows={2}
        placeholder={placeholder}
        onChange={(e) => {
          setText(e.target.value);
          track(e.target.value, e.target.selectionStart);
        }}
        onKeyDown={(e) => {
          if (matches.length > 0) {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setPick((p) => (p + 1) % matches.length);
              return;
            }
            if (e.key === "ArrowUp") {
              e.preventDefault();
              setPick((p) => (p - 1 + matches.length) % matches.length);
              return;
            }
            if (e.key === "Enter" || e.key === "Tab") {
              e.preventDefault();
              complete(matches[pick].name);
              return;
            }
            if (e.key === "Escape") {
              e.preventDefault();
              setQuery(null);
              return;
            }
          }
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            void send();
          }
        }}
        className="carter-field w-full resize-none rounded-lg border border-edge bg-base px-2.5 py-2 text-xs leading-relaxed text-ink outline-none"
      />
      <div className="mt-1.5 flex items-center gap-2 text-[10px] text-dim">
        <AtSign size={10} />
        输入 @ 提到成员 · ⌘↵ 发送
        <button
          onClick={() => void send()}
          disabled={busy || !text.trim()}
          className="ml-auto flex items-center gap-1 rounded-md bg-ink px-2.5 py-1 text-[11px] font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-40"
        >
          <Send size={10} />
          {sendLabel}
        </button>
      </div>
    </div>
  );
}
