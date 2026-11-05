import { describe, expect, test } from "bun:test";

import { isNotice, reportNumberOf, ticketNumberOf } from "./reportCard";

// 标题都是后端 `Ticket::label()` 拼的：`{状态} · #{编号} {标题}`。

describe("汇报卡绑定哪张工单", () => {
  test("等着人处理的汇报按标题里的编号绑", () => {
    expect(ticketNumberOf("等你拍板 · #3 修登录")).toBe(3);
    expect(ticketNumberOf("跑完未汇报 · #13 陈小凤@我待确认")).toBe(13);
    expect(ticketNumberOf("待查看 · #12 目标刷新卡顿优化")).toBe(12);
  });

  // 这是曾经丢掉按钮的地方：这四类也走 `label()`，标题里同样有编号，
  // 当时被拿去解析归属的是「该不该给回答框」那套过滤规则，于是全被当成没编号。
  test("提示类的卡也绑得到工单，只是不给回答框", () => {
    expect(ticketNumberOf("已结束 · #12 目标刷新")).toBe(12);
    expect(ticketNumberOf("已答复 · #13 陈小凤")).toBe(13);
    expect(ticketNumberOf("已忽略 · #13 陈小凤")).toBe(13);
    for (const title of ["已结束 · #12 目标刷新", "已答复 · #13 陈小凤", "已忽略 · #13 陈小凤"]) {
      expect(isNotice(title)).toBe(true);
      expect(reportNumberOf(title)).toBeNull();
    }
  });

  test("没有编号的不瞎认", () => {
    // 「本来会接」带的是提议编号（`dec_...`），来信带的是工单标题，都不是可以随便指的编号。
    expect(ticketNumberOf("本来会接 · 修主干 build")).toBeNull();
    expect(ticketNumberOf("来信 · #1 修登录")).toBe(1);
    expect(reportNumberOf("本来会接 · 修主干 build")).toBeNull();
  });

  test("标题里编号后没有空格也认得", () => {
    expect(ticketNumberOf("待查看 · #7")).toBe(7);
  });
});
