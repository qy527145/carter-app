import { describe, expect, test } from "bun:test";

import { NOTHING_USER_FACING, parseSubject, renderNotes } from "./release-notes";

describe("parseSubject", () => {
  test("认 conventional commits 的几种写法", () => {
    expect(parseSubject("feat: 设置页新增「报文」")).toEqual(["feat", "设置页新增「报文」"]);
    expect(parseSubject("fix(team): 看板不再闪")).toEqual(["fix", "看板不再闪"]);
    expect(parseSubject("feat!: 换掉配置格式")).toEqual(["feat", "换掉配置格式"]);
    // 中文输入法下顺手打出来的全角冒号。
    expect(parseSubject("fix：全角冒号也认")).toEqual(["fix", "全角冒号也认"]);
  });

  test("不是约定格式的不认", () => {
    expect(parseSubject("Merge pull request #1 from x/y")).toBeNull();
    expect(parseSubject("随手一提")).toBeNull();
  });
});

describe("renderNotes", () => {
  test("按新增 / 修复 / 优化分组，用户感知不到的类型不收", () => {
    const notes = renderNotes([
      "chore: 发布 v0.3.7",
      "fix: 更新后重启不再弹「意外退出」",
      "feat: 设置页新增「报文」",
      "refactor: 拆 store",
      "perf: 长对话滚动更顺",
      "feat(chat): 接口报错时自动重试",
    ]);
    expect(notes).toBe(
      [
        "### 新增",
        "",
        "- 设置页新增「报文」",
        "- 接口报错时自动重试",
        "",
        "### 修复",
        "",
        "- 更新后重启不再弹「意外退出」",
        "",
        "### 优化",
        "",
        "- 长对话滚动更顺",
      ].join("\n"),
    );
  });

  test("同一句话只留一条", () => {
    expect(renderNotes(["fix: 不再丢动画", "fix: 不再丢动画"])).toBe("### 修复\n\n- 不再丢动画");
  });

  test("没有面向用户的改动时说实话，不留空白", () => {
    expect(renderNotes(["chore: 发布 v0.3.7", "ci: 换缓存"])).toBe(NOTHING_USER_FACING);
    expect(renderNotes([])).toBe(NOTHING_USER_FACING);
  });
});
