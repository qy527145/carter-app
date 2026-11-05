// 会话列表 + 行内动作（重命名 / fork / 导出 / 删除）。
//
// 侧栏与「会话」设置页共用这一份，只是密度不同（`variant`）—— 两处各写一遍
// 必然会长歪，而「在侧栏删掉一条，设置页里还在」这类不一致最难解释。

import {
  Copy,
  FileJson,
  GitBranch,
  ListChecks,
  MessageSquare,
  MoreHorizontal,
  Pencil,
  Share,
  Trash,
} from "lucide-react";
import { useRef, useState } from "react";

import type { SessionRow } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { bucketOf, fmtBytes, fullTime, relTime } from "../lib/format";
import { displayName, modelTitle, useModelCatalog } from "../lib/modelLabel";
import * as ops from "../lib/sessionOps";
import { ticketMarkTitle } from "../lib/ticketUi";
import { useStore } from "../store";
import { IconButton } from "./ui/Button";
import { hasSelection, type MenuEntry, useContextMenu } from "./ui/ContextMenu";
import { ConfirmDialog } from "./ui/Panel";

const BUCKETS = ["今天", "昨天", "更早"] as const;

/** 行内重命名。Enter 提交、Esc 取消、失焦提交。打开时全选，直接打字就是覆盖。 */
function InlineRename({
  initial,
  onDone,
  onCancel,
}: {
  initial: string;
  onDone: (title: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initial);
  // Esc 之后 input 卸载会触发一次 blur —— 不挡的话「取消」会变成「提交」。
  const settled = useRef(false);
  const finish = (commit: boolean) => {
    if (settled.current) return;
    settled.current = true;
    const t = value.trim();
    if (commit && t && t !== initial) onDone(t);
    else onCancel();
  };
  return (
    <input
      autoFocus
      value={value}
      aria-label="会话名称"
      onFocus={(e) => e.currentTarget.select()}
      onChange={(e) => setValue(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onBlur={() => finish(true)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.nativeEvent.isComposing) return;
        if (e.key === "Enter") {
          e.preventDefault();
          finish(true);
        }
        if (e.key === "Escape") {
          e.preventDefault();
          finish(false);
        }
      }}
      className="carter-field min-w-0 flex-1 rounded-md border border-edge-strong bg-elevated px-1.5 py-0.5 text-sm outline-none"
    />
  );
}

/** 把命中的那一段加粗标出来 —— 搜索结果里一眼看出「为什么是它」。 */
function Highlight({ text, query }: { text: string; query?: string }) {
  const q = query?.trim().toLowerCase();
  if (!q) return <>{text}</>;
  const i = text.toLowerCase().indexOf(q);
  if (i < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, i)}
      <mark className="rounded-[3px] bg-tone-orange-soft px-px text-ink">
        {text.slice(i, i + q.length)}
      </mark>
      {text.slice(i + q.length)}
    </>
  );
}

/**
 * 列表里的方向键：↑↓ 在行之间移动焦点，Home/End 跳首尾。
 * 行本身是 button，Enter/空格打开天然就有。按 `data-session-row` 找邻居，
 * 跨项目分组也能一路走下去。
 */
function moveFocus(from: HTMLElement, key: string): boolean {
  const scope = from.closest("[data-session-scope]") ?? document;
  const rows = Array.from(scope.querySelectorAll<HTMLElement>("[data-session-row]"));
  const i = rows.indexOf(from);
  let next = -1;
  if (key === "ArrowDown") next = Math.min(rows.length - 1, i + 1);
  else if (key === "ArrowUp") next = Math.max(0, i - 1);
  else if (key === "Home") next = 0;
  else if (key === "End") next = rows.length - 1;
  if (next < 0 || next === i) return next >= 0;
  rows[next].focus();
  rows[next].scrollIntoView({ block: "nearest" });
  return true;
}

function Row({
  s,
  active,
  dense,
  query,
}: {
  s: SessionRow;
  active: boolean;
  dense: boolean;
  query?: string;
}) {
  const [renaming, setRenaming] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const menu = useContextMenu();
  // 当前这条正在生成：行首给一个呼吸点，切到别处也看得见它还在跑。
  const running = useStore((st) => active && st.streaming);
  // 这条会话是不是助理派出去的工单。按 `session_id` 认（工单上记着它的执行会话），
  // 不读标题 —— 工单对话的标题就是普通对话的标题，编号只留在面板那一侧。
  const ticket = useStore((st) => st.assistant?.tickets.find((t) => t.session_id === s.id));
  const title = s.title || "未命名会话";
  // 会话文件里记的是模型引用（`ds/opus`）。显示成 `渠道/显示名`，悬停给出
  // 实际请求名与 meta —— 见 `lib/modelLabel.ts` 的说明。
  const models = useModelCatalog();
  const modelLabel = displayName(s.model, models);

  // 行内「更多」与右键走同一份动作 —— 两处各列一遍必然会走样
  //（比如右键多了「重命名」，按钮菜单里却没有）。
  const actions: MenuEntry[] = [
    // 当前这条也给「打开」：在设置页里点它，本意就是回到对话视图，
    // 而不是去点一个没反应的行。
    {
      label: active ? "回到对话" : "打开",
      icon: <MessageSquare size={12} />,
      shortcut: "↵",
      onSelect: () => ops.openSession(s.id),
    },
    {
      label: "重命名",
      icon: <Pencil size={12} />,
      shortcut: "F2",
      onSelect: () => setRenaming(true),
    },
    {
      label: "复制标题",
      icon: <Copy size={12} />,
      onSelect: () => void copyText(title, "已复制标题"),
    },
    "separator",
    {
      label: "复制一份",
      icon: <GitBranch size={12} />,
      onSelect: () => void ops.fork(s.id),
    },
    {
      label: "导出 Markdown",
      icon: <Share size={12} />,
      onSelect: () => void ops.export_(s.id, "markdown"),
    },
    {
      label: "导出 JSON",
      icon: <FileJson size={12} />,
      onSelect: () => void ops.export_(s.id, "json"),
    },
    "separator",
    {
      label: "删除…",
      icon: <Trash size={12} />,
      danger: true,
      shortcut: "⌫",
      onSelect: () => setConfirming(true),
    },
  ];

  const pad = dense ? "px-2 py-1.5" : "px-3 py-2";

  return (
    <>
      <div
        onContextMenu={(e) => {
          // 选中了文字就交给系统菜单 —— 复制选中的那一段比整行动作更可能是本意。
          if (hasSelection()) return;
          e.preventDefault();
          menu.open({ x: e.clientX, y: e.clientY }, actions);
        }}
        className={`group relative flex items-center rounded-md transition-colors duration-150 ${
          active ? "bg-ink/[0.08]" : "hover:bg-ink/[0.045]"
        }`}
      >
        {/* 当前会话左侧一道短竖条：比只靠底色深一档更容易在长列表里一眼找到。 */}
        <span
          aria-hidden
          className={`absolute left-0 top-1/2 h-4 w-[3px] -translate-y-1/2 rounded-full bg-ink transition-[opacity,transform] duration-200 ${
            active ? "opacity-100" : "scale-y-0 opacity-0"
          }`}
        />
        {renaming ? (
          <div className={`flex min-w-0 flex-1 ${pad}`}>
            <InlineRename
              initial={title}
              onDone={(t) => {
                setRenaming(false);
                void ops.rename(s.id, t);
              }}
              onCancel={() => setRenaming(false)}
            />
          </div>
        ) : (
          <button
            data-session-row
            // 不用 disabled：禁用的按钮在 WebKit 里不派发鼠标事件，右键就落不到行上。
            //
            // 当前这条也接点击：侧栏里点它什么都不用发生（已经在对话视图），
            // 但「会话」设置页里点它应当回到对话 —— 否则这行看起来是死的。
            onClick={() => ops.openSession(s.id)}
            // 双击标题直接改名 —— 桌面应用里改名的肌肉记忆。
            onDoubleClick={(e) => {
              e.preventDefault();
              setRenaming(true);
            }}
            onKeyDown={(e) => {
              if (e.key === "F2") {
                e.preventDefault();
                setRenaming(true);
              } else if (
                (e.key === "Backspace" || e.key === "Delete") &&
                (e.metaKey || e.key === "Delete")
              ) {
                e.preventDefault();
                setConfirming(true);
              } else if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                e.preventDefault();
                const r = e.currentTarget.getBoundingClientRect();
                menu.open({ x: r.left + 24, y: r.bottom }, actions);
              } else if (moveFocus(e.currentTarget, e.key)) {
                e.preventDefault();
              }
            }}
            title={active ? "当前会话 · 双击改名" : "打开这个会话 · 双击改名"}
            aria-current={active ? "true" : undefined}
            // 右侧给悬停按钮留出位置：按钮浮在上面而不是挤占布局，
            // 鼠标划过时标题不会被突然截短一截、整行跳一下。
            className={`min-w-0 flex-1 select-none rounded-md text-left ${pad}`}
          >
            <div className="flex items-center gap-1.5">
              {running && (
                <span
                  title="正在生成"
                  className="size-1.5 shrink-0 animate-pulse rounded-full bg-tone-blue"
                />
              )}
              {ticket && (
                // 工单会话的标记：一个清单图标，不占标题的位置。悬停给出状态 ——
                // 「这条为什么自己动起来了」的答案在这儿，而不在标题里。
                <ListChecks size={11} className="shrink-0 text-tone-violet" aria-label="工单对话">
                  <title>{ticketMarkTitle(ticket.status)}</title>
                </ListChecks>
              )}
              <span
                className={`truncate text-sm ${active ? "font-medium text-ink" : "text-ink/90"}`}
              >
                <Highlight text={title} query={query} />
              </span>
            </div>
            <div className="mt-0.5 flex items-center gap-1.5 text-[11px] text-dim">
              <span className="truncate" title={modelTitle(s.model, models)}>
                {modelLabel}
              </span>
              <span className="text-dim/50">·</span>
              <span className="shrink-0" title={fullTime(s.created_at)}>
                {relTime(s.created_at)}
              </span>
              {s.forked_from && (
                <span title="由别的会话分叉而来" className="shrink-0">
                  <GitBranch size={10} />
                </span>
              )}
              {!dense && s.bytes > 0 && (
                <span className="shrink-0 text-dim/70">· {fmtBytes(s.bytes)}</span>
              )}
            </div>
          </button>
        )}

        {/* 悬停才出现的行内动作 —— 界面静止时不吵。浮在行的右端（绝对定位 +
            同色渐隐底），不占布局：出现与消失都不会让标题换行或截短。 */}
        {!renaming && (
          <div
            className={`pointer-events-none absolute inset-y-0 right-0 flex items-center gap-0.5 rounded-r-md pl-6 pr-1 opacity-0 transition-opacity duration-150 group-hover:pointer-events-auto group-hover:opacity-100 group-focus-within:pointer-events-auto group-focus-within:opacity-100 ${
              active
                ? "bg-gradient-to-l from-[color-mix(in_srgb,var(--text)_8%,var(--row-bg))] from-60% to-transparent"
                : "bg-gradient-to-l from-[color-mix(in_srgb,var(--text)_4.5%,var(--row-bg))] from-60% to-transparent"
            }`}
          >
            <IconButton
              title="重命名（F2）"
              tabIndex={-1}
              onClick={(e) => {
                e.stopPropagation();
                setRenaming(true);
              }}
            >
              <Pencil size={12} />
            </IconButton>
            <IconButton
              title="更多"
              tabIndex={-1}
              onClick={(e) => {
                e.stopPropagation();
                // 对着按钮右缘弹出，不用猜菜单多宽。
                const r = e.currentTarget.getBoundingClientRect();
                menu.open({ x: r.right, y: r.bottom + 4, align: "end" }, actions);
              }}
            >
              <MoreHorizontal size={12} />
            </IconButton>
          </div>
        )}
      </div>

      {confirming && (
        <ConfirmDialog
          title="删除会话"
          danger
          confirmLabel="确认删除"
          message={
            <>
              将删除 <span className="text-ink">{title}</span>，包括它的全部对话历史。
              <br />
              这会直接删掉磁盘上的会话文件，<strong className="font-medium">不可恢复</strong>。
            </>
          }
          onCancel={() => setConfirming(false)}
          onConfirm={() => {
            setConfirming(false);
            void ops.remove(s.id);
          }}
        />
      )}
    </>
  );
}

/** 一处目录下的一批会话。 */
export type ProjectGroup = { cwd: string; sessions: SessionRow[] };

/**
 * 按所属目录分组，组间按组内最近一次写入（`updated_at`）倒序 —— 最近在用的项目排
 * 最上面。用写入时间而不是创建时间：续接一条老会话接着聊，项目也该浮上来。
 *
 * 当前目录**不**置顶：它通常本来就是最近的那个；不是的时候（打开了一条旧项目里的
 * 老会话），硬顶上去反而打乱了「最近项目」的顺序。当前组靠展开 + 加粗来认。
 *
 * 组内不重排，保持后端给的 created_at 倒序 —— 行上显示的就是创建时间，两者一致。
 * 侧栏与「会话」设置页共用 —— 两处分组顺序不一致会让人对不上号。
 */
export function groupByProject(sessions: SessionRow[]): ProjectGroup[] {
  const groups = new Map<string, SessionRow[]>();
  for (const s of sessions) {
    const list = groups.get(s.cwd);
    if (list) list.push(s);
    else groups.set(s.cwd, [s]);
  }
  const latest = (list: SessionRow[]) => Math.max(...list.map((s) => s.updated_at));
  const out = [...groups].map(([cwd, sessions]) => ({ cwd, sessions, at: latest(sessions) }));
  return out.sort((a, b) => b.at - a.at).map(({ cwd, sessions }) => ({ cwd, sessions }));
}

/** 按 今天 / 昨天 / 更早 分组。空组不显示。 */
export function SessionList({
  sessions,
  activeId,
  dense = true,
  groupByTime = true,
  query,
}: {
  sessions: SessionRow[];
  activeId: string | null;
  dense?: boolean;
  /** 搜索词：命中的那一段在标题里标出来。 */
  query?: string;
  /**
   * 是否按时间分组。
   *
   * 「会话」设置页自己按目录分组（见 `SessionsPage`），里面再套一层今天/昨天
   * 就成了两层表头 —— 所以那一处关掉。
   */
  groupByTime?: boolean;
}) {
  if (!groupByTime) {
    return (
      <div className="flex flex-col gap-0.5">
        {sessions.map((s) => (
          <Row key={s.id} s={s} active={s.id === activeId} dense={dense} query={query} />
        ))}
      </div>
    );
  }

  const groups = BUCKETS.map((b) => ({
    label: b,
    items: sessions.filter((s) => bucketOf(s.created_at) === b),
  })).filter((g) => g.items.length > 0);

  if (groups.length === 0) return null;

  return (
    <div className="flex flex-col gap-0.5">
      {groups.map((g) => (
        <div key={g.label}>
          {!dense && (
            <div className="px-3 pb-1 pt-3 text-[11px] font-medium text-dim">{g.label}</div>
          )}
          {g.items.map((s) => (
            <Row key={s.id} s={s} active={s.id === activeId} dense={dense} query={query} />
          ))}
        </div>
      ))}
    </div>
  );
}
