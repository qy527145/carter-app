// 「格式化」页：请求或响应**单独**摊成能读的结构化 JSON。
//
// ## 为什么不是一个 `JSON.stringify(record, null, 2)`
//
// 记录里 `request_body` 与 `stream_frames` 都是**字符串**（存的是原始字节），
// 直接序列化整条记录，得到的是一大坨转义过的 `\"`，等于没有格式化。这里把两侧
// 各自解开：
//
// * 请求 —— 请求体本来就是 JSON 对象，排成可折叠的树；
// * 响应 —— 非流式是一整份 JSON，同样成树；流式是一串 SSE 事件，逐事件列出，
//   点开看它 `data` 里的 JSON（与「可视化」页共用同一个事件流浏览器）。
//
// 与可视化页的分工：那一页回答「模型说了什么」（解回 IR 的语义），这一页回答
// 「上游实际收到/发出了什么字节，结构化之后长什么样」。

import type { ExchangeRecord } from "../../lib/api";
import { fmtNumber } from "../../lib/format";
import { EventStream } from "./EventStream";
import { JsonTree, isPlainObject } from "./JsonTree";
import { CopyButton, Direction, HeadersTable, Pill, Section, parseJson, type Side } from "./kit";

/** 一段可能不是 JSON 的正文：能成树就成树，否则原样。 */
function JsonBody({
  text,
  empty = "（空）",
  defaultDepth = 2,
  maxHeight = "max-h-[46vh]",
}: {
  text: string;
  empty?: string;
  defaultDepth?: number;
  maxHeight?: string;
}) {
  const parsed = parseJson(text);
  if (parsed === undefined) {
    return (
      <div className="space-y-1">
        <div className="text-[11px] text-dim">不是 JSON，原样显示（网关的 HTML 错误页等）</div>
        <pre
          className={`${maxHeight} overflow-auto whitespace-pre-wrap break-all rounded bg-base p-2 mono text-[11px] leading-relaxed text-ink`}
        >
          {text.trim() ? text : empty}
        </pre>
      </div>
    );
  }
  if (!isPlainObject(parsed) && !Array.isArray(parsed)) {
    return (
      <pre className="rounded bg-base p-2 mono text-[11px] text-ink">{JSON.stringify(parsed)}</pre>
    );
  }
  return <JsonTree value={parsed} defaultDepth={defaultDepth} className={maxHeight} />;
}

export function FormattedView({ rec, side }: { rec: ExchangeRecord; side: Side }) {
  const streaming = rec.stream_frames.length > 0;
  // 帧是原始字节块，空串拼接才等于上游实际吐出的字节流；用换行拼会凭空插入字符。
  const streamText = rec.stream_frames.join("");

  const statusTone =
    rec.status === null ? undefined : rec.status >= 500 ? "bad" : rec.status >= 400 ? "warn" : "ok";

  if (side === "request") {
    return (
      <div className="space-y-3">
        <Direction
          label="请求"
          // URL 在上方那一行已经有了，这里放「有多大」。
          headline={`${fmtNumber(rec.request_body.length)} 字符`}
          copy={rec.request_body}
          meta={
            <>
              {rec.protocol && <Pill>{rec.protocol}</Pill>}
              {rec.channel && <Pill>{rec.channel}</Pill>}
              {rec.model && <Pill>{rec.model}</Pill>}
            </>
          }
        >
          <JsonBody text={rec.request_body} />
          <Section title="请求头" count={`${rec.request_headers.length} 个`} defaultOpen={false}>
            <HeadersTable headers={rec.request_headers} />
          </Section>
        </Direction>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <Direction
        label="响应"
        headline={
          streaming
            ? `流式 · ${fmtNumber(rec.stream_frames.length)} 个字节块 · ${fmtNumber(streamText.length)} 字符`
            : `${fmtNumber(rec.response_body.length)} 字符`
        }
        copy={(streaming ? streamText : rec.response_body) || undefined}
        meta={
          <>
            {rec.status !== null && <Pill tone={statusTone}>{rec.status}</Pill>}
            <Pill>{streaming ? "流式" : "非流式"}</Pill>
          </>
        }
      >
        {rec.stream_error && (
          <div className="rounded border border-[var(--error-border)] bg-[var(--error-bg)] px-2 py-1.5 text-[11px] text-[var(--danger)]">
            流内错误：{rec.stream_error}
          </div>
        )}
        {streaming ? (
          <EventStream raw={streamText} live={rec.total_ms === null} />
        ) : (
          <JsonBody text={rec.response_body} empty="（还没有响应体）" />
        )}
        <Section title="响应头" count={`${rec.response_headers.length} 个`} defaultOpen={false}>
          <HeadersTable headers={rec.response_headers} />
        </Section>
      </Direction>

      <Section title="整条记录" count="含轮次、耗时、帧数等元信息" defaultOpen={false}>
        <JsonTree value={rec} defaultDepth={2} className="max-h-72" />
      </Section>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 原始
// ---------------------------------------------------------------------------

/**
 * 原始字节。**不做任何改动** —— 不美化、不重排、不修空白。
 *
 * 排障时经常要拿它和上游的返回逐字节对照；格式化一旦动了空白就比较不了。
 * 所以这里只做两件事：按请求 / 响应分开、标清楚字节数。
 */
export function RawView({ rec, side }: { rec: ExchangeRecord; side: Side }) {
  const streaming = rec.stream_frames.length > 0;
  const response = streaming ? rec.stream_frames.join("") : rec.response_body;

  if (side === "request") {
    return (
      <div className="space-y-3">
        <Direction
          label="请求"
          headline={`${fmtNumber(rec.request_body.length)} 字符`}
          copy={rec.request_body}
          meta={<Pill>未改动</Pill>}
        >
          <pre className="max-h-[56vh] overflow-auto whitespace-break-spaces rounded bg-base p-2 mono text-[11px] leading-relaxed text-ink">
            {rec.request_body || "（空）"}
          </pre>
        </Direction>
      </div>
    );
  }

  return (
    <div className="space-y-3">
      <Direction
        label="响应"
        headline={
          streaming
            ? `流式原始字节（${fmtNumber(rec.stream_frames.length)} 块按到达顺序原样拼接）`
            : "响应体"
        }
        copy={response || undefined}
        meta={
          <>
            {rec.status !== null && <Pill>{rec.status}</Pill>}
            <Pill>{fmtNumber(response.length)} 字符</Pill>
            <Pill>未改动</Pill>
          </>
        }
      >
        {rec.stream_error && (
          <div className="rounded border border-[var(--error-border)] bg-[var(--error-bg)] px-2 py-1.5 text-[11px] text-[var(--danger)]">
            流内错误：{rec.stream_error}
          </div>
        )}
        <pre className="max-h-[56vh] overflow-auto whitespace-break-spaces rounded bg-base p-2 mono text-[11px] leading-relaxed text-ink">
          {response || "（空）"}
        </pre>

        {streaming && (
          <Section
            title="字节块边界"
            count={`${rec.stream_frames.length} 块（网络到达的分块，不是 SSE 帧）`}
            defaultOpen={false}
          >
            <div className="space-y-1">
              {rec.stream_frames.map((f, i) => (
                <div key={i} className="flex items-start gap-2">
                  <span className="w-8 shrink-0 pt-1 text-right tabular-nums text-[10px] text-dim">
                    #{i + 1}
                  </span>
                  <pre className="min-w-0 flex-1 overflow-auto whitespace-pre-wrap break-all rounded bg-base p-1.5 mono text-[11px] leading-relaxed text-ink">
                    {f}
                  </pre>
                  <span className="shrink-0 pt-1 text-[10px] text-dim">{fmtNumber(f.length)}</span>
                </div>
              ))}
            </div>
          </Section>
        )}
      </Direction>

      <div className="flex justify-end">
        <CopyButton text={JSON.stringify(rec, null, 2)} title="复制整条记录（JSON）" />
      </div>
    </div>
  );
}