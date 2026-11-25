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

describe("模型请求自动重试", () => {
  const base = () => ({
    ...useStore.getState(),
    streaming: true,
    retry: null,
    turns: [
      { role: "user" as const, text: "查一下", turn: 0 },
      {
        role: "assistant" as const,
        turn: 0,
        items: [
          { kind: "text" as const, text: "我先看看", closed: true },
          { kind: "tool" as const, callId: "c1", name: "bash", args: {} },
          { kind: "thinking" as const, text: "嗯" },
          { kind: "text" as const, text: "结果是", closed: false },
        ],
      },
    ],
  });
  const retrying = { type: "retrying" as const, attempt: 2, max: 5, delay_ms: 2000, error: "503" };

  // 没成的那次说到一半的话不进历史，重发之后模型会从头再说 —— 留着就会出现两遍。
  test("撤掉这次请求的半截正文与思考，之前的工具卡留着", () => {
    const next = reduceConv(base(), retrying);
    const last = next.turns?.at(-1);
    expect(last?.role === "assistant" && last.items.map((i) => i.kind)).toEqual(["text", "tool"]);
    expect(next.retry).toMatchObject({ attempt: 2, max: 5, error: "503" });
    expect(next.retry?.at).toBeGreaterThan(Date.now());
  });

  test("还没产出任何东西时也能进入重试态", () => {
    const conv = { ...base(), turns: [{ role: "user" as const, text: "查一下", turn: 0 }] };
    const next = reduceConv(conv, retrying);
    expect(next.turns).toEqual(conv.turns);
    expect(next.retry?.attempt).toBe(2);
  });

  test("重试成了（又有字流出来）或放弃了（报错）都把提示收掉", () => {
    const conv = { ...base(), ...reduceConv(base(), retrying) };
    expect(conv.retry).not.toBeNull();
    expect(reduceConv(conv, { type: "text_delta", text: "好" }).retry).toBeNull();
    expect(reduceConv(conv, { type: "thinking_delta", text: "想" }).retry).toBeNull();
    expect(reduceConv(conv, { type: "error", message: "503" }).retry).toBeNull();
    expect(reduceConv(conv, { type: "idle" }).retry).toBeNull();
  });
});
