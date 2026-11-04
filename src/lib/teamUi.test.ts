import { describe, expect, test } from "bun:test";

import type { TeamTask } from "./api";
import { layerize, orderByBarycenter } from "./teamUi";

/** 只填排布用得上的字段，其余给默认值 —— 这些用例关心的是「次序」，不是数据是否完整。 */
function task(id: string, depends_on: string[] = []): TeamTask {
  return {
    id,
    title: id,
    status: "todo",
    status_label: "待办",
    priority: "P2",
    assignee: null,
    parent_id: null,
    depends_on,
    children: [],
    waiting_on: [],
    progress: 0,
    ready: depends_on.length === 0,
    blocked_reason: "",
    estimate_hours: null,
    due_at: null,
  };
}

const layersOf = (tasks: TeamTask[]) => layerize(tasks);

describe("依赖图的层内排布", () => {
  test("子任务排到它前置的正对面", () => {
    // 起点的两条并列，各自有一个后继。如果层内顺序随机，a / b 与 c / d 可能是
    // 交叉的（a→d、b→c）；重心法要把它排成 a、b / c、d。
    const tasks = [task("a"), task("b"), task("c", ["a"]), task("d", ["b"])];
    const order = orderByBarycenter(tasks, layersOf(tasks));
    expect(order.indexOf("c")).toBeLessThan(order.indexOf("d"));
  });

  test("输入顺序打乱也排成不交叉", () => {
    const tasks = [task("a"), task("b"), task("c", ["a"]), task("d", ["b"])];
    const shuffled = [tasks[2], tasks[3], tasks[1], tasks[0]];
    // 起点层的先后无所谓，但**子层必须跟着起点的次序走**：a 在 b 前面，c 就得在 d
    // 前面。反过来的话两条线就交叉了 —— 那正是原来「看着乱」的根源。
    for (const input of [tasks, shuffled]) {
      const order = orderByBarycenter(input, layersOf(input));
      const [ab, cd] = [
        order.indexOf("a") < order.indexOf("b"),
        order.indexOf("c") < order.indexOf("d"),
      ];
      expect(ab).toBe(cd);
    }
  });

  test("环不会把排布卡死，也不会漏掉节点", () => {
    // 手改坏的目录里出现过互相依赖。层号守住环（都算 0），排布也得把谁都列出来。
    const tasks = [task("a", ["b"]), task("b", ["a"])];
    const order = orderByBarycenter(tasks, layersOf(tasks));
    expect(new Set(order)).toEqual(new Set(["a", "b"]));
    expect(order).toHaveLength(2);
  });

  test("指向图外的前置被忽略", () => {
    const tasks = [task("a", ["不存在"])];
    expect(orderByBarycenter(tasks, layersOf(tasks))).toEqual(["a"]);
  });
});
