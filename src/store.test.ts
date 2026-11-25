import { describe, expect, test } from "bun:test";

import { reduceConv, useStore } from "./store";

describe("接回一条正在跑的会话", () => {
  // 接回那一刻这一轮还没产出东西：历史里只有提问，`turn_started` 早就发过了。
  // 之后第一个事件开出来的助手轮得认得出自己是哪一轮 —— 「正在生成」的圆点靠它。
  test("错过 turn_started 时，新开的助手轮沿用提问的轮次号", () => {
    const conv = {
      ...useStore.getState(),
      streaming: true,
      turns: [{ role: "user" as const, text: "接着做", turn: 3 }],
    };
    const next = reduceConv(conv, {
      type: "tool_started",
      call_id: "c1",
      name: "bash",
      args: { command: "ls" },
    });
    const last = next.turns?.at(-1);
    expect(last?.role).toBe("assistant");
    expect(last?.turn).toBe(3);
  });
});
