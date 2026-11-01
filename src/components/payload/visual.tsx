// 「可视化」页：把一次交换的字节**解回 IR** 之后按语义渲染。
//
// 一次**只画一侧**：`side` 说画请求还是响应。这是「请求 / 响应」页签在视图里的
// 落点 —— 请求常常几万字（系统提示词 + 工具 + 整段历史），响应又可能很长，
// 两块上下堆在一起，看响应要先滚过整个请求。分成两页各自满屏，切换是一次换向。
//
// 数据来自后端的 `inspect_exchange`（`crates/carter-app-core/src/inspect.rs`），
// 那里用的是与真实链路同一套 codec，所以这里显示的就是模型实际收到/说出的内容。
// 解不开时不白屏：把原因顶出来并指向「原始」页。

import { Lock } from "lucide-react";

import type {
  BlockView,
  InspectedExchange,
  InspectedRequest,
  InspectedResponse,
  StreamStats,
  UsageView,
} from "../../lib/api";
import { fmtMs, fmtNumber } from "../../lib/format";
import { BlockList, MessageCard, ToolCard } from "./blocks";
import { EventStream } from "./EventStream";
import {
  Chips,
  CopyButton,
  Direction,
  HeadersTable,
  LongText,
  Pill,
  Section,
  type Side,
} from "./kit";

// ---------------------------------------------------------------------------
// 请求
// ---------------------------------------------------------------------------

function RequestBlocks({ req }: { req: InspectedRequest }) {
  return (
    <div className="space-y-2">
      <Section title="请求参数" count={`${req.params.length} 项`}>
        <Chips items={req.params} />
      </Section>

      {req.system && (
        <Section
          title="系统提示词"
          count={`${fmtNumber(req.system.length)} 字`}
          defaultOpen={false}
          action={<CopyButton text={req.system} title="复制系统提示词" />}
        >
          <LongText text={req.system} limit={2000} />
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

      <Section
        title="对话上下文"
        count={`${req.messages.length} 条`}
        // 上下文往往是整个请求里最长的一段；默认收起时至少给个头尾。
        defaultOpen={req.messages.length <= 6}
      >
        <div className="space-y-1.5">
          {req.messages.map((m, i) => (
            <MessageCard key={i} msg={m} index={i} />
          ))}
        </div>
      </Section>

      {req.extra_keys.length > 0 && (
        <Section
          title="其它字段"
          count={`${req.extra_keys.length} 个（协议原生、未建模）`}
          defaultOpen={false}
        >
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
 * 只给统计，**不铺事件** —— 事件在下面那个「事件流」区里，按需点开。
 * 这一行存在的意义是回答「上游吐了多少次」，那是判断"卡不卡"的线索之一。
 */
function StreamSummary({ stats }: { stats: StreamStats }) {
  if (!stats.streaming) return <div className="text-[11px] text-dim">非流式响应。</div>;
  return (
    <div className="flex flex-wrap items-center gap-2 text-[11px] text-dim">
      <span className="tabular-nums">{fmtNumber(stats.events)} 个 SSE 事件</span>
      <span className="tabular-nums">· {fmtNumber(stats.frames)} 个网络字节块</span>
      {stats.event_kinds.slice(0, 4).map((k) => (
        <span key={k.name} className="rounded bg-bubble px-1 mono">
          {k.name} × {fmtNumber(k.count)}
        </span>
      ))}
    </div>
  );
}

function ResponseBlocks({
  resp,
  stats,
  raw,
  live,
}: {
  resp: InspectedResponse | null;
  stats: StreamStats;
  raw: string;
  live: boolean;
}) {
  const answer = (resp?.blocks ?? [])
    .filter((b): b is Extract<BlockView, { kind: "text" }> => b.kind === "text")
    .map((b) => b.text)
    .join("\n\n")
    .trim();

  return (
    <div className="space-y-2">
      <Section
        title="响应内容"
        count={resp?.finish ?? undefined}
        action={answer ? <CopyButton text={answer} title="复制回答正文" /> : undefined}
      >
        {resp ? (
          <BlockList blocks={resp.blocks} />
        ) : (
          <div className="text-xs text-dim">（没有解析出内容）</div>
        )}
        <StreamSummary stats={stats} />
        {resp && resp.usage && (
          <div className="rounded bg-base p-2">
            <div className="mb-1.5 text-[11px] font-medium text-dim">Token 消耗</div>
            <UsagePanel usage={resp.usage} />
          </div>
        )}
      </Section>

      {raw && (
        <Section
          title="事件流"
          count={`${fmtNumber(stats.events)} 个事件（按需查看）`}
          defaultOpen={false}
        >
          <div className="flex items-center gap-1.5 text-[11px] text-dim">
            <Lock size={11} />
            上游真正发出的每一个事件。左边挑一个，右边看它发了什么 —— 正常看报文不必展开。
          </div>
          <EventStream raw={raw} live={live} />
        </Section>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

export type VisualProps = {
  inspected: InspectedExchange | null;
  /** 看哪一半。与「请求 / 响应」页签一致，三个视图共用同一个取值。 */
  side: Side;
  status: number | null;
  ttfbMs: number | null;
  totalMs: number | null;
  /** 流式响应的原始字节（拼好的）。 */
  streamText: string;
  headers: { request: [string, string][]; response: [string, string][] };
  requestBody: string;
  responseBody: string;
  streamError: string | null;
  /** 还在这条请求还在跑。 */
  live: boolean;
};

export function VisualView({
  inspected,
  side,
  status,
  ttfbMs,
  totalMs,
  streamText,
  headers,
  requestBody,
  responseBody,
  streamError,
  live,
}: VisualProps) {
  if (!inspected) {
    return (
      <div className="flex items-center gap-2 py-6 text-xs text-dim">
        <span className="carter-dots">
          <span />
          <span />
          <span />
        </span>
        正在解读报文…
      </div>
    );
  }

  const statusTone = status === null ? undefined : status >= 500 ? "bad" : status >= 400 ? "warn" : "ok";
  const respRaw = streamText || responseBody;

  // 解不开时只对**解不出来的那一侧**提示：上游请求体常常不是它自己的协议
  // （比如 Anthropic 渠道上的探测请求），请求解开了却把响应也指向「原始」页
  // 就白吓人一跳。
  const decodeError = (missing: boolean) =>
    missing && inspected.decode_error ? (
      <div className="rounded-md border border-[var(--warn-border)] bg-[var(--warn-bg)] px-2 py-1.5 text-xs text-tone-orange">
        {inspected.decode_error}
        <div className="mt-1 text-[11px] text-dim">
          完整字节在「原始」页 —— 那里不做任何解释，能看到上游实际发了什么。
        </div>
      </div>
    ) : null;

  if (side === "request") {
    return (
      <div className="space-y-3">
        {decodeError(!inspected.request)}
        <Direction
          label="请求"
          // URL 已经在详情页顶部一行里了，这里不重复，换成「有多大」。
          headline={`${fmtNumber(requestBody.length)} 字符`}
          copy={requestBody}
          meta={
            <>
              <Pill>{inspected.protocol || "未知协议"}</Pill>
              {inspected.protocol_guessed && <Pill tone="warn">协议按 URL 猜的</Pill>}
              {inspected.request && <Pill>{inspected.request.model}</Pill>}
              {inspected.request && <Pill>{inspected.request.stream ? "流式" : "非流式"}</Pill>}
            </>
          }
        >
          {inspected.request ? (
            <RequestBlocks req={inspected.request} />
          ) : (
            <div className="text-xs text-dim">（请求体没解出来）</div>
          )}
          <Section title="请求头" count={`${headers.request.length} 个`} defaultOpen={false}>
            <HeadersTable headers={headers.request} />
          </Section>
        </Direction>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {/* 进行中解不出响应是正常的（还没收到），不该报错。 */}
      {!live && decodeError(!inspected.response)}
      {streamError && (
        <div className="rounded-md border border-[var(--error-border)] bg-[var(--error-bg)] px-2 py-1.5 text-xs text-[var(--danger)]">
          流内错误：{streamError}
        </div>
      )}

      <Direction
        label="响应"
        headline={respRaw ? `${fmtNumber(respRaw.length)} 字符${streamText ? "（流式）" : ""}` : "还没有响应"}
        copy={respRaw || undefined}
        meta={
          <>
            {status !== null && <Pill tone={statusTone}>{status}</Pill>}
            {ttfbMs !== null && <Pill>首字节 {fmtMs(ttfbMs)}</Pill>}
            <Pill>{totalMs !== null ? `总耗时 ${fmtMs(totalMs)}` : "进行中"}</Pill>
          </>
        }
      >
        <ResponseBlocks
          resp={inspected.response}
          stats={inspected.stream}
          raw={streamText}
          live={live}
        />
        {headers.response.length > 0 && (
          <Section title="响应头" count={`${headers.response.length} 个`} defaultOpen={false}>
            <HeadersTable headers={headers.response} />
          </Section>
        )}
      </Direction>
    </div>
  );
}