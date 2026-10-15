// 会话列表 + 行内动作（重命名 / fork / 导出 / 删除）。
//
// 侧栏与「会话」设置页共用这一份，只是密度不同（`variant`）—— 两处各写一遍
// 必然会长歪，而「在侧栏删掉一条，设置页里还在」这类不一致最难解释。

import { useState } from "react";
import { GitBranch, MoreHorizontal, Pencil, Share, Trash, X } from "lucide-react";

import type { SessionRow } from "../lib/api";
import { bucketOf, fmtBytes, fullTime, relTime } from "../lib/format";
import * as ops from "../lib/sessionOps";
import { ConfirmDialog } from "./ui/Panel";
import { IconButton } from "./ui/Button";

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
      className="min-w-0 flex-1 rounded border border-dim bg-base px-1.5 py-0.5 text-sm outline-none"
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
  const [menu, setMenu] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const title = s.title || "未命名会话";

  return (
    <>
      <div
        className={`group relative flex items-center gap-1 rounded-md ${
          dense ? "px-2 py-1.5" : "px-3 py-2"
        } ${active ? "bg-card" : "hover:bg-card"}`}
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
            disabled={active}
            onClick={() => ops.openSession(s.id)}
            title={active ? "当前会话" : "打开这个会话"}
            className={`min-w-0 flex-1 text-left ${active ? "cursor-default" : ""}`}
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
          <div
            className={`flex shrink-0 items-center ${
              menu ? "block" : "hidden group-hover:flex"
            }`}
          >
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
                setMenu((m) => !m);
              }}
            >
              {menu ? <X size={12} /> : <MoreHorizontal size={12} />}
            </IconButton>
          </div>
        )}

        {menu && (
          <>
            {/* 点任何地方都收起菜单。放在行内而不是 portal：菜单本来就跟这一行贴在一起。 */}
            <div className="fixed inset-0 z-30" onClick={() => setMenu(false)} />
            <div className="absolute right-1 top-[calc(100%-4px)] z-40 min-w-32 rounded-lg border border-edge bg-card py-1 shadow-lift">
              <MenuItem
                icon={<GitBranch size={12} />}
                label="从此处分支"
                onClick={() => {
                  setMenu(false);
                  void ops.fork(s.id);
                }}
              />
              <MenuItem
                icon={<Share size={12} />}
                label="导出 Markdown"
                onClick={() => {
                  setMenu(false);
                  void ops.export_(s.id, "markdown");
                }}
              />
              <MenuItem
                icon={<Share size={12} />}
                label="导出 JSON"
                onClick={() => {
                  setMenu(false);
                  void ops.export_(s.id, "json");
                }}
              />
              <MenuItem
                icon={<Trash size={12} />}
                label="删除"
                danger
                onClick={() => {
                  setMenu(false);
                  setConfirming(true);
                }}
              />
            </div>
          </>
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

function MenuItem({
  icon,
  label,
  onClick,
  danger,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
  danger?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-xs transition-colors hover:bg-base ${
        danger ? "text-dim hover:text-[var(--danger)]" : "text-ink"
      }`}
    >
      {icon}
      {label}
    </button>
  );
}

/** 按 今天 / 昨天 / 更早 分组。空组不显示。 */
export function SessionList({
  sessions,
  activeId,
  dense = true,
}: {
  sessions: SessionRow[];
  activeId: string | null;
  dense?: boolean;
}) {
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
