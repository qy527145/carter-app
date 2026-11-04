// 任务图的另外三种排法：看板、清单、时间线。
//
// ## 为什么不止一种
//
// 依赖泳道（`TeamPage.tsx` 里的 `GoalGraph`）回答的是「谁等谁」—— 它把 DAG 摊成
// 从左往右推进的层，是这块数据最独特的一面。但它回答不了另外三个同样常见的问题：
//
// * 「现在卡在哪」 → **看板**按状态分列，一眼看出哪一列堆着东西（Trello / Jira /
//   Linear 都是这个范式，因为「按状态分组」是所有看板的共同点）。
// * 「我要一条一条过一遍」 → **清单**把每件事压成一行，字号小、密度高，适合
//   从上往下扫（Jira 的列表视图、GitHub Projects 的表格）。
// * 「什么时候到期」 → **时间线**把截止日摆在一条时间轴上（Asana / Monday 的
//   Gantt）。只有 `due_at` 没有开始时间，所以这里画的是「到截止日还剩多少」，
//   而不是完整甘特条 —— 画一根没有起点的条是在编数据。
//
// 三个视图共用同一份数据、同一个任务抽屉与同一个「按人筛」。换视图不丢选中，
// 因为选中状态存在抽屉那边（`opened` 在 TeamPage）。
//
// ## 为什么看板可以拖
//
// 「把这件标成完成」是看板上最高频的动作。原来只有模型能改状态（`task_status`），
// 人要点开抽屉、开一条对话让卡特去做。拖一下直接发 `task_status`，改了立刻提交，
// 别人同步得到 —— 与界面上指派走的是同一条路。
//
// 拖拽之外还有键盘：卡片聚焦后按 ←/→ 在列之间挪。只有拖拽的话，这块对键盘用户
// 等于不存在。

import { useCallback, useMemo, useState } from "react";
import {
  ArrowRight,
  CalendarRange,
  Check,
  Columns3,
  LayoutList,
} from "lucide-react";

import { teamAct, type TeamGoal, type TeamTask } from "../../lib/api";
import { personName, priorityTone, statusTone, taskHandoff } from "../../lib/teamUi";
import { toastError } from "../../lib/toast";
import { HandoffButton } from "./HandoffButton";
import { usePeople } from "./people";

export type GraphView = "lanes" | "board" | "list" | "timeline";

export const GRAPH_VIEWS: { id: GraphView; label: string; icon: typeof Columns3 }[] = [
  { id: "lanes", label: "依赖", icon: ArrowRight },
  { id: "board", label: "看板", icon: Columns3 },
  { id: "list", label: "清单", icon: LayoutList },
  { id: "timeline", label: "时间线", icon: CalendarRange },
];

const VIEW_KEY = "carter.team.graphView";

/** 记住上次选的视图：这是「我怎么看这块数据」的偏好，不该每次进来重问。 */
export function useGraphView(): [GraphView, (v: GraphView) => void] {
  const [view, setView] = useState<GraphView>(() => {
    try {
      const v = localStorage.getItem(VIEW_KEY);
      return v === "board" || v === "list" || v === "timeline" || v === "lanes" ? v : "lanes";
    } catch {
      return "lanes";
    }
  });
  const pick = useCallback((v: GraphView) => {
    setView(v);
    try {
      localStorage.setItem(VIEW_KEY, v);
    } catch {
      // 存不下就只是下次回到默认视图。
    }
  }, []);
  return [view, pick];
}

// ---------------------------------------------------------------------------
// 公用：状态列、卡片
// ---------------------------------------------------------------------------

/** 看板的四列。`dropped` 不单独占一列 —— 放弃的事留在清单里查得到就够了。 */
const COLUMNS = [
  { status: "todo", label: "待办" },
  { status: "doing", label: "进行中" },
  { status: "blocked", label: "受阻" },
  { status: "done", label: "已完成" },
] as const;

type ColumnStatus = (typeof COLUMNS)[number]["status"];

function columnOf(task: TeamTask): ColumnStatus {
  if (task.status === "done" || task.status === "dropped") return "done";
  if (task.status === "doing") return "doing";
  if (task.status === "blocked") return "blocked";
  return "todo";
}

/**
 * 把任务挪到某一列。
 *
 * 「已完成」写 `done`；从已完成拖回别的列时写 `todo` —— 后端没有「重新打开」
 * 这个动作，`todo` 是它最接近的语义。`blocked` 必须有原因，这里不弹框追问：
 * 拖进去就写一句占位的说明，抽屉里再改。宁可留一行可编辑的占位，也不要在
 * 拖拽中途弹一个输入框打断动作。
 */
async function moveTask(cwd: string, task: TeamTask, to: ColumnStatus) {
  const next =
    to === "done" ? "done" : to === "doing" ? "doing" : to === "blocked" ? "blocked" : "todo";
  // 「已完成」这一列还收着 `dropped`：放弃的事拖进这一列不该被悄悄改成「完成」，
  // 那是两种完全不同的结论。它已经在这一列里了，什么都不用做。
  if (task.status === "dropped" && to === "done") return;
  if (task.status === next) return;
  try {
    await teamAct(cwd, {
      action: "task_status",
      task_id: task.id,
      status: next,
      ...(to === "blocked" ? { blocked_reason: "在界面上标成受阻，原因待补" } : {}),
    });
  } catch (e) {
    toastError(e);
    throw e;
  }
}

// ---------------------------------------------------------------------------
// 看板
// ---------------------------------------------------------------------------

export function BoardView({
  cwd,
  goal,
  who,
  onOpen,
  onChanged,
  onAsk,
}: {
  cwd: string;
  goal: TeamGoal;
  /** 按人筛。`null` = 全部，`""` = 没人负责。 */
  who: string | null;
  onOpen: (task: TeamTask) => void;
  onChanged: () => void;
  onAsk: (draft?: string) => void;
}) {
  const [drag, setDrag] = useState<string | null>(null);
  const [over, setOver] = useState<ColumnStatus | null>(null);
  /** 正在提交的那一条：卡片半透明 + 不响应，避免连拖出两次冲突的提交。 */
  const [busy, setBusy] = useState<string | null>(null);

  const visible = useMemo(
    () =>
      goal.tasks.filter((t) =>
        who === null ? true : who === "" ? !t.assignee : t.assignee === who,
      ),
    [goal.tasks, who],
  );

  const byColumn = useMemo(() => {
    const out: Record<ColumnStatus, TeamTask[]> = { todo: [], doing: [], blocked: [], done: [] };
    for (const t of visible) out[columnOf(t)].push(t);
    return out;
  }, [visible]);

  async function drop(taskId: string, to: ColumnStatus) {
    const task = goal.tasks.find((t) => t.id === taskId);
    if (!task || busy) return;
    setBusy(task.id);
    try {
      await moveTask(cwd, task, to);
      onChanged();
    } catch {
      // `moveTask` 已经弹过错误；这里只是不再往下走。
    } finally {
      setBusy(null);
      setDrag(null);
      setOver(null);
    }
  }

  return (
    <div className="flex gap-3 overflow-x-auto pb-3">
      {COLUMNS.map((col) => {
        const tone = statusTone(col.status);
        const items = byColumn[col.status];
        const active = over === col.status && drag !== null;
        return (
          <section
            key={col.status}
            onDragOver={(e) => {
              // 不靠 `drag` 这个 state 判断：它要等 React 下一次渲染才更新，
              // 而 `dragstart` 之后的第一批 `dragover` 可能赶在那之前。看
              // `dataTransfer` 里有没有我们放进去的 id 才准 —— 也从外面拖进来的
              // 文字 / 文件不该被当成一张卡片。两个条件取「或」：Safari 在某些
              // 版本上 dragover 阶段拿不到 types，那时退回 state 兜底。
              if (!drag && !e.dataTransfer.types.includes("text/plain")) return;
              // 必须 preventDefault，否则浏览器不认这里是可以放的地方。
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              setOver(col.status);
            }}
            onDragLeave={(e) => {
              // 只在真的离开这一列时清掉：子元素之间的移动也会触发 leave。
              if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
                setOver((v) => (v === col.status ? null : v));
              }
            }}
            onDrop={(e) => {
              e.preventDefault();
              // 拖拽中的那一张以 dataTransfer 为准：`drag` state 只用来做视觉反馈。
              const id = e.dataTransfer.getData("text/plain") || drag;
              if (id) void drop(id, col.status);
            }}
            className={`flex w-[17rem] shrink-0 flex-col rounded-xl p-2 transition-colors duration-150 ${
              active ? "bg-card ring-1 ring-edge-strong/50" : "bg-card/50"
            }`}
          >
            <header className="mb-1.5 flex items-center gap-1.5 px-1">
              <span
                className="size-1.5 shrink-0 rounded-full"
                style={{ background: tone.fg }}
              />
              <span className="text-[11px] font-medium">{col.label}</span>
              <span className="text-[10px] tabular-nums text-dim">{items.length}</span>
            </header>

            <div className="flex min-h-[3rem] flex-col gap-1.5">
              {items.map((t) => (
                <BoardCard
                  key={t.id}
                  task={t}
                  busy={busy === t.id}
                  dragging={drag === t.id}
                  onDragStart={(e) => {
                    setDrag(t.id);
                    // 有些 WebKit 版本不带 dataTransfer 就不认拖拽。
                    e.dataTransfer.setData("text/plain", t.id);
                    e.dataTransfer.effectAllowed = "move";
                  }}
                  onDragEnd={() => {
                    setDrag(null);
                    setOver(null);
                  }}
                  onMove={(dir) => {
                    const i = COLUMNS.findIndex((c) => c.status === col.status);
                    const j = Math.max(0, Math.min(COLUMNS.length - 1, i + dir));
                    if (j !== i) void drop(t.id, COLUMNS[j].status);
                  }}
                  onOpen={() => onOpen(t)}
                  onAsk={() => onAsk(taskHandoff(goal, t))}
                />
              ))}

              {/* 空列也要有落点：不然「把事拖进受阻」这种操作在空列上做不了。 */}
              {items.length === 0 && (
                <div className="flex flex-1 items-center justify-center rounded-lg border border-dashed border-edge px-2 py-4 text-[10px] text-dim">
                  {active ? "放到这里" : "空"}
                </div>
              )}
            </div>
          </section>
        );
      })}
    </div>
  );
}

function BoardCard({
  task,
  busy,
  dragging,
  onDragStart,
  onDragEnd,
  onMove,
  onOpen,
  onAsk,
}: {
  task: TeamTask;
  busy: boolean;
  dragging: boolean;
  onDragStart: (e: React.DragEvent) => void;
  onDragEnd: () => void;
  onMove: (dir: -1 | 1) => void;
  onOpen: () => void;
  onAsk: () => void;
}) {
  const { people } = usePeople();
  return (
    // 与图层节点同样的结构：卡片本身不是按钮，「打开」由一层透明按钮承担，
    // 「交给卡特」压在它上面。外层挂 role="button" 再嵌 `<button>` 是嵌套交互元素，
    // 读屏会念两遍，←/→ 也说不清该归谁。
    //
    // 但这一种卡片**可以拖**，而 HTML5 拖拽只认「按下时所在的那个元素」是否
    // `draggable`。所以下面那层透明按钮必须是 `pointer-events-none`：它一旦接住
    // 指针，按下就落在按钮上（按钮不可拖），`dragstart` 永远冒不到这张卡 ——
    // 表现就是「拖不动，但点得开」。鼠标点击交给外层的 onClick，键盘走它自己的
    // Enter / ←/→，两条路都还在。
    <div
      draggable={!busy}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onClick={onOpen}
      title="拖到别的列可以改状态；聚焦后按 ←/→ 也行"
      className={`group relative cursor-grab rounded-lg bg-elevated p-2 shadow-soft transition-[opacity,box-shadow] hover:shadow-lift active:cursor-grabbing ${
        dragging ? "opacity-40" : ""
      } ${busy ? "animate-pulse" : ""}`}
      style={{
        boxShadow: task.ready ? "inset 2px 0 0 0 var(--tone-green), var(--shadow-soft)" : undefined,
      }}
    >
      <button
        type="button"
        onClick={onOpen}
        aria-label={`打开任务：${task.title}`}
        className="pointer-events-none absolute inset-0 z-0 rounded-lg outline-none focus-visible:ring-2 focus-visible:ring-edge-strong"
        onKeyDown={(e) => {
          // ←/→ 换列：拖拽之外的备用通道，键盘用户也改得动状态。
          if (e.key === "ArrowLeft") {
            e.preventDefault();
            onMove(-1);
          }
          if (e.key === "ArrowRight") {
            e.preventDefault();
            onMove(1);
          }
        }}
      />
      <div className="pointer-events-none relative z-[1] flex items-center gap-1.5">
        <span className="text-[10px]" style={{ color: priorityTone(task.priority).fg }}>
          {task.priority}
        </span>
        {task.ready && <span className="text-[10px] text-tone-green">可开工</span>}
        <span className="ml-auto flex items-center gap-1 text-[10px] text-dim">
          {task.assignee ? personName(people, task.assignee) : "待指派"}
        </span>
      </div>
      <div className="pointer-events-none relative z-[1] mt-1 line-clamp-2 text-xs font-medium leading-snug">
        {task.title}
      </div>
      {task.blocked_reason && columnOf(task) === "blocked" && (
        <div className="pointer-events-none relative z-[1] mt-1 line-clamp-2 rounded bg-tone-orange-soft px-1.5 py-1 text-[10px] text-tone-orange">
          {task.blocked_reason}
        </div>
      )}
      <div className="relative z-[1] mt-1.5 flex items-center gap-1.5 text-[10px] text-dim">
        <span className="mono truncate">{task.id}</span>
        {task.estimate_hours ? <span>· {task.estimate_hours}h</span> : null}
        <HandoffButton className="-mr-1 -my-0.5 ml-auto" onClick={onAsk} />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 清单
// ---------------------------------------------------------------------------

/**
 * 清单：按状态分组的一行行。
 *
 * 与看板的分工是密度：看板一张卡要说清一件事，清单一行只留标题、人、截止日 ——
 * 一屏能扫十几条，适合「过一遍看有没有漏」。
 */
export function ListView({
  goal,
  who,
  onOpen,
  onAsk,
  cwd,
  onChanged,
}: {
  goal: TeamGoal;
  who: string | null;
  onOpen: (task: TeamTask) => void;
  onAsk: (draft?: string) => void;
  cwd: string;
  onChanged: () => void;
}) {
  const visible = goal.tasks.filter((t) =>
    who === null ? true : who === "" ? !t.assignee : t.assignee === who,
  );
  const groups = useMemo(() => {
    const order: { status: string; label: string }[] = [
      { status: "doing", label: "进行中" },
      { status: "todo", label: "待办" },
      { status: "blocked", label: "受阻" },
      { status: "done", label: "已完成" },
      { status: "dropped", label: "已放弃" },
    ];
    return order
      .map((g) => ({ ...g, items: visible.filter((t) => t.status === g.status) }))
      .filter((g) => g.items.length > 0);
  }, [visible]);

  if (visible.length === 0) {
    return (
      <div className="rounded-xl bg-card px-4 py-10 text-center text-xs text-dim">
        这个筛选下没有任务。
      </div>
    );
  }

  return (
    <div className="space-y-5">
      {groups.map((g) => {
        const tone = statusTone(g.status);
        return (
          <section key={g.status}>
            <div className="mb-1.5 flex items-center gap-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">
              <span className="size-1.5 rounded-full" style={{ background: tone.fg }} />
              {g.label}
              <span className="tabular-nums">{g.items.length}</span>
            </div>
            <div className="overflow-hidden rounded-xl bg-card">
              {g.items.map((t, i) => (
                <ListRow
                  key={t.id}
                  task={t}
                  cwd={cwd}
                  last={i === g.items.length - 1}
                  onOpen={() => onOpen(t)}
                  onAsk={() => onAsk(taskHandoff(goal, t))}
                  onChanged={onChanged}
                />
              ))}
            </div>
          </section>
        );
      })}
    </div>
  );
}

function ListRow({
  task,
  cwd,
  last,
  onOpen,
  onAsk,
  onChanged,
}: {
  task: TeamTask;
  cwd: string;
  last: boolean;
  onOpen: () => void;
  onAsk: () => void;
  onChanged: () => void;
}) {
  const { people } = usePeople();
  const [done, setDone] = useState(false);
  const closed = task.status === "done" || task.status === "dropped";

  async function toggleDone(e: React.MouseEvent) {
    e.stopPropagation();
    // 行本身是一个可点区域（打开抽屉）。按钮点完不该把焦点留在行上 ——
    // 否则紧接着按空格会「打开抽屉」而不是再切一次完成状态。
    e.currentTarget instanceof HTMLElement && e.currentTarget.blur();
    setDone(true);
    try {
      await teamAct(cwd, {
        action: "task_status",
        task_id: task.id,
        status: closed ? "todo" : "done",
      });
      onChanged();
    } catch (err) {
      toastError(err);
    } finally {
      setDone(false);
    }
  }

  return (
    <div
      className={`group relative flex w-full items-center gap-2.5 px-3 py-2 text-left transition-colors hover:bg-ink/[0.03] ${
        last ? "" : "border-b border-edge/60"
      }`}
    >
      {/* 打开任务的透明层。清单一行里有两个真按钮（完成圈、交给卡特），
          它们都压在它上面，所以不用在外层挂 role="button"。 */}
      <button
        type="button"
        onClick={onOpen}
        aria-label={`打开任务：${task.title}`}
        className="absolute inset-0 z-0 cursor-pointer outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-edge-strong"
      />
      <button
        onClick={toggleDone}
        disabled={done}
        // 勾选按钮要在透明层之上，否则点它是「打开任务」。
        style={{ position: "relative", zIndex: 1 }}
        title={closed ? "重新打开" : "标记完成"}
        aria-label={closed ? "重新打开" : "标记完成"}
        className={`flex size-4 shrink-0 items-center justify-center rounded-full transition-colors ${
          closed
            ? "bg-tone-green text-white"
            : "ring-1 ring-inset ring-edge-strong/50 hover:ring-tone-green"
        } ${done ? "opacity-50" : ""}`}
      >
        {closed && <Check size={10} strokeWidth={3} />}
      </button>

      {/* 文字层不接指针：它们压在透明层上面，接了就轮到它们吃掉点击，
          点标题会变成什么都不发生。真正要接指针的只有下面那两个按钮。 */}
      <span
        className="pointer-events-none relative z-[1] shrink-0 text-[10px]"
        style={{ color: priorityTone(task.priority).fg }}
      >
        {task.priority}
      </span>

      <span
        className={`pointer-events-none relative z-[1] min-w-0 flex-1 truncate text-xs ${
          closed ? "text-dim line-through" : "text-ink"
        }`}
      >
        {task.title}
      </span>

      {task.children.length > 0 && (
        <span className="pointer-events-none relative z-[1] shrink-0 text-[10px] tabular-nums text-dim">
          {task.progress}%
        </span>
      )}
      {task.waiting_on.length > 0 && (
        <span className="pointer-events-none relative z-[1] shrink-0 text-[10px] text-dim">
          等 {task.waiting_on.length} 个
        </span>
      )}
      {task.due_at && (
        <span className="pointer-events-none relative z-[1] shrink-0 text-[10px] text-dim">
          {shortDate(task.due_at)}
        </span>
      )}
      <span className="pointer-events-none relative z-[1] w-14 shrink-0 truncate text-right text-[10px] text-dim">
        {task.assignee ? personName(people, task.assignee) : "—"}
      </span>
      <HandoffButton className="-my-1 -mr-1" onClick={onAsk} />
    </div>
  );
}

// ---------------------------------------------------------------------------
// 时间线
// ---------------------------------------------------------------------------

const DAY = 86_400_000;

/** 刻度上的日期：月/日。跨年时补上年份。 */
function shortDate(ms: number): string {
  const d = new Date(ms);
  const now = new Date();
  const sameYear = d.getFullYear() === now.getFullYear();
  return `${d.getMonth() + 1}/${d.getDate()}${sameYear ? "" : `/${d.getFullYear()}`}`;
}

/**
 * 时间线：以「今天」为左端，把每件事的截止日摆到轴上。
 *
 * 只有 `due_at` 没有开始时间，所以画的是**墨迹到截止日**这一段，而不是完整甘特条 ——
 * 一根没有起点的条只能靠编一个起点，那种图好看但不能信。没有截止日期的任务
 * 单独列在下面，不装进轴里（塞到轴尾等于谎称它们都有个远期的期限）。
 */
export function TimelineView({
  goal,
  who,
  onOpen,
  onAsk,
}: {
  goal: TeamGoal;
  who: string | null;
  onOpen: (task: TeamTask) => void;
  onAsk: (draft?: string) => void;
}) {
  const { people } = usePeople();
  const visible = goal.tasks.filter((t) =>
    who === null ? true : who === "" ? !t.assignee : t.assignee === who,
  );
  const dated = visible.filter((t) => t.due_at).sort((a, b) => (a.due_at ?? 0) - (b.due_at ?? 0));
  const undated = visible.filter((t) => !t.due_at);

  const now = Date.now();
  // 轴的范围：今天 → 最晚截止日，至少铺两周。往前留一点，让「已经逾期」看得见。
  const min = Math.min(now - 2 * DAY, ...dated.map((t) => t.due_at ?? now));
  const max = Math.max(now + 14 * DAY, ...dated.map((t) => t.due_at ?? now));
  const span = Math.max(DAY, max - min);
  const pct = (ms: number) => `${((ms - min) / span) * 100}%`;

  // 刻度：每七天一格，从今天往前对齐。
  const ticks: number[] = [];
  for (let t = min; t <= max; t += 7 * DAY) ticks.push(t);

  if (visible.length === 0) {
    return (
      <div className="rounded-xl bg-card px-4 py-10 text-center text-xs text-dim">
        这个筛选下没有任务。
      </div>
    );
  }

  return (
    <div>
      {dated.length > 0 ? (
        <div className="rounded-xl bg-card p-3">
          {/* 刻度与条形必须落在同一个坐标系里：两处都用 `grid-cols-[15rem_1fr]` 加同一条
              gap，左边那一栏的任务名才会与右边的条对齐。早先用 `ml-[15rem]` 对齐刻度，
              漏掉了 flex 的 gap，刻度和条会整体差出几像素 —— 这种偏差在密集的轴上看
              得出来，而且会随着窗口宽度变。 */}
          <div className="grid grid-cols-[15rem_1fr] gap-2">
            <div />
            <div className="relative h-4 border-b border-edge/70">
              {ticks.map((t) => (
                <div
                  key={t}
                  style={{ left: pct(t) }}
                  className="absolute bottom-0 -translate-x-1/2 text-[9px] tabular-nums text-dim"
                >
                  <span className="block h-1.5 w-px bg-edge-strong/40" />
                  <span className="absolute left-1/2 top-2 -translate-x-1/2 whitespace-nowrap">
                    {shortDate(t)}
                  </span>
                </div>
              ))}
            </div>
          </div>

          <div className="relative mt-2 space-y-1">
            {/* 今天：一条贯穿全表的竖线。它是这张图唯一要读的参照。
                左边界 = 标签栏宽度 + gap，再按比例铺到剩余的轨道上。 */}
            <div
              className="pointer-events-none absolute bottom-0 top-0 z-10 w-px bg-tone-blue/50"
              style={{
                left: `calc(15rem + 0.5rem + (100% - 15.5rem) * ${((now - min) / span).toFixed(6)})`,
              }}
            />
            {dated.map((t) => {
              const late = (t.due_at ?? 0) < now && t.status !== "done";
              const tone = statusTone(t.status);
              return (
                <div
                  key={t.id}
                  className="group relative grid grid-cols-[15rem_1fr] items-center gap-2 rounded-md py-0.5 transition-colors hover:bg-ink/[0.03]"
                >
                  {/* 同样的三段式：透明层管「打开」，交办按钮压在它上面。 */}
                  <button
                    type="button"
                    onClick={() => onOpen(t)}
                    aria-label={`打开任务：${t.title}`}
                    className="absolute inset-0 z-0 cursor-pointer rounded-md outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-edge-strong"
                  />
                  <div className="pointer-events-none relative z-[1] flex min-w-0 items-center gap-1.5 pl-1">
                    <span
                      className="size-1.5 shrink-0 rounded-full"
                      style={{ background: tone.fg }}
                    />
                    <span className="shrink-0 text-[9px]" style={{ color: priorityTone(t.priority).fg }}>
                      {t.priority}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-[11px]">{t.title}</span>
                    <HandoffButton
                      className="pointer-events-auto -my-1"
                      onClick={() => onAsk(taskHandoff(goal, t))}
                    />
                  </div>
                  <div className="pointer-events-none relative z-[1] h-5 min-w-0">
                    {/* 从今天到截止日的一段。逾期的那一段用橙色，不靠文字说明。 */}
                    <div
                      className="absolute top-1/2 h-1 -translate-y-1/2 rounded-full transition-opacity group-hover:opacity-80"
                      style={{
                        left: pct(Math.min(now, t.due_at ?? now)),
                        width: `max(2px, calc(${pct(Math.max(now, t.due_at ?? now))} - ${pct(Math.min(now, t.due_at ?? now))}))`,
                        background: late ? "var(--tone-orange)" : tone.fg,
                        opacity: t.status === "done" ? 0.35 : 0.55,
                      }}
                    />
                    <span
                      className={`absolute top-1/2 -translate-y-1/2 whitespace-nowrap text-[9px] tabular-nums ${
                        // 靠右端时把标签翻到点的左边：否则 `whitespace-nowrap` 会一路
                        // 溢出到卡片外面，正好是被裁掉的那一边。
                        (t.due_at ?? now) > min + span * 0.82 ? "pr-1.5" : "pl-1.5"
                      }`}
                      style={{
                        left: pct(t.due_at ?? now),
                        transform: `translate(${
                          (t.due_at ?? now) > min + span * 0.82 ? "-100%" : "0"
                        }, -50%)`,
                        color: late ? "var(--tone-orange)" : "var(--text-dim)",
                      }}
                    >
                      {shortDate(t.due_at ?? now)}
                      {late ? " 逾期" : ""}
                    </span>
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      ) : (
        <div className="mb-3 rounded-xl bg-card px-4 py-8 text-center text-xs text-dim">
          这个目标下的任务都还没有截止日期。
        </div>
      )}

      {undated.length > 0 && (
        <section className="mt-4">
          <div className="mb-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">
            没有截止日期 · {undated.length}
          </div>
          <div className="flex flex-wrap gap-1.5">
            {undated.map((t) => (
              <button
                key={t.id}
                onClick={() => onOpen(t)}
                className="flex min-w-0 max-w-[18rem] items-center gap-1.5 rounded-lg bg-card px-2 py-1 text-[11px] text-ink/90 transition-colors hover:bg-bubble"
              >
                <span
                  className="size-1.5 shrink-0 rounded-full"
                  style={{ background: statusTone(t.status).fg }}
                />
                <span className="truncate">{t.title}</span>
                {t.assignee && (
                  <span className="shrink-0 text-[10px] text-dim">
                    {personName(people, t.assignee)}
                  </span>
                )}
              </button>
            ))}
          </div>
        </section>
      )}
    </div>
  );
}
