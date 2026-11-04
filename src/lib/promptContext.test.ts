import { describe, expect, test } from "bun:test";

import type { TeamGoal, TeamTask, TeamTodo } from "./api";
import { contextKey, formatContext, parseContexts } from "./promptContext";
import { taskCard, taskHandoff } from "./teamUi";

const todo: TeamTodo = {
  id: "t-1",
  title: "子任务 A",
  status: "todo",
  status_label: "待办",
  priority: "P3",
  goal_id: "g-1",
  goal_title: "目标",
  blocked_reason: "",
  waiting_on: 0,
  ready: true,
};

describe("上下文卡片的身份", () => {
  test("ref 经过文本来回一趟不丢", () => {
    const card = taskCard(todo);
    const { contexts, rest } = parseContexts(formatContext(card));
    expect(contexts).toEqual([card]);
    expect(rest).toBe("");
  });

  test("看板交过来的卡片和待办条上的是同一张", () => {
    const goal = { id: "g-1", title: "目标" } as TeamGoal;
    const task = { ...todo, waiting_on: [] as string[] } as unknown as TeamTask;
    const [fromBoard] = parseContexts(taskHandoff(goal, task)).contexts;
    expect(contextKey(fromBoard)).toBe(contextKey(taskCard(todo)));
    expect(fromBoard).toEqual(taskCard(todo));
  });

  test("状态变了身份不变", () => {
    const later = taskCard({ ...todo, status_label: "进行中" });
    expect(contextKey(later)).toBe(contextKey(taskCard(todo)));
  });

  test("标题里长得像属性的字不会被当成 ref", () => {
    const card = taskCard({ ...todo, title: 'a ref="x" ref=' });
    const [back] = parseContexts(formatContext(card)).contexts;
    expect(back.ref).toBe("t-1");
    expect(back.title).toBe(card.title);
  });

  test("旧版本写下的标签没有 ref，照样能解析", () => {
    const { contexts } = parseContexts(
      '<carter-context kind="goal" title="目标">\n正文\n</carter-context>',
    );
    expect(contexts).toEqual([{ kind: "goal", title: "目标", body: "正文" }]);
  });
});
