// 语义块渲染：内容块、对话消息、工具清单。
//
// 这些块在「可视化」页出现两处 —— 请求的对话上下文里、响应的输出里。同一个
// `BlockView` 两处长一样，是这一层的意义：切到请求还是响应，看到的都是同一套语法。
//
// 与对话区的关系：对话区（`ChatView`）渲染的是 `Item`，这里是 `BlockView`。两者
// 形状不同（报文里 `tool_result` 单独成块），所以不能直接复用组件；但它们讲的
// 是同一批概念，配色与措辞保持一致。

import { useState } from "react";
import { ChevronDown, ChevronRight, FileText, Wrench } from "lucide-react";

import type { BlockView, InspectedTool, MessageView } from "../../lib/api";
import { LongText } from "./kit";

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
          {block.text.trim() ? (
            <LongText text={block.text} limit={400} className="italic" />
          ) : (
            <div className="text-[11px] text-dim">只有签名，内容未公开</div>
          )}
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
            <span className="rounded bg-bubble px-1 text-[10px] text-dim">服务端活动</span>
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
export function BlockList({ blocks }: { blocks: BlockView[] }) {
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

/**
 * 对话上下文里的一条消息。
 *
 * 收起时给一行摘要（长文本前 120 字，或块数统计）—— 一屏十条带工具结果的消息
 * 每条都摊开是没法读的。
 */
export function MessageCard({
  msg,
  index,
  defaultOpen = false,
}: {
  msg: MessageView;
  index: number;
  defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
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

/** 一个工具：名字 + 描述 + JSON Schema。 */
export function ToolCard({ tool }: { tool: InspectedTool }) {
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
          <span className="rounded bg-bubble px-1 text-[10px] text-dim">厂商执行</span>
        )}
        {!open && tool.description && (
          <span className="min-w-0 flex-1 truncate text-[11px] text-dim">
            {tool.description}
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

