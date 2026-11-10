import { describe, expect, test } from "bun:test";

import type { TeamCheck, TeamCheckState } from "../../lib/api";
import { actionable, checkGlyph, healthSummary, previewPaths, splitChecks } from "./teamHealthUi";

function check(id: string, state: TeamCheckState, action: string | null = null): TeamCheck {
  return {
    id,
    title: id,
    state,
    detail: "",
    action,
    action_kind: action ? "x" : null,
    paths: [],
  };
}

describe("healthSummary", () => {
  test("全好时没有任何标签", () => {
    const s = healthSummary([check("git", "ok"), check("files", "ok")]);
    expect(s).toEqual({ broken: 0, warn: 0, attention: 0, label: "" });
  });

  test("坏掉的项优先报「没在工作」，而不是混成「待处理」", () => {
    const s = healthSummary([check("sync_source", "broken"), check("remote", "warn")]);
    expect(s.broken).toBe(1);
    expect(s.warn).toBe(1);
    expect(s.attention).toBe(2);
    expect(s.label).toBe("1 项没在工作");
  });

  test("只有警告时说「待处理」", () => {
    const s = healthSummary([check("remote", "warn"), check("dirty", "warn")]);
    expect(s.attention).toBe(2);
    expect(s.label).toBe("2 项待处理");
  });

  test("「不适用」不计入待修", () => {
    const s = healthSummary([
      check("inbox_source", "not_applicable"),
      check("joined", "not_applicable"),
    ]);
    expect(s.attention).toBe(0);
    expect(s.label).toBe("");
  });
});

describe("checkGlyph", () => {
  test("每个状态都有字形与文字，不只靠颜色", () => {
    for (const state of ["ok", "warn", "broken", "not_applicable"] as TeamCheckState[]) {
      const g = checkGlyph(state);
      expect(g.label.length).toBeGreaterThan(0);
      expect(g.tone).toBeTruthy();
    }
  });

  test("不适用是安静的，不是错误色", () => {
    expect(checkGlyph("not_applicable").tone).toBe("muted");
    expect(checkGlyph("broken").tone).toBe("broken");
  });
});

describe("actionable", () => {
  test("只挑能动手的，坏的排在警告前面", () => {
    const list = actionable([
      check("git", "ok"),
      check("remote", "warn", "配远端"),
      check("sync_source", "broken", "重新创建"),
      check("joined", "not_applicable"),
    ]);
    expect(list.map((c) => c.id)).toEqual(["sync_source", "remote"]);
  });

  test("没有动作的项不进列表（比如没加入团队时的消息源）", () => {
    expect(actionable([check("inbox_source", "broken", null)])).toEqual([]);
  });
});

describe("splitChecks", () => {
  test("把安静的行与要动手的分开", () => {
    const { attention, quiet } = splitChecks([
      check("git", "ok"),
      check("remote", "warn", "配远端"),
      check("joined", "not_applicable"),
    ]);
    expect(attention.map((c) => c.id)).toEqual(["remote"]);
    expect(quiet.map((c) => c.id)).toEqual(["git", "joined"]);
  });
});

describe("previewPaths", () => {
  test("超过上限时折成「等 N 个」", () => {
    const paths = Array.from({ length: 20 }, (_, i) => `.carter/f${i}.json`);
    const { shown, more } = previewPaths(paths);
    expect(shown).toHaveLength(8);
    expect(more).toBe(12);
  });

  test("没超过时一个不少", () => {
    const { shown, more } = previewPaths(["a", "b"]);
    expect(shown).toEqual(["a", "b"]);
    expect(more).toBe(0);
  });
});
