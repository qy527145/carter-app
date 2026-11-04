// 消息框下方的「我的待办」条。
//
// ## 为什么要放在输入框下面
//
// 它要回答的问题是「接下来让 agent 做什么」。这个问题和输入框是同一件事 ——
// 摆在输入框上方就成了「背景信息」，摆在下方才是「下一步」。勾一条摆进输入框
// 成为一枚上下文卡片，不需要用户再手打一遍任务 id 与目标；要不要补一句、什么时候
// 发，仍由用户决定。
//
// ## 为什么可以多选
//
// 手上有几件事要一起交代是常态（同一目标下拆出来的、一次重构牵动的几个文件）。
// 单选不是取舍，是早期实现只装得下一张卡片留下的形状 —— 见 `lib/promptContext`。
// 勾选与「摆进输入框」分成两步：连点几下不该让输入框抖三下，也不该让用户每勾一条
// 就去看一眼卡片有没有摆对。
//
// ## 什么时候不显示
//
// 没有待办、项目没有团队目录、或者用户手动收起了 —— 这三种情况下它都该让位。
// 常驻一条「暂无待办」是纯噪音：它不提供任何可行动的东西，却每天占用一行高度。

import { useCallback, useEffect, useState } from "react";
import { Check, ChevronRight, CircleAlert, ListChecks, Send } from "lucide-react";

import { teamMyTasks, type TeamTodo } from "../lib/api";
import type { PromptContext } from "../lib/promptContext";
import { priorityTone } from "../lib/teamUi";
import { useStore } from "../store";

/** 收起状态存在 localStorage：这是一个「我不看这个」的偏好，不该每次启动重问。 */
const COLLAPSE_KEY = "carter.team.todosCollapsed";

export function TeamTodoDock() {
  const cwd = useStore((s) => s.cwd);
  const idle = useStore((s) => !s.streaming);
  const [todos, setTodos] = useState<TeamTodo[]>([]);
  /** 勾中的任务 id。拉数据时按 id 保留，任务消失了自然就不在选中里了。 */
  const [picked, setPicked] = useState<Set<string>>(() => new Set());
  const sessionId = useStore((s) => s.sessionId);
  const addComposerCards = useStore((s) => s.addComposerCards);
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem(COLLAPSE_KEY) === "1";
    } catch {
      return false;
    }
  });

  const refresh = useCallback(async () => {
    if (!cwd) {
      setTodos([]);
      return;
    }
    try {
      setTodos(await teamMyTasks(cwd));
    } catch {
      // 团队目录没初始化、或者 cwd 不是 git 仓库 —— 这里不该弹错。
      // 这个条是锦上添花的东西，坏了就当没有。
      setTodos([]);
    }
  }, [cwd]);

  // 换目录立刻重拉；一轮结束再拉一次 —— 模型可能刚把某个任务标成了完成。
  useEffect(() => {
    void refresh();
  }, [refresh, idle]);

  // 团队数据是磁盘上的东西，别的窗口 / git pull 都可能改它。窗口重新获得焦点时对一次。
  useEffect(() => {
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh]);

  function toggle() {
    const next = !collapsed;
    setCollapsed(next);
    try {
      localStorage.setItem(COLLAPSE_KEY, next ? "1" : "0");
    } catch {
      // 存不进去就只是下次启动恢复默认，不影响功能。
    }
  }

  if (todos.length === 0) return null;

  const open = todos.filter((t) => t.status !== "blocked").length;
  const blocked = todos.length - open;
  const chosen = todos.filter((t) => picked.has(t.id));

  function togglePick(id: string) {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  return (
    <div className="shrink-0 px-4 pt-2">
      <div className="mx-auto max-w-3xl rounded-xl bg-card px-3 py-1.5">
        <button
          onClick={toggle}
          className="flex w-full items-center gap-2 py-0.5 text-left text-xs text-dim transition-colors hover:text-ink"
        >
          <ChevronRight
            size={12}
            className={`shrink-0 transition-transform ${collapsed ? "" : "rotate-90"}`}
          />
          <ListChecks size={12} className="shrink-0" />
          <span className="shrink-0">我的任务</span>
          <span className="tabular-nums text-ink">{todos.length}</span>
          {blocked > 0 && (
            <span style={{ color: "var(--tone-orange)" }} className="flex items-center gap-0.5">
              <CircleAlert size={10} />
              {blocked} 受阻
            </span>
          )}
          {chosen.length > 0 ? (
            <span className="text-ink">已勾选 {chosen.length} 项</span>
          ) : (
            collapsed && (
              <span className="ml-auto min-w-0 truncate text-ink/80">
                {todos.find((t) => t.ready)?.title ?? todos[0].title}
              </span>
            )
          )}
        </button>

        {!collapsed && (
          <div className="mt-1 space-y-0.5 pb-1">
            {todos.map((t) => (
              <TodoRow
                key={t.id}
                todo={t}
                picked={picked.has(t.id)}
                onToggle={() => togglePick(t.id)}
              />
            ))}
          </div>
        )}

        {/* 勾了才出现：常态下这一条不存在，不占一行高度。 */}
        {chosen.length > 0 && (
          <div className="carter-enter mb-1 flex items-center gap-1.5 border-t border-edge/70 px-1 pt-1.5">
            <span className="text-[10px] text-dim">
              选中的 {chosen.length} 件事一起交给卡特
            </span>
            <button
              onClick={() => setPicked(new Set())}
              className="ml-auto rounded px-1.5 py-0.5 text-[10px] text-dim transition-colors hover:bg-ink/5 hover:text-ink"
            >
              取消
            </button>
            <button
              onClick={() => {
                addComposerCards(chosen.map(todoCard));
                setPicked(new Set());
              }}
              disabled={!sessionId}
              title={sessionId ? "摆进输入框，等你补一句再发" : "先开一条对话"}
              className="carter-press flex items-center gap-1 rounded-md bg-ink px-2 py-1 text-[10px] font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-40"
            >
              <Send size={10} />
              摆进输入框
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/** 一条待办 → 一枚上下文卡片。勾选几条就按这个形状摆几条。 */
function todoCard(todo: TeamTodo): PromptContext {
  return {
    kind: "task",
    title: todo.title,
    body:
      `接着做我名下的这个任务：\n\n` +
      `- 任务：${todo.title}\n` +
      `- 任务 id：${todo.id}\n` +
      `- 所属目标：${todo.goal_title}（${todo.goal_id}）\n` +
      `- 当前状态：${todo.status_label}\n` +
      (todo.blocked_reason ? `- 受阻原因：${todo.blocked_reason}\n` : "") +
      (todo.waiting_on > 0 ? `- 还有 ${todo.waiting_on} 个前置没完成\n` : "") +
      `\n先看一眼任务的具体要求（acceptance 里写了验收标准），再动手。`,
  };
}

function TodoRow({
  todo,
  picked,
  onToggle,
}: {
  todo: TeamTodo;
  picked: boolean;
  onToggle: () => void;
}) {
  return (
    <div
      role="checkbox"
      aria-checked={picked}
      tabIndex={0}
      onClick={onToggle}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onToggle();
        }
      }}
      title={`${todo.goal_title} · ${todo.id}`}
      className={`group flex cursor-pointer items-center gap-2 rounded-md px-1 py-1 outline-none transition-colors ${
        picked ? "bg-ink/[0.06]" : "hover:bg-ink/[0.04]"
      }`}
    >
      {/* 勾选框：常态下只有一圈虚线，悬停/选中才实心 —— 一排方框会把待办标题挤散。 */}
      <span
        className={`flex size-3.5 shrink-0 items-center justify-center rounded-[4px] transition-colors ${
          picked
            ? "bg-ink text-[var(--accent-contrast)]"
            : "ring-1 ring-inset ring-edge-strong/50 group-hover:ring-edge-strong"
        }`}
      >
        {picked && <Check size={9} strokeWidth={3} />}
      </span>
      <span
        className="size-1.5 shrink-0 rounded-full"
        style={{
          background: todo.ready
            ? "var(--tone-green)"
            : todo.status === "blocked"
              ? "var(--tone-orange)"
              : "var(--border-strong)",
        }}
      />
      <span className="shrink-0 text-[10px]" style={{ color: priorityTone(todo.priority).fg }}>
        {todo.priority}
      </span>
      <span className="min-w-0 flex-1 truncate text-left text-xs text-ink/90">
        {todo.title}
      </span>
      {todo.status === "doing" && (
        <span className="shrink-0 text-[10px] text-tone-blue">进行中</span>
      )}
      {todo.waiting_on > 0 && (
        <span className="shrink-0 text-[10px] text-dim">等 {todo.waiting_on} 个</span>
      )}
      {todo.status === "blocked" && todo.blocked_reason && (
        <span
          className="max-w-[14rem] shrink-0 truncate text-[10px]"
          style={{ color: "var(--tone-orange)" }}
          title={todo.blocked_reason}
        >
          {todo.blocked_reason}
        </span>
      )}
    </div>
  );
}
