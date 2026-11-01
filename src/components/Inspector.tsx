// 报文检视（需求 4）：把**整个会话实际发出的每一个 HTTP 请求**翻出来看。
//
// ## 形状：列表 + 详情
//
// 一条会话对应多个请求（每一轮对话、每次工具回环、起标题、压缩各是一次），
// 所以左边是**这次会话的全部请求**，右边是选中那一条的详情。不是「一个模态看一份
// 报文」—— 排障时要对照的是「这一轮和上一轮差在哪」，来回关开面板就没法对照了。
//
// ## 详情里请求与响应分块
//
// 「可视化 / 格式化 / 原始」三个视图都是**同一种切法**：先请求、后响应，各自成块、
// 各自可收起。三个视图回答的是同一份东西的三个问题（语义是什么 / 结构是什么 /
// 字节是什么），边界长得一样，切换时眼睛不用重新找位置。
//
// ## 为什么留着抽屉而不是整屏
//
// 看报文时要能同时看到对话，才知道「这条请求是给哪句回答的」。
//
// ## 事件流为什么不是一条长列表往下铺
//
// 一次流式回答几百到几千个 SSE 事件，一屏一屏铺下去要么滚不动、要么卡死。
// 现在收在「事件流」区里：清单 + 详情、虚拟滚动、按事件名过滤（见 payload/EventStream.tsx）。

import { useEffect, useMemo, useRef, useState } from "react";
import {
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  Clock,
  FileSearch,
  Inbox,
  LoaderCircle,
  Search,
  X,
} from "lucide-react";

import {
  getExchange,
  inspectExchange,
  listExchanges,
  purposeLabel,
  type ExchangeRecord,
  type ExchangeSummary,
  type InspectedExchange,
} from "../lib/api";
import { fmtMs, fmtNumber } from "../lib/format";
import { useStore } from "../store";
import { IconButton } from "./ui/Button";
import { Input } from "./ui/Input";
import { FormattedView, RawView } from "./payload/formatted";
import { VisualView } from "./payload/visual";

type View = "visual" | "formatted" | "raw";

const VIEW_TABS: { id: View; label: string; hint: string }[] = [
  { id: "visual", label: "可视化", hint: "按语义拆开：请求参数、系统提示词、工具、上下文、回答、思考、token" },
  { id: "formatted", label: "格式化", hint: "把请求体与响应体排成可折叠的 JSON 树" },
  { id: "raw", label: "原始", hint: "未经任何加工的字节，用来和上游逐字节对照" },
];

// ---------------------------------------------------------------------------
// 小件
// ---------------------------------------------------------------------------

function ViewTabs({ value, onChange }: { value: View; onChange: (v: View) => void }) {
  return (
    <div className="inline-flex rounded-md bg-base p-0.5 text-xs">
      {VIEW_TABS.map((t) => (
        <button
          key={t.id}
          onClick={() => onChange(t.id)}
          title={t.hint}
          className={`rounded px-2.5 py-1 transition-colors ${
            value === t.id ? "bg-card text-ink" : "text-dim hover:text-ink"
          }`}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

/** 状态码配色：2xx 绿、4xx 琥珀、5xx 红。 */
function statusClass(status: number | null): string {
  if (status === null) return "border border-edge text-dim";
  if (status >= 500) return "bg-[var(--error-bg)] text-[var(--danger)]";
  if (status >= 400) return "bg-[var(--warn-bg)] text-tone-orange";
  if (status >= 200 && status < 300) return "bg-tone-green-soft text-tone-green";
  return "border border-edge text-dim";
}

function StatusBadge({ status }: { status: number | null }) {
  return (
    <span className={`rounded px-1.5 py-0.5 mono text-[11px] tabular-nums ${statusClass(status)}`}>
      {status ?? "—"}
    </span>
  );
}

function Spinner() {
  return <LoaderCircle size={11} className="animate-spin" />;
}

// ---------------------------------------------------------------------------
// 左侧列表
// ---------------------------------------------------------------------------

/** 按 turn 分段。列表本身是时间序，turn 单调递增，所以连续同类即一组。 */
function groupByTurn(list: ExchangeSummary[]): { turn: number | null; items: ExchangeSummary[] }[] {
  const groups: { turn: number | null; items: ExchangeSummary[] }[] = [];
  for (const s of list) {
    const last = groups[groups.length - 1];
    if (last && last.turn === s.turn) last.items.push(s);
    else groups.push({ turn: s.turn, items: [s] });
  }
  return groups;
}

function ExchangeRow({
  s,
  active,
  onClick,
}: {
  s: ExchangeSummary;
  active: boolean;
  onClick: () => void;
}) {
  const label = purposeLabel(s.purpose);
  return (
    <button
      onClick={onClick}
      title={`${s.method} ${s.url}`}
      className={`w-full border-b border-edge px-3 py-2 text-left transition-colors ${
        active ? "bg-ink/10" : "hover:bg-ink/5"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className={`text-xs ${active ? "text-ink" : "text-ink/90"}`}>
          第 {s.turn ?? "?"} 轮 · 第 {s.round ?? "?"} 次
        </span>
        {label && (
          <span className="rounded bg-tone-violet-soft px-1 py-0.5 text-[10px] text-tone-violet">
            {label}
          </span>
        )}
        <span className="ml-auto flex shrink-0 items-center gap-1">
          {s.in_flight ? (
            <span className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-tone-blue">
              <Spinner />
              进行中
            </span>
          ) : (
            <StatusBadge status={s.status} />
          )}
        </span>
      </div>

      <div className="mt-1 flex items-center gap-2 text-[11px] text-dim" title="首字节 / 总耗时">
        <Clock size={11} className="shrink-0" />
        <span className="tabular-nums">
          {s.ttfb_ms !== null ? fmtMs(s.ttfb_ms) : s.in_flight ? "等待首字节" : "—"} /{" "}
          {s.in_flight ? "进行中" : fmtMs(s.total_ms)}
        </span>
        {s.frame_count > 0 && (
          <span className="rounded bg-bubble px-1 text-dim">{fmtNumber(s.frame_count)} 帧</span>
        )}
      </div>

      {/* 同一个模型可能配在多个渠道上 —— 光看 URL 与请求体分不出来，渠道要单独标。 */}
      {(s.channel || s.model) && (
        <div className="mt-0.5 flex items-center gap-1.5 text-[10px]">
          {s.channel && (
            <span className="rounded bg-tone-blue-soft px-1 py-0.5 text-tone-blue" title="上游渠道">
              {s.channel}
            </span>
          )}
          {s.model && (
            <span className="truncate mono text-dim" title={s.model}>
              {s.model}
            </span>
          )}
        </div>
      )}
    </button>
  );
}

// ---------------------------------------------------------------------------
// 右侧详情
// ---------------------------------------------------------------------------

/**
 * 可视化页：把字节**解回 IR** 之后按语义渲染。
 *
 * 解出来的东西由后端给（`inspect_exchange`）—— 那里用的是与真实链路同一套 codec，
 * 所以这一页显示的就是模型实际收到/说出的内容。解不开时不白屏：视图顶部会把
 * 原因顶出来并指向「原始」页。
 */
function Visual({ rec, inspected }: { rec: ExchangeRecord; inspected: InspectedExchange | null }) {
  return (
    <VisualView
      inspected={inspected}
      method={rec.method}
      url={rec.url}
      status={rec.status}
      ttfbMs={rec.ttfb_ms}
      totalMs={rec.total_ms}
      // 帧是原始字节块，空串拼接才等于上游实际吐出的字节流；用换行拼会凭空插入字符。
      streamText={rec.stream_frames.join("")}
      headers={{ request: rec.request_headers, response: rec.response_headers }}
      requestBody={rec.request_body}
      responseBody={rec.response_body}
      streamError={rec.stream_error}
      live={rec.total_ms === null}
    />
  );
}

function Detail({
  record,
  inspected,
  view,
}: {
  record: ExchangeRecord;
  inspected: InspectedExchange | null;
  view: View;
}) {
  if (view === "visual") return <Visual rec={record} inspected={inspected} />;
  if (view === "formatted") return <FormattedView rec={record} />;
  return <RawView rec={record} />;
}

// ---------------------------------------------------------------------------
// 面板
// ---------------------------------------------------------------------------

export function Inspector() {
  const open = useStore((s) => s.inspectorOpen);
  const turn = useStore((s) => s.inspectorTurn);
  const sessionId = useStore((s) => s.sessionId);
  const closeInspector = useStore((s) => s.closeInspector);
  const setInspectorTurn = useStore((s) => s.setInspectorTurn);
  const setNotice = useStore((s) => s.setNotice);

  const [list, setList] = useState<ExchangeSummary[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  // undefined = 加载中，null = 后端说没有这条（已被清理）
  const [record, setRecord] = useState<ExchangeRecord | null | undefined>(undefined);
  // 可视化页的数据：后端把字节解回 IR 的结果。与 `record` 分开取 ——
  // 解码要跑一遍协议 codec，只在真要画「可视化」时才值得花这个钱。
  //
  // 记下是**哪一条**的解，而不是裸存一份：进行中的流每秒都在长，每次重解都先
  // 清空的话，界面会每秒闪一次「正在解读报文…」。换了一条才清。
  const [inspected, setInspected] = useState<{ id: string; value: InspectedExchange } | null>(
    null,
  );
  const [view, setView] = useState<View>("visual");
  const [query, setQuery] = useState("");
  const detailRef = useRef<HTMLDivElement>(null);

  // 打开面板 / 换会话时拉列表，之后**每秒刷新**：进行中的请求在请求发出的那一刻
  // 就该出现（而不是等首字甚至等收完），完成后状态与耗时也要跟着变。
  useEffect(() => {
    if (!open || !sessionId) return;
    let cancelled = false;
    setList(null);
    setListError(null);
    const fetchList = () => {
      listExchanges(sessionId)
        .then((s) => {
          if (cancelled) return;
          setList(s);
          setListError(null);
        })
        .catch((e) => {
          if (cancelled) return;
          // 已经拿到过列表就别清空 —— 一次轮询失败不该把面板变成空白。
          setList((prev) => prev ?? []);
          setListError(String(e));
        });
    };
    fetchList();
    const timer = setInterval(fetchList, 1000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [open, sessionId]);

  // 换筛选 / 换会话就清掉选中项：选中的是**上一条**会话里的报文，带着这个 id
  // 去新会话里查一定查不到，然后那句「该报文已不存在」就冒出来了 ——
  // 而用户不过是换了个对话。
  useEffect(() => {
    setSelected(null);
  }, [turn, sessionId]);

  // 选中某条时取完整记录。
  useEffect(() => {
    if (!selected || !sessionId) {
      setRecord(undefined);
      return;
    }
    let cancelled = false;
    setRecord(undefined);
    getExchange(sessionId, selected)
      .then((r) => {
        if (cancelled) return;
        setRecord(r);
        // 记录没了（被清理 / 换了会话）时用提示条说明，而不是给个空白面板。
        if (r === null) setNotice("该报文已不存在（可能已被清理）");
      })
      .catch((e) => {
        if (!cancelled) {
          setRecord(null);
          setNotice(String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [selected, sessionId, setNotice]);

  // 进行中的交换还在长（响应体、帧数、耗时）：每秒重取一次。
  // 收尾后 `total_ms` 有了，`live` 变假，这个 effect 自动停。
  const live = record !== undefined && record !== null && record.total_ms === null;
  useEffect(() => {
    if (!live || !selected || !sessionId) return;
    let cancelled = false;
    const timer = setInterval(() => {
      getExchange(sessionId, selected)
        .then((r) => {
          // 刚收尾那一瞬间可能两处都取不到（pending 已移除、文件还没写完）——
          // 保留上一份，下一次轮询就补上了。
          if (!cancelled && r) setRecord(r);
        })
        .catch(() => {});
    }, 1000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [live, selected, sessionId]);

  // 可视化页：选中某条、且看的是这一页时，才去解它。
  //
  // 依赖里是「记录长到哪了」而不是整条记录：进行中的流每秒都在长，解一次
  // （跑一遍协议 codec）比把整个 `record` 对象放进依赖更可控 —— 后者会让
  // 同一个版本解两遍。
  const recordVersion =
    record == null
      ? ""
      : `${record.stream_frames.length}:${record.response_body.length}:${record.total_ms}`;
  useEffect(() => {
    if (!selected || !sessionId || view !== "visual") {
      setInspected(null);
      return;
    }
    let cancelled = false;
    // 同一条的旧结果先留着（见上面 `inspected` 的注释）；只是换了 id 时清掉。
    setInspected((prev) => (prev && prev.id === selected ? prev : null));
    inspectExchange(sessionId, selected)
      .then((v) => {
        if (cancelled) return;
        if (v) setInspected({ id: selected, value: v });
      })
      .catch((e) => {
        if (!cancelled) {
          setInspected(null);
          setNotice(String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [selected, sessionId, view, recordVersion, setNotice]);

  // 换一条请求时详情回到顶部：上一条的行位置毫无意义。
  useEffect(() => {
    detailRef.current?.scrollTo({ top: 0 });
  }, [selected]);

  const filtered = useMemo(() => {
    const all = list?.filter((s) => turn === null || s.turn === turn) ?? [];
    const q = query.trim().toLowerCase();
    if (!q) return all;
    return all.filter((s) =>
      [s.url, s.model, s.channel, s.method, purposeLabel(s.purpose)]
        .some((f) => f?.toLowerCase().includes(q)),
    );
  }, [list, turn, query]);

  const groups = useMemo(() => groupByTurn(filtered), [filtered]);
  const order = useMemo(() => filtered.map((s) => s.id), [filtered]);
  const index = selected ? order.indexOf(selected) : -1;

  // 键盘翻条：↑ / ↓ 在请求之间走，Esc 关面板。输入框里不拦 —— 那里要能用方向键。
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName)) return;
      if (e.key === "Escape") {
        closeInspector();
        return;
      }
      if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
      if (order.length === 0) return;
      e.preventDefault();
      const next =
        index === -1
          ? e.key === "ArrowDown"
            ? 0
            : order.length - 1
          : Math.min(order.length - 1, Math.max(0, index + (e.key === "ArrowDown" ? 1 : -1)));
      setSelected(order[next]);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, order, index, closeInspector]);

  if (!open) return null;

  const go = (delta: number) => {
    const i = Math.min(order.length - 1, Math.max(0, index + delta));
    if (i >= 0 && i !== index) setSelected(order[i]);
  };

  return (
    // 用 vw 兜底：窗口窄时（默认只有 800px）也要留下一截对话可见，
    // 否则「抽屉」就变成了全屏、失去了与对话对照的意义。
    <div className="fixed inset-y-0 right-0 z-40 flex w-[min(78rem,92vw)] flex-col border-l border-edge bg-sidebar shadow-2xl">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-edge px-3">
        <FileSearch size={15} className="text-dim" />
        <span className="text-sm font-semibold">报文</span>
        <span className="text-[11px] text-dim">
          本次会话的全部请求
          {list ? ` · ${fmtNumber(list.length)} 条` : ""}
        </span>
        {turn !== null && (
          <button
            onClick={() => setInspectorTurn(null)}
            title="清除筛选"
            className="inline-flex items-center gap-1 rounded-full bg-bubble px-2 py-0.5 text-[11px] text-dim transition-colors hover:text-ink"
          >
            <X size={11} />
            第 {turn} 轮
          </button>
        )}
        <IconButton className="ml-auto" title="关闭（Esc）" onClick={closeInspector}>
          <X size={16} />
        </IconButton>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* 左：请求列表 */}
        <div className="flex w-72 shrink-0 flex-col border-r border-edge">
          <div className="relative shrink-0 p-2">
            <Search
              size={12}
              className="pointer-events-none absolute left-4 top-1/2 -translate-y-1/2 text-dim"
            />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜 URL / 模型 / 渠道"
              className="h-7 py-0 pl-7 text-[11px]"
            />
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto">
            {list === null && (
              <div className="flex items-center gap-2 px-3 py-4 text-xs text-dim">
                <Spinner />
                加载中…
              </div>
            )}
            {listError && (
              <div className="m-2 rounded-md border border-[var(--error-border)] bg-[var(--error-bg)] px-2 py-2 text-xs">
                <div className="flex items-center gap-1.5 font-medium">
                  <CircleAlert size={13} />
                  读取报文失败
                </div>
                <div className="mt-1 break-all text-dim">{listError}</div>
              </div>
            )}
            {list !== null && groups.length === 0 && !listError && (
              <div className="m-2 rounded-md border border-dashed border-edge px-2 py-3 text-xs leading-relaxed text-dim">
                {list.length > 0 ? (
                  <>没有匹配的请求。</>
                ) : turn !== null ? (
                  <>这一轮没有抓到请求。</>
                ) : (
                  <>
                    还没有抓到报文。
                    <br />
                    抓包只记录<strong className="font-medium text-ink">本应用运行期间</strong>
                    发出的请求 —— 启动前的历史不会补录，也不包含其它程序发出的流量。
                  </>
                )}
              </div>
            )}
            {groups.map((g, gi) => (
              <div key={gi}>
                <div className="sticky top-0 z-10 bg-sidebar px-3 py-1 text-[11px] font-medium text-dim">
                  {g.turn === null ? "未标注轮次" : `第 ${g.turn} 轮`}
                  <span className="ml-1 text-dim/60">· {g.items.length} 次</span>
                </div>
                {g.items.map((s) => (
                  <ExchangeRow
                    key={s.id}
                    s={s}
                    active={s.id === selected}
                    onClick={() => setSelected(s.id)}
                  />
                ))}
              </div>
            ))}
          </div>
        </div>

        {/* 右：详情 */}
        <div ref={detailRef} className="min-w-0 flex-1 overflow-y-auto">
          {!selected && (
            <div className="flex h-full flex-col items-center justify-center gap-2 text-sm text-dim">
              <Inbox size={22} />
              从左侧选择一条请求
              <span className="text-[11px] text-dim/70">↑ ↓ 可以在请求之间翻</span>
            </div>
          )}
          {selected && record === undefined && (
            <div className="flex items-center gap-2 p-4 text-xs text-dim">
              <Spinner />
              加载中…
            </div>
          )}
          {selected && record === null && (
            <div className="p-4 text-sm text-dim">该报文已不存在（可能已被清理）。</div>
          )}
          {record && (
            <div className="space-y-3 p-3">
              <div className="flex flex-wrap items-center gap-2">
                <div className="flex items-center gap-0.5">
                  <IconButton
                    title="上一条请求（↑）"
                    disabled={index <= 0}
                    onClick={() => go(-1)}
                  >
                    <ChevronLeft size={14} />
                  </IconButton>
                  <span className="min-w-14 text-center text-[11px] tabular-nums text-dim">
                    {index >= 0 ? `${index + 1} / ${order.length}` : "—"}
                  </span>
                  <IconButton
                    title="下一条请求（↓）"
                    disabled={index < 0 || index >= order.length - 1}
                    onClick={() => go(1)}
                  >
                    <ChevronRight size={14} />
                  </IconButton>
                </div>

                <ViewTabs value={view} onChange={setView} />

                <span className="text-[11px] text-dim">
                  {new Date(record.ts).toLocaleString("zh-CN")}
                </span>
                {/* 上游渠道与模型引用：同一个模型可能配在多个渠道上，详情里也要写明。 */}
                {record.channel && (
                  <span
                    className="rounded bg-tone-blue-soft px-1.5 py-0.5 text-[11px] text-tone-blue"
                    title="上游渠道"
                  >
                    {record.channel}
                  </span>
                )}
                {record.model && (
                  <span className="mono text-[11px] text-dim" title="模型引用">
                    {record.model}
                  </span>
                )}
                {live && (
                  <span className="inline-flex items-center gap-1 text-[11px] text-tone-blue">
                    <Spinner />
                    进行中
                  </span>
                )}
              </div>

              <Detail
                record={record}
                inspected={inspected?.id === record.id ? inspected.value : null}
                view={view}
              />
            </div>
          )}
        </div>
      </div>
    </div>
  );
}