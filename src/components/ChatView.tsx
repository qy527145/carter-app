// 对话主区：消息列表 + 变更 / 待办 + 提问面板 + 输入框。
//
// 列表**没有**做虚拟滚动。`@tanstack/react-virtual` 已经装好，但流式追加时每一帧
// 都在改末尾元素的高度，虚拟器要不断重测；在这个「跟着底部滚」的场景里，它带来
// 的复杂度（动态高度 + 保持贴底 + markdown 异步布局）比收益大。
// TODO: 长会话需要时再引入虚拟滚动（依赖已在 package.json）。
//
// 视觉上照参考实现的方向：**助手输出不套气泡也不带头像**，正文就是页面上的正文；
// 只有用户消息给一个气泡。工具与思考是嵌在其中的卡片 —— 它们是流程的一部分，
// 不该跟正文抢同一层视觉重量。

import { useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  Bot,
  Brain,
  Check,
  ChevronRight,
  CircleCheck,
  Copy,
  ArrowDown,
  Eraser,
  FileText,
  GitBranch,
  LoaderCircle,
  Megaphone,
  MessagesSquare,
  MoreHorizontal,
  Pencil,
  Radar,
  RotateCcw,
  ScanSearch,
  Share,
  Target,
  Terminal,
  TriangleAlert,
  Wrench,
  X,
} from "lucide-react";

import { historyToTurns, useStore, type Item, type Turn } from "../store";
import { getAssistantEpisode, newAssistantEpisode, type TurnTime } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { useConversationMode } from "../lib/conversationMode";
import { fmtDuration, fullTime, msgTime } from "../lib/format";
import { displayName, modelTitle, useModelCatalog } from "../lib/modelLabel";
import {
  ASSISTANT_CHAT_ID,
  clearChanges,
  export_,
  fork,
  regenerate,
  rename,
  retryReply,
} from "../lib/sessionOps";
import { TOAST_MS, toastError } from "../lib/toast";
import { FilePreviewModal } from "./FilePreview";
import { MarkdownImage, MarkdownLink, MarkdownPre, transformMarkdownUrl } from "./Media";
import { AskForm } from "./AskForm";
import { Composer, MessageImages } from "./Composer";
import { TeamTodoDock } from "./TeamTodoDock";
import { TicketReply } from "./TicketReply";
import { GoalBar } from "./GoalBar";
import { MemorySuggestBar } from "./MemorySuggestBar";
import { ModePill } from "./ModePill";
import { Glyph, ServerCard, ToolCard } from "./toolcards/ToolCard";
import { IconButton } from "./ui/Button";
import { hasSelection, useContextMenu, type MenuEntry } from "./ui/ContextMenu";
import { Banner, Collapse, ConfirmDialog } from "./ui/Panel";

type ImageItem = Extract<Item, { kind: "image" }>;
type AssistantTurnT = Extract<Turn, { role: "assistant" }>;

// ---------------------------------------------------------------------------
// 片段渲染
// ---------------------------------------------------------------------------

/**
 * 正文里 `[img:<hash>.<ext>]` 的引用换成 markdown 图片。
 *
 * 这些 token 来自 `read_file` 读图与粘贴的图 —— 模型自己也会把它写进正文（工具描述
 * 里就教了它「可直接嵌入回答」）。原样显示是一串看不懂的 hash，换成图片才对得上
 * 那句话的意思。地址走 `carter-img:` 这个私有协议，由 `img` 组件去图片库取字节
 * （见 `Media.tsx`）。
 *
 * 代码块与行内代码里**不换**：那些地方写的是「这个 token 长什么样」，
 * 换掉就成了把示例篡改成一句 markdown —— 用户复制出去的东西也跟着变了。
 */
function inlineImageTokens(text: string): string {
  const IMG = /\[img:([0-9a-fA-F]+\.[A-Za-z0-9]+)\]/g;
  const CODE = /```[\s\S]*?```|~~~[\s\S]*?~~~|`[^`\n]*`/g;
  let out = "";
  let last = 0;
  for (let m = CODE.exec(text); m; m = CODE.exec(text)) {
    out += text.slice(last, m.index).replace(IMG, (_s, id: string) => `![图片](carter-img:${id})`);
    out += m[0];
    last = m.index + m[0].length;
  }
  return out + text.slice(last).replace(IMG, (_s, id: string) => `![图片](carter-img:${id})`);
}

/**
 * 助手正文。`live` = 这一段还在流式生成。
 *
 * 流式时不渲染 HTML 预览（每来一个增量 `srcDoc` 就整页重载，图表会闪），
 * 这一条由 `Media.tsx` 里的 `HtmlArtifact` 自己处理。
 */
function MarkdownText({ text, cwd, live }: { text: string; cwd: string; live: boolean }) {
  const md = useMemo(() => inlineImageTokens(text), [text]);
  const components = useMemo(
    () => ({
      img: (props: { src?: string; alt?: string }) => (
        <MarkdownImage src={props.src ?? ""} alt={props.alt} cwd={cwd} />
      ),
      a: (props: { href?: string; children?: React.ReactNode }) => (
        <MarkdownLink href={props.href} cwd={cwd}>
          {props.children}
        </MarkdownLink>
      ),
      pre: (props: { children?: React.ReactNode }) => (
        <MarkdownPre live={live}>{props.children}</MarkdownPre>
      ),
    }),
    [cwd, live],
  );
  return (
    <div className="carter-md">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        urlTransform={transformMarkdownUrl}
        components={components}
      >
        {md}
      </ReactMarkdown>
    </div>
  );
}

/**
 * 思考块。
 *
 * 渠道给思考的方式有两种，都是正常情况，不该提示成异常：
 * - 给明文（OpenAI Chat 系的 `reasoning_content` 等）：可能没有签名，照常展开看；
 * - 只给签名 / 加密内容（Anthropic 的 redacted thinking、Responses 的
 *   `encrypted_content`）：内容不透明，下一轮原样回传给上游即可，界面上只留一个
 *   「这里想过」的记号，不给展开（展开了也是空的）。
 */
function ThinkingBlock({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const opaque = text.trim() === "";
  if (opaque) {
    return (
      <div
        className="flex items-center gap-2 rounded-lg bg-ink/[0.03] px-3 py-1.5 text-xs text-dim"
        title="渠道只返回了加密的思考（签名），内容不公开；下一轮会原样回传给上游"
      >
        <span className="w-3 shrink-0" />
        <Glyph tone="violet">
          <Brain size={11} />
        </Glyph>
        <span>思考</span>
        <span className="text-dim/70">内容未公开</span>
      </div>
    );
  }
  return (
    <div className="rounded-lg bg-ink/[0.03]">
      <button
        onClick={() => setOpen((o) => !o)}
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-dim transition-colors hover:text-ink"
      >
        <ChevronRight
          size={12}
          className={`shrink-0 transition-transform ${open ? "rotate-90" : ""}`}
        />
        <Glyph tone="violet">
          <Brain size={11} />
        </Glyph>
        <span>思考</span>
      </button>
      <Collapse open={open}>
        <div className="whitespace-pre-wrap border-t border-edge px-3 py-2 text-xs leading-relaxed text-dim">
          {text}
        </div>
      </Collapse>
    </div>
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
    <div className="flex items-center gap-2 rounded-lg bg-card px-3 py-2 text-xs text-dim">
      <Glyph tone="pink">
        <FileText size={11} />
      </Glyph>
      <span className="mono">{item.mediaType}</span>
      {item.hash && <span className="truncate text-dim/70">#{item.hash.slice(0, 12)}</span>}
    </div>
  );
}

/**
 * 精简模式下把一段「过程」折成一行。
 *
 * 这里**不是**把工具卡片藏起来：整条对话里真正带信息量的是正文，思考与工具调用
 * 属于过程记录 —— 回看时用户想读的是模型说了什么，而不是它敲了哪些命令。所以
 * 折起来，但留着入口，展开后仍是原来那些卡片，一张不少。
 *
 * 两条例外，都是照搬工具卡片那边的纪律：
 * - **失败一定展开**：出错时用户第一反应是看输出，让他再展开一次是多余的；
 * - 跑着的调用要把当前那一步显示在标题上，否则这一轮看起来像卡住了。
 */
function summaryOf(items: Item[]): string[] {
  const counts = new Map<string, number>();
  let thinking = false;
  let server = false;
  let image = false;
  for (const it of items) {
    if (it.kind === "tool") counts.set(it.name, (counts.get(it.name) ?? 0) + 1);
    else if (it.kind === "thinking") thinking = true;
    else if (it.kind === "server") server = true;
    else if (it.kind === "image") image = true;
  }
  const parts: string[] = [];
  for (const [name, n] of counts) parts.push(n > 1 ? `${name} ×${n}` : name);
  if (thinking) parts.push("思考");
  if (server) parts.push("服务端");
  if (image) parts.push("图片");
  return parts;
}

/** 正在跑的那条调用的名字 —— 标题上要有东西在动，否则看起来像卡住了。 */
function runningName(items: Item[]): string | null {
  for (const it of items) {
    if (it.kind === "tool" && !it.result) return it.name;
  }
  return null;
}

function ProcessGroup({ items, cwd }: { items: Item[]; cwd: string }) {
  const failed = items.filter(
    (i): i is Extract<Item, { kind: "tool" }> =>
      i.kind === "tool" && i.result !== undefined && !i.result.ok,
  );
  const running = runningName(items);
  const [manual, setManual] = useState<boolean | null>(null);
  // 失败时强制展开；用户手动点过之后以用户为准（与工具卡片的开关同一套规矩）。
  const open = manual ?? failed.length > 0;
  const parts = summaryOf(items);

  return (
    <div className="rounded-lg bg-ink/[0.03]">
      <button
        onClick={() => setManual(!open)}
        title={parts.join(" · ")}
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-dim transition-colors hover:text-ink"
      >
        <ChevronRight
          size={12}
          className={`shrink-0 transition-transform ${open ? "rotate-90" : ""}`}
        />
        <Glyph tone={failed.length > 0 ? "orange" : "blue"}>
          {running ? <LoaderCircle size={11} className="animate-spin" /> : <Wrench size={11} />}
        </Glyph>
        <span className="shrink-0">过程</span>
        <span className="min-w-0 flex-1 truncate text-dim/80">
          {running ? `正在 ${running}…` : parts.join(" · ")}
        </span>
        {failed.length > 0 && (
          <span className="shrink-0 text-[11px] text-[var(--danger)]">
            {failed.length} 步失败
          </span>
        )}
      </button>
      <Collapse open={open}>
        <div className="space-y-2 border-t border-edge px-3 py-2">
          {items.map((it, i) => (
            <ItemView
              key={i}
              item={it}
              cwd={cwd}
              writing={false}
              isLast={i === items.length - 1}
            />
          ))}
        </div>
      </Collapse>
    </div>
  );
}

/**
 * 把一条助手轮的片段分成「正文 / 过程」两类。
 *
 * 正文（`text`）与系统说明（`note`）各自单独成段：前者是用户要读的东西，
 * 后者是「上下文压缩了」这类必须看见的提示，两者都不该被折进过程里。
 * 其余（思考、工具、服务端活动、图片块）按到达顺序并成一段过程 —— 顺序不能打乱，
 * 那正是模型当时的思路。
 */
type Segment =
  | { kind: "text"; item: Extract<Item, { kind: "text" }> }
  | { kind: "note"; item: Extract<Item, { kind: "note" }> }
  | { kind: "process"; items: Item[] };

function segmentsOf(items: Item[]): Segment[] {
  const out: Segment[] = [];
  for (const item of items) {
    if (item.kind === "text") {
      out.push({ kind: "text", item });
      continue;
    }
    if (item.kind === "note") {
      out.push({ kind: "note", item });
      continue;
    }
    const last = out[out.length - 1];
    if (last && last.kind === "process") last.items.push(item);
    else out.push({ kind: "process", items: [item] });
  }
  return out;
}

function ItemView({
  item,
  writing,
  isLast,
  cwd,
}: {
  item: Item;
  writing: boolean;
  isLast: boolean;
  /** 解析正文里的相对图片路径、判断本地链接用。 */
  cwd: string;
}) {
  switch (item.kind) {
    case "thinking":
      return <ThinkingBlock text={item.text} />;
    case "text":
      return (
        // 流式时给整段文字一个「墨落纸」的落定动画（见 index.css 的 carter-bloom）。
        // 只在正在生成的这一段上挂 —— 历史消息静默出现，不该一条条播动画。
        // 同一段后续增量复用同一个 DOM 元素，不会每帧重播。
        <div className={`text-sm text-ink ${writing ? "carter-bloom" : ""}`}>
          {/* writing 只挂在正在生成的那一段上：它就是「这段还在写」的判据，
              内联的 HTML 预览等它写完再挂载，否则每个增量都会把 iframe 重载一次。 */}
          <MarkdownText text={item.text} cwd={cwd} live={writing} />
        </div>
      );
    // 按名字分派到各自的呈现。所有工具共用一种卡片是之前数据不够的产物，
    // 不是设计选择 —— 见 toolcards/ToolCard.tsx 的模块头。
    case "tool":
      return <ToolCard item={item} isLast={isLast} cwd={cwd} />;
    case "server":
      return <ServerCard item={item} />;
    case "image":
      return <ImageCard item={item} />;
    case "note":
      if (item.cursor) return <CursorLine label={item.text} />;
      return <div className="whitespace-pre-wrap text-xs text-dim">{item.text}</div>;
  }
}

/** 一段波浪线（CSS mask 画，颜色跟着文字走）。 */
const WAVE_MASK =
  "url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='6'%3E%3Cpath d='M0 3 Q3 0 6 3 T12 3' fill='none' stroke='black' stroke-width='1.2'/%3E%3C/svg%3E\")";

function Wave() {
  return (
    <span
      aria-hidden
      className="h-1.5 min-w-6 flex-1 bg-current opacity-50"
      style={{ maskImage: WAVE_MASK, WebkitMaskImage: WAVE_MASK, maskRepeat: "repeat-x", WebkitMaskRepeat: "repeat-x" }}
    />
  );
}

/**
 * 上下文起点：压缩、助理对话开新一期。线以上的内容模型不再看到原文 ——
 * 界面上照样留着能翻，只用这条线把「还算数」和「已经翻篇」分开。
 */
export function CursorLine({ label, title }: { label: string; title?: string }) {
  return (
    <div className="flex items-center gap-3 text-[11px] text-dim" title={title}>
      <Wave />
      <span className="max-w-[70%] shrink-0 truncate">{label}</span>
      <Wave />
    </div>
  );
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
function assistantActionSlots(turns: Turn[]): (boolean | undefined)[] {
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
function forkTarget(
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
function mergeAssistantRuns(turns: Turn[]): Turn[] {
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

// ---------------------------------------------------------------------------
// 轮次
// ---------------------------------------------------------------------------

/** 「还在跑」的跳动圆点。还什么都没出来时（`label`）多带一句字。 */
function GeneratingHint({ label = false }: { label?: boolean }) {
  return (
    <div className="flex h-5 items-center gap-2 text-sm text-dim" aria-label="正在生成">
      <span className="carter-dots">
        <span />
        <span />
        <span />
      </span>
      {label && "正在生成…"}
    </div>
  );
}

/**
 * 气泡下面那一行小按钮（复制 / 重试 / 报文）。
 *
 * 悬停才浮出来：静止时它不该跟正文抢注意力；但**始终占着位置**（`opacity-0`
 * 而不是不渲染），否则鼠标移上去的一瞬间整段对话会跳一下。
 * `focus-within` 让键盘也能把它叫出来 —— 透明不等于按不到。
 */
function ActionChip({
  icon,
  label,
  title,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  title?: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      title={title ?? label}
      className="carter-press inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-dim hover:bg-ink/5 hover:text-ink"
    >
      {icon}
      {label}
    </button>
  );
}

/** 操作行的共同外壳：悬停/聚焦时出现。 */
const ACTION_ROW =
  "flex flex-wrap items-center gap-1 opacity-0 transition-opacity duration-150 group-hover/msg:opacity-100 focus-within:opacity-100";

/**
 * 消息旁边那一小段时间。**常显**，不跟操作行一起悬停才出来 —— 翻一段长对话时
 * 要一眼看出「这两句隔了多久」，每条都去悬停一下就没法用了。
 *
 * `at` 缺省或为 0（老会话没有时刻记录）时什么都不画，不占位。
 */
function MsgTime({ at, extra }: { at?: number | null; extra?: string }) {
  if (!at) return null;
  const label = msgTime(at);
  if (!label) return null;
  return (
    <time
      dateTime={new Date(at).toISOString()}
      title={fullTime(at)}
      className="shrink-0 select-none text-[11px] tabular-nums text-dim"
    >
      {label}
      {extra && <span className="ml-1">· {extra}</span>}
    </time>
  );
}

function AssistantTurn({
  turn: t,
  time,
  isLast,
  streaming,
  awaitingUser,
  showPayload,
  cwd,
  onRetry,
  retryAsks,
  onFork,
}: {
  turn: AssistantTurnT;
  /** 这一轮的起止时刻。回答末尾显示答完的时刻与用时。 */
  time?: TurnTime;
  isLast: boolean;
  streaming: boolean;
  awaitingUser: boolean;
  showPayload: boolean;
  /** 预览文件时用来解析相对路径。 */
  cwd: string;
  /** 重跑这一轮。不给就不画「重试」（侧边聊天、继承来的上下文）。 */
  onRetry?: () => void;
  /** 重试会顺带丢掉后面几轮对话 —— 点之前要先问一句。 */
  retryAsks?: boolean;
  /** 从这一轮之后的对话分叉出一条新会话。 */
  onFork?: () => void;
}) {
  const openInspector = useStore((s) => s.openInspector);
  const mode = useConversationMode();
  const menu = useContextMenu();
  const items = t.items;
  // 「复制」就地变对勾，2.5 秒后复位（与文件预览弹窗同一个节奏）。
  const [copied, setCopied] = useState(false);
  const [confirmingRetry, setConfirmingRetry] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), TOAST_MS);
    return () => clearTimeout(timer);
  }, [copied]);

  // 复制的是**正文**：思考与工具输出属于过程，不是用户想带走的那段话。
  const answer = items
    .filter((i): i is Extract<Item, { kind: "text" }> => i.kind === "text")
    .map((i) => i.text)
    .join("\n\n")
    .trim();

  // 重试 = 就地重写这条回答（提问与前面的工具往返都留着）。流式期间整排重试都由
  // `TurnList` 收掉（见那边的注释），这里只认「有没有回调」。
  const canRetry = onRetry !== undefined;
  // 一条动作都没有时整行不渲染：纯工具输出的回答末尾不该留一条看不见的空白。
  const showPayloadChip = showPayload && t.turn !== undefined;
  const actions = !!answer || canRetry || showPayloadChip || onFork !== undefined;
  // 答完的时刻 + 用时。还在写的那一轮不显示（时刻还没定）；只有收尾没有开头的
  // （`started_at` 为 0）只显示时刻，不瞎算用时。
  const done = streaming && isLast ? null : (time?.ended_at ?? null);
  const took =
    done && time?.started_at && done >= time.started_at
      ? `用时 ${fmtDuration(done - time.started_at)}`
      : undefined;

  const doCopy = async () => {
    // 成功才打勾；失败时 copyText 自己会 toast。
    if (await copyText(answer, null)) setCopied(true);
  };

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
    if (canRetry) {
      entries.push({
        label: "重新生成",
        icon: <RotateCcw size={12} />,
        onSelect: () => (retryAsks ? setConfirmingRetry(true) : onRetry?.()),
      });
    }
    if (onFork) {
      entries.push({
        label: "从这里分叉",
        icon: <GitBranch size={12} />,
        onSelect: onFork,
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

  // 精简模式只在渲染这一层换一种切法（见 `segmentsOf`），数据与动作都不变。
  const segs = mode === "compact" ? segmentsOf(items) : null;

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
      {segs
        ? segs.map((seg, i) => {
            const last = i === segs.length - 1;
            if (seg.kind === "process")
              return <ProcessGroup key={i} items={seg.items} cwd={cwd} />;
            // 「正在写」只算正在生成的那一段正文，与详细模式的判断口径一致。
            return (
              <ItemView
                key={i}
                item={seg.item}
                cwd={cwd}
                isLast={last}
                writing={streaming && isLast && last && seg.kind === "text"}
              />
            );
          })
        : items.map((item, i) => (
            <ItemView
              key={i}
              item={item}
              cwd={cwd}
              isLast={i === items.length - 1}
              writing={streaming && isLast && i === items.length - 1 && item.kind === "text"}
            />
          ))}
      {/* 挂在整轮末尾，不挂在某一段正文后面：思考、跑工具的时候最后一项不是正文，
          挂在正文上这几段就看不出还在跑。等用户回答提问时不算在跑。 */}
      {streaming && isLast && !awaitingUser && (
        <GeneratingHint label={items.length === 0} />
      )}
      {/* 回答下面那一行小动作。悬停才浮出来，但一直占着位置 —— 否则鼠标移上来
          的一瞬间整段对话会跳一下。一条动作都没有时整行不渲染（纯工具输出的回答
          不该在末尾留一条空白）。 */}
      {(actions || done) && (
        <div className="flex items-center gap-2">
          {/* 时间常显，在操作行**前面**：操作行静止时是透明的，时间不该跟着隐身。 */}
          <MsgTime at={done} extra={took} />
          {actions && (
        <div className={ACTION_ROW}>
          {answer && (
            <ActionChip
              icon={copied ? <Check size={12} className="carter-pop text-[var(--ok)]" /> : <Copy size={12} />}
              label={copied ? "已复制" : "复制"}
              onClick={() => void doCopy()}
            />
          )}
          {canRetry && (
            <ActionChip
              icon={<RotateCcw size={12} />}
              label="重试"
              title={
                retryAsks
                  ? "沿用同样的上下文重写这条回复（会丢掉后面的对话）"
                  : "沿用同样的上下文重写这条回复"
              }
              onClick={() => (retryAsks ? setConfirmingRetry(true) : onRetry?.())}
            />
          )}
          {onFork && (
            <ActionChip
              icon={<GitBranch size={12} />}
              label="分叉"
              title="带着到这条回答为止的对话分叉出一条新会话，原来这条不受影响"
              onClick={onFork}
            />
          )}
          {/* 把这条回答对回产生它的那次 API 请求。轮次号对**历史消息**也成立
              （后端按同样口径算好，见 `view.rs`），所以切走再回来入口还在。
              侧边聊天不给（`showPayload=false`）：它不落盘、不抓包，按主对话的轮次号
              去开主对话的报文只会指错地方。 */}
          {showPayloadChip && (
            <ActionChip
              icon={<ScanSearch size={12} />}
              label="报文"
              title={`查看第 ${t.turn} 轮发出的请求`}
              onClick={() => openInspector(t.turn ?? null)}
            />
          )}
        </div>
          )}
        </div>
      )}
      {confirmingRetry && (
        <ConfirmDialog
          title="重新生成这条回复？"
          message={
            <>
              这条回复之后的对话会被丢掉，模型会拿写这条回复时的同一份上下文重写一遍。
              <br />
              <span className="text-dim">
                只回滚对话，已经改过的文件、跑过的命令不会撤销。
              </span>
            </>
          }
          confirmLabel="重新生成"
          onCancel={() => setConfirmingRetry(false)}
          onConfirm={() => {
            setConfirmingRetry(false);
            onRetry?.();
          }}
        />
      )}
    </div>
  );
}

function UserTurn({
  turn,
  time,
  onRetry,
  retryAsks,
  onFork,
}: {
  turn: Extract<Turn, { role: "user" }>;
  /** 这一轮的起止时刻。气泡下方显示发问的时刻。 */
  time?: TurnTime;
  /** 把这句话原样再问一遍（这一轮的回答及之后的对话丢掉）。 */
  onRetry?: () => void;
  /** 重试会顺带丢掉后面几轮对话 —— 点之前要先问一句。 */
  retryAsks?: boolean;
  /** 从这句话之前分叉出一条新会话，原话摆进输入框。 */
  onFork?: () => void;
}) {
  const menu = useContextMenu();
  // 与回答那边同一套反馈：就地变对勾，2.5 秒后复位。
  const [copied, setCopied] = useState(false);
  const [confirmingRetry, setConfirmingRetry] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), TOAST_MS);
    return () => clearTimeout(timer);
  }, [copied]);

  const retry = () => (retryAsks ? setConfirmingRetry(true) : onRetry?.());

  const menuEntries = (): MenuEntry[] => {
    const entries: MenuEntry[] = [
      {
        label: "复制消息",
        icon: <Copy size={12} />,
        onSelect: () => void copyText(turn.text),
      },
    ];
    if (onRetry) {
      entries.push({ label: "重新提问", icon: <RotateCcw size={12} />, onSelect: retry });
    }
    if (onFork) {
      entries.push({
        label: "从这里分叉",
        icon: <GitBranch size={12} />,
        onSelect: onFork,
      });
    }
    return entries;
  };

  return (
    <div
      className="group/msg flex flex-col items-end"
      onContextMenu={(e) => {
        if (hasSelection()) return;
        e.preventDefault();
        menu.open({ x: e.clientX, y: e.clientY }, menuEntries());
      }}
    >
      {/* 粘贴的图片在会话里存的是 `[img:...]` 引用 token，回看时要渲染回图。
          纯文本消息走 MessageImages 的文本分支，行为与直接输出一致。 */}
      <div className="max-w-[75%] break-words rounded-2xl bg-bubble px-3.5 py-2 text-sm leading-relaxed text-ink">
        <MessageImages text={turn.text} />
      </div>
      <div className="mt-1 flex items-center justify-end gap-2">
      <div className={`${ACTION_ROW} justify-end`}>
        <ActionChip
          icon={copied ? <Check size={12} className="carter-pop text-[var(--ok)]" /> : <Copy size={12} />}
          label={copied ? "已复制" : "复制"}
          onClick={async () => {
            if (await copyText(turn.text, null)) setCopied(true);
          }}
        />
        {onRetry && (
          <ActionChip
            icon={<RotateCcw size={12} />}
            label="重试"
            title={
              retryAsks
                ? "把这句话原样再问一遍（会丢掉后面的对话）"
                : "把这句话原样再问一遍"
            }
            onClick={retry}
          />
        )}
        {onFork && (
          <ActionChip
            icon={<GitBranch size={12} />}
            label="分叉"
            title="从这句话之前分叉出一条新会话，原话放进输入框（原来这条不受影响）"
            onClick={onFork}
          />
        )}
      </div>
        {/* 时间常显，放在操作行**外面**：操作行静止时是透明的。 */}
        <MsgTime at={time?.started_at} />
      </div>
      {confirmingRetry && (
        <ConfirmDialog
          title="重新问这句话？"
          message={
            <>
              这句之后的对话会被丢掉，模型会基于这句话重新回答。
              <br />
              <span className="text-dim">
                只回滚对话，已经改过的文件、跑过的命令不会撤销。
              </span>
            </>
          }
          confirmLabel="重新提问"
          onCancel={() => setConfirmingRetry(false)}
          onConfirm={() => {
            setConfirmingRetry(false);
            onRetry?.();
          }}
        />
      )}
    </div>
  );
}

/** 与后端 `assistant::REPORT_MARK` / `EVENT_MARK` 逐字对应。 */
const REPORT_MARK = "【助理汇报】";
const EVENT_MARK = "【新事件】";

/**
 * 助理送进来的一条消息：汇报（进助理对话）或观察源的新事件（进工单会话）。
 * 第一行是标题，后面是正文。靠左 —— 不是用户在说话。
 */
function ReportTurn({ mark, text, latest }: { mark: string; text: string; latest?: boolean }) {
  const rest = text.slice(mark.length);
  const cut = rest.indexOf("\n");
  const title = cut < 0 ? rest : rest.slice(0, cut);
  let body = cut < 0 ? "" : rest.slice(cut + 1).trim();
  // 汇报里的工单：只在这是它最近一条汇报、而且它还等着人时给回答框 —— 旧汇报上的
  // 按钮点了会回答一个早就不是这个问题的工单。
  const number = mark === REPORT_MARK ? reportTicket(title) : null;
  const ticket = useStore((s) =>
    number === null || !latest ? undefined : s.assistant?.tickets.find((t) => t.number === number),
  );
  const answerable = ticket && TICKET_WAITING.includes(ticket.status);
  // 候选已经画成按钮了，正文里那行给模型看的「候选：」不再重复。
  if (answerable && ticket.options?.length) body = body.replace(/\n?候选：.*$/, "").trim();
  return (
    <div className="rounded-xl border border-edge bg-card px-3.5 py-2.5">
      <div className="flex items-center gap-1.5 text-xs font-medium text-tone-blue">
        {mark === EVENT_MARK ? (
          <Radar size={12} className="shrink-0" />
        ) : (
          <Megaphone size={12} className="shrink-0" />
        )}
        <span className="truncate">{title}</span>
      </div>
      {body && <div className="mt-1 whitespace-pre-wrap break-words text-sm text-ink">{body}</div>}
      {answerable && (
        <div className="mt-2">
          <TicketReply t={ticket} />
        </div>
      )}
    </div>
  );
}

/** 等人回答的工单状态（与面板「等你处理」里能回话的那几种一致）。 */
const TICKET_WAITING = ["ready", "needs_user", "blocked", "failed", "unreported"];

/** 汇报标题里的工单编号：`等你拍板 · #3 标题` → 3。提议、已答复这类不算。 */
function reportTicket(title: string): number | null {
  if (title.startsWith("已答复") || title.startsWith("本来会接")) return null;
  const m = / · #(\d+) /.exec(title);
  return m ? Number(m[1]) : null;
}

/** 每张工单最近一条汇报在第几轮 —— 只有那一条给回答框。 */
function latestReports(turns: Turn[]): Set<number> {
  const seen = new Set<number>();
  const out = new Set<number>();
  for (let i = turns.length - 1; i >= 0; i--) {
    const t = turns[i];
    if (t.role !== "user" || !t.text.startsWith(REPORT_MARK)) continue;
    const rest = t.text.slice(REPORT_MARK.length);
    const n = reportTicket(rest.split("\n", 1)[0]);
    if (n === null || seen.has(n)) continue;
    seen.add(n);
    out.add(i);
  }
  return out;
}

/** 与后端 `tools::background::DONE_MARK` 逐字对应。 */
const DONE_MARK = "【后台命令】";

/**
 * 后台命令结束时系统送进来的通知：第一行是「shell_id · 已结束（退出码 N）」，
 * 后面是给模型看的说明与输出。不是用户说的话，画成一张可展开的卡片。
 */
function BackgroundDoneTurn({ text }: { text: string }) {
  const rest = text.slice(DONE_MARK.length);
  const cut = rest.indexOf("\n");
  const title = cut < 0 ? rest : rest.slice(0, cut);
  const body = cut < 0 ? "" : rest.slice(cut + 1).trim();
  const failed = !/退出码 0）/.test(title);
  return (
    <details className="group rounded-xl border border-edge bg-card px-3.5 py-2.5">
      <summary
        className={`flex cursor-pointer list-none items-center gap-1.5 text-xs font-medium ${
          failed ? "text-danger" : "text-ok"
        }`}
      >
        <Terminal size={12} className="shrink-0" />
        <span className="truncate">后台命令 {title}</span>
        <ChevronRight size={12} className="shrink-0 transition-transform group-open:rotate-90" />
      </summary>
      {body && (
        <pre className="mt-1.5 max-h-80 overflow-auto whitespace-pre-wrap break-words font-mono text-xs text-dim">
          {body}
        </pre>
      )}
    </details>
  );
}

/** 与后端 `peers::PEER_MARK` 逐字对应。 */
const PEER_MARK = "【跨会话】";

/**
 * 另一条会话发来的信（或回执）：第一行是「来自 xxx · m3」，后面是给模型看的说明与
 * `<来信>` 正文。界面只摊开正文，那段说明折在「原文」里。
 */
function PeerTurn({ text }: { text: string }) {
  const rest = text.slice(PEER_MARK.length);
  const cut = rest.indexOf("\n");
  const title = cut < 0 ? rest : rest.slice(0, cut);
  const letters = [...text.matchAll(/<(来信|回执)([^>]*)>\n([\s\S]*?)\n<\/\1>/g)].map((m) => ({
    receipt: m[1] === "回执",
    from: /from="([^"]*)"/.exec(m[2])?.[1] ?? "",
    body: m[3],
  }));
  return (
    <div className="rounded-xl border border-edge bg-card px-3.5 py-2.5">
      <div className="flex items-center gap-1.5 text-xs font-medium text-tone-blue">
        <MessagesSquare size={12} className="shrink-0" />
        <span className="truncate">跨会话 · {title}</span>
      </div>
      {letters.map((l, i) => (
        <div key={i} className="mt-1.5">
          {letters.length > 1 && <div className="text-xs text-dim">{l.from}</div>}
          <div className={`whitespace-pre-wrap break-words text-sm ${l.receipt ? "text-dim" : "text-ink"}`}>
            {l.body}
          </div>
        </div>
      ))}
      <details className="group mt-1.5">
        <summary className="flex cursor-pointer list-none items-center gap-1 text-xs text-dim">
          <ChevronRight size={11} className="shrink-0 transition-transform group-open:rotate-90" />
          模型看到的原文
        </summary>
        <pre className="mt-1 max-h-80 overflow-auto whitespace-pre-wrap break-words font-mono text-xs text-dim">
          {text}
        </pre>
      </details>
    </div>
  );
}

function TurnView({
  turn,
  time,
  isLast,
  streaming,
  awaitingUser,
  showPayload,
  cwd,
  onRetry,
  retryAsks,
  onFork,
  latestReport,
}: {
  turn: Turn;
  time?: TurnTime;
  isLast: boolean;
  streaming: boolean;
  awaitingUser: boolean;
  showPayload: boolean;
  cwd: string;
  onRetry?: () => void;
  retryAsks?: boolean;
  onFork?: () => void;
  /** 这是某张工单最近的一条汇报（只有它给回答框）。 */
  latestReport?: boolean;
}) {
  if (turn.role === "user") {
    // 助理自己送进来的汇报：不是用户说的话，不画成用户气泡，也不给重试 / 分叉。
    if (turn.text.startsWith(REPORT_MARK))
      return <ReportTurn mark={REPORT_MARK} text={turn.text} latest={latestReport} />;
    if (turn.text.startsWith(EVENT_MARK)) return <ReportTurn mark={EVENT_MARK} text={turn.text} />;
    if (turn.text.startsWith(DONE_MARK)) return <BackgroundDoneTurn text={turn.text} />;
    if (turn.text.startsWith(PEER_MARK)) return <PeerTurn text={turn.text} />;
    return (
      <UserTurn turn={turn} time={time} onRetry={onRetry} retryAsks={retryAsks} onFork={onFork} />
    );
  }
  return (
    <AssistantTurn
      turn={turn}
      time={time}
      isLast={isLast}
      streaming={streaming}
      awaitingUser={awaitingUser}
      showPayload={showPayload}
      cwd={cwd}
      onRetry={onRetry}
      retryAsks={retryAsks}
      onFork={onFork}
    />
  );
}

/**
 * 一段对话的正文。**纯展示**（turns 从参数来，不读 store）—— 主对话与侧边聊天
 * 共用这一份：两边的事件词汇、item 形状完全相同，各画一遍必然会走样。
 */
export function TurnList({
  turns: raw,
  times,
  streaming,
  awaitingUser = false,
  showPayload = true,
  cwd = "",
  onRetry,
  onFork,
}: {
  turns: Turn[];
  /** 每一轮的起止时刻（轮次号 → 时刻）。不传 = 不显示时间（侧边聊天继承来的那段）。 */
  times?: Record<number, TurnTime>;
  streaming: boolean;
  /** 模型停下来等用户回答提问：还算这一轮，但不该再显示「正在生成」。 */
  awaitingUser?: boolean;
  /** 是否给助手轮画「查看报文」入口。侧边聊天传 false（它不抓包）。 */
  showPayload?: boolean;
  /** 预览工具卡片里的文件时用来解析相对路径。 */
  cwd?: string;
  /**
   * 重试。**只有真正跑着的那条会话才传** —— 侧边聊天、侧边聊天里那段继承来的
   * 上下文都不该往回改。
   *
   * `reply = true` 是回答上的重试：就地重写这条回答（`retry_reply`）；
   * `false` 是提问上的重试：把这句话原样再问一遍（`regenerate`）。
   */
  onRetry?: (turn: number, reply: boolean) => void;
  /**
   * 只保留第 `keepBefore` 轮之前的历史，分叉出一条新会话。`draft` 是提问上的分叉
   * 带过去的那句话，新会话里摆进输入框。
   */
  onFork?: (keepBefore: number, draft?: string) => void;
}) {
  const turns = useMemo(() => mergeAssistantRuns(raw), [raw]);
  // 一圈正在跑着的时候整排都不给改动：重试会插进队列、等这一轮跑完才生效，那时
  // 用户多半已经忘了自己点过它；分叉 / 编辑要关掉当前会话再开新的，正在跑的那一轮
  // 与排队里的消息会跟着没掉 —— 按钮看着只是「分一条出去」，不该有这个副作用。
  const live = !streaming;
  const slots = onRetry && live ? assistantActionSlots(turns) : [];
  const reports = useMemo(() => latestReports(turns), [turns]);

  return (
    <div className="flex flex-col gap-6">
      {turns.map((t, i) => {
        const asks = slots[i];
        const turn = t.turn;
        return (
          <TurnView
            key={i}
            turn={t}
            time={turn !== undefined ? times?.[turn] : undefined}
            isLast={i === turns.length - 1}
            streaming={streaming}
            awaitingUser={awaitingUser}
            showPayload={showPayload}
            cwd={cwd}
            // 没有轮次号（历史里那种极端竞态）就没法指名改哪一轮。
            onRetry={
              onRetry && live && turn !== undefined && (t.role === "user" || asks !== undefined)
                ? () => onRetry(turn, t.role === "assistant")
                : undefined
            }
            // 提问上的重试：这句之后还有回答以外的对话（下一句提问）才要先问一句。
            retryAsks={t.role === "user" ? turns.slice(i + 1).some((x) => x.role === "user") : asks}
            onFork={forkTarget(t, onFork, live)}
            latestReport={reports.has(i)}
          />
        );
      })}
      {/* 消息已发出、助手轮还没开始：和空的助手轮同一个位置，接上时不会跳。 */}
      {streaming && !awaitingUser && turns[turns.length - 1]?.role === "user" && (
        <GeneratingHint label />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 列表 + 贴底滚动
// ---------------------------------------------------------------------------

function MessageList() {
  const stored = useStore((s) => s.turns);
  const times = useStore((s) => s.times);
  const generating = useStore((s) => s.streaming);
  const awaitingUser = useStore((s) => s.pendingForm !== null);
  // 直发的那条先画成气泡（见 `QueuedPrompt.direct`）。`turn_started` 到了换成真的
  // 那条，两者下标相同，React 就地复用，看不出替换。
  const direct = useStore((s) => s.queued.find((q) => q.direct));
  const turns = useMemo<Turn[]>(
    () => (direct ? [...stored, { role: "user", text: direct.text }] : stored),
    [stored, direct],
  );
  const streaming = generating || direct !== undefined;
  const cwd = useStore((s) => s.cwd);
  const sessionId = useStore((s) => s.sessionId);
  const hasEpisodes = useStore((s) => (s.assistant?.episodes.length ?? 0) > 0);
  const scrollRef = useRef<HTMLDivElement>(null);
  // 用户往上翻时不要把他拽回底部 —— 只在「本来就在底部」时跟随。
  const stick = useRef(true);
  // 离底部够远时浮出「回到最新」—— 翻回去看了一眼，不该靠拖滚动条找回来。
  const [away, setAway] = useState(false);

  useEffect(() => {
    const el = scrollRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [turns, streaming]);

  // 换会话：直接落到底部，并复位「跟随」—— 上一条会话里翻到中间的状态不该带过来。
  useEffect(() => {
    stick.current = true;
    setAway(false);
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [sessionId]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
    stick.current = gap < 48;
    setAway(gap > 400);
  };

  const toBottom = () => {
    const el = scrollRef.current;
    if (!el) return;
    stick.current = true;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
    <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto px-4 py-6 [overflow-anchor:none]">
      <div className="mx-auto max-w-3xl">
        {sessionId === ASSISTANT_CHAT_ID && (
          <PastEpisodes
            onGrow={() => {
              const el = scrollRef.current;
              if (el && stick.current) el.scrollTop = el.scrollHeight;
            }}
          />
        )}
        {turns.length === 0 ? (
          sessionId === ASSISTANT_CHAT_ID ? (
            hasEpisodes ? null : <AssistantEmptyState />
          ) : (
            <EmptyState />
          )
        ) : (
          <TurnList
            turns={turns}
            times={times}
            streaming={streaming}
            awaitingUser={awaitingUser}
            cwd={cwd}
            // 重试与分叉都作用在**这条会话**上 —— 侧边聊天那份 TurnList 一概不传。
            onRetry={
              sessionId
                ? (turn, reply) =>
                    reply ? retryReply(sessionId, turn) : regenerate(sessionId, turn)
                : undefined
            }
            onFork={
              sessionId ? (keep, draft) => void fork(sessionId, keep, draft) : undefined
            }
          />
        )}
      </div>
    </div>
      {/* 「回到最新」：贴着输入框上沿居中浮着。生成中且不在底部时带一个点 ——
          告诉人下面还在出字。不用时淡出并让出点击。 */}
      <button
        onClick={toBottom}
        tabIndex={away ? 0 : -1}
        aria-hidden={!away}
        title="回到最新"
        className={`carter-press absolute bottom-3 left-1/2 z-10 flex -translate-x-1/2 items-center gap-1.5 rounded-full bg-elevated px-3 py-1.5 text-xs text-dim shadow-lift ring-1 ring-edge transition-[opacity,translate] duration-200 ease-[var(--ease-out)] hover:text-ink ${
          away ? "translate-y-0 opacity-100" : "pointer-events-none translate-y-2 opacity-0"
        }`}
      >
        {streaming && <span className="size-1.5 animate-pulse rounded-full bg-tone-blue" />}
        <ArrowDown size={12} />
        回到最新
      </button>
    </div>
  );
}

const ASSISTANT_EXAMPLES = [
  "帮我盯着主干 CI，挂了就看看原因，能修就修",
  "有人给我提 PR 时告诉我，先别动手",
  "现在在盯什么？有什么要我处理的？",
];

/** 一开始带出最近几期的原话；更早的点「再往前看」。 */
const EPISODES_SHOWN = 2;

/**
 * 助理对话的往期原话，画在本期上面，每期末尾一条波浪线（游标）。
 *
 * 切期之后模型只看得到纪要与记忆，但人还要能往回翻 —— 原话在往期存档里，
 * 这里只读地画出来，不给重试、分叉这些改历史的入口。
 */
function PastEpisodes({ onGrow }: { onGrow: () => void }) {
  const episodes = useStore((s) => s.assistant?.episodes) ?? [];
  const [shown, setShown] = useState(EPISODES_SHOWN);
  const [loaded, setLoaded] = useState<Record<number, Turn[]>>({});
  const visible = episodes.slice(-shown);
  const missing = visible.filter((e) => !(e.number in loaded)).map((e) => e.number);
  const key = missing.join(",");

  useEffect(() => {
    if (!key) return;
    let alive = true;
    Promise.all(
      missing.map((n) =>
        getAssistantEpisode(n)
          .then((h) => [n, historyToTurns(h)] as const)
          .catch(() => [n, [] as Turn[]] as const),
      ),
    ).then((got) => {
      if (!alive) return;
      setLoaded((p) => ({ ...p, ...Object.fromEntries(got) }));
      onGrow();
    });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  if (episodes.length === 0) return null;
  return (
    <div className="mb-6 flex flex-col gap-6">
      {episodes.length > shown && (
        <button
          onClick={() => setShown((n) => n + EPISODES_SHOWN)}
          className="self-center rounded-md px-2 py-1 text-xs text-dim hover:bg-ink/5 hover:text-ink"
        >
          再往前看（还有 {episodes.length - shown} 期）
        </button>
      )}
      {visible.map((e) => (
        <div key={e.number} className="flex flex-col gap-6">
          {loaded[e.number]?.length ? (
            <div className="opacity-70">
              <TurnList turns={loaded[e.number]} streaming={false} showPayload={false} />
            </div>
          ) : null}
          <CursorLine
            label={`第 ${e.number} 期到此 · 以上不再进上下文 · ${e.summary.split("\n")[0]}`}
            title={`${fullTime(e.started_at)} ~ ${fullTime(e.ended_at)}\n${e.summary}`}
          />
        </div>
      ))}
    </div>
  );
}

/** 助理对话的开场：说清它是什么，给几句能直接发的话。点一下填进输入框。 */
function AssistantEmptyState() {
  const setDraft = useStore((s) => s.setComposerDraft);
  return (
    <div className="flex flex-col items-center py-20 text-center">
      <div className="mb-5 flex size-11 items-center justify-center rounded-xl bg-ink text-[var(--accent-contrast)]">
        <Radar size={22} />
      </div>
      <div className="mb-1.5 text-[22px] font-semibold tracking-tight">卡特助理</div>
      <div className="mb-5 text-sm text-dim">
        告诉我要盯什么、什么事可以替你做。有事我会回到这里找你。
      </div>
      <div className="flex w-full max-w-md flex-col gap-1.5">
        {ASSISTANT_EXAMPLES.map((x) => (
          <button
            key={x}
            onClick={() => setDraft(x)}
            className="rounded-lg border border-edge px-3 py-2 text-left text-sm text-dim transition-colors hover:bg-ink/5 hover:text-ink"
          >
            {x}
          </button>
        ))}
      </div>
    </div>
  );
}

/** 助理对话的顶部条：它是谁、现在什么状态、要人处理几件，开新一期，以及去面板的入口。 */
function AssistantHeader() {
  const a = useStore((s) => s.assistant);
  const setView = useStore((s) => s.setView);
  const [confirmNew, setConfirmNew] = useState(false);
  const running = a?.tickets.filter((t) => t.status === "working").length ?? 0;
  const facts = a
    ? [
        a.settings.paused ? "已暂停" : a.settings.mode === "auto" ? "自动接单" : "只观察",
        `盯着 ${a.sources.filter((s) => s.enabled).length} 个观察源`,
        ...(running > 0 ? [`${running} 张工单在跑`] : []),
      ]
    : [];
  return (
    <div className="flex h-11 shrink-0 items-center gap-2 border-b border-edge px-4">
      <Radar size={14} className="shrink-0 text-tone-blue" />
      <span className="shrink-0 text-sm font-medium text-ink">卡特助理</span>
      <span className="truncate text-xs text-dim">{facts.join(" · ")}</span>
      <button
        onClick={() => setConfirmNew(true)}
        title="之前的对话整理成纪要，不再进上下文"
        className="ml-auto flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-xs text-dim transition-colors hover:bg-ink/5 hover:text-ink"
      >
        <Eraser size={12} />
        开新一期
      </button>
      <button
        onClick={() => setView("assistant")}
        className="shrink-0 rounded-md px-2 py-1 text-xs text-dim transition-colors hover:bg-ink/5 hover:text-ink"
      >
        {a && a.attention > 0 ? `面板 · ${a.attention} 件待处理` : "面板"}
      </button>
      {confirmNew && (
        <ConfirmDialog
          title="开新一期对话"
          confirmLabel="开新一期"
          message={
            <>
              之前的对话整理成一条纪要，你说过的偏好记进长期记忆，原话存进往期存档（面板「往期纪要」里能翻）。
              之后助理只看纪要与记忆，不再带着原话。正在说话的话，等这一轮说完再切。
            </>
          }
          onCancel={() => setConfirmNew(false)}
          onConfirm={() => {
            setConfirmNew(false);
            newAssistantEpisode().catch(toastError);
          }}
        />
      )}
    </div>
  );
}

function EmptyState() {
  const modelRef = useStore((s) => s.modelRef);
  const models = useModelCatalog();
  return (
    <div className="flex flex-col items-center justify-center py-20 text-center">
      <div className="mb-5 flex size-11 items-center justify-center rounded-xl bg-ink text-[var(--accent-contrast)]">
        <Bot size={22} />
      </div>
      <div className="mb-1.5 text-[22px] font-semibold tracking-tight">开始一段对话</div>
      <div className="text-sm text-dim">
        {modelRef ? (
          <>
            当前模型{" "}
            {/* 悬停给出实际请求名与 meta —— 与输入框下方那处同一个写法。 */}
            <span className="mono text-ink" title={modelTitle(modelRef, models)}>
              {displayName(modelRef, models)}
            </span>
          </>
        ) : (
          "发条消息就能开始。"
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 变更 / 待办 / 提示条 / 提问面板 / 输入框
// ---------------------------------------------------------------------------

/**
 * 底栏的待办进度。默认折叠成一行（进度条 + 当前项），点击展开整张清单。
 *
 * **全部完成就自动收起** —— 之前它会一直挂在那里（`todos.length > 0` 就渲染），
 * 一轮做完之后还占着版面，用户分不清那是「还要做」还是「已经做完了」。
 * 清单本身在对话流里有一张 todo_write 卡片（可折叠、可回看），底栏这条只是
 * 「此刻进行到哪」的提示，做完就该让位。
 */
function TodoPanel() {
  const todos = useStore((s) => s.todos);
  const [dismissed, setDismissed] = useState(false);
  // 默认折叠成一行：整张清单常驻底栏太占地方，对话流里那张 todo_write 卡片
  // 才是看全貌的地方，这里点开即可临时展开。
  const [expanded, setExpanded] = useState(false);

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
  // 折叠时标题行只露「此刻在做的那一项」；没有进行中的就露下一项待做的。
  const current =
    todos.find((t) => t.status === "in_progress") ?? todos.find((t) => t.status === "pending");

  return (
    <div className="shrink-0 px-4 pt-2">
      <div className="mx-auto max-w-3xl rounded-xl bg-card px-3 py-1.5">
        <div className="flex items-center gap-2">
          <button
            onClick={() => setExpanded((o) => !o)}
            title={expanded ? "折叠待办" : "展开待办"}
            className="flex min-w-0 flex-1 items-center gap-2 py-0.5 text-left text-xs text-dim transition-colors hover:text-ink"
          >
            <ChevronRight
              size={12}
              className={`shrink-0 transition-transform ${expanded ? "rotate-90" : ""}`}
            />
            <span className="shrink-0 tabular-nums">
              待办 {done}/{todos.length}
            </span>
            <span className="h-1 w-16 shrink-0 overflow-hidden rounded-full bg-ink/[0.08]">
              <span
                className="block h-full rounded-full bg-[var(--ok)] transition-[width] duration-300"
                style={{ width: `${(done / todos.length) * 100}%` }}
              />
            </span>
            {allDone ? (
              <span className="text-[var(--ok)]">已全部完成</span>
            ) : (
              !expanded &&
              current && (
                <span className="flex min-w-0 items-center gap-1.5 text-ink">
                  {current.status === "in_progress" && (
                    <LoaderCircle size={12} className="shrink-0 animate-spin text-tone-blue" />
                  )}
                  <span className="truncate">
                    {current.status === "in_progress" ? current.active_form : current.content}
                  </span>
                </span>
              )
            )}
          </button>
          <IconButton title="关闭待办" onClick={() => setDismissed(true)}>
            <X size={12} />
          </IconButton>
        </div>
        <Collapse open={expanded}>
          {/* 清单很长时限高滚动，别把对话区挤没了。 */}
          <div className="max-h-48 space-y-1 overflow-y-auto pb-1 pl-5 pt-1">
            {todos.map((t, i) => (
              <div key={i} className="flex items-center gap-2 text-xs">
                {t.status === "completed" ? (
                  <CircleCheck size={13} className="carter-pop shrink-0 text-[var(--ok)]" />
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
        </Collapse>
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
            <TriangleAlert size={14} className="icon-optical shrink-0" />
            <span className="whitespace-pre-wrap break-words">{notice}</span>
          </div>
        </Banner>
      </div>
    </div>
  );
}

/**
 * 顶部条：当前会话标题 + 呈现密度 + 打开报文面板的入口。
 *
 * 标题**点一下就能改**：会话名是对话管理里最常改的东西，不该要人回到侧栏、
 * 找到那一行再右键。还没落盘的新对话（列表里没有这一条）不给改 —— 后端没有文件可写。
 */
function ChatHeader() {
  const sessionId = useStore((s) => s.sessionId);
  const sessions = useStore((s) => s.sessions);
  const openInspector = useStore((s) => s.openInspector);
  const menu = useContextMenu();
  const row = sessions.find((s) => s.id === sessionId);
  const title = row?.title;
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const settled = useRef(false);

  if (sessionId === ASSISTANT_CHAT_ID) return <AssistantHeader />;

  const startEdit = () => {
    if (!row || !sessionId) return;
    settled.current = false;
    setDraft(title || "");
    setEditing(true);
  };
  const finish = (commit: boolean) => {
    if (settled.current) return;
    settled.current = true;
    setEditing(false);
    const t = draft.trim();
    if (commit && sessionId && t && t !== title) void rename(sessionId, t);
  };

  const sessionMenu = (): MenuEntry[] =>
    sessionId && row
      ? [
          { label: "重命名", icon: <Pencil size={12} />, onSelect: startEdit },
          {
            label: "复制一份",
            icon: <GitBranch size={12} />,
            onSelect: () => void fork(sessionId),
          },
          "separator",
          {
            label: "导出 Markdown",
            icon: <Share size={12} />,
            onSelect: () => void export_(sessionId, "markdown"),
          },
          {
            label: "导出 JSON",
            icon: <FileText size={12} />,
            onSelect: () => void export_(sessionId, "json"),
          },
          "separator",
          {
            label: "查看 HTTP 报文",
            icon: <ScanSearch size={12} />,
            onSelect: () => openInspector(null),
          },
        ]
      : [];

  return (
    <div className="flex h-11 shrink-0 items-center gap-2 border-b border-edge px-4">
      {editing ? (
        <input
          autoFocus
          value={draft}
          aria-label="会话名称"
          onFocus={(e) => e.currentTarget.select()}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => finish(true)}
          onKeyDown={(e) => {
            if (e.nativeEvent.isComposing) return;
            if (e.key === "Enter") finish(true);
            if (e.key === "Escape") finish(false);
          }}
          className="carter-field -ml-1.5 w-80 max-w-[60%] rounded-md border border-edge-strong bg-elevated px-1.5 py-0.5 text-sm font-medium text-ink outline-none"
        />
      ) : (
        <button
          onClick={startEdit}
          disabled={!row}
          title={row ? "点击重命名" : "发出第一句话后可以重命名"}
          className="group/title -ml-1.5 flex min-w-0 items-center gap-1.5 rounded-md px-1.5 py-0.5 text-left outline-none enabled:hover:bg-ink/[0.045]"
        >
          <span className={`truncate text-sm ${title ? "font-medium text-ink" : "text-dim"}`}>
            {title || "新对话"}
          </span>
          {row && (
            <Pencil
              size={11}
              className="shrink-0 text-dim opacity-0 transition-opacity group-hover/title:opacity-100"
            />
          )}
        </button>
      )}
      <div className="ml-auto flex items-center gap-1">
        <ModePill className="mr-1" />
        <IconButton
          disabled={!sessionId}
          onClick={() => openInspector(null)}
          title="查看本次会话抓到的 HTTP 报文"
        >
          <ScanSearch size={15} />
        </IconButton>
        <IconButton
          disabled={!row}
          title="会话操作"
          aria-label="会话操作"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            menu.open({ x: r.right, y: r.bottom + 4, align: "end" }, sessionMenu());
          }}
        >
          <MoreHorizontal size={15} />
        </IconButton>
      </div>
    </div>
  );
}

function MainChangesPanel() {
  const sessionId = useStore((s) => s.sessionId);
  const turns = useStore((s) => s.turns);
  const changed = useStore((s) => s.changed);
  const since = useStore((s) => s.changesSince);
  const skip = useStore((s) => s.changesSkip);
  const cwd = useStore((s) => s.cwd);
  return (
    <ChangesPanel
      sessionId={sessionId}
      turns={turns}
      changedByTurn={changed}
      since={since}
      skip={skip}
      cwd={cwd}
    />
  );
}

export function ChatView() {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ChatHeader />
      <NoticeBanner />
      <MessageList />
      <MainChangesPanel />
      <TodoPanel />
      {/* 团队待办：贴着输入框，点一条就把它交给这条对话。
          放在 `TodoPanel` 之后 —— 那一块是「这一轮模型自己列的清单」，
          这一块是「团队账本里归我的活」，前者更临时，后者更持久。 */}
      <TeamTodoDock />
      <AskForm />
      {/* 目标条贴着输入框：它和「我要它干什么」是一件事，读起来是一句话。 */}
      <GoalBar />
      <MemorySuggestBar />
      <Composer />
    </div>
  );
}
