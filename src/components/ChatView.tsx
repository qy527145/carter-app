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
  FileText,
  GitBranch,
  LoaderCircle,
  Pencil,
  RotateCcw,
  ScanSearch,
  Target,
  TriangleAlert,
  Wrench,
  X,
} from "lucide-react";

import { useStore, type Item, type Turn } from "../store";
import { copyText } from "../lib/clipboard";
import { useConversationMode } from "../lib/conversationMode";
import { displayName, modelTitle, useModelCatalog } from "../lib/modelLabel";
import { fork, regenerate, retryReply } from "../lib/sessionOps";
import { TOAST_MS } from "../lib/toast";
import { FilePreviewModal } from "./FilePreview";
import { MarkdownImage, MarkdownLink, MarkdownPre, transformMarkdownUrl } from "./Media";
import { AskForm } from "./AskForm";
import { Composer, MessageImages } from "./Composer";
import { GoalBar } from "./GoalBar";
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
              showCursor={false}
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
  showCursor,
  isLast,
  cwd,
}: {
  item: Item;
  showCursor: boolean;
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
        <div className={`text-sm text-ink ${showCursor ? "carter-bloom" : ""}`}>
          {/* showCursor 只挂在正在生成的那一段上：同一件事也是「这段还在写」的判据，
              内联的 HTML 预览等它写完再挂载，否则每个增量都会把 iframe 重载一次。 */}
          <MarkdownText text={item.text} cwd={cwd} live={showCursor} />
          {showCursor && <span className="carter-cursor">▍</span>}
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
  // 默认摊开：一轮干完，用户最先想看的就是动了哪些文件。但长任务可能一次改几十上百个
  // 文件（跑测试、装依赖），清单本身会把输入框顶到屏幕外，所以标题行就是开关，
  // 展开时也只占一段可滚动的高度。
  const [open, setOpen] = useState(true);
  if (files.length === 0) return null;
  const label: Record<ChangedFile["kind"], string> = {
    write: "写入",
    edit: "修改",
    other: "变更",
  };
  return (
    <div className="overflow-hidden rounded-lg bg-card">
      <button
        onClick={() => setOpen((o) => !o)}
        title={`${files.length} 个文件`}
        className="flex w-full cursor-pointer items-center gap-1.5 px-3 py-2 text-left text-[11px] text-dim transition-colors hover:text-ink"
      >
        <ChevronRight
          size={12}
          className={`shrink-0 transition-transform ${open ? "rotate-90" : ""}`}
        />
        <Pencil size={11} className="shrink-0" />
        <span className="shrink-0">本轮变更了 {files.length} 个文件</span>
        {/* 折起来时标题行要能说明里头是什么，否则一串路径缩成一行等于什么都没说。 */}
        {!open && (
          <span className="mono min-w-0 flex-1 truncate text-dim/70">
            {files
              .slice(0, 3)
              .map((f) => f.path)
              .join(" · ")}
            {files.length > 3 ? " …" : ""}
          </span>
        )}
      </button>
      <Collapse open={open}>
        <ul className="max-h-72 space-y-1 overflow-y-auto border-t border-edge px-3 py-2">
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
      </Collapse>
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
  for (const { start, end } of assistantGroups(turns)) {
    const t = turns[start];
    if (t.role !== "assistant") continue;
    const items = turns
      .slice(start, end + 1)
      .flatMap((x) => (x.role === "assistant" ? x.items : []));
    const watched = t.turn !== undefined ? (changedByTurn[t.turn] ?? []) : [];
    out[end] = changedFiles(items, watched, cwd);
  }
  return out;
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
      className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-dim transition-colors hover:bg-ink/5 hover:text-ink"
    >
      {icon}
      {label}
    </button>
  );
}

/** 操作行的共同外壳：悬停/聚焦时出现。 */
const ACTION_ROW =
  "flex flex-wrap items-center gap-1 opacity-0 transition-opacity duration-150 group-hover/msg:opacity-100 focus-within:opacity-100";

function AssistantTurn({
  turn: t,
  isLast,
  streaming,
  showPayload,
  changed,
  cwd,
  onRetry,
  retryAsks,
  onFork,
}: {
  turn: AssistantTurnT;
  isLast: boolean;
  streaming: boolean;
  showPayload: boolean;
  /** 这一轮用户输入改过的文件，由 `TurnList` 按 `turn` 聚合（见那边的注释）。 */
  changed: ChangedFile[];
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
      {segs
        ? segs.map((seg, i) => {
            const last = i === segs.length - 1;
            if (seg.kind === "process")
              return <ProcessGroup key={i} items={seg.items} cwd={cwd} />;
            // 光标只挂在正在生成的那一段正文上，与详细模式的判断口径一致。
            return (
              <ItemView
                key={i}
                item={seg.item}
                cwd={cwd}
                isLast={last}
                showCursor={streaming && isLast && last && seg.kind === "text"}
              />
            );
          })
        : items.map((item, i) => (
            <ItemView
              key={i}
              item={item}
              cwd={cwd}
              isLast={i === items.length - 1}
              showCursor={streaming && isLast && i === items.length - 1 && item.kind === "text"}
            />
          ))}
      {changed.length > 0 && <ChangedFiles files={changed} cwd={cwd} turn={t.turn} />}
      {/* 回答下面那一行小动作。悬停才浮出来，但一直占着位置 —— 否则鼠标移上来
          的一瞬间整段对话会跳一下。一条动作都没有时整行不渲染（纯工具输出的回答
          不该在末尾留一条空白）。 */}
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
  onRetry,
  retryAsks,
  onFork,
}: {
  turn: Extract<Turn, { role: "user" }>;
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
      <div className={`${ACTION_ROW} mt-1 justify-end`}>
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

function TurnView({
  turn,
  isLast,
  streaming,
  showPayload,
  changed,
  cwd,
  onRetry,
  retryAsks,
  onFork,
}: {
  turn: Turn;
  isLast: boolean;
  streaming: boolean;
  showPayload: boolean;
  changed: ChangedFile[];
  cwd: string;
  onRetry?: () => void;
  retryAsks?: boolean;
  onFork?: () => void;
}) {
  if (turn.role === "user") {
    return <UserTurn turn={turn} onRetry={onRetry} retryAsks={retryAsks} onFork={onFork} />;
  }
  return (
    <AssistantTurn
      turn={turn}
      isLast={isLast}
      streaming={streaming}
      showPayload={showPayload}
      changed={changed}
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
  streaming,
  showPayload = true,
  changedByTurn = {},
  cwd = "",
  onRetry,
  onFork,
}: {
  turns: Turn[];
  streaming: boolean;
  /** 是否给助手轮画「查看报文」入口。侧边聊天传 false（它不抓包）。 */
  showPayload?: boolean;
  /** 后端记下的按轮实际变更（`files_changed` 事件 / `create_session` 的历史）。 */
  changedByTurn?: Record<number, string[]>;
  /** 用来把工具参数里的绝对路径对回监听器给的相对路径。 */
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
  // 挂在「这一轮用户输入的最后一条助手消息」上，而不是每条助手消息各算各的。
  const changed = changedFilesPerTurn(turns, changedByTurn, cwd);
  // 一圈正在跑着的时候整排都不给改动：重试会插进队列、等这一轮跑完才生效，那时
  // 用户多半已经忘了自己点过它；分叉 / 编辑要关掉当前会话再开新的，正在跑的那一轮
  // 与排队里的消息会跟着没掉 —— 按钮看着只是「分一条出去」，不该有这个副作用。
  const live = !streaming;
  const slots = onRetry && live ? assistantActionSlots(turns) : [];

  return (
    <div className="flex flex-col gap-6">
      {turns.map((t, i) => {
        const asks = slots[i];
        const turn = t.turn;
        return (
          <TurnView
            key={i}
            turn={t}
            isLast={i === turns.length - 1}
            streaming={streaming}
            showPayload={showPayload}
            changed={changed[i]}
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
          />
        );
      })}
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
  const sessionId = useStore((s) => s.sessionId);
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
    <div className="border-t border-edge bg-card px-4 py-2">
      <div className="mx-auto flex max-w-3xl items-start gap-2">
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex items-center gap-2 text-[10px] text-dim">
            <span>待办 {done}/{todos.length}</span>
            {allDone && <span className="text-[var(--ok)]">已全部完成</span>}
          </div>
          {todos.map((t, i) => (
            <div key={i} className="flex items-center gap-2 text-xs">
              {t.status === "completed" ? (
                <CircleCheck
                  size={13}
                  className="carter-pop shrink-0 text-[var(--ok)]"
                />
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
            <TriangleAlert size={14} className="icon-optical shrink-0" />
            <span className="whitespace-pre-wrap break-words">{notice}</span>
          </div>
        </Banner>
      </div>
    </div>
  );
}

/** 顶部条：当前会话标题 + 呈现密度 + 打开报文面板的入口。 */
function ChatHeader() {
  const sessionId = useStore((s) => s.sessionId);
  const sessions = useStore((s) => s.sessions);
  const openInspector = useStore((s) => s.openInspector);
  const title = sessions.find((s) => s.id === sessionId)?.title;

  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b border-edge px-4">
      <span className="truncate text-sm text-dim">{title || "新对话"}</span>
      <div className="ml-auto flex items-center gap-2">
        <ModePill />
        <IconButton
          disabled={!sessionId}
          onClick={() => openInspector(null)}
          title="查看本次会话抓到的 HTTP 报文"
        >
          <ScanSearch size={15} />
        </IconButton>
      </div>
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
