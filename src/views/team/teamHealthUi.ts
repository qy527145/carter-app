// 团队就绪状态的展示逻辑：把一堆检查收敛成 header 上的一个角标 + 每行的状态说法。
//
// 单独一个文件是为了能测。这几条判断很容易悄悄变歪 —— 比如把「不适用」算进待修，
// 或者「警告」也让角标变红 —— 而那种错误在界面上只表现为「怎么有个数字」，没人
// 会当成 bug 报上来。

import type { TeamCheck, TeamCheckState } from "../../lib/api";

/** 一种状态的视觉说法。字形与文字成对给，不靠颜色单独传达。 */
export type Glyph = {
  /** `ok` | `warn` | `broken` | `muted`，用来选图标与色。 */
  tone: "ok" | "warn" | "broken" | "muted";
  /** 状态标签。放在 detail 前面，让人一眼知道这一行是「好」还是「要动手」。 */
  label: string;
};

export function checkGlyph(state: TeamCheckState): Glyph {
  switch (state) {
    case "ok":
      return { tone: "ok", label: "就绪" };
    case "warn":
      return { tone: "warn", label: "该处理" };
    case "broken":
      return { tone: "broken", label: "没在工作" };
    case "not_applicable":
      return { tone: "muted", label: "不适用" };
  }
}

export type Summary = {
  /** 真的坏了：这一块没在干活。 */
  broken: number;
  /** 能用，但有该做的事。 */
  warn: number;
  /** 要人动手的总数 = broken + warn。header 的角标用它。 */
  attention: number;
  /** 给 header 药丸用的短语。一切正常时为空串 —— 正常的界面不该有字。 */
  label: string;
};

/**
 * 把检查收敛成一句总结。
 *
 * `not_applicable` **不计入** attention：还没加入团队时「团队消息」那条源本来就不该
 * 存在，把它算进去等于让人去修一件不该修的事。同理 `ok` 也不用管。
 */
export function healthSummary(checks: TeamCheck[]): Summary {
  let broken = 0;
  let warn = 0;
  for (const c of checks) {
    if (c.state === "broken") broken++;
    else if (c.state === "warn") warn++;
  }
  const attention = broken + warn;
  // 「坏了 2 项」与「该处理 2 项」不是一回事：前者意味着团队同步/消息已经停了，
  // 后者只是一些收尾的事。说清是哪种，用户才知道要不要现在放下手头的事。
  const label = broken > 0 ? `${broken} 项没在工作` : attention > 0 ? `${attention} 项待处理` : "";
  return { broken, warn, attention, label };
}

/** 需要人动手的那些，坏的在前 —— 抽屉里「全部修复」按这个顺序。 */
export function actionable(checks: TeamCheck[]): TeamCheck[] {
  const rank = (s: TeamCheckState) => (s === "broken" ? 0 : s === "warn" ? 1 : s === "ok" ? 2 : 3);
  return [...checks]
    .filter((c) => c.action && c.state !== "ok" && c.state !== "not_applicable")
    .sort((a, b) => rank(a.state) - rank(b.state));
}

/**
 * 抽屉里每行要不要收起来。
 *
 * 就绪与不适用的行只在**没有待修项**时全部展开一次（让人确认「我看过了」）；
 * 一旦有东西要修，就把它们折进一行小字，把版面留给真正要动手的。
 */
export function splitChecks(checks: TeamCheck[]): {
  attention: TeamCheck[];
  quiet: TeamCheck[];
} {
  const attention: TeamCheck[] = [];
  const quiet: TeamCheck[] = [];
  for (const c of checks) {
    if (c.state === "broken" || c.state === "warn") attention.push(c);
    else quiet.push(c);
  }
  return { attention, quiet };
}

/**
 * 恢复预览里最多列几个路径，其余折成「等 N 个」。
 *
 * 全列出来会把确认框撑成一面墙，而人真正要判断的是「这批文件对不对」，不是逐条读。
 */
export function previewPaths(paths: string[], max = 8): { shown: string[]; more: number } {
  return { shown: paths.slice(0, max), more: Math.max(0, paths.length - max) };
}
