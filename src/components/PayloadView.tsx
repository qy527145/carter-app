// 报文的**语义视图** —— 报文面板「可视化」那一页。
//
// ## 为什么不铺原始字节
//
// 一次流式回答抓下来是一百多个网络字节块、几百个 SSE 事件，每片一个字
// （`{"choices":[{"delta":{"content":"好"}}]}`）。摊开看：看不出模型答了什么，
// 只看出传输有多碎 —— 那是传输层的问题，不是用户的问题。
//
// 所以这一页渲染的是后端**解回 IR** 之后的东西（`crates/carter-app-core/src/inspect.rs`）：
// 请求参数、系统提示词、工具清单、对话上下文、回答与思考、token 消耗。
// 原始字节仍然在「原始」页，逐事件也留了一个折叠区 —— 排障时偶尔要用。
//
// ## 与对话区共用一套块渲染
//
// `BlockView` 是同一套（历史消息也是它），所以「对话里这么显示、报文里那么显示」
// 这种自相矛盾不会出现。差别只在上下文：这里 `tool_result` 也成块出现（对话区里
// 它是回填到工具卡片上的）。

import { useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  FileText,
  Lock,
  Wrench,
} from "lucide-react";

import type {
  BlockView,
  ExchangeRecord,
  InspectedExchange,
  InspectedRequest,
  InspectedResponse,
  InspectedTool,
  MessageView,
  StreamStats,
  UsageView,
} from "../lib/api";
import { fmtNumber } from "../lib/format";

// ---------------------------------------------------------------------------
// 基础件
// ---------------------------------------------------------------------------

/** 可折叠分区。默认全开，长内容（系统提示词、工具清单）默认收起。 */
function Section({
  title,
  count,
  defaultOpen = true,
  children,
}: {
  title: string;
  count?: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  return (
    <div className="overflow-hidden rounded-md border border-edge-strong/60">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-xs font-medium text-ink transition-colors hover:bg-ink/5"
      >
        {open ? (
          <ChevronDown size={13} className="shrink-0 text-dim" />
        ) : (
          <ChevronRight size={13} className="shrink-0 text-dim" />
        )}
        <span>{title}</span>
        {count && <span className="font-normal text-dim">{count}</span>}
      </button>
      {open && <div className="space-y-2 px-2.5 pb-2.5">{children}</div>}
    </div>
  );
}

/**
 * 长文本：先给一截，展开看全部。
 *
 * **始终给出真实字数** —— 截断了却不说，读的人会以为"就这么多"。
 */
function LongText({
  text,
  limit = 800,
  className = "",
}: {
  text: string;
  limit?: number;
  className?: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const tooLong = text.length > limit;
  return (
    <div className="space-y-1">
      <pre
        className={`max-h-72 overflow-auto whitespace-pre-wrap break-words rounded bg-base p-2 mono text-[11px] leading-relaxed text-ink ${className}`}
      >
        {expanded || !tooLong ? text : `${text.slice(0, limit)}…`}
      </pre>
      {tooLong && (
        <button
          onClick={() => setExpanded((v) => !v)}
          className="text-[11px] text-dim underline transition-colors hover:text-ink"
        >
          {expanded ? "收起" : `展开全部（共 ${fmtNumber(text.length)} 字）`}
        </button>
      )}
    </div>
  );
}

/**
 * 一段正文：能当 JSON 解析就美化，否则**原样**显示。
 *
 * 非 JSON 的正文是真实存在的（网关的 HTML 错误页、`data: [DONE]` 这种控制帧），
 * 把它们藏起来或强行格式化，都会把"上游到底回了什么"这件事搞丢。
 */
export function JsonOrRaw({ text, label }: { text: string; label?: string }) {
  const pretty = tryPretty(text);
  return (
    <div className="space-y-1">
      {!pretty && label && (
        <div className="text-[11px] text-dim">{label}</div>
      )}
      <pre className="max-h-[50vh] overflow-auto whitespace-pre-wrap break-all rounded bg-base p-2 mono text-[11px] leading-relaxed text-ink">
        {pretty ?? (text.trim() ? text : "（空）")}
      </pre>
    </div>
  );
}

function tryPretty(text: string): string | null {
  if (!text.trim()) return null;
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return null;
  }
}

/** 键少值短的信息：一行一个胶囊，比表格省地方。 */
function Chips({ items }: { items: { key: string; value: string }[] }) {
  if (items.length === 0) return <div className="text-xs text-dim">（无）</div>;
  return (
    <div className="flex flex-wrap gap-1.5">
      {items.map(({ key, value }) => (
        <span
          key={key}
          className="rounded bg-base px-1.5 py-0.5 mono text-[11px]"
        >
          <span className="text-dim">{key}=</span>
          <span className="text-ink">{value}</span>
        </span>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 内容块
// ---------------------------------------------------------------------------

const ROLE_STYLE: Record<string, string> = {
  system: "text-dim",
  user: "text-tone-blue",
  assistant: "text-tone-green",
};

/** `tool_use` / `tool_call`：名字 + 参数。 */
function ToolCallBlock({ name, args }: { name: string; args: string }) {
  // 参数在 IR 里是 JSON 字符串（保住模型原样的分片拼接结果），能美化就美化。
  let body = args;
  try {
    body = JSON.stringify(JSON.parse(args), null, 2);
  } catch {
    /* 不是合法 JSON 就原样显示 —— 那本身就是要看的东西。 */
  }
  return (
    <div className="rounded bg-card p-2">
      <div className="mb-1 flex items-center gap-1.5">
        <Wrench size={12} className="shrink-0 text-tone-blue" />
        <span className="mono text-xs text-ink">{name}</span>
        <span className="text-[11px] text-dim">调用参数</span>
      </div>
      <LongText text={body} limit={400} />
    </div>
  );
}

/**
 * 按块类型渲染。
 *
 * ⚠️ 判类型一律用 `kind`，**不要**用 `"text" in block` —— 思考块也带 `text`
 * 属性（后端那个 `BlockView` 就是这么定义的），用 `in` 判断会把思考内容当成
 * 回答拼进去。
 */
function BlockRenderer({ block }: { block: BlockView }) {
  switch (block.kind) {
    case "text":
      return <LongText text={block.text} />;

    case "thinking":
      return (
        <div className="border-l-2 border-dashed border-edge pl-2">
          <div className="mb-1 text-[11px] text-dim">
            思考内容{block.has_signature ? "（带签名）" : ""}
          </div>
          <LongText text={block.text} limit={400} className="italic" />
        </div>
      );

    case "tool_call":
      return <ToolCallBlock name={block.name} args={block.args} />;

    case "tool_result":
      return (
        <div className="rounded bg-card p-2">
          <div className="mb-1 flex items-center gap-1.5 text-[11px]">
            <span className="mono text-dim">结果 → {block.tool_use_id}</span>
            {block.is_error && (
              <span className="rounded bg-[var(--error-bg)] px-1 text-[10px] text-[var(--danger)]">
                出错
              </span>
            )}
          </div>
          <LongText text={block.text} limit={400} />
        </div>
      );

    case "server_activity":
      return (
        <div className="rounded border border-dashed border-edge bg-ink/[0.03] p-2">
          <div className="flex items-center gap-1.5">
            <span className="mono text-[11px] text-ink">{block.raw_type}</span>
            <span className="rounded bg-bubble px-1 text-[10px] text-dim">
              服务端活动
            </span>
          </div>
          {block.summary && <div className="mt-1 text-[11px] text-dim">{block.summary}</div>}
        </div>
      );

    case "image":
      return (
        <div className="flex items-center gap-1.5 text-[11px] text-dim">
          <FileText size={12} />
          <span className="mono">{block.media_type}</span>
          {block.hash && <span>#{block.hash.slice(0, 12)}</span>}
        </div>
      );
  }
}

/** 一段内容块的列表。响应里的块按**事件流顺序**排：想 → 答 → 决定调工具。 */
function BlockList({ blocks }: { blocks: BlockView[] }) {
  if (blocks.length === 0) {
    return <div className="text-xs text-dim">（这一轮没有输出任何内容块）</div>;
  }
  return (
    <div className="space-y-2">
      {blocks.map((b, i) => (
        <BlockRenderer key={i} block={b} />
      ))}
    </div>
  );
}

/** 对话上下文里的一条消息。收起时给一行摘要 —— 一屏十条带工具结果的消息没法读。 */
function MessageCard({ msg, index }: { msg: MessageView; index: number }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="overflow-hidden rounded border border-edge-strong/60">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-start gap-2 px-2 py-1.5 text-left transition-colors hover:bg-ink/5"
      >
        {open ? (
          <ChevronDown size={13} className="icon-optical shrink-0 text-dim" />
        ) : (
          <ChevronRight size={13} className="icon-optical shrink-0 text-dim" />
        )}
        <span className="w-6 shrink-0 mono text-[11px] text-dim">#{index}</span>
        <span className={`w-16 shrink-0 mono text-xs ${ROLE_STYLE[msg.role] ?? ""}`}>
          {msg.role}
        </span>
        {!open && (
          <span className="min-w-0 flex-1 truncate mono text-[11px] text-dim">
            {summarize(msg)}
          </span>
        )}
      </button>
      {open && (
        <div className="space-y-2 border-t border-edge px-2 py-2">
          <BlockList blocks={msg.blocks} />
        </div>
      )}
    </div>
  );
}

/** 收起状态下的一行摘要：先给文本，没有文本就说清有几块什么。 */
function summarize(msg: MessageView): string {
  const text = msg.blocks
    .filter((b): b is Extract<BlockView, { kind: "text" }> => b.kind === "text")
    .map((b) => b.text)
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
  if (text) return text.slice(0, 120);

  const counts: string[] = [];
  const n = (kind: BlockView["kind"], label: string) => {
    const c = msg.blocks.filter((b) => b.kind === kind).length;
    if (c > 0) counts.push(`${c} ${label}`);
  };
  n("thinking", "思考");
  n("tool_call", "工具调用");
  n("tool_result", "工具结果");
  n("server_activity", "服务端活动");
  n("image", "图片");
  return counts.join("、") || "（空消息）";
}

// ---------------------------------------------------------------------------
// 请求
// ---------------------------------------------------------------------------

function ToolCard({ tool }: { tool: InspectedTool }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="overflow-hidden rounded border border-edge-strong/60">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 px-2 py-1.5 text-left transition-colors hover:bg-ink/5"
      >
        {open ? (
          <ChevronDown size={13} className="shrink-0 text-dim" />
        ) : (
          <ChevronRight size={13} className="shrink-0 text-dim" />
        )}
        <Wrench size={12} className="shrink-0 text-tone-blue" />
        <span className="mono text-xs text-ink">{tool.name}</span>
        {tool.server_side && (
          <span className="rounded bg-bubble px-1 text-[10px] text-dim">
            厂商执行
          </span>
        )}
      </button>
      {open && (
        <div className="space-y-2 border-t border-edge px-2 py-2">
          {tool.description && (
            <p className="text-[11px] leading-relaxed text-dim">{tool.description}</p>
          )}
          <LongText text={tool.schema} limit={400} />
        </div>
      )}
    </div>
  );
}

function RequestBlocks({ req }: { req: InspectedRequest }) {
  return (
    <div className="space-y-2">
      <Section title="请求参数" count={`${req.params.length} 项`}>
        <Chips items={req.params} />
      </Section>

      {req.system && (
        <Section title="系统提示词" count={`${fmtNumber(req.system.length)} 字`} defaultOpen={false}>
          <LongText text={req.system} />
        </Section>
      )}

      <Section
        title="工具"
        count={`${req.tools.length} 个`}
        defaultOpen={req.tools.length > 0 && req.tools.length <= 8}
      >
        {req.tools.length === 0 ? (
          <div className="text-xs text-dim">这次请求没带工具（模型调用不了任何工具）。</div>
        ) : (
          req.tools.map((t) => <ToolCard key={t.name} tool={t} />)
        )}
      </Section>

      <Section title="对话上下文" count={`${req.messages.length} 条`}>
        <div className="space-y-1.5">
          {req.messages.map((m, i) => (
            <MessageCard key={i} msg={m} index={i} />
          ))}
        </div>
      </Section>

      {req.extra_keys.length > 0 && (
        <Section title="其它字段" count={`${req.extra_keys.length} 个（协议原生、未建模）`} defaultOpen={false}>
          <div className="flex flex-wrap gap-1.5">
            {req.extra_keys.map((k) => (
              <span key={k} className="rounded bg-base px-1.5 py-0.5 mono text-[11px] text-ink">
                {k}
              </span>
            ))}
          </div>
          <div className="text-[11px] text-dim">
            值和整份请求体在「格式化」页里看 —— 这里只说明还有别的字段。
          </div>
        </Section>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 响应
// ---------------------------------------------------------------------------

/** token 明细。缓存单独列 —— 它是省钱与否的关键。 */
function UsagePanel({ usage }: { usage: UsageView }) {
  const totalIn = usage.input_tokens + usage.cache_read_tokens;
  const hitRate = totalIn > 0 ? usage.cache_read_tokens / totalIn : 0;
  const rows: [string, number, string?][] = [
    ["输入（未命中缓存）", usage.input_tokens],
    ["输入（缓存命中）", usage.cache_read_tokens, "按更低的倍率计费"],
    ["缓存写入", usage.cache_write_tokens],
    ["输出", usage.output_tokens],
    ["思考", usage.thinking_tokens, "含在输出里"],
  ];
  return (
    <div className="space-y-2">
      <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs sm:grid-cols-3">
        {rows.map(([label, value, hint]) => (
          <div key={label} className="flex items-center justify-between gap-2">
            <span className="text-dim" title={hint}>
              {label}
            </span>
            <span className="tabular-nums text-ink">{fmtNumber(value)}</span>
          </div>
        ))}
      </div>
      {totalIn > 0 && (
        <div className="space-y-1">
          <div className="flex justify-between text-[11px] text-dim">
            <span>输入里命中缓存的比例</span>
            <span className="tabular-nums">
              {usage.cache_read_tokens} / {totalIn}（{(hitRate * 100).toFixed(1)}%）
            </span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-edge">
            <div
              className="h-full bg-[var(--ok)]"
              style={{ width: `${Math.min(100, hitRate * 100)}%` }}
            />
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * 流式有多碎。
 *
 * 只给统计，**不铺事件** —— 事件本身在下面那个折叠区里。这一行存在的意义是
 * 回答「上游吐了多少次」，那是判断"卡不卡"的线索之一。
 */
function StreamSummary({ stats }: { stats: StreamStats }) {
  if (!stats.streaming) return <div className="text-[11px] text-dim">非流式响应。</div>;
  return (
    <div className="flex flex-wrap items-center gap-2 text-[11px] text-dim">
      <span className="tabular-nums">{stats.events} 个 SSE 事件</span>
      <span className="tabular-nums">· {stats.frames} 个网络字节块</span>
      {stats.event_kinds.slice(0, 4).map((k) => (
        <span key={k.name} className="rounded bg-bubble px-1 mono">
          {k.name} × {k.count}
        </span>
      ))}
    </div>
  );
}

/** SSE 事件：事件名 + `data:` 内容 + 这一片真正新增的内容（看得见"每片一个字"这件事）。 */
type SseEvent = { name: string; data: string; fragment: string | null };

/**
 * 把原始 SSE 正文切成事件。
 *
 * 按**空行**切，而不是引入完整解析器：SSE 规范里事件本来就是空行分隔的。这里只做
 * 最小识别 —— 解析器会顺手丢掉注释行（`: keep-alive` 心跳）和不认识的字段，
 * 而那些恰恰是排障时要看的。
 *
 * `fragment` 只认几种最常见的位置（Anthropic 的 `delta.text` / `delta.thinking`、
 * OpenAI 的 `choices[0].delta.content`、Responses 的顶层 `delta`）—— 认不出就
 * 不硬猜，下面的 JSON 照样能看到全部字段。
 */
function parseSseEvents(raw: string): SseEvent[] {
  return raw
    .split(/\r?\n\r?\n/)
    .filter((block) => block.trim() !== "")
    .map((block) => {
      let name = "message";
      const data: string[] = [];
      for (const line of block.split(/\r?\n/)) {
        if (line.startsWith(":")) continue; // 注释 / 心跳
        const idx = line.indexOf(":");
        const field = idx === -1 ? line : line.slice(0, idx);
        let value = idx === -1 ? "" : line.slice(idx + 1);
        if (value.startsWith(" ")) value = value.slice(1);
        if (field === "event") name = value;
        else if (field === "data") data.push(value);
      }
      const joined = data.join("\n");
      return { name, data: joined, fragment: fragmentOf(joined) };
    });
}

function fragmentOf(data: string): string | null {
  let v: unknown;
  try {
    v = JSON.parse(data);
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null) return null;
  const obj = v as Record<string, unknown>;
  const delta = obj.delta as Record<string, unknown> | undefined;
  const fromDelta =
    str(delta?.text) ?? str(delta?.thinking) ?? str(delta?.partial_json) ?? str(obj.delta);
  if (fromDelta) return fromDelta;
  const choices = Array.isArray(obj.choices) ? obj.choices : [];
  const first = choices[0] as Record<string, unknown> | undefined;
  const d = first?.delta as Record<string, unknown> | undefined;
  return str(d?.content) ?? str(d?.reasoning_content) ?? null;
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}

/** 一条 SSE 事件的小标题行：序号 + 事件名 + 这一片新增的内容。 */
function EventHeader({ index, name, fragment }: { index: number; name: string; fragment: string | null }) {
  return (
    <div className="flex items-center gap-2 border-b border-edge bg-base px-2 py-1">
      <span className="w-8 shrink-0 tabular-nums text-[10px] text-dim">{index}</span>
      <span className="mono shrink-0 text-[11px] text-tone-blue">{name}</span>
      {fragment && <span className="min-w-0 flex-1 truncate text-[11px] text-dim">{fragment}</span>}
    </div>
  );
}

/**
 * 一下最多铺多少个事件。
 *
 * 一个事件一个 DOM 块，而流式回答**一个 token 一个事件**（DeepSeek 实测：
 * 一次 200 字的回答 191 个事件）。几千个事件一起挂上去，界面会卡到不能滚动 ——
 * 那不是"信息更全"，是这一页不能用了。超出先只铺前 [EVENT_LIMIT] 个，
 * 剩下的给一个明说的入口，用户点了才铺。
 */
const EVENT_LIMIT = 200;

/** 流式响应：逐个 SSE 事件排开，每个事件的 JSON 单独美化。 */
function SseEvents({ raw }: { raw: string }) {
  const [all, setAll] = useState(false);
  const events = parseSseEvents(raw);
  if (events.length === 0) {
    // 切不出事件（上游吐的不是 SSE，或者只吐了半截）：原样给，别谎称"没有内容"。
    return <JsonOrRaw text={raw} label="正文切不出 SSE 事件，原样显示" />;
  }
  const shown = all ? events : events.slice(0, EVENT_LIMIT);
  return (
    <div className="space-y-2">
      {shown.map((ev, i) => (
        <div key={i} className="overflow-hidden rounded border border-edge-strong/60">
          <EventHeader index={i + 1} name={ev.name} fragment={ev.fragment} />
          <pre className="max-h-60 overflow-auto whitespace-pre-wrap break-all p-2 mono text-[11px] leading-relaxed text-ink">
            {tryPretty(ev.data) ?? ev.data}
          </pre>
        </div>
      ))}
      {!all && events.length > EVENT_LIMIT && (
        <button
          onClick={() => setAll(true)}
          className="w-full rounded border border-dashed border-edge py-1.5 text-[11px] text-dim transition-colors hover:text-ink"
        >
          还有 {events.length - EVENT_LIMIT} 个事件没铺开 · 点这里全部展开（很长，可能卡）
        </button>
      )}
    </div>
  );
}

function ResponseBlocks({
  resp,
  stats,
  raw,
}: {
  resp: InspectedResponse | null;
  stats: StreamStats;
  raw: string;
}) {
  return (
    <div className="space-y-2">
      <Section
        title="响应内容"
        count={resp?.finish ?? undefined}
        defaultOpen
      >
        {resp ? <BlockList blocks={resp.blocks} /> : <div className="text-xs text-dim">（没有解析出内容）</div>}
        <StreamSummary stats={stats} />
        {resp && resp.usage && (
          <div className="rounded bg-base p-2">
            <div className="mb-1.5 text-[11px] font-medium text-dim">Token 消耗</div>
            <UsagePanel usage={resp.usage} />
          </div>
        )}
      </Section>

      {stats.streaming && raw && (
        <Section title="事件流" count={`${stats.events} 个事件（逐条展开）`} defaultOpen={false}>
          <div className="flex items-center gap-1.5 text-[11px] text-dim">
            <Lock size={11} />
            这里是上游真正发出的每一个事件（与「格式化」页同一份渲染），正常看报文不必展开。
          </div>
          <SseEvents raw={raw} />
        </Section>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/** 概要一行：谁、用什么协议、什么状态、多快。 */
function Summary({
  inspected,
  method,
  url,
  status,
  ttfbMs,
  totalMs,
}: {
  inspected: InspectedExchange;
  method: string;
  url: string;
  status: number | null;
  ttfbMs: number | null;
  totalMs: number | null;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className="rounded bg-bubble px-1.5 py-0.5 mono text-[11px] text-ink">
          {method}
        </span>
        <span className="break-all mono text-xs text-ink">{url}</span>
      </div>
      <Chips
        items={[
          { key: "协议", value: inspected.protocol || "未知" },
          ...(inspected.protocol_guessed ? [{ key: "注意", value: "协议按 URL 猜的" }] : []),
          ...(inspected.request
            ? [
                { key: "模型", value: inspected.request.model },
                { key: "stream", value: String(inspected.request.stream) },
              ]
            : []),
          ...(status !== null ? [{ key: "状态", value: String(status) }] : []),
          ...(ttfbMs !== null ? [{ key: "首字节", value: `${ttfbMs} ms` }] : []),
          ...(totalMs !== null ? [{ key: "总耗时", value: `${totalMs} ms` }] : []),
        ]}
      />
    </div>
  );
}

export function PayloadView({
  inspected,
  method,
  url,
  status,
  ttfbMs,
  totalMs,
  streamText,
  headers,
}: {
  inspected: InspectedExchange;
  method: string;
  url: string;
  status: number | null;
  ttfbMs: number | null;
  totalMs: number | null;
  /** 流式响应的原始字节（拼好的），给「事件流」折叠区用。 */
  streamText: string;
  headers: { request: [string, string][]; response: [string, string][] };
}) {
  return (
    <div className="space-y-3">
      <Summary
        inspected={inspected}
        method={method}
        url={url}
        status={status}
        ttfbMs={ttfbMs}
        totalMs={totalMs}
      />

      {inspected.decode_error && (
        <div className="rounded-md border border-[var(--warn-border)] bg-[var(--warn-bg)] px-2 py-1.5 text-xs text-tone-orange">
          {inspected.decode_error}
          <div className="mt-1 text-[11px] text-dim">
            完整字节在「原始」页 —— 那里不做任何解释，能看到上游实际发了什么。
          </div>
        </div>
      )}

      {inspected.request && <RequestBlocks req={inspected.request} />}
      <ResponseBlocks resp={inspected.response} stats={inspected.stream} raw={streamText} />

      <Section title="请求头" count={`${headers.request.length} 个`} defaultOpen={false}>
        <HeadersTable headers={headers.request} />
      </Section>
      <Section title="响应头" count={`${headers.response.length} 个`} defaultOpen={false}>
        <HeadersTable headers={headers.response} />
      </Section>
    </div>
  );
}

/** 头表。敏感头已在后端脱敏（`authorization` → `***`），这里不做二次处理。 */
export function HeadersTable({ headers }: { headers: [string, string][] }) {
  if (headers.length === 0) return <div className="text-xs text-dim">（无）</div>;
  return (
    <div className="overflow-hidden rounded border border-edge-strong/60">
      <table className="w-full border-collapse text-xs">
        <tbody>
          {headers.map(([k, v], i) => (
            <tr key={i} className={i % 2 ? "bg-base" : ""}>
              <td className="w-40 border-r border-edge px-2 py-1 align-top mono text-dim">
                {k}
              </td>
              <td className="break-all px-2 py-1 mono text-ink">{v}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/**
 * 「格式化」页：请求与响应**分开**，各自摊成能读的 JSON。
 *
 * ## 为什么不是一个 `JSON.stringify(record)`
 *
 * 记录里 `request_body` 与 `stream_frames` 都是**字符串**（存的是原始字节），
 * 直接序列化整条记录，得到的是一大坨转义过的 `\"`，等于没有格式化。
 * 这里把两侧各自解开：
 *
 * * 请求 —— 请求体本来就是 json 对象，直接美化；
 * * 响应 —— 流式是**一串 SSE 事件**，逐个事件、逐块 JSON 地排开（事件之间靠外框
 *   与序号对齐）；非流式则是一整份 JSON，美化后直出。
 *
 * 与可视化页的分工：那一页回答「模型说了什么」（解回 IR 的语义），这一页回答
 * 「上游实际收到/发出了什么字节，结构化之后长什么样」。
 */
export function FormattedView({ rec }: { rec: ExchangeRecord }) {
  const streaming = rec.stream_frames.length > 0;
  // 帧是原始字节块，空串拼接才等于上游实际吐出的字节流；用换行拼会凭空插入字符。
  const streamText = rec.stream_frames.join("");

  return (
    <div className="space-y-4">
      <section className="space-y-1.5">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="text-xs font-semibold text-dim">请求</h3>
          <span className="rounded bg-bubble px-1.5 py-0.5 mono text-[11px] text-ink">
            {rec.method}
          </span>
          <span className="break-all mono text-[11px] text-dim">{rec.url}</span>
        </div>
        <HeadersTable headers={rec.request_headers} />
        <JsonOrRaw text={rec.request_body} />
      </section>

      <section className="space-y-1.5">
        <div className="flex items-center gap-2">
          <h3 className="text-xs font-semibold text-dim">响应</h3>
          <span className="mono text-[11px] text-ink">{rec.status ?? "—"}</span>
          {streaming && (
            <span className="text-[11px] text-dim">
              流式 · {parseSseEvents(streamText).length} 个 SSE 事件
            </span>
          )}
        </div>
        <HeadersTable headers={rec.response_headers} />
        {rec.stream_error && (
          <div className="rounded border border-[var(--error-border)] bg-[var(--error-bg)] px-2 py-1.5 text-[11px] text-[var(--danger)]">
            流内错误：{rec.stream_error}
          </div>
        )}
        {streaming ? (
          <SseEvents raw={streamText} />
        ) : (
          <JsonOrRaw text={rec.response_body} label="不是 JSON 就原样显示（网关的 HTML 错误页等）" />
        )}
      </section>

      <Section title="整条记录" count="含轮次、耗时、帧数等元信息" defaultOpen={false}>
        <JsonOrRaw text={JSON.stringify(rec, null, 2)} />
      </Section>
    </div>
  );
}
