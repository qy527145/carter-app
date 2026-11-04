// 消息框下方的「我的待办」条。
//
// ## 为什么要放在输入框下面
//
// 它要回答的问题是「接下来让 agent 做什么」。这个问题和输入框是同一件事 ——
// 摆在输入框上方就成了「背景信息」，摆在下方才是「下一步」。勾一条摆进输入框
// 成为一枚上下文卡片，不需要用户再手打一遍任务 id 与目标；要不要补一句、什么时候
// 发，仍由用户决定。
//
// ## 勾选即加入
//
// 手上有几件事要一起交代是常态（同一目标下拆出来的、一次重构牵动的几个文件），
// 所以可以连点几条、一起摆在输入框里。但「勾选」本身就是那个动作 —— 勾上就是
// 摆进输入框了，再点一下撤掉；没有单独的确认按钮。
//
// 早先版本把两件事拆成两步（先勾、再点「摆进输入框」），理由是「连点几下不该让
// 输入框抖三下」。那个顾虑是假的：输入框上方的卡片本来就是随着操作变长的，
// 抖的是人自己的动作，而中间多出来的那一次点击是纯负担。勾选状态也不另存一份 ——
// 它直接就是输入框里那份卡片列表（见 store 的 `composerContexts`），
// 两处各记一份必然要夹一个同步步骤，而那个步骤就是那个按钮。
//
// ## 什么时候不显示
//
// 没有待办、项目没有团队目录、或者用户手动收起了 —— 这三种情况下它都该让位。
// 常驻一条「暂无待办」是纯噪音：它不提供任何可行动的东西，却每天占用一行高度。

import { Check, ChevronRight, CircleAlert, ListChecks } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";

import { type TeamTodo, teamMyTasks } from "../lib/api";
import { priorityTone, taskCard } from "../lib/teamUi";
import { useStore } from "../store";

/** 收起状态存在 localStorage：这是一个「我不看这个」的偏好，不该每次启动重问。 */
const COLLAPSE_KEY = "carter.team.todosCollapsed";

export function TeamTodoDock() {
  const cwd = useStore((s) => s.cwd);
  const idle = useStore((s) => !s.streaming);
  const [todos, setTodos] = useState<TeamTodo[]>([]);
  // 勾选状态不另存一份：它就是输入框里那叠卡片。两处各记一份的话，中间必然
  // 需要一次同步 —— 那个同步就是原来那个多余的「摆进输入框」按钮。
  const contexts = useStore((s) => s.composerContexts);
  const toggleContext = useStore((s) => s.toggleComposerContext);
  // 只认任务类卡片，并且按 id 比 —— 不拼正文，正文里有「当前状态」这类随时会变的东西。
  const pickedIds = useMemo(
    () => new Set(contexts.filter((c) => c.kind === "task" && c.ref).map((c) => c.ref as string)),
    [contexts],
  );
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
  const isPicked = (t: TeamTodo) => pickedIds.has(t.id);

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
          {contexts.length > 0 ? (
            // 摆在框里几条，这里就说几条 —— 直接数输入框上方那叠卡片。只数列表里勾中的话，
            // 从看板交过来的别人的任务、目标卡片都不算，这个数就和眼前对不上了。
            <span className="text-ink">已放进输入框 {contexts.length} 条</span>
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
                picked={isPicked(t)}
                onToggle={() => toggleContext(taskCard(t))}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
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
      <span className="min-w-0 flex-1 truncate text-left text-xs text-ink/90">{todo.title}</span>
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
