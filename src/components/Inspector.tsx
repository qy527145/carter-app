// 报文检视（需求 4）：把某条对话内容对应的**实际 HTTP 请求**翻出来看。
//
// 右侧抽屉而不是整屏模态：看报文时要能同时看到对话，才知道「这条请求是给哪句回答的」。
//
// 三个视图（可视化 / 格式化 / 原始）用同一个 `ViewTabs` 切换，用户学一次就够。
// 「可视化」是给人读的**语义**拆解（`PayloadView.tsx`：后端把字节解回 IR 之后的样子），
// 「格式化」把请求与响应**分开**摊成结构化 JSON（响应是流式的话，就是一串排开、
// 各自美化的 SSE 事件），「原始」是不做任何改动的字节 —— 排障时经常要拿原始字节去
// 比对上游返回，格式化会把空白改掉。
//
// 「可视化」刻意**不铺逐帧碎片**：一次流式回答是一百多个网络字节块、每片一个字，
// 摊开只能看出传输有多碎，看不出模型答了什么。逐事件留在那一页的折叠区里。

import { useEffect, useState } from "react";
import { CircleAlert, Clock, FileSearch, Inbox, LoaderCircle, X } from "lucide-react";

import {
  getExchange,
  inspectExchange,
  listExchanges,
  purposeLabel,
  type ExchangeRecord,
  type ExchangeSummary,
  type InspectedExchange,
} from "../lib/api";
import { fmtMs } from "../lib/format";
import { useStore } from "../store";
import { FormattedView, PayloadView } from "./PayloadView";

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
    <div className="inline-flex rounded-md border border-edge bg-base p-0.5 text-xs">
      {VIEW_TABS.map((t) => (
        <button
          key={t.id}
          onClick={() => onChange(t.id)}
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
    <span className={`rounded px-1.5 py-0.5 font-mono text-[11px] ${statusClass(status)}`}>
      {status ?? "—"}
    </span>
  );
}

/**
 * 可视化页：把字节**解回 IR** 之后按语义渲染。
 *
 * 解出来的东西由后端给（`inspect_exchange`）—— 那里用的是与真实链路同一套 codec，
 * 所以这一页显示的就是模型实际收到/说出的内容。解不开时不白屏：`PayloadView` 会把
 * 原因顶出来并指向「原始」页。
 */
function VisualView({
  rec,
  inspected,
}: {
  rec: ExchangeRecord;
  inspected: InspectedExchange | null;
}) {
  if (!inspected) {
    return (
      <div className="flex items-center gap-2 text-xs text-dim">
        <LoaderCircle size={13} className="animate-spin" />
        正在解读报文…
      </div>
    );
  }
  return (
    <div className="space-y-3">
      {rec.stream_error && (
        <div className="rounded-md border border-[var(--error-border)] bg-[var(--error-bg)] px-2 py-1.5 text-xs text-[var(--danger)]">
          流内错误：{rec.stream_error}
        </div>
      )}
      <PayloadView
        inspected={inspected}
        method={rec.method}
        url={rec.url}
        status={rec.status}
        ttfbMs={rec.ttfb_ms}
        totalMs={rec.total_ms}
        // 帧是原始字节块，空串拼接才等于上游实际吐出的字节流；用换行拼会凭空插入字符。
        streamText={rec.stream_frames.join("")}
        headers={{ request: rec.request_headers, response: rec.response_headers }}
      />
    </div>
  );
}

function RawView({ rec }: { rec: ExchangeRecord }) {
  const streaming = rec.stream_frames.length > 0;
  // 帧是原始字节块，空串拼接才等于上游实际吐出的字节流；用换行拼会凭空插入字符。
  const response = streaming ? rec.stream_frames.join("") : rec.response_body;
  return (
    <div className="space-y-3">
      <div>
        <div className="mb-1 text-[11px] text-dim">请求体（原样）</div>
        <pre className="max-h-[40vh] overflow-auto whitespace-pre-wrap rounded-md border border-edge bg-base p-2 mono text-[11px] leading-relaxed text-ink">
          {rec.request_body || "（空）"}
        </pre>
      </div>
      <div>
        <div className="mb-1 text-[11px] text-dim">
          {streaming ? "流式响应原始字节（按到达顺序原样拼接）" : "响应体（原样）"}
        </div>
        <pre className="max-h-[40vh] overflow-auto whitespace-pre-wrap rounded-md border border-edge bg-base p-2 mono text-[11px] leading-relaxed text-ink">
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
      className={`w-full border-b border-edge px-3 py-2 text-left transition-colors ${
        active ? "bg-card" : "hover:bg-base"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className="text-xs text-ink">
          第 {s.turn ?? "?"} 轮 · 第 {s.round ?? "?"} 次
        </span>
        {label && (
          <span className="rounded bg-tone-violet-soft px-1 py-0.5 text-[10px] text-tone-violet">
            {label}
          </span>
        )}
        <span className="ml-auto">
          <StatusBadge status={s.status} />
        </span>
      </div>
      <div className="mt-1 flex items-center gap-2 text-[11px] text-dim" title="首字节 / 总耗时">
        <Clock size={11} />
        <span className="tabular-nums">
          {fmtMs(s.ttfb_ms)} / {fmtMs(s.total_ms)}
        </span>
        {s.frame_count > 0 && (
          <span className="rounded border border-edge px-1 text-dim">{s.frame_count} 帧</span>
        )}
      </div>
      <div className="mt-0.5 truncate mono text-[10px] text-dim/70">
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
  // 可视化页的数据：后端把字节解回 IR 的结果。与 `record` 分开取 ——
  // 解码要跑一遍协议 codec，只在真要画「可视化」时才值得花这个钱。
  const [inspected, setInspected] = useState<InspectedExchange | null>(null);
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

  // 换会话同样清掉：选中的是**上一条**会话里的报文，带着这个 id 去新会话里查
  // 一定查不到，然后那句「该报文已不存在」就冒出来了 —— 用户不过是换了个对话。
  useEffect(() => {
    setSelected(null);
  }, [sessionId]);

  // 选中某条、且看的是可视化页时，才去解它。
  useEffect(() => {
    if (!selected || !sessionId || view !== "visual") {
      setInspected(null);
      return;
    }
    let cancelled = false;
    setInspected(null);
    inspectExchange(sessionId, selected)
      .then((v) => {
        if (!cancelled) setInspected(v);
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
  }, [selected, sessionId, view, setNotice]);

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
    <div className="fixed inset-y-0 right-0 z-40 flex w-[min(56rem,85vw)] flex-col border-l border-edge bg-sidebar shadow-2xl">
      <div className="flex h-10 items-center gap-2 border-b border-edge px-3">
        <FileSearch size={15} className="text-dim" />
        <span className="text-sm font-semibold">报文</span>
        {turn !== null && (
          <button
            onClick={() => setInspectorTurn(null)}
            title="清除筛选"
            className="inline-flex items-center gap-1 rounded-full border border-edge px-2 py-0.5 text-[11px] text-dim transition-colors hover:text-ink"
          >
            <X size={11} />
            清除筛选 · 只看第 {turn} 轮
          </button>
        )}
        <button
          onClick={closeInspector}
          aria-label="关闭"
          className="ml-auto rounded p-1 text-dim transition-colors hover:text-ink"
        >
          <X size={16} />
        </button>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* 左：交换列表 */}
        <div className="w-64 shrink-0 overflow-y-auto border-r border-edge">
          {list === null && (
            <div className="flex items-center gap-2 px-3 py-4 text-xs text-dim">
              <LoaderCircle size={13} className="animate-spin" />
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
          {groups && groups.length === 0 && !listError && (
            <div className="m-2 rounded-md border border-dashed border-edge px-2 py-3 text-xs leading-relaxed text-dim">
              {turn !== null ? (
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
          {groups?.map((g, gi) => (
            <div key={gi}>
              <div className="sticky top-0 bg-sidebar px-3 py-1 text-[11px] font-medium text-dim backdrop-blur">
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
            <div className="flex h-full flex-col items-center justify-center gap-2 text-sm text-dim">
              <Inbox size={22} />
              从左侧选择一条报文
            </div>
          )}
          {selected && record === undefined && (
            <div className="flex items-center gap-2 p-4 text-xs text-dim">
              <LoaderCircle size={13} className="animate-spin" />
              加载中…
            </div>
          )}
          {selected && record === null && (
            <div className="p-4 text-sm text-dim">
              该报文已不存在（可能已被清理）。
            </div>
          )}
          {record && (
            <div className="space-y-3 p-3">
              <div className="flex items-center gap-2">
                <ViewTabs value={view} onChange={setView} />
                <span className="text-[11px] text-dim">
                  {new Date(record.ts).toLocaleString("zh-CN")}
                </span>
              </div>
              {view === "visual" && <VisualView rec={record} inspected={inspected} />}
              {view === "formatted" && <FormattedView rec={record} />}
              {view === "raw" && <RawView rec={record} />}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
