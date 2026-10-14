// 报文检视（需求 4）：把某条对话内容对应的**实际 HTTP 请求**翻出来看。
//
// 右侧抽屉而不是整屏模态：看报文时要能同时看到对话，才知道「这条请求是给哪句回答的」。
//
// 三个视图（可视化 / 格式化 / 原始）用同一个 `ViewTabs` 切换，用户学一次就够。
// 「可视化」是给人读的拆解，「格式化」是整条记录的 JSON，「原始」是不做任何改动的
// 字节 —— 排障时经常要拿原始字节去比对上游返回，格式化会把空白改掉。

import { useEffect, useState } from "react";
import { CircleAlert, Clock, FileSearch, Inbox, LoaderCircle, Radio, X } from "lucide-react";

import {
  getExchange,
  listExchanges,
  purposeLabel,
  type ExchangeRecord,
  type ExchangeSummary,
} from "../lib/api";
import { useStore } from "../store";

type View = "visual" | "formatted" | "raw";

const VIEW_TABS: { id: View; label: string }[] = [
  { id: "visual", label: "可视化" },
  { id: "formatted", label: "格式化" },
  { id: "raw", label: "原始" },
];

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

function ViewTabs({ value, onChange }: { value: View; onChange: (v: View) => void }) {
  return (
    <div className="inline-flex rounded-md border border-slate-700 bg-slate-900/60 p-0.5 text-xs">
      {VIEW_TABS.map((t) => (
        <button
          key={t.id}
          onClick={() => onChange(t.id)}
          className={`rounded px-2.5 py-1 transition-colors ${
            value === t.id ? "bg-slate-700 text-slate-100" : "text-slate-400 hover:text-slate-200"
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
  if (status === null) return "bg-slate-700/60 text-slate-300";
  if (status >= 500) return "bg-red-900/60 text-red-300";
  if (status >= 400) return "bg-amber-900/60 text-amber-300";
  if (status >= 200 && status < 300) return "bg-emerald-900/60 text-emerald-300";
  return "bg-slate-700/60 text-slate-300";
}

function StatusBadge({ status }: { status: number | null }) {
  return (
    <span className={`rounded px-1.5 py-0.5 font-mono text-[11px] ${statusClass(status)}`}>
      {status ?? "—"}
    </span>
  );
}

function ms(v: number | null): string {
  if (v === null) return "—";
  return v < 1000 ? `${v}ms` : `${(v / 1000).toFixed(2)}s`;
}

/** 能当 JSON 解析就美化，否则返回 null 让调用方回落成原文。 */
function tryPretty(text: string): string | null {
  if (!text.trim()) return null;
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return null;
  }
}

type SseEvent = { name: string; data: string };

/**
 * 把一段原始 SSE 文本切成事件。
 *
 * 按**空行**切，而不是引入完整解析器：SSE 规范里事件本来就是空行分隔的。我们只想
 * 把原始帧显示得更可读，而解析器会顺手丢掉注释行（`: keep-alive` 心跳）和不认识的
 * 字段 —— 那些恰恰是排障时要看的东西。所以这里只做最小识别。
 */
function parseSseEvents(frame: string): SseEvent[] {
  return frame
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
      return { name, data: data.join("\n") };
    });
}

// ---------------------------------------------------------------------------
// 视图
// ---------------------------------------------------------------------------

function HeadersTable({ headers }: { headers: [string, string][] }) {
  if (headers.length === 0) return <div className="text-xs text-slate-600">（无）</div>;
  return (
    <div className="overflow-hidden rounded border border-slate-700/70">
      <table className="w-full border-collapse text-xs">
        <tbody>
          {headers.map(([k, v], i) => (
            <tr key={i} className={i % 2 ? "bg-slate-900/40" : ""}>
              <td className="w-44 border-r border-slate-700/70 px-2 py-1 align-top font-mono text-slate-400">
                {k}
              </td>
              <td className="break-all px-2 py-1 font-mono text-slate-300">{v}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** 请求/响应正文：是 JSON 就美化，不是就原样显示。 */
function BodyBlock({ text }: { text: string }) {
  if (!text.trim()) return <div className="text-xs text-slate-600">（空）</div>;
  return (
    <pre className="max-h-[40vh] overflow-auto whitespace-pre-wrap rounded border border-slate-700/70 bg-slate-950/70 p-2 font-mono text-[11px] leading-relaxed text-slate-300">
      {tryPretty(text) ?? text}
    </pre>
  );
}

/** 流式响应：逐个帧、逐个 SSE 事件地展开，不把整坨拼成一个框。 */
function SseFrames({ frames }: { frames: string[] }) {
  return (
    <div className="space-y-3">
      {frames.map((frame, fi) => {
        const events = parseSseEvents(frame);
        return (
          <div key={fi} className="space-y-1.5">
            <div className="text-[11px] text-slate-600">
              帧 #{fi + 1} · {frame.length} 字节
            </div>
            {events.length === 0 ? (
              <pre className="overflow-auto whitespace-pre-wrap rounded border border-slate-700/70 bg-slate-950/70 p-2 font-mono text-[11px] text-slate-400">
                {frame}
              </pre>
            ) : (
              events.map((ev, ei) => (
                <div key={ei} className="overflow-hidden rounded border border-slate-700/70 bg-slate-900/40">
                  <div className="flex items-center gap-2 border-b border-slate-700/50 px-2 py-1">
                    <Radio size={12} className="text-sky-400" />
                    <span className="font-mono text-[11px] text-sky-300">{ev.name}</span>
                  </div>
                  <pre className="max-h-80 overflow-auto whitespace-pre-wrap p-2 font-mono text-[11px] leading-relaxed text-slate-300">
                    {tryPretty(ev.data) ?? ev.data}
                  </pre>
                </div>
              ))
            )}
          </div>
        );
      })}
    </div>
  );
}

function VisualView({ rec }: { rec: ExchangeRecord }) {
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <span className="rounded bg-slate-700 px-1.5 py-0.5 font-mono text-[11px] text-slate-200">
          {rec.method}
        </span>
        <span className="break-all font-mono text-xs text-slate-300">{rec.url}</span>
      </div>

      <section className="space-y-1.5">
        <h3 className="text-xs font-semibold text-slate-400">请求头</h3>
        <HeadersTable headers={rec.request_headers} />
      </section>

      <section className="space-y-1.5">
        <h3 className="text-xs font-semibold text-slate-400">请求体</h3>
        <BodyBlock text={rec.request_body} />
      </section>

      <section className="space-y-1.5">
        <div className="flex items-center gap-2">
          <h3 className="text-xs font-semibold text-slate-400">响应</h3>
          <StatusBadge status={rec.status} />
        </div>
        <HeadersTable headers={rec.response_headers} />
        {rec.stream_error && (
          <div className="rounded border border-red-800/60 bg-red-950/30 px-2 py-1.5 text-xs text-red-300">
            流内错误：{rec.stream_error}
          </div>
        )}
        {rec.stream_frames.length > 0 ? (
          <SseFrames frames={rec.stream_frames} />
        ) : (
          <BodyBlock text={rec.response_body} />
        )}
      </section>
    </div>
  );
}

function FormattedView({ rec }: { rec: ExchangeRecord }) {
  return (
    <pre className="overflow-auto rounded border border-slate-700/70 bg-slate-950/70 p-2 font-mono text-[11px] leading-relaxed text-slate-300">
      {JSON.stringify(rec, null, 2)}
    </pre>
  );
}

function RawView({ rec }: { rec: ExchangeRecord }) {
  const streaming = rec.stream_frames.length > 0;
  // 帧是原始字节块，空串拼接才等于上游实际吐出的字节流；用换行拼会凭空插入字符。
  const response = streaming ? rec.stream_frames.join("") : rec.response_body;
  return (
    <div className="space-y-3">
      <div>
        <div className="mb-1 text-[11px] text-slate-600">请求体（原样）</div>
        <pre className="max-h-[40vh] overflow-auto whitespace-pre-wrap rounded border border-slate-700/70 bg-slate-950/70 p-2 font-mono text-[11px] leading-relaxed text-slate-300">
          {rec.request_body || "（空）"}
        </pre>
      </div>
      <div>
        <div className="mb-1 text-[11px] text-slate-600">
          {streaming ? "流式响应原始字节（按到达顺序原样拼接）" : "响应体（原样）"}
        </div>
        <pre className="max-h-[40vh] overflow-auto whitespace-pre-wrap rounded border border-slate-700/70 bg-slate-950/70 p-2 font-mono text-[11px] leading-relaxed text-slate-300">
          {response || "（空）"}
        </pre>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 列表
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
      className={`w-full border-b border-slate-800/70 px-3 py-2 text-left transition-colors ${
        active ? "bg-sky-600/20" : "hover:bg-slate-800/60"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className="text-xs text-slate-200">
          第 {s.turn ?? "?"} 轮 · 第 {s.round ?? "?"} 次
        </span>
        {label && (
          <span className="rounded bg-indigo-900/50 px-1 py-0.5 text-[10px] text-indigo-300">
            {label}
          </span>
        )}
        <span className="ml-auto">
          <StatusBadge status={s.status} />
        </span>
      </div>
      <div className="mt-1 flex items-center gap-2 text-[11px] text-slate-500" title="首字节 / 总耗时">
        <Clock size={11} />
        <span className="tabular-nums">
          {ms(s.ttfb_ms)} / {ms(s.total_ms)}
        </span>
        {s.frame_count > 0 && (
          <span className="rounded bg-slate-700/60 px-1 text-slate-300">{s.frame_count} 帧</span>
        )}
      </div>
      <div className="mt-0.5 truncate font-mono text-[10px] text-slate-600">
        {s.method} {s.url}
      </div>
    </button>
  );
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
  const [view, setView] = useState<View>("visual");

  // 打开面板 / 换会话时拉列表。
  useEffect(() => {
    if (!open || !sessionId) return;
    let cancelled = false;
    setList(null);
    setListError(null);
    listExchanges(sessionId)
      .then((s) => {
        if (!cancelled) setList(s);
      })
      .catch((e) => {
        if (!cancelled) {
          setList([]);
          setListError(String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [open, sessionId]);

  // 换筛选就清掉选中项，免得看到一行不属于当前筛选的报文。
  useEffect(() => {
    setSelected(null);
  }, [turn]);

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

  if (!open) return null;

  const filtered = list?.filter((s) => turn === null || s.turn === turn) ?? null;
  const groups = filtered ? groupByTurn(filtered) : null;

  return (
    // 用 vw 兜底：窗口窄时（默认只有 800px）也要留下一截对话可见，
    // 否则「抽屉」就变成了全屏、失去了与对话对照的意义。
    <div className="fixed inset-y-0 right-0 z-40 flex w-[min(56rem,85vw)] flex-col border-l border-slate-700 bg-slate-900 shadow-2xl">
      <div className="flex items-center gap-2 border-b border-slate-800 px-3 py-2">
        <FileSearch size={16} className="text-slate-400" />
        <span className="text-sm font-semibold text-slate-200">报文</span>
        {turn !== null && (
          <button
            onClick={() => setInspectorTurn(null)}
            title="清除筛选"
            className="inline-flex items-center gap-1 rounded-full bg-sky-900/50 px-2 py-0.5 text-[11px] text-sky-300 hover:bg-sky-900/80"
          >
            <X size={11} />
            清除筛选 · 只看第 {turn} 轮
          </button>
        )}
        <button
          onClick={closeInspector}
          aria-label="关闭"
          className="ml-auto rounded p-1 text-slate-400 hover:bg-slate-800 hover:text-slate-200"
        >
          <X size={16} />
        </button>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* 左：交换列表 */}
        <div className="w-64 shrink-0 overflow-y-auto border-r border-slate-800">
          {list === null && (
            <div className="flex items-center gap-2 px-3 py-4 text-xs text-slate-500">
              <LoaderCircle size={13} className="animate-spin" />
              加载中…
            </div>
          )}
          {listError && (
            <div className="m-2 rounded border border-amber-900/50 bg-amber-950/30 px-2 py-2 text-xs text-amber-200">
              <div className="flex items-center gap-1.5 font-medium">
                <CircleAlert size={13} />
                读取报文失败
              </div>
              <div className="mt-1 break-all text-amber-200/80">{listError}</div>
            </div>
          )}
          {groups && groups.length === 0 && !listError && (
            <div className="m-2 rounded border border-slate-800 px-2 py-3 text-xs leading-relaxed text-slate-500">
              {turn !== null ? (
                <>这一轮没有抓到请求。</>
              ) : (
                <>
                  还没有抓到报文。
                  <br />
                  抓包只记录<strong className="font-medium text-slate-400">本应用运行期间</strong>
                  发出的请求 —— 启动前的历史不会补录，也不包含其它程序发出的流量。
                </>
              )}
            </div>
          )}
          {groups?.map((g, gi) => (
            <div key={gi}>
              <div className="sticky top-0 bg-slate-900/95 px-3 py-1 text-[11px] font-medium text-slate-400 backdrop-blur">
                {g.turn === null ? "未标注轮次" : `第 ${g.turn} 轮`}
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

        {/* 右：详情 */}
        <div className="min-w-0 flex-1 overflow-y-auto">
          {!selected && (
            <div className="flex h-full flex-col items-center justify-center gap-2 text-sm text-slate-600">
              <Inbox size={22} />
              从左侧选择一条报文
            </div>
          )}
          {selected && record === undefined && (
            <div className="flex items-center gap-2 p-4 text-xs text-slate-500">
              <LoaderCircle size={13} className="animate-spin" />
              加载中…
            </div>
          )}
          {selected && record === null && (
            <div className="p-4 text-sm text-slate-500">
              该报文已不存在（可能已被清理）。
            </div>
          )}
          {record && (
            <div className="space-y-3 p-3">
              <div className="flex items-center gap-2">
                <ViewTabs value={view} onChange={setView} />
                <span className="text-[11px] text-slate-600">
                  {new Date(record.ts).toLocaleString("zh-CN")}
                </span>
              </div>
              {view === "visual" && <VisualView rec={record} />}
              {view === "formatted" && <FormattedView rec={record} />}
              {view === "raw" && <RawView rec={record} />}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
