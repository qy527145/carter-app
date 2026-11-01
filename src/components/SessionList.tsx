// 会话列表 + 行内动作（重命名 / fork / 导出 / 删除）。
//
// 侧栏与「会话」设置页共用这一份，只是密度不同（`variant`）—— 两处各写一遍
// 必然会长歪，而「在侧栏删掉一条，设置页里还在」这类不一致最难解释。

import { useState } from "react";
import { GitBranch, MessageSquare, MoreHorizontal, Pencil, Share, Trash } from "lucide-react";

import type { SessionRow } from "../lib/api";
import { bucketOf, fmtBytes, fullTime, relTime } from "../lib/format";
import * as ops from "../lib/sessionOps";
import { ConfirmDialog } from "./ui/Panel";
import { IconButton } from "./ui/Button";
import { hasSelection, useContextMenu, type MenuEntry } from "./ui/ContextMenu";

const BUCKETS = ["今天", "昨天", "更早"] as const;

/** 行内重命名。Enter 提交、Esc 取消、失焦提交。 */
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
  return (
    <input
      autoFocus
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onBlur={() => (value.trim() ? onDone(value.trim()) : onCancel())}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          if (value.trim()) onDone(value.trim());
          else onCancel();
        }
        if (e.key === "Escape") onCancel();
      }}
      className="carter-field min-w-0 flex-1 rounded border border-dim bg-base px-1.5 py-0.5 text-sm outline-none"
    />
  );
}

function Row({
  s,
  active,
  dense,
}: {
  s: SessionRow;
  active: boolean;
  dense: boolean;
}) {
  const [renaming, setRenaming] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const menu = useContextMenu();
  const title = s.title || "未命名会话";

  // 行内「更多」与右键走同一份动作 —— 两处各列一遍必然会走样
  //（比如右键多了「重命名」，按钮菜单里却没有）。
  const actions: MenuEntry[] = [];
  // 当前这条也给「打开」：在设置页里点它，本意就是回到对话视图，
  // 而不是去点一个没反应的行。
  actions.push({
    label: active ? "回到对话" : "打开",
    icon: <MessageSquare size={12} />,
    onSelect: () => ops.openSession(s.id),
  });
  actions.push(
    { label: "重命名", icon: <Pencil size={12} />, onSelect: () => setRenaming(true) },
    "separator",
    {
      label: "从此处分支",
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
      icon: <Share size={12} />,
      onSelect: () => void ops.export_(s.id, "json"),
    },
    "separator",
    {
      label: "删除",
      icon: <Trash size={12} />,
      danger: true,
      onSelect: () => setConfirming(true),
    },
  );

  return (
    <>
      <div
        onContextMenu={(e) => {
          // 选中了文字就交给系统菜单 —— 复制选中的那一段比整行动作更可能是本意。
          if (hasSelection()) return;
          e.preventDefault();
          menu.open({ x: e.clientX, y: e.clientY }, actions);
        }}
        className={`group relative flex items-center gap-1 rounded-md ${
          dense ? "px-2 py-1.5" : "px-3 py-2"
        } ${active ? "bg-ink/10" : "hover:bg-ink/5"}`}
      >
        {renaming ? (
          <InlineRename
            initial={title}
            onDone={(t) => {
              setRenaming(false);
              void ops.rename(s.id, t);
            }}
            onCancel={() => setRenaming(false)}
          />
        ) : (
          <button
            // 不用 disabled：禁用的按钮在 WebKit 里不派发鼠标事件，右键就落不到行上。
            //
            // 当前这条也接点击：侧栏里点它什么都不用发生（已经在对话视图），
            // 但「会话」设置页里点它应当回到对话 —— 否则这行看起来是死的。
            onClick={() => ops.openSession(s.id)}
            title={active ? "当前会话 · 点击回到对话" : "打开这个会话"}
            className="min-w-0 flex-1 text-left"
          >
            <div className="truncate text-sm text-ink">{title}</div>
            <div className="mt-0.5 flex items-center gap-1.5 text-[11px] text-dim">
              <span className="truncate">{s.model}</span>
              <span>·</span>
              <span className="shrink-0" title={fullTime(s.created_at)}>
                {relTime(s.created_at)}
              </span>
              {s.forked_from && <GitBranch size={10} className="shrink-0" />}
              {!dense && s.bytes > 0 && <span className="shrink-0">· {fmtBytes(s.bytes)}</span>}
            </div>
          </button>
        )}

        {/* 悬停才出现的行内动作 —— 界面静止时不吵。 */}
        {!renaming && (
          <div className="hidden shrink-0 items-center group-hover:flex">
            <IconButton
              title="重命名"
              onClick={(e) => {
                e.stopPropagation();
                setRenaming(true);
              }}
            >
              <Pencil size={12} />
            </IconButton>
            <IconButton
              title="更多"
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

/** 按 今天 / 昨天 / 更早 分组。空组不显示。 */
export function SessionList({
  sessions,
  activeId,
  dense = true,
  groupByTime = true,
}: {
  sessions: SessionRow[];
  activeId: string | null;
  dense?: boolean;
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
          <Row key={s.id} s={s} active={s.id === activeId} dense={dense} />
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
            <Row key={s.id} s={s} active={s.id === activeId} dense={dense} />
          ))}
        </div>
      ))}
    </div>
  );
}
