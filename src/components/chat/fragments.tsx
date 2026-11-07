import { Brain, ChevronRight, FileText, LoaderCircle, Wrench } from "lucide-react";
import { useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Item, Turn } from "../../store";
import { MarkdownImage, MarkdownLink, MarkdownPre, transformMarkdownUrl } from "../Media";
import { Glyph, ServerCard, ToolCard } from "../toolcards/ToolCard";
import { Collapse } from "../ui/Panel";

type ImageItem = Extract<Item, { kind: "image" }>;
export type AssistantTurnT = Extract<Turn, { role: "assistant" }>;

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

export function ProcessGroup({
  items,
  cwd,
  liveTurn,
}: {
  items: Item[];
  cwd: string;
  liveTurn: boolean;
}) {
  const failed = items.filter(
    (i): i is Extract<Item, { kind: "tool" }> =>
      i.kind === "tool" && i.result !== undefined && !i.result.ok,
  );
  // 只有这一轮真在跑时才算「正在 …」：切走再回来的历史里，最后一个工具没有结果
  // 只是因为那一轮当时被中断了，不是还在执行。
  const running = liveTurn ? runningName(items) : null;
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
          <span className="shrink-0 text-[11px] text-[var(--danger)]">{failed.length} 步失败</span>
        )}
      </button>
      <Collapse open={open}>
        <div className="space-y-2 border-t border-edge px-3 py-2">
          {items.map((it, i) => (
            <ItemView
              // biome-ignore lint/suspicious/noArrayIndexKey: 工具调用条目只追加不重排，内容重复时序号最稳
              key={i}
              item={it}
              cwd={cwd}
              writing={false}
              isLast={i === items.length - 1}
              liveTurn={liveTurn}
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

export function segmentsOf(items: Item[]): Segment[] {
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

export function ItemView({
  item,
  writing,
  isLast,
  liveTurn,
  cwd,
}: {
  item: Item;
  writing: boolean;
  isLast: boolean;
  /** 这一轮还在跑（见 `AssistantTurn` 的 `liveTurn`）。 */
  liveTurn: boolean;
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
      // 只有「这一轮还在跑」时最后那一项才算在跑：切走再回来时历史整段重建，
      // 最后一项若还是张工具卡，`isLast` 一身都会把它画成转圈 —— 而那一轮其实早结束了。
      return <ToolCard item={item} isLast={isLast && liveTurn} cwd={cwd} />;
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
      style={{
        maskImage: WAVE_MASK,
        WebkitMaskImage: WAVE_MASK,
        maskRepeat: "repeat-x",
        WebkitMaskRepeat: "repeat-x",
      }}
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
