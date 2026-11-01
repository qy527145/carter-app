// 流式响应的**事件流浏览器**。
//
// ## 为什么不铺一屏
//
// 一次流式回答是一百多个网络字节块、几百到几千个 SSE 事件，其中绝大多数是
// 「一个 token 一片」。以前把每个事件都当一张卡片从上往下铺：想看第 800 个事件
// 得先滚过 799 张卡，滚到底界面还会卡。那是把传输层的碎屑当成了内容。
//
// 所以改成**清单 + 详情**：左边一行一个事件（虚拟滚动，只挂视口里的那些），
// 点一行，右边给这一条的完整内容（新增的正文、data 的 JSON 树、原始块）。
// 事件再多，代价也只是滚动条变短。
//
// ## 过滤
//
// 按事件名分档 + 关键字搜。名字那一列来自上游自己的 `event:` 行 —— 我们不翻译，
// 因为排障时用户要对着上游文档找的就是这个名字。搜索只做**原文子串**匹配，
// 不做正则：用户敲的是字面量。

import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ArrowDownToLine, Lock, Search } from "lucide-react";

import { fmtNumber } from "../../lib/format";
import { IconButton } from "../ui/Button";
import { Input } from "../ui/Input";
import { JsonTree, isPlainObject } from "./JsonTree";
import { CopyButton, JsonOrRaw, parseJson } from "./kit";
import { kindsOf, parseSse, previewOf, type SseEvent } from "./sse";

/** 一行的高度。虚拟滚动要用定值 —— 行里的内容本来就截断成一行。 */
const ROW_H = 26;

export function EventStream({ raw, live }: { raw: string; live: boolean }) {
  const { events, emptyBlocks } = useMemo(() => parseSse(raw), [raw]);
  const kinds = useMemo(() => kindsOf(events), [events]);

  const [kind, setKind] = useState<string>("all");
  const [query, setQuery] = useState("");
  const [onlyContent, setOnlyContent] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return events.filter((e) => {
      if (kind !== "all" && e.name !== kind) return false;
      if (onlyContent && !e.fragment) return false;
      if (q && !(e.name.toLowerCase().includes(q) || e.data.toLowerCase().includes(q))) {
        return false;
      }
      return true;
    });
  }, [events, kind, query, onlyContent]);

  const scrollRef = useRef<HTMLDivElement>(null);
  const virt = useVirtualizer({
    count: filtered.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 24,
  });

  // 选中项从**全量**里找，而不是过滤后的那份：改了筛选项时详情不该跟着消失，
  // 那样用户会以为这个事件被删了。
  const current = selected === null ? null : (events[selected] ?? null);

  // 切过滤条件后列表变短，滚动位置可能落在空白处 —— 回到顶部更符合预期。
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: 0 });
  }, [kind, query, onlyContent]);

  const jumpToLatest = () => {
    const last = filtered[filtered.length - 1];
    if (!last) return;
    setSelected(last.seq);
    virt.scrollToIndex(filtered.length - 1, { align: "end" });
  };

  if (events.length === 0) {
    // 切不出事件（上游吐的不是 SSE，或者只吐了半截）：原样给，别谎称"没有内容"。
    return <JsonOrRaw text={raw} label="正文切不出 SSE 事件，原样显示" />;
  }

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-1.5">
        <div className="relative w-48">
          <Search
            size={12}
            className="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-dim"
          />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜事件名 / 内容"
            className="h-7 py-0 pl-7 text-[11px]"
            mono
          />
        </div>
        <KindChip active={kind === "all"} onClick={() => setKind("all")} count={events.length}>
          全部
        </KindChip>
        {kinds.map((k) => (
          <KindChip
            key={k.name}
            active={kind === k.name}
            onClick={() => setKind(k.name)}
            count={k.count}
            mono
          >
            {k.name}
          </KindChip>
        ))}
        <button
          type="button"
          onClick={() => setOnlyContent((v) => !v)}
          title="只列有新增正文 / 思考 / 参数分片的事件"
          className={`rounded px-1.5 py-0.5 text-[11px] transition-colors ${
            onlyContent ? "bg-ink/10 text-ink" : "text-dim hover:bg-ink/5 hover:text-ink"
          }`}
        >
          只看含内容
        </button>
        {live && (
          <span className="rounded bg-tone-blue-soft px-1.5 py-0.5 text-[10px] text-tone-blue">
            进行中
          </span>
        )}
        <IconButton
          className="ml-auto"
          title="跳到最后一个事件"
          onClick={jumpToLatest}
          disabled={filtered.length === 0}
        >
          <ArrowDownToLine size={13} />
        </IconButton>
      </div>

      <div className="text-[11px] text-dim">
        {filtered.length === events.length
          ? `${fmtNumber(events.length)} 个事件`
          : `筛出 ${fmtNumber(filtered.length)} / ${fmtNumber(events.length)} 个事件`}
        {emptyBlocks > 0 && `，另有 ${fmtNumber(emptyBlocks)} 个无 data 的保活块未列出`}
      </div>

      <div className="grid gap-2 lg:grid-cols-[minmax(0,20rem)_minmax(0,1fr)]">
        {/* 左：事件清单。高度固定、自己滚 —— 事件几千条也不该把整页撑长。 */}
        <div className="overflow-hidden rounded border border-edge-strong/60">
          <div className="flex items-center gap-2 border-b border-edge bg-base px-2 py-1 text-[10px] text-dim">
            <span className="w-7 shrink-0 text-right">#</span>
            <span className="w-24 shrink-0">事件</span>
            <span className="min-w-0 flex-1">这一片新增</span>
          </div>
          <div
            ref={scrollRef}
            className="h-[40vh] min-h-40 overflow-y-auto overscroll-contain"
          >
            {filtered.length === 0 ? (
              <div className="px-2 py-4 text-[11px] text-dim">没有匹配的事件。</div>
            ) : (
              <div style={{ height: virt.getTotalSize(), position: "relative" }}>
                {virt.getVirtualItems().map((v) => {
                  const ev = filtered[v.index];
                  const active = ev.seq === selected;
                  return (
                    <button
                      key={ev.seq}
                      onClick={() => setSelected(ev.seq)}
                      style={{
                        position: "absolute",
                        top: 0,
                        left: 0,
                        width: "100%",
                        height: v.size,
                        transform: `translateY(${v.start}px)`,
                      }}
                      className={`flex items-center gap-2 px-2 text-left text-[11px] transition-colors ${
                        active ? "bg-ink/10 text-ink" : "text-dim hover:bg-ink/5"
                      }`}
                      title={previewOf(ev)}
                    >
                      <span className="w-7 shrink-0 text-right tabular-nums text-dim/70">
                        {ev.seq + 1}
                      </span>
                      <span className="w-24 shrink-0 truncate mono text-tone-blue">
                        {ev.name}
                      </span>
                      <span className={`min-w-0 flex-1 truncate ${active ? "text-ink" : ""}`}>
                        {previewOf(ev) || "—"}
                      </span>
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        </div>

        {/* 右：选中事件的详情。 */}
        <div className="min-h-[18rem] overflow-y-auto rounded border border-edge-strong/60 bg-base p-2 lg:h-[calc(40vh+1.7rem)]">
          <EventDetail ev={current} />
        </div>
      </div>
    </div>
  );
}

function KindChip({
  active,
  onClick,
  count,
  mono,
  children,
}: {
  active: boolean;
  onClick: () => void;
  count: number;
  mono?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={`只看 ${children} 事件`}
      className={`inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] transition-colors ${
        active ? "bg-ink/10 text-ink" : "text-dim hover:bg-ink/5 hover:text-ink"
      }`}
    >
      <span className={mono ? "mono" : ""}>{children}</span>
      <span className="tabular-nums text-dim/70">{fmtNumber(count)}</span>
    </button>
  );
}

function EventDetail({ ev }: { ev: SseEvent | null }) {
  if (!ev) {
    return (
      <div className="flex h-full min-h-32 items-center justify-center px-6 text-center text-[11px] leading-relaxed text-dim">
        点左边任意一个事件，这里显示它到底发了什么。
      </div>
    );
  }

  const parsed = parseJson(ev.data);

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="rounded bg-bubble px-1.5 py-0.5 text-[10px] text-dim">
          第 {ev.seq + 1} 个
        </span>
        <span className="mono text-xs text-tone-blue">{ev.name}</span>
        <span className="ml-auto">
          <CopyButton text={ev.data} title="复制这个事件的 data" />
        </span>
      </div>

      {ev.fragment && (
        <div className="space-y-1">
          <div className="text-[10px] text-dim">这一片新增的内容</div>
          <pre className="max-h-32 overflow-auto whitespace-pre-wrap break-words rounded bg-card p-2 mono text-[11px] leading-relaxed text-ink">
            {ev.fragment}
          </pre>
        </div>
      )}

      <div className="space-y-1">
        <div className="text-[10px] text-dim">data 解出来的 JSON</div>
        {parsed === undefined ? (
          <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-all rounded bg-card p-2 mono text-[11px] text-ink">
            {ev.data}
          </pre>
        ) : isPlainObject(parsed) || Array.isArray(parsed) ? (
          <JsonTree value={parsed} defaultDepth={3} className="max-h-52 bg-card" />
        ) : (
          <pre className="rounded bg-card p-2 mono text-[11px] text-ink">
            {JSON.stringify(parsed)}
          </pre>
        )}
      </div>

      <details>
        <summary className="flex cursor-pointer list-none items-center gap-1 text-[10px] text-dim transition-colors hover:text-ink">
          <Lock size={10} />
          原始事件块（含 event: / data: 行）
        </summary>
        <div className="mt-1 flex justify-end">
          <CopyButton text={ev.raw} title="复制原始块" />
        </div>
        <pre className="mt-1 max-h-32 overflow-auto whitespace-pre-wrap break-all rounded bg-card p-2 mono text-[11px] text-ink">
          {ev.raw}
        </pre>
      </details>
    </div>
  );
}