// 对话主区：消息列表 + 待办 + 提问面板 + 输入框。
//
// 列表**没有**做虚拟滚动。`@tanstack/react-virtual` 已经装好，但流式追加时每一帧
// 都在改末尾元素的高度，虚拟器要不断重测；在这个「跟着底部滚」的场景里，它带来
// 的复杂度（动态高度 + 保持贴底 + markdown 异步布局）比收益大。
// TODO: 长会话需要时再引入虚拟滚动（依赖已在 package.json）。
//
// 视觉上照参考实现的方向：**助手输出不套气泡也不带头像**，正文就是页面上的正文；
// 只有用户消息给一个气泡。工具与思考是嵌在其中的卡片 —— 它们是流程的一部分，
// 不该跟正文抢同一层视觉重量。

import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  Bot,
  Brain,
  ChevronRight,
  CircleCheck,
  Copy,
  FileText,
  LoaderCircle,
  Pencil,
  ScanSearch,
  Target,
  TriangleAlert,
  X,
} from "lucide-react";

import { useStore, type Item, type Turn } from "../store";
import { copyText } from "../lib/clipboard";
import { FilePreviewModal } from "./FilePreview";
import { AskForm } from "./AskForm";
import { Composer, MessageImages } from "./Composer";
import { GoalBar } from "./GoalBar";
import { Glyph, ServerCard, ToolCard } from "./toolcards/ToolCard";
import { IconButton } from "./ui/Button";
import { hasSelection, useContextMenu, type MenuEntry } from "./ui/ContextMenu";
import { Banner } from "./ui/Panel";

type ImageItem = Extract<Item, { kind: "image" }>;
type AssistantTurnT = Extract<Turn, { role: "assistant" }>;

// ---------------------------------------------------------------------------
// 片段渲染
// ---------------------------------------------------------------------------

function MarkdownText({ text }: { text: string }) {
  return (
    <div className="carter-md">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>
    </div>
  );
}

function ThinkingBlock({ text, hasSignature }: { text: string; hasSignature?: boolean }) {
  return (
    <details className="group rounded-lg border border-edge bg-card/50">
      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-1.5 text-xs text-dim transition-colors hover:text-ink">
        <ChevronRight size={12} className="transition-transform group-open:rotate-90" />
        <Glyph tone="violet">
          <Brain size={11} />
        </Glyph>
        <span>思考</span>
        {/* hasSignature 只在历史消息里带值；实时增量拿不到签名状态，不显示提示。 */}
        {hasSignature === false && (
          <span className="ml-1 text-[11px] text-tone-orange">
            无签名 · 历史消息无法原样回传
          </span>
        )}
      </summary>
      <div className="whitespace-pre-wrap border-t border-edge px-3 py-2 text-xs leading-relaxed text-dim">
        {text}
      </div>
    </details>
  );
}

/**
 * 历史里的图片块。
 *
 * **不是**粘贴的图 —— 粘贴的图在文本里是 `[img:hash.ext]`，由 `MessageImages`
 * 渲染成真实缩略图。这一条是 IR 里 `Block::Image` 的投影，视图层刻意只带类型与
 * hash、不背字节（一张图几百 KB，塞进 IPC 会让界面卡死），所以只给一个占位标识。
 */
function ImageCard({ item }: { item: ImageItem }) {
  return (
    <div className="flex items-center gap-2 rounded-lg border border-edge bg-card px-3 py-2 text-xs text-dim">
      <Glyph tone="pink">
        <FileText size={11} />
      </Glyph>
      <span className="mono">{item.mediaType}</span>
      {item.hash && <span className="truncate text-dim/70">#{item.hash.slice(0, 12)}</span>}
    </div>
  );
}

function ItemView({
  item,
  showCursor,
  isLast,
}: {
  item: Item;
  showCursor: boolean;
  isLast: boolean;
}) {
  switch (item.kind) {
    case "thinking":
      return <ThinkingBlock text={item.text} hasSignature={item.hasSignature} />;
    case "text":
      return (
        <div className="text-sm text-ink">
          <MarkdownText text={item.text} />
          {showCursor && <span className="carter-cursor">▍</span>}
        </div>
      );
    // 按名字分派到各自的呈现。所有工具共用一种卡片是之前数据不够的产物，
    // 不是设计选择 —— 见 toolcards/ToolCard.tsx 的模块头。
    case "tool":
      return <ToolCard item={item} isLast={isLast} />;
    case "server":
      return <ServerCard item={item} />;
    case "image":
      return <ImageCard item={item} />;
    case "note":
      return <div className="whitespace-pre-wrap text-xs text-dim">{item.text}</div>;
  }
}

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
 * 一轮跑完后「到底改了哪几个文件」。
 *
 * 工具卡片已经在流里各画了一张，但十几次调用之后，这件事不该靠用户往上翻着数。
 * 清单主要来自后端轮首轮末的文件系统监听（见 core 的 `changes.rs`），而不是解析
 * 工具参数 —— 后者只用来补 cwd 之外的写入与「写入 / 修改」标签。
 */
function ChangedFiles({
  files,
  cwd,
  turn,
}: {
  files: ChangedFile[];
  cwd: string;
  /** 这一轮的用户输入序号：算本轮 diff 用。 */
  turn?: number;
}) {
  // 点开的是**现在磁盘上的内容**，不是这一轮的历史快照。
  const [preview, setPreview] = useState<string | null>(null);
  if (files.length === 0) return null;
  const label: Record<ChangedFile["kind"], string> = {
    write: "写入",
    edit: "修改",
    other: "变更",
  };
  return (
    <div className="rounded-lg border border-edge bg-card px-3 py-2">
      <div className="mb-1.5 flex items-center gap-1.5 text-[11px] text-dim">
        <Pencil size={11} />
        本轮变更了 {files.length} 个文件
      </div>
      <ul className="space-y-1">
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
              <span className="mono min-w-0 flex-1 truncate text-xs text-ink" title={f.path}>
                {f.path}
              </span>
              {f.count > 1 && <span className="shrink-0 text-[10px] text-dim">×{f.count}</span>}
              <span className="shrink-0 text-[10px] text-dim">{label[f.kind]}</span>
            </button>
          </li>
        ))}
      </ul>
      {preview !== null && (
        <FilePreviewModal
          path={preview}
          cwd={cwd}
          turn={turn}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}

/**
 * 每一轮用户输入收尾时要列的文件。
 *
 * 实时那一轮里，整个工具循环都落在同一条助手轮上（`TurnStarted` 按用户输入发），
 * 但历史里工具的每次往返各是一条助手消息（见 `store.ts::historyToTurns`）。
 * 按 `turn` 号把相邻的助手轮并成一组、把汇总挂在**最后一个**上面，实时与回看
 * 才会落在同一个位置 —— 这条回答的末尾，而不是半途某次工具调用下面。
 *
 * `changedByTurn` 是后端按轮存下来的**实际**变更（历史回看时靠它，事件不回放）。
 */
function changedFilesPerTurn(
  turns: Turn[],
  changedByTurn: Record<number, string[]>,
  cwd: string,
): ChangedFile[][] {
  const out: ChangedFile[][] = turns.map(() => []);
  let i = 0;
  while (i < turns.length) {
    const t = turns[i];
    if (t.role !== "assistant") {
      i += 1;
      continue;
    }
    let j = i;
    // 没有 turn 号（理论上只在事件竞态里出现）就不合并，按自己的 items 算。
    if (t.turn !== undefined) {
      while (j + 1 < turns.length) {
        const next = turns[j + 1];
        if (next.role !== "assistant" || next.turn !== t.turn) break;
        j += 1;
      }
    }
    const items = turns
      .slice(i, j + 1)
      .flatMap((x) => (x.role === "assistant" ? x.items : []));
    const watched = t.turn !== undefined ? (changedByTurn[t.turn] ?? []) : [];
    out[j] = changedFiles(items, watched, cwd);
    i = j + 1;
  }
  return out;
}

// ---------------------------------------------------------------------------
// 轮次
// ---------------------------------------------------------------------------

function AssistantTurn({
  turn: t,
  isLast,
  streaming,
  showPayload,
  changed,
  cwd,
}: {
  turn: AssistantTurnT;
  isLast: boolean;
  streaming: boolean;
  showPayload: boolean;
  /** 这一轮用户输入改过的文件，由 `TurnList` 按 `turn` 聚合（见那边的注释）。 */
  changed: ChangedFile[];
  /** 预览文件时用来解析相对路径。 */
  cwd: string;
}) {
  const openInspector = useStore((s) => s.openInspector);
  const menu = useContextMenu();
  const items = t.items;

  // 复制的是**正文**：思考与工具输出属于过程，不是用户想带走的那段话。
  const answer = items
    .filter((i): i is Extract<Item, { kind: "text" }> => i.kind === "text")
    .map((i) => i.text)
    .join("\n\n")
    .trim();

  const onContextMenu = (e: React.MouseEvent) => {
    // 有选中文字时让系统菜单出来：用户要的多半是「复制选中的这一段」。
    if (hasSelection()) return;
    const entries: MenuEntry[] = [];
    if (answer) {
      entries.push({
        label: "复制回答",
        icon: <Copy size={12} />,
        onSelect: () => void copyText(answer),
      });
    }
    if (showPayload && t.turn !== undefined) {
      entries.push({
        label: "查看本轮报文",
        icon: <ScanSearch size={12} />,
        onSelect: () => openInspector(t.turn ?? null),
      });
    }
    if (entries.length === 0) return;
    e.preventDefault();
    menu.open({ x: e.clientX, y: e.clientY }, entries);
  };

  return (
    <div className="group/msg min-w-0 space-y-2" onContextMenu={onContextMenu}>
      {/* 自动继续的轮次：用户没说话，是长任务目标推起来的。画一条分隔说明来历 ——
          不标的话，界面上会看起来像模型自己莫名其妙又说了很长一段。 */}
      {t.auto && (
        <div className="flex items-center gap-2 text-[11px] text-dim">
          <span className="h-px flex-1 bg-edge" />
          <span className="inline-flex items-center gap-1">
            <Target size={11} />
            自动继续
          </span>
          <span className="h-px flex-1 bg-edge" />
        </div>
      )}
      {items.length === 0 && streaming && (
        <div className="flex items-center gap-2 text-sm text-dim">
          <span className="carter-dots">
            <span />
            <span />
            <span />
          </span>
          正在生成…
        </div>
      )}
      {items.map((item, i) => (
        <ItemView
          key={i}
          item={item}
          isLast={i === items.length - 1}
          showCursor={streaming && isLast && i === items.length - 1 && item.kind === "text"}
        />
      ))}
      {changed.length > 0 && <ChangedFiles files={changed} cwd={cwd} turn={t.turn} />}
      {/* 把这条回答对回产生它的那次 API 请求。轮次号对**历史消息**也成立
          （后端按同样口径算好，见 `view.rs`），所以切走再回来入口还在。
          侧边聊天不给（`showPayload=false`）：它不落盘、不抓包，按主对话的轮次号
          去开主对话的报文只会指错地方。 */}
      {showPayload && t.turn !== undefined && items.length > 0 && (
        <button
          onClick={() => openInspector(t.turn ?? null)}
          title={`查看第 ${t.turn} 轮发出的请求`}
          className="group/msg inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-dim opacity-0 transition-opacity hover:bg-card hover:text-ink group-hover/msg:opacity-100"
        >
          <ScanSearch size={12} />
          查看报文
        </button>
      )}
    </div>
  );
}

function UserTurn({ turn }: { turn: Extract<Turn, { role: "user" }> }) {
  const menu = useContextMenu();
  return (
    <div
      className="flex justify-end"
      onContextMenu={(e) => {
        if (hasSelection()) return;
        e.preventDefault();
        menu.open({ x: e.clientX, y: e.clientY }, [
          {
            label: "复制消息",
            icon: <Copy size={12} />,
            onSelect: () => void copyText(turn.text),
          },
        ]);
      }}
    >
      {/* 粘贴的图片在会话里存的是 `[img:...]` 引用 token，回看时要渲染回图。
          纯文本消息走 MessageImages 的文本分支，行为与直接输出一致。 */}
      <div className="max-w-[75%] break-words rounded-2xl bg-bubble px-3.5 py-2 text-sm leading-relaxed text-ink">
        <MessageImages text={turn.text} />
      </div>
    </div>
  );
}

function TurnView({
  turn,
  isLast,
  streaming,
  showPayload,
  changed,
  cwd,
}: {
  turn: Turn;
  isLast: boolean;
  streaming: boolean;
  showPayload: boolean;
  changed: ChangedFile[];
  cwd: string;
}) {
  if (turn.role === "user") return <UserTurn turn={turn} />;
  return (
    <AssistantTurn
      turn={turn}
      isLast={isLast}
      streaming={streaming}
      showPayload={showPayload}
      changed={changed}
      cwd={cwd}
    />
  );
}

/**
 * 一段对话的正文。**纯展示**（turns 从参数来，不读 store）—— 主对话与侧边聊天
 * 共用这一份：两边的事件词汇、item 形状完全相同，各画一遍必然会走样。
 */
export function TurnList({
  turns,
  streaming,
  showPayload = true,
  changedByTurn = {},
  cwd = "",
}: {
  turns: Turn[];
  streaming: boolean;
  /** 是否给助手轮画「查看报文」入口。侧边聊天传 false（它不抓包）。 */
  showPayload?: boolean;
  /** 后端记下的按轮实际变更（`files_changed` 事件 / `create_session` 的历史）。 */
  changedByTurn?: Record<number, string[]>;
  /** 用来把工具参数里的绝对路径对回监听器给的相对路径。 */
  cwd?: string;
}) {
  // 挂在「这一轮用户输入的最后一条助手消息」上，而不是每条助手消息各算各的。
  const changed = changedFilesPerTurn(turns, changedByTurn, cwd);

  return (
    <div className="flex flex-col gap-6">
      {turns.map((t, i) => (
        <TurnView
          key={i}
          turn={t}
          isLast={i === turns.length - 1}
          streaming={streaming}
          showPayload={showPayload}
          changed={changed[i]}
          cwd={cwd}
        />
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 列表 + 贴底滚动
// ---------------------------------------------------------------------------

function MessageList() {
  const turns = useStore((s) => s.turns);
  const streaming = useStore((s) => s.streaming);
  const changed = useStore((s) => s.changed);
  const cwd = useStore((s) => s.cwd);
  const scrollRef = useRef<HTMLDivElement>(null);
  // 用户往上翻时不要把他拽回底部 —— 只在「本来就在底部」时跟随。
  const stick = useRef(true);

  useEffect(() => {
    const el = scrollRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [turns, streaming]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  };

  return (
    <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto px-4 py-6">
      <div className="mx-auto max-w-3xl">
        {turns.length === 0 ? (
          <EmptyState />
        ) : (
          <TurnList
            turns={turns}
            streaming={streaming}
            changedByTurn={changed}
            cwd={cwd}
          />
        )}
      </div>
    </div>
  );
}

function EmptyState() {
  const model = useStore((s) => s.model);
  return (
    <div className="flex flex-col items-center justify-center py-20 text-center">
      <div className="mb-5 flex size-11 items-center justify-center rounded-xl bg-ink text-[var(--accent-contrast)]">
        <Bot size={22} />
      </div>
      <div className="mb-1.5 text-[22px] font-semibold tracking-tight">开始一段对话</div>
      <div className="text-sm text-dim">
        {model ? (
          <>
            当前模型 <span className="mono text-ink">{model}</span>
          </>
        ) : (
          "发条消息就能开始。"
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 待办 / 提示条 / 提问面板 / 输入框
// ---------------------------------------------------------------------------

/**
 * 底栏的待办进度。
 *
 * **全部完成就自动收起** —— 之前它会一直挂在那里（`todos.length > 0` 就渲染），
 * 一轮做完之后还占着版面，用户分不清那是「还要做」还是「已经做完了」。
 * 清单本身在对话流里有一张 todo_write 卡片（可折叠、可回看），底栏这条只是
 * 「此刻进行到哪」的提示，做完就该让位。
 */
function TodoPanel() {
  const todos = useStore((s) => s.todos);
  const [dismissed, setDismissed] = useState(false);

  const allDone = todos.length > 0 && todos.every((t) => t.status === "completed");

  // 全部完成之后**自己收起来**。
  //
  // 留 6 秒而不是立刻消失：最后一项打勾得让人看见，否则「做完了没有」这件事
  // 会变成一个悬案。留太久又等于没做 —— 一轮做完之后它还挂在那里，用户分不清
  // 那是「还要做」还是「已经做完了」。
  useEffect(() => {
    if (!allDone) {
      // 新一轮开始（出现未完成项）时重置，否则后续任务不再显示进度。
      setDismissed(false);
      return;
    }
    const t = setTimeout(() => setDismissed(true), 6000);
    return () => clearTimeout(t);
  }, [allDone]);

  if (todos.length === 0) return null;
  if (allDone && dismissed) return null;

  const done = todos.filter((t) => t.status === "completed").length;

  return (
    <div className="border-t border-edge bg-sidebar px-4 py-2">
      <div className="mx-auto flex max-w-3xl items-start gap-2">
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex items-center gap-2 text-[10px] text-dim">
            <span>待办 {done}/{todos.length}</span>
            {allDone && <span className="text-[var(--ok)]">已全部完成</span>}
          </div>
          {todos.map((t, i) => (
            <div key={i} className="flex items-center gap-2 text-xs">
              {t.status === "completed" ? (
                <CircleCheck size={13} className="shrink-0 text-[var(--ok)]" />
              ) : t.status === "in_progress" ? (
                <LoaderCircle size={13} className="shrink-0 animate-spin text-tone-blue" />
              ) : (
                <span className="inline-block size-3 shrink-0 rounded-full border border-edge" />
              )}
              <span className={t.status === "completed" ? "text-dim line-through" : "text-ink"}>
                {t.status === "in_progress" ? t.active_form : t.content}
              </span>
            </div>
          ))}
        </div>
        <IconButton title="收起待办" onClick={() => setDismissed(true)}>
          <X size={12} />
        </IconButton>
      </div>
    </div>
  );
}

function NoticeBanner() {
  const notice = useStore((s) => s.notice);
  const setNotice = useStore((s) => s.setNotice);
  if (!notice) return null;
  return (
    <div className="px-4 pt-3">
      <div className="mx-auto max-w-3xl">
        <Banner kind="warn" onClose={() => setNotice(null)}>
          <div className="flex items-start gap-2">
            <TriangleAlert size={14} className="mt-0.5 shrink-0" />
            <span className="whitespace-pre-wrap break-words">{notice}</span>
          </div>
        </Banner>
      </div>
    </div>
  );
}

/** 顶部条：当前会话标题 + 打开报文面板的入口。 */
function ChatHeader() {
  const sessionId = useStore((s) => s.sessionId);
  const sessions = useStore((s) => s.sessions);
  const openInspector = useStore((s) => s.openInspector);
  const title = sessions.find((s) => s.id === sessionId)?.title;

  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b border-edge px-4">
      <span className="truncate text-sm text-dim">{title || "新对话"}</span>
      <IconButton
        className="ml-auto"
        disabled={!sessionId}
        onClick={() => openInspector(null)}
        title="查看本次会话抓到的 HTTP 报文"
      >
        <ScanSearch size={15} />
      </IconButton>
    </div>
  );
}

export function ChatView() {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ChatHeader />
      <NoticeBanner />
      <MessageList />
      <TodoPanel />
      <AskForm />
      {/* 目标条贴着输入框：它和「我要它干什么」是一件事，读起来是一句话。 */}
      <GoalBar />
      <Composer />
    </div>
  );
}
