import { ChevronRight, Eraser, FileText, Pencil } from "lucide-react";
import { useMemo, useState } from "react";
import { clearChanges } from "../../lib/sessionOps";
import type { Item, Turn } from "../../store";
import { FilePreviewModal } from "../FilePreview";
import { Glyph } from "../toolcards/ToolCard";
import { IconButton } from "../ui/Button";
import { Collapse } from "../ui/Panel";

// ---------------------------------------------------------------------------
// 本轮动过的文件
// ---------------------------------------------------------------------------

/**
 * 一条「本轮动过这个文件」的记录。
 *
 * `kind` 只影响**来源标签**：`write` / `edit` 来自成功的 `write_file` / `edit_file`
 * 调用，`other` 表示只有文件系统监听看见了（`bash`、python、MCP、子进程…）。
 * 清单本身以监听为准，工具参数只补两件事：cwd 之外的写入，以及更具体的标签。
 */
type ChangedFile = { path: string; kind: "write" | "edit" | "other"; count: number };

/** 同一路径被多种方式改过时，标签取更强的那个。 */
const KIND_RANK: Record<ChangedFile["kind"], number> = { other: 1, edit: 2, write: 3 };

function argStr(args: unknown, key: string): string | null {
  if (args && typeof args === "object" && key in args) {
    const v = (args as Record<string, unknown>)[key];
    if (typeof v === "string") return v;
  }
  return null;
}

/**
 * 路径对齐：工具参数可能是 `./a.txt`、绝对路径或 Windows 反斜杠，监听器给的是
 * 相对 cwd 的 `/` 分隔路径。两边不化成同一形状，同一个文件就会占两行。
 */
function normalizePath(raw: string, cwd: string): string {
  let p = raw.trim().replace(/\\/g, "/");
  const root = cwd.replace(/\\/g, "/").replace(/\/+$/, "");
  if (root && p.startsWith(`${root}/`)) p = p.slice(root.length + 1);
  while (p.startsWith("./")) p = p.slice(2);
  return p;
}

/**
 * 合并两种来源，得到这一轮真正动过的文件。
 *
 * 1. 文件系统监听（`watched`）是**权威集合** —— 不管谁写的都在里面；
 * 2. 成功的 `write_file` / `edit_file` 参数补 cwd 之外的写入，并给出具体标签。
 *
 * 同一个文件两边都有时只算一条：计数用工具调用次数，监听只补「没被工具报过」的。
 */
function changedFiles(items: Item[], watched: string[], cwd: string): ChangedFile[] {
  const byPath = new Map<string, ChangedFile>();
  const add = (raw: string, kind: ChangedFile["kind"], count: number) => {
    const path = normalizePath(raw, cwd);
    if (!path) return;
    const prev = byPath.get(path);
    if (prev) {
      prev.count += count;
      // 先 write 再 edit 的文件仍标「写入」：同一条路径的语义不要来回跳。
      if (KIND_RANK[kind] > KIND_RANK[prev.kind]) prev.kind = kind;
    } else {
      byPath.set(path, { path, kind, count });
    }
  };

  for (const item of items) {
    if (item.kind !== "tool") continue;
    // 失败的调用没改到磁盘；还在跑（没有结果）的先不算，等结果回来再说。
    if (!item.result?.ok) continue;
    if (item.name !== "write_file" && item.name !== "edit_file") continue;
    const path = argStr(item.args, "path") ?? argStr(item.args, "file_path");
    if (path) add(path, item.name === "write_file" ? "write" : "edit", 1);
  }

  for (const path of watched) {
    const key = normalizePath(path, cwd);
    if (key && !byPath.has(key)) byPath.set(key, { path: key, kind: "other", count: 1 });
  }
  return [...byPath.values()];
}

/**
 * 统计起点（`since`）以来**一共**改了哪几个文件 —— 底栏变更条的内容。
 *
 * 不再按轮挂在每条回答末尾：长任务一轮接一轮地改，用户关心的是「到现在为止动过
 * 哪些」，而不是逐条回答往上翻着拼。同一个文件跨轮只算一行，计数累加。
 *
 * 实时那一轮的工具参数在 `turns` 里，监听结果在 `changedByTurn` 里（历史回看时
 * 靠它，事件不回放）。没有轮次号的助手轮（极端竞态）只在从头统计时算进去。
 */
function changedSince(
  turns: Turn[],
  changedByTurn: Record<number, string[]>,
  since: number,
  skip: number,
  cwd: string,
): ChangedFile[] {
  const counted = (turn: number | undefined) => (turn === undefined ? since === 0 : turn >= since);
  // 起点那一轮的前 `skip` 条是清空之前就有的（生成中途点的清空），跳过。
  let skipped = 0;
  const items = turns.flatMap((t) => {
    if (t.role !== "assistant" || !counted(t.turn)) return [];
    if (t.turn !== since || skipped >= skip) return t.items;
    const drop = Math.min(skip - skipped, t.items.length);
    skipped += drop;
    return t.items.slice(drop);
  });
  const watched = Object.entries(changedByTurn).flatMap(([k, v]) => (counted(Number(k)) ? v : []));
  return changedFiles(items, watched, cwd);
}

/**
 * 底栏的变更条：与待办条同一级，默认折叠成一行（文件数 + 前几个路径）。
 *
 * 工具卡片已经在流里各画了一张，但十几次调用之后，这件事不该靠用户往上翻着数。
 * 清单主要来自后端轮首轮末的文件系统监听（见 core 的 `changes.rs`），而不是解析
 * 工具参数 —— 后者只用来补 cwd 之外的写入与「写入 / 修改」标签。
 *
 * 「清空」= 从下一轮起重新统计（磁盘上的文件不动）。主对话与侧边聊天各用一条。
 */
export function ChangesPanel({
  sessionId,
  turns,
  changedByTurn,
  since,
  skip,
  cwd,
}: {
  sessionId: string | null;
  turns: Turn[];
  changedByTurn: Record<number, string[]>;
  /** 统计起点（轮次号）。点开文件时的 diff 也从这一轮的基线算起。 */
  since: number;
  /** 起点那一轮里清空之前就有的 item 数（见 store 的 `changesSkip`）。 */
  skip: number;
  cwd: string;
}) {
  // 点开的是**现在磁盘上的内容**与「起点以来的 diff」，不是某一轮的历史快照。
  const [preview, setPreview] = useState<string | null>(null);
  // 默认折叠：长任务可能一次改几十上百个文件，整张清单常驻底栏会把输入框顶走。
  const [expanded, setExpanded] = useState(false);
  const files = useMemo(
    () => changedSince(turns, changedByTurn, since, skip, cwd),
    [turns, changedByTurn, since, skip, cwd],
  );
  if (!sessionId || files.length === 0) return null;
  const label: Record<ChangedFile["kind"], string> = {
    write: "写入",
    edit: "修改",
    other: "变更",
  };
  return (
    <div className="shrink-0 px-4 pt-2">
      <div className="mx-auto max-w-3xl rounded-xl bg-card px-3 py-1.5">
        <div className="flex items-center gap-2">
          <button
            onClick={() => setExpanded((o) => !o)}
            title={expanded ? "折叠变更" : "展开变更"}
            className="flex min-w-0 flex-1 items-center gap-2 py-0.5 text-left text-xs text-dim transition-colors hover:text-ink"
          >
            <ChevronRight
              size={12}
              className={`shrink-0 transition-transform ${expanded ? "rotate-90" : ""}`}
            />
            <span className="shrink-0 tabular-nums">变更 {files.length} 个文件</span>
            {/* 折起来时这一行要能说明里头是什么，否则只有个数等于什么都没说。 */}
            {!expanded && (
              <span className="mono min-w-0 flex-1 truncate text-dim/70">
                {files
                  .slice(0, 3)
                  .map((f) => f.path)
                  .join(" · ")}
                {files.length > 3 ? " …" : ""}
              </span>
            )}
          </button>
          <IconButton
            title="清空：从现在起重新统计变更（不动文件）"
            onClick={() => {
              setExpanded(false);
              clearChanges(sessionId);
            }}
          >
            <Eraser size={12} />
          </IconButton>
        </div>
        <Collapse open={expanded}>
          {/* 清单很长时限高滚动，别把对话区挤没了。 */}
          <ul className="max-h-48 space-y-0.5 overflow-y-auto pb-1 pl-3 pt-1">
            {files.map((f) => (
              <li key={f.path} className="min-w-0">
                <button
                  onClick={() => setPreview(f.path)}
                  title={`预览 ${f.path}`}
                  className="flex w-full min-w-0 cursor-pointer items-center gap-2 rounded px-1 py-0.5 text-left transition-colors hover:bg-elevated"
                >
                  <Glyph
                    tone={f.kind === "write" ? "blue" : f.kind === "edit" ? "orange" : "violet"}
                  >
                    {f.kind === "edit" ? <Pencil size={11} /> : <FileText size={11} />}
                  </Glyph>
                  <span className="mono min-w-0 flex-1 truncate text-xs text-ink">{f.path}</span>
                  {f.count > 1 && <span className="shrink-0 text-[10px] text-dim">×{f.count}</span>}
                  <span className="shrink-0 text-[10px] text-dim">{label[f.kind]}</span>
                </button>
              </li>
            ))}
          </ul>
        </Collapse>
      </div>
      {preview !== null && (
        <FilePreviewModal
          path={preview}
          cwd={cwd}
          sessionId={sessionId}
          since={since}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}

/**
 * 相邻的、同号的助手轮并成一组 —— 一次用户输入一组（工具往返会把一组拆成多条）。
 *
 * 没有轮次号的那条（理论上只在事件竞态里出现）自己单独算一组。
 */
function assistantGroups(turns: Turn[]): { start: number; end: number }[] {
  const out: { start: number; end: number }[] = [];
  let i = 0;
  while (i < turns.length) {
    const t = turns[i];
    if (t.role !== "assistant") {
      i += 1;
      continue;
    }
    let j = i;
    if (t.turn !== undefined) {
      while (j + 1 < turns.length) {
        const next = turns[j + 1];
        if (next.role !== "assistant" || next.turn !== t.turn) break;
        j += 1;
      }
    }
    out.push({ start: i, end: j });
    i = j + 1;
  }
  return out;
}

/**
 * 「重试 / 分叉」挂在哪条助手轮上。
 *
 * 与文件清单同一套分组口径：一轮回答可能由多条助手轮拼成，动作的落点必须是
 * **最后一条** —— 挂在前面的那几条上，点一下看起来只重跑了半截。
 *
 * 值 `true` = 后面还有对话，做这件事会先把它们丢掉，要先问一句；
 * `false` = 只影响这一轮本身；`undefined` = 这一条上没有入口。
 */
export function assistantActionSlots(turns: Turn[]): (boolean | undefined)[] {
  const out = new Array<boolean | undefined>(turns.length);
  for (const { end } of assistantGroups(turns)) {
    const t = turns[end];
    // 自动继续那几轮没有用户提问可重跑（真正跑的是目标引擎追加的那条「继续」），
    // 重试它等于凭空多推一轮 —— 不给这个入口。分叉同理：它是个内部记号。
    if (t.role !== "assistant" || t.auto) continue;
    out[end] = end < turns.length - 1;
  }
  return out;
}

/**
 * 这一条上「分叉」的落点。
 *
 * 回答上 = 保留到这条**回答之后**（`turn + 1`），接着这条思路往别的方向聊。
 * 提问上 = 停在这句话**之前**（`turn`），原话摆进新会话的输入框 —— 改一改再发，
 * 或原样再问一遍。第一句提问之前是空历史，那就是「带着这句话另开一条对话」。
 */
export function forkTarget(
  turn: Turn,
  onFork: ((keepBefore: number, draft?: string) => void) | undefined,
  live: boolean,
): (() => void) | undefined {
  if (!onFork || !live || turn.turn === undefined) return undefined;
  const n = turn.turn;
  if (turn.role === "assistant") return () => onFork(n + 1);
  const draft = turn.raw ?? turn.text;
  return () => onFork(n, draft);
}

/**
 * 同一轮里相邻的几条助手轮并成一条再渲染。
 *
 * 历史里工具的每次往返各是一条助手消息（见 `store.ts::historyToTurns`），原样画出来
 * 就是一串各自带间距、各自带一行（看不见但占位的）操作按钮的小块 —— 而实时那一轮
 * 在界面上本来就是一整块。并起来之后两边长得一样，精简模式也能把整轮的过程折成
 * 一行，操作按钮只在这一轮末尾出现一次。
 */
export function mergeAssistantRuns(turns: Turn[]): Turn[] {
  const out: Turn[] = [];
  for (const t of turns) {
    const last = out[out.length - 1];
    if (
      t.role === "assistant" &&
      last?.role === "assistant" &&
      t.turn !== undefined &&
      last.turn === t.turn
    ) {
      out[out.length - 1] = { ...last, items: [...last.items, ...t.items] };
      continue;
    }
    out.push(t);
  }
  return out;
}
