// 长任务目标（/goal）的那一条：目标正文 + 状态 + 暂停/继续/完成/编辑/清除。
//
// 放在输入框**上方**而不是顶栏：它和「我要它干什么」是一回事，挨着输入框读起来是
// 一句话；而顶栏那一行是「这条会话是谁」（标题、报文入口），两件事混在一起就都看不清。
//
// 只在有目标时出现 —— 没目标还常年占一行，是最容易让人学会「忽略这一行」的做法。

import { useEffect, useState } from "react";
import { CircleCheck, CircleDashed, CirclePause, CircleSlash, Pencil, Play, X } from "lucide-react";

import * as ops from "../lib/sessionOps";
import { useStore } from "../store";
import type { GoalView } from "../lib/api";
import { IconButton } from "./ui/Button";

type Tone = { icon: typeof CircleCheck; cls: string; bg: string };

/** 状态 → 图标与配色。四种状态要在余光里就能分清。 */
function tone(status: GoalView["status"]): Tone {
  switch (status) {
    case "active":
      return { icon: CircleDashed, cls: "text-tone-blue", bg: "bg-tone-blue-soft" };
    case "paused":
      return { icon: CirclePause, cls: "text-tone-orange", bg: "bg-[var(--warn-bg)]" };
    case "completed":
      return { icon: CircleCheck, cls: "text-[var(--ok)]", bg: "bg-tone-green-soft" };
    case "blocked":
      return { icon: CircleSlash, cls: "text-[var(--danger)]", bg: "bg-[var(--error-bg)]" };
  }
}

export function GoalBar() {
  const goal = useStore((s) => s.goal);
  const sessionId = useStore((s) => s.sessionId);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  // 换目标（或被清掉）之后不该还停在编辑态 —— 那会让人以为改动没生效。
  useEffect(() => {
    setEditing(false);
  }, [goal?.objective, goal === null]);

  if (!goal || !sessionId) return null;
  const t = tone(goal.status);
  const Icon = t.icon;

  const submit = () => {
    const text = draft.trim();
    if (text) ops.goal(sessionId, { type: "set", objective: text });
    setEditing(false);
  };

  return (
    <div className="shrink-0 px-4 pt-2">
      <div
        className={`mx-auto flex max-w-3xl items-start gap-2 rounded-xl px-3 py-2 ${t.bg}`}
      >
        <Icon size={14} className={`icon-optical shrink-0 ${t.cls}`} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 text-[11px] text-dim">
            <span className={t.cls}>目标 · {goal.status_label}</span>
            {goal.auto_continues > 0 && <span>已自动继续 {goal.auto_continues} 轮</span>}
          </div>

          {editing ? (
            <div className="mt-1 flex items-center gap-1.5">
              <input
                autoFocus
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") submit();
                  if (e.key === "Escape") setEditing(false);
                }}
                className="carter-field min-w-0 flex-1 rounded border border-edge bg-base px-2 py-1 text-xs text-ink outline-none"
              />
              <button
                onClick={submit}
                className="rounded bg-bubble px-2 py-1 text-[11px] text-dim hover:text-ink"
              >
                保存
              </button>
            </div>
          ) : (
            <div className="truncate text-sm text-ink" title={goal.objective}>
              {goal.objective}
            </div>
          )}

          {/* 模型说「卡住了 / 要你拍板」时把原因显出来 —— 那是它停下来说的话，
              只显示一个「被卡住」而不说卡在哪，等于没做这个功能。 */}
          {goal.note && (
            <div className={`mt-1 whitespace-pre-wrap break-words text-xs ${t.cls}`}>
              {goal.note}
            </div>
          )}
        </div>

        <div className="flex shrink-0 items-center gap-0.5">
          {goal.status === "active" ? (
            <IconButton title="暂停：不再自动继续" onClick={() => ops.goal(sessionId, { type: "pause" })}>
              <CirclePause size={13} />
            </IconButton>
          ) : (
            <IconButton title="继续推进" onClick={() => ops.goal(sessionId, { type: "resume" })}>
              <Play size={13} />
            </IconButton>
          )}
          <IconButton
            title="编辑目标"
            onClick={() => {
              setDraft(goal.objective);
              setEditing(true);
            }}
          >
            <Pencil size={12} />
          </IconButton>
          {goal.status !== "completed" && (
            <IconButton
              title="标记完成"
              onClick={() => ops.goal(sessionId, { type: "complete" })}
            >
              <CircleCheck size={13} />
            </IconButton>
          )}
          <IconButton title="清除目标" onClick={() => ops.goal(sessionId, { type: "clear" })}>
            <X size={12} />
          </IconButton>
        </div>
      </div>
    </div>
  );
}
