// 依赖图：把目标下的任务按依赖深度摆成从左往右的列，并**真的把线画出来**。
//
// ## 为什么不是泳道
//
// 这个视图原来是一列一列卡片，谁等谁全靠底下一张文字表去查。可「谁等谁」正是
// 这块数据最独特的一面 —— 它是一张 DAG，而当时界面上没有一根线。更糟的是层内
// 顺序来自后端的 `order`，与前置节点在不在正对面毫无关系：眼睛得在两列之间来回
// 跳着找，那就是「乱」的全部来源。
//
// 所以这里做两件事：
//
// 1. **连边。** 父卡右缘 → 子卡左缘，正交折线（先出、再竖直、再进）。跨层的边
//    不穿中间的卡片，统一从图下方的走廊绕 —— 一条线从几个节点中间穿过去，读的人
//    会以为它连着那些节点。
// 2. **对齐。** 层内次序用重心法排（见 `orderByBarycenter`），让每件事尽量落在
//    它前置的正对面，交叉线能少一大截。
//
// ## 为什么节点是等高的
//
// 原来的卡片里有受阻原因色块、子任务进度条、父任务缩进…… 高度从 90px 到 160px
// 都有，于是同一层的节点根本不对齐，列与列之间的节奏全断。等高是让线画得直的
// **前提**，也是让眼睛能从上往下扫的前提。代价是信息密度：受阻原因、子任务明细
// 收进了抽屉（点开就有），卡面上只留「状态 · 优先级 · 标题 · 负责人」这四件
// 决定「要不要现在处理它」的事。
//
// ## 悬停高亮血缘
//
// DAG 好不好用，八成取决于交互而不是线条画得漂不漂亮：一个几十个节点的图，
// 人真正想看的永远是「这一条链上都有谁」。悬停（或键盘聚焦）一个节点，它的
// 全部祖先与后代保持原样，其余退到 15% —— 线也跟着淡出，整张图只剩那一条链。
//
// ## 布局为什么自己算
//
// 不用图库、不用力导向：力导向每次打开排得都不一样，人就记不住东西在哪。这里
// 位置是全确定的（层号 × 行号），同一份数据任何一次打开都长得一模一样。

import { useMemo, useState } from "react";

import type { TeamGoal, TeamTask } from "../../lib/api";
import {
  layerize,
  orderByBarycenter,
  personName,
  priorityTone,
  statusTone,
  taskHandoff,
} from "../../lib/teamUi";
import { HandoffButton } from "./HandoffButton";
import { usePeople } from "./people";
import { PersonAvatar } from "./peopleUi";

// ---------------------------------------------------------------------------
// 尺寸
// ---------------------------------------------------------------------------

const NODE_W = 232;
const NODE_H = 76;
/** 同层节点之间的竖向间距。留得比横向松一点：横向的空要留给折线拐弯。 */
const GAP_Y = 12;
/** 列间距 = 折线的走廊。太窄的话线会蹭着卡片边框走。 */
const COL_GAP = 88;
const HEADER_H = 24;
/** 跨层边从图下方绕行时，每条之间隔开这么多，免得叠成一根粗线。 */
const CORRIDOR_STEP = 10;
const CORRIDOR_PAD = 28;
/** 折线圆角。图小、线细，圆角太大反而像在画示意图。 */
const CORNER = 9;

// ---------------------------------------------------------------------------
// 布局
// ---------------------------------------------------------------------------

type Pos = { x: number; y: number; layer: number };

type Layout = {
  pos: Map<string, Pos>;
  /** 每列的行数，用来画表头下方的「空」提示。 */
  columns: string[][];
  width: number;
  height: number;
  /** 跨层边的走廊起点，边自己按顺序往下叠。 */
  corridor: number;
};

function computeLayout(tasks: TeamTask[], layers: Map<string, number>): Layout {
  const order = orderByBarycenter(tasks, layers);
  const columns: string[][] = [];
  const pos = new Map<string, Pos>();
  // 每层已经放了几行 —— 直接记计数，不用每次遍历 `pos` 数同层节点。
  const rowsIn = new Map<number, number>();
  // 层号是「最长链」算出来的，中间可能空一层（比如某个前置不在这个目标里）。
  // 空层不占地方：把出现过的层号重新压成 0、1、2…，既不画一格空的列，
  // 也不会因为 `columns` 有空位把宽度算多。
  const layerIndex = new Map<number, number>();

  for (const id of order) {
    const raw = layers.get(id) ?? 0;
    if (!layerIndex.has(raw)) layerIndex.set(raw, layerIndex.size);
    const layer = layerIndex.get(raw) as number;
    const row = rowsIn.get(layer) ?? 0;
    rowsIn.set(layer, row + 1);
    pos.set(id, { x: layer * (NODE_W + COL_GAP), y: HEADER_H + row * (NODE_H + GAP_Y), layer });
    columns[layer] ??= [];
    columns[layer].push(id);
  }

  const colCount = columns.length;
  const rows = Math.max(1, ...columns.map((c) => c?.length ?? 0));
  return {
    pos,
    columns,
    width: colCount > 0 ? colCount * NODE_W + (colCount - 1) * COL_GAP : 0,
    height: HEADER_H + rows * (NODE_H + GAP_Y),
    corridor: HEADER_H + rows * (NODE_H + GAP_Y) + CORRIDOR_PAD,
  };
}

// ---------------------------------------------------------------------------
// 连线
// ---------------------------------------------------------------------------

/** 把一串折点画成带圆角的路径 —— 直角折线在这张图里比曲线更像「线路」。 */
function roundedPath(pts: [number, number][], r = CORNER): string {
  if (pts.length === 0) return "";
  if (pts.length === 1) return `M ${pts[0][0]} ${pts[0][1]}`;
  let d = `M ${pts[0][0]} ${pts[0][1]}`;
  for (let i = 1; i < pts.length - 1; i++) {
    const [px, py] = pts[i - 1];
    const [cx, cy] = pts[i];
    const [nx, ny] = pts[i + 1];
    const l1 = Math.hypot(cx - px, cy - py) || 1;
    const l2 = Math.hypot(nx - cx, ny - cy) || 1;
    const rr = Math.min(r, l1 / 2, l2 / 2);
    const ax = cx - ((cx - px) / l1) * rr;
    const ay = cy - ((cy - py) / l1) * rr;
    const bx = cx + ((nx - cx) / l2) * rr;
    const by = cy + ((ny - cy) / l2) * rr;
    d += ` L ${ax} ${ay} Q ${cx} ${cy} ${bx} ${by}`;
  }
  const last = pts[pts.length - 1];
  return `${d} L ${last[0]} ${last[1]}`;
}

type Edge = {
  from: string;
  to: string;
  d: string;
  /** 箭头那个小尖：单独一段，用同样的颜色画，不做 SVG marker。 */
  head: string;
};

function buildEdges(tasks: TeamTask[], layout: Layout): { edges: Edge[]; bottom: number } {
  const edges: Edge[] = [];
  let corridor = layout.corridor;

  for (const t of tasks) {
    const to = layout.pos.get(t.id);
    if (!to) continue;
    for (const dep of t.depends_on) {
      const from = layout.pos.get(dep);
      if (!from || from.layer >= to.layer) continue; // 环或脏数据：不画，免得画出回头路

      const x1 = from.x + NODE_W;
      const y1 = from.y + NODE_H / 2;
      const x2 = to.x;
      const y2 = to.y + NODE_H / 2;
      const x2h = x2 - 2; // 让线在卡片边缘停下来，别扎进去
      const head = `M ${x2 - 7} ${y2 - 4.5} L ${x2h} ${y2} L ${x2 - 7} ${y2 + 4.5}`;

      // 同一行上的相邻两列：直接连，不用绕。
      if (y1 === y2 && to.layer - from.layer === 1) {
        edges.push({ from: dep, to: t.id, d: `M ${x1} ${y1} L ${x2h} ${y2}`, head });
        continue;
      }

      if (to.layer - from.layer === 1) {
        // 相邻两层：出 → 在列间走廊的中线竖直转向 → 进。
        const mid = x1 + (x2 - x1) / 2;
        edges.push({
          from: dep,
          to: t.id,
          d: roundedPath([
            [x1, y1],
            [mid, y1],
            [mid, y2],
            [x2h, y2],
          ]),
          head,
        });
        continue;
      }

      // 跨层：从下方走廊绕过去，别穿过中间的列。
      // 注意这里不能按「同一行」走直线 —— 那样会横穿中间那几列的所有卡片，
      // 读的人会以为它连着它们。
      const y = corridor;
      corridor += CORRIDOR_STEP;
      edges.push({
        from: dep,
        to: t.id,
        d: roundedPath([
          [x1, y1],
          [x1 + 26, y1],
          [x1 + 26, y],
          [x2 - 26, y],
          [x2 - 26, y2],
          [x2h, y2],
        ]),
        head,
      });
    }
  }
  return { edges, bottom: corridor > layout.corridor ? corridor + 12 : layout.height };
}

/** 悬停节点的全部祖先 + 后代（含自己）。 */
function lineage(id: string, tasks: TeamTask[]): Set<string> {
  const out = new Set<string>([id]);
  const byId = new Map(tasks.map((t) => [t.id, t]));
  const walk = (start: string, dir: "up" | "down") => {
    const stack = [start];
    while (stack.length > 0) {
      const cur = stack.pop() as string;
      const t = byId.get(cur);
      if (!t) continue;
      const next =
        dir === "up"
          ? t.depends_on
          : tasks.filter((x) => x.depends_on.includes(cur)).map((x) => x.id);
      for (const n of next) {
        if (out.has(n) || !byId.has(n)) continue;
        out.add(n);
        stack.push(n);
      }
    }
  };
  walk(id, "up");
  walk(id, "down");
  return out;
}

// ---------------------------------------------------------------------------
// 组件
// ---------------------------------------------------------------------------

export function DependencyGraph({
  goal,
  who,
  onOpen,
  onAsk,
}: {
  goal: TeamGoal;
  /** 按人筛。`null` = 全部，`""` = 没人负责。其余的人在图上淡出而不是藏起来。 */
  who: string | null;
  onOpen: (task: TeamTask) => void;
  onAsk: (draft?: string) => void;
}) {
  const [hover, setHover] = useState<string | null>(null);
  const layers = useMemo(() => layerize(goal.tasks), [goal.tasks]);
  const layout = useMemo(() => computeLayout(goal.tasks, layers), [goal.tasks, layers]);
  const { edges, bottom } = useMemo(() => buildEdges(goal.tasks, layout), [goal.tasks, layout]);
  const chain = useMemo(() => (hover ? lineage(hover, goal.tasks) : null), [hover, goal.tasks]);

  return (
    <div
      className="relative overflow-x-auto pb-3"
      // 指针离开整张图才清掉高亮：在卡片之间移动时不能闪。
      onMouseLeave={() => setHover(null)}
    >
      <div className="relative" style={{ width: layout.width, height: bottom }}>
        {/* 表头：只标「起点 / 第 n 层」。数据本身没有阶段名，编一个名字不如不编。 */}
        {layout.columns.map((col, i) => (
          <div
            // biome-ignore lint/suspicious/noArrayIndexKey: 列的身份就是层号，层号不会重排
            key={`layer-${i}`}
            className="absolute top-0 text-[10px] uppercase tracking-wider text-dim"
            style={{ left: i * (NODE_W + COL_GAP) + 2 }}
          >
            {i === 0 ? "起点" : `第 ${i} 层`}
            <span className="ml-1 opacity-50 tabular-nums">{col.length}</span>
          </div>
        ))}

        <svg
          className="pointer-events-none absolute left-0 top-0 overflow-visible"
          width={layout.width}
          height={bottom}
          aria-hidden="true"
        >
          {edges.map((e) => {
            const on = chain?.has(e.from) && chain.has(e.to);
            const off = chain !== null && !on;
            return (
              <g
                key={`${e.from}->${e.to}`}
                className="transition-opacity duration-150"
                style={{
                  opacity: off ? 0.12 : 1,
                  color: on ? "var(--text)" : "var(--border-strong)",
                }}
              >
                <path
                  d={e.d}
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={on ? 1.5 : 1}
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
                <path
                  d={e.head}
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={on ? 1.5 : 1}
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </g>
            );
          })}
        </svg>

        {goal.tasks.map((t) => {
          const p = layout.pos.get(t.id);
          if (!p) return null;
          return (
            <GraphNode
              key={t.id}
              task={t}
              x={p.x}
              y={p.y}
              dim={who !== null && (who === "" ? !!t.assignee : t.assignee !== who)}
              faded={chain !== null && !chain.has(t.id)}
              onHover={setHover}
              onOpen={() => onOpen(t)}
              onHandoff={() => onAsk(taskHandoff(goal, t))}
            />
          );
        })}
      </div>
    </div>
  );
}

function GraphNode({
  task,
  x,
  y,
  dim,
  faded,
  onHover,
  onOpen,
  onHandoff,
}: {
  task: TeamTask;
  x: number;
  y: number;
  dim: boolean;
  faded: boolean;
  onHover: (id: string | null) => void;
  onOpen: () => void;
  onHandoff: () => void;
}) {
  const { people } = usePeople();
  const closed = task.status === "done" || task.status === "dropped";
  const tone = statusTone(task.status);

  return (
    <div
      className={`group absolute rounded-lg bg-card shadow-soft transition-[opacity,box-shadow] duration-150 hover:shadow-lift ${
        dim || faded ? "opacity-25" : ""
      }`}
      style={{
        left: x,
        top: y,
        width: NODE_W,
        height: NODE_H,
        // 能开工的左边挂一条彩色竖线 —— 一眼扫出「现在能做哪几件」。
        boxShadow: task.ready ? "inset 2px 0 0 0 var(--tone-green), var(--shadow-soft)" : undefined,
      }}
      onMouseEnter={() => onHover(task.id)}
    >
      {/* 整块可点的那一层。抬到 `z-10` 是为了接住点击，同时让上面文字层
          （`pointer-events-none`）不抢指针。 */}
      <button
        type="button"
        onClick={onOpen}
        onFocus={() => onHover(task.id)}
        onBlur={() => onHover(null)}
        aria-label={`打开任务：${task.title}`}
        title={`${task.title}\n${task.id}${task.blocked_reason ? `\n受阻：${task.blocked_reason}` : ""}`}
        className="absolute inset-0 z-0 cursor-pointer rounded-lg outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-edge-strong"
      />

      <div className="pointer-events-none relative z-10 flex h-full flex-col justify-center gap-1 px-2.5 py-2">
        <div className="flex items-center gap-1.5">
          <span
            className="rounded px-1.5 py-px text-[10px] font-medium"
            style={{ background: tone.bg, color: tone.fg }}
          >
            {task.status_label}
          </span>
          <span className="text-[10px]" style={{ color: priorityTone(task.priority).fg }}>
            {task.priority}
          </span>
          {task.ready && <span className="text-[10px] text-tone-green">可开工</span>}
          <span className="ml-auto flex items-center gap-1">
            {task.assignee ? (
              <>
                <span className="max-w-[4.5rem] truncate text-[10px] text-dim">
                  {personName(people, task.assignee)}
                </span>
                <PersonAvatar person={task.assignee} size={16} />
              </>
            ) : (
              !closed && (
                <span className="rounded-full border border-dashed border-edge-strong px-1.5 text-[9px] leading-4 text-dim">
                  待指派
                </span>
              )
            )}
          </span>
        </div>

        <div className="truncate text-xs font-medium leading-tight">{task.title}</div>

        <div className="flex items-center gap-1.5 text-[10px] text-dim">
          <span className="mono truncate">{task.id}</span>
          {task.estimate_hours ? <span className="shrink-0">{task.estimate_hours}h</span> : null}
          <HandoffButton className="ml-auto" onClick={onHandoff} />
        </div>
      </div>

      {/* 子任务的完成度：只给一条 2px 的线，不给数字 —— 卡面已经够挤了，
          而「这条链走到哪了」这件事，一根长度就说得清。 */}
      {task.children.length > 0 && (
        <div className="absolute inset-x-0 bottom-0 h-[2px] overflow-hidden rounded-b-lg bg-bubble">
          <div
            className="h-full transition-[width] duration-200 ease-out"
            style={{ width: `${task.progress}%`, background: tone.fg, opacity: 0.5 }}
          />
        </div>
      )}
    </div>
  );
}
