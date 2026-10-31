// 模型选择相关的三件套：从上游拉、从目录挑、手动搜 meta。
//
// 渠道编辑框里「模型」那一栏的三条路，对应这里的三个对话框：
//
// 1. **从上游拉取**（[`UpstreamFetchDialog`]）—— 中转站 / OpenRouter / 各家兼容端点都有
//    `GET /v1/models`。拉回来的是**上游模型名**，后端顺手对好了 models.dev 元数据，
//    这里只负责勾选。名字对不上元数据的那些照常能加，只是要人去手动绑。
// 2. **从目录挑**（[`CatalogDialog`]）—— 官方渠道（base_url 认得出是哪家）的模型名与
//    目录 id 一一对应，不用联网问，直接从目录里勾。推荐的那几个排在最前。
// 3. **手动搜**（[`MetaSearchDialog`]）—— 名字对不上时，跨服务商搜一个 meta 绑上去。
//
// 三个对话框都是「选完立刻回填、由调用方负责关」，不在这里保存 —— 保存只有一个
// 入口（渠道编辑框底部的「保存」），否则用户取消编辑时会有一些行已经落盘了。

import { useEffect, useMemo, useState } from "react";
import {
  CircleAlert,
  CloudDownload,
  LoaderCircle,
  RefreshCw,
  Search,
  TriangleAlert,
} from "lucide-react";

import {
  fetchUpstreamModels,
  listCatalogModels,
  lookupMetas,
  recommendedCatalogModels,
  searchCatalogModels,
  type CatalogEntry,
  type CatalogHit,
  type MetaInfo,
  type MetaMatch,
} from "../../lib/api";
import { fmtTokens } from "../../lib/format";
import { Button } from "../../components/ui/Button";
import { Input } from "../../components/ui/Input";
import { Modal, Spinner } from "../../components/ui/Panel";

/** 匹配可靠度的中文标签与色调。`none` 不在这里 —— 那种行不该被默认勾上。 */
export function matchLabel(kind: string): string {
  switch (kind) {
    case "exact":
      return "同名";
    case "alias":
      return "推断";
    case "reseller":
      return "转售条目";
    default:
      return "未匹配";
  }
}

/**
 * 一批 meta 的目录摘要。`null` = 目录里查不到（缓存缺失或写错了）。
 *
 * 用一次往返拿一整批：渠道编辑框里往往有十几行，逐行往返会让界面在打开时闪一串
 * 「加载中」。查不到不报错 —— 那一行会显示「未绑定」，用户可以点开重新绑。
 */
export function useMetaInfo(metas: string[]): Record<string, MetaInfo | null> {
  const [map, setMap] = useState<Record<string, MetaInfo | null>>({});
  // 把数组做进依赖里：调用方每次渲染都会给一个新数组，直接依赖会让 effect 每帧都跑。
  const key = metas.filter(Boolean).join("\n");

  useEffect(() => {
    const list = key ? key.split("\n") : [];
    if (list.length === 0) {
      setMap({});
      return;
    }
    let cancelled = false;
    lookupMetas(list)
      .then((infos) => {
        if (cancelled) return;
        const next: Record<string, MetaInfo | null> = {};
        list.forEach((m, i) => {
          next[m] = infos[i] ?? null;
        });
        setMap(next);
      })
      // 查不了目录不是致命错误：那一行退化成「只有 meta 字符串」，用户照样能保存。
      .catch(() => !cancelled && setMap({}));
    return () => {
      cancelled = true;
    };
  }, [key]);

  return map;
}

/** 一行模型在界面上的一句话说明：目录展示名 · 窗口。查不到时给一句提示。 */
export function metaSummary(meta: string, info: MetaInfo | null | undefined): {
  text: string;
  warn: boolean;
} {
  if (!meta.trim()) return { text: "未绑定元数据 —— 点右侧手动绑一个", warn: true };
  if (info === undefined) return { text: "查元数据中…", warn: false };
  if (info === null) return { text: "目录里查不到这个 meta（写错了？）", warn: true };
  const parts = [info.label];
  if (info.context_window) parts.push(fmtTokens(info.context_window));
  if (info.deprecated) parts.push("已弃用");
  return { text: parts.join(" · "), warn: info.deprecated };
}

// ---------------------------------------------------------------------------
// 从上游拉取
// ---------------------------------------------------------------------------

/** 一条可勾选的上游行。`checked` 是自己维护的，因为「默认勾哪些」要在数据回来时决定。 */
type UpstreamRow = MetaMatch & { checked: boolean };

/** 哪些行默认勾上：能对上元数据的、像对话模型的、没弃用的。 */
function shouldPreselect(m: MetaMatch): boolean {
  return m.chat_capable && !m.deprecated && m.match_kind !== "none";
}

export function UpstreamFetchDialog({
  kind,
  baseUrl,
  apiKey,
  provider,
  onClose,
  onApply,
}: {
  kind: string;
  baseUrl: string;
  apiKey: string;
  /** 编辑已有渠道时传渠道名 —— 空密钥时后端会用它读已存的那份。 */
  provider: string | null;
  onClose: () => void;
  onApply: (rows: { name: string; meta: string | null }[]) => void;
}) {
  const [rows, setRows] = useState<UpstreamRow[] | null>(null);
  const [url, setUrl] = useState("");
  const [hint, setHint] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [search, setSearch] = useState("");
  const [onlyChat, setOnlyChat] = useState(true);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    fetchUpstreamModels(kind, baseUrl, apiKey, provider)
      .then((res) => {
        if (cancelled) return;
        setUrl(res.url);
        setHint(res.provider_hint);
        setRows(res.models.map((m) => ({ ...m, checked: shouldPreselect(m) })));
      })
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [kind, baseUrl, apiKey, provider, nonce]);

  const shown = useMemo(() => {
    const list = rows ?? [];
    const q = search.trim().toLowerCase();
    return list.filter((r) => {
      if (onlyChat && !r.chat_capable) return false;
      if (!q) return true;
      return (
        r.name.toLowerCase().includes(q) ||
        (r.meta ?? "").toLowerCase().includes(q) ||
        (r.label ?? "").toLowerCase().includes(q)
      );
    });
  }, [rows, search, onlyChat]);

  const checkedCount = (rows ?? []).filter((r) => r.checked).length;
  const unmatched = (rows ?? []).filter((r) => r.checked && r.match_kind === "none").length;

  const toggle = (i: number) =>
    setRows((rs) => rs && rs.map((r, j) => (j === i ? { ...r, checked: !r.checked } : r)));

  const setShown = (on: boolean) => {
    const names = new Set(shown.map((r) => r.name));
    setRows((rs) => rs && rs.map((r) => (names.has(r.name) ? { ...r, checked: on } : r)));
  };

  return (
    <Modal
      title="从上游拉取模型"
      width={620}
      onClose={onClose}
      footer={
        <>
          <span className="mr-auto text-[11px] text-dim">
            已选 <span className="tabular-nums text-ink">{checkedCount}</span> 个
            {unmatched > 0 && `，其中 ${unmatched} 个没匹配到元数据`}
          </span>
          <Button onClick={onClose}>取消</Button>
          <Button
            variant="primary"
            disabled={checkedCount === 0}
            onClick={() =>
              onApply(
                (rows ?? [])
                  .filter((r) => r.checked)
                  .map((r) => ({ name: r.name, meta: r.meta })),
              )
            }
          >
            添加 {checkedCount} 个
          </Button>
        </>
      }
    >
      {loading ? (
        <div className="flex items-center gap-2 py-8 text-sm text-dim">
          <Spinner />
          正在问上游要模型列表…
        </div>
      ) : error ? (
        <div className="space-y-3">
          <div className="flex items-start gap-2 rounded-lg border border-[var(--error-border)] bg-[var(--error-bg)] px-3 py-2 text-xs leading-relaxed">
            <CircleAlert size={13} className="mt-0.5 shrink-0 text-[var(--danger)]" />
            <span>{error}</span>
          </div>
          <p className="text-[11px] leading-relaxed text-dim">
            很多服务商没有 <span className="mono">/v1/models</span>（或者不对外开放）。这种
            情况可以改用「从目录选」或手动添加 —— 名字填对就行，元数据随时能补。
          </p>
          <Button size="sm" onClick={() => setNonce((n) => n + 1)}>
            <RefreshCw size={12} />
            重试
          </Button>
        </div>
      ) : (
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <span className="flex items-center gap-1.5 text-[11px] text-dim">
              <CloudDownload size={12} className="shrink-0" />
              共 {rows?.length ?? 0} 个
              {hint && (
                <>
                  ，元数据优先按 <span className="mono text-ink">{hint}</span> 匹配
                </>
              )}
            </span>
          </div>
          <div className="mono truncate text-[10px] text-dim" title={url}>
            GET {url}
          </div>

          <div className="flex items-center gap-2">
            <div className="relative min-w-0 flex-1">
              <Search size={12} className="absolute left-2 top-1/2 -translate-y-1/2 text-dim" />
              <Input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder="筛选模型名"
                className="py-1.5 pl-7 text-xs"
              />
            </div>
            <label className="flex shrink-0 items-center gap-1 text-[11px] text-dim">
              <input
                type="checkbox"
                checked={onlyChat}
                onChange={(e) => setOnlyChat(e.target.checked)}
              />
              只看对话模型
            </label>
          </div>

          <div className="flex items-center gap-3 text-[11px]">
            <button className="text-dim hover:text-ink" onClick={() => setShown(true)}>
              全选当前 {shown.length} 个
            </button>
            <button className="text-dim hover:text-ink" onClick={() => setShown(false)}>
              取消选择
            </button>
          </div>

          <div className="max-h-[45vh] space-y-1 overflow-y-auto pr-1">
            {shown.length === 0 && (
              <div className="py-6 text-center text-xs text-dim">没有符合筛选的模型</div>
            )}
            {shown.map((r) => {
              const i = (rows ?? []).indexOf(r);
              return (
                <button
                  key={r.name}
                  onClick={() => toggle(i)}
                  className="flex w-full items-start gap-2 rounded-md border border-edge bg-base px-2.5 py-1.5 text-left transition-colors hover:border-dim"
                >
                  <input
                    type="checkbox"
                    checked={r.checked}
                    onChange={() => toggle(i)}
                    className="mt-0.5 shrink-0"
                  />
                  <span className="min-w-0 flex-1">
                    <span className="mono block truncate text-xs text-ink">{r.name}</span>
                    <span className="block truncate text-[11px] text-dim">
                      {r.meta ? (
                        <>
                          {matchLabel(r.match_kind)} · {r.meta}
                          {r.label && r.label !== r.meta.split("/").pop() && ` · ${r.label}`}
                          {r.context_window ? ` · ${fmtTokens(r.context_window)}` : ""}
                        </>
                      ) : (
                        <span className="text-tone-orange">没匹配到元数据，加进去后可以手动绑</span>
                      )}
                      {r.deprecated && <span className="ml-1 text-tone-orange">已弃用</span>}
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// 从目录挑
// ---------------------------------------------------------------------------

/**
 * 官方渠道的模型多选。
 *
 * 「推荐」= 后端按 family 收敛过的那几个（同家族只要最新、跳过弃用与非对话模型）——
 * 官方目录动辄几十条快照，全列出来只会让人更慢。搜索框照样能翻到全部。
 */
export function CatalogDialog({
  providerId,
  onClose,
  onApply,
}: {
  providerId: string;
  onClose: () => void;
  onApply: (entries: CatalogEntry[]) => void;
}) {
  const [recommended, setRecommended] = useState<CatalogEntry[]>([]);
  const [all, setAll] = useState<CatalogEntry[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    Promise.all([recommendedCatalogModels(providerId), listCatalogModels(providerId)])
      .then(([rec, full]) => {
        if (cancelled) return;
        setRecommended(rec);
        setAll(full);
        setPicked(new Set(rec.map((e) => e.id)));
      })
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [providerId]);

  const q = search.trim().toLowerCase();
  const shown = q
    ? (all ?? []).filter(
        (e) => e.id.toLowerCase().includes(q) || e.name.toLowerCase().includes(q),
      )
    : recommended;

  const toggle = (id: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  return (
    <Modal
      title="从目录选择模型"
      width={580}
      onClose={onClose}
      footer={
        <>
          <span className="mr-auto text-[11px] text-dim">
            已选 <span className="tabular-nums text-ink">{picked.size}</span> 个
          </span>
          <Button onClick={onClose}>取消</Button>
          <Button
            variant="primary"
            disabled={picked.size === 0}
            onClick={() =>
              onApply((all ?? []).filter((e) => picked.has(e.id)))
            }
          >
            添加 {picked.size} 个
          </Button>
        </>
      }
    >
      {loading ? (
        <div className="flex items-center gap-2 py-8 text-sm text-dim">
          <Spinner />
          读模型目录…
        </div>
      ) : error ? (
        <div className="space-y-2">
          <div className="flex items-start gap-2 rounded-lg border border-[var(--error-border)] bg-[var(--error-bg)] px-3 py-2 text-xs leading-relaxed">
            <CircleAlert size={13} className="mt-0.5 shrink-0 text-[var(--danger)]" />
            <span>{error}</span>
          </div>
          <p className="text-[11px] text-dim">模型页有「刷新模型元数据」按钮，拉一次目录就好。</p>
        </div>
      ) : (
        <div className="space-y-2">
          <div className="relative">
            <Search size={12} className="absolute left-2 top-1/2 -translate-y-1/2 text-dim" />
            <Input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={`搜索 ${providerId} 名下的全部 ${all?.length ?? 0} 个模型`}
              className="py-1.5 pl-7 text-xs"
            />
          </div>
          {!q && (
            <div className="text-[11px] text-dim">
              默认勾上的是推荐模型（每个系列取最新、已弃用与 embedding / 生图不在列）。
              搜索可以翻到全部。
            </div>
          )}
          <div className="max-h-[45vh] space-y-1 overflow-y-auto pr-1">
            {shown.length === 0 && (
              <div className="py-6 text-center text-xs text-dim">没有匹配的模型</div>
            )}
            {shown.map((e) => (
              <button
                key={e.id}
                onClick={() => toggle(e.id)}
                className="flex w-full items-center gap-2 rounded-md border border-edge bg-base px-2.5 py-1.5 text-left transition-colors hover:border-dim"
              >
                <input
                  type="checkbox"
                  checked={picked.has(e.id)}
                  onChange={() => toggle(e.id)}
                  className="shrink-0"
                />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-xs text-ink">{e.name}</span>
                  <span className="mono block truncate text-[11px] text-dim">
                    {e.id}
                    {e.context_window ? ` · ${fmtTokens(e.context_window)}` : ""}
                  </span>
                </span>
                {e.deprecated && <span className="shrink-0 text-[10px] text-tone-orange">已弃用</span>}
              </button>
            ))}
          </div>
        </div>
      )}
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// 手动搜 meta
// ---------------------------------------------------------------------------

/**
 * 跨服务商搜一个 meta 绑到某一行上。
 *
 * 这是「名字对不上时」的出口：中转站常常把模型叫成 `deepseek-v4.1-flash`，而目录里
 * 同一个东西叫 `deepseek-flash`。搜一下就能绑上，比让人去翻 models.dev 网站强。
 */
export function MetaSearchDialog({
  title,
  initial,
  onClose,
  onPick,
}: {
  title: string;
  initial?: string;
  onClose: () => void;
  onPick: (hit: CatalogHit) => void;
}) {
  const [query, setQuery] = useState(initial ?? "");
  const [hits, setHits] = useState<CatalogHit[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // 输入停下 250ms 再搜：每敲一个字母打一次近万条的目录没必要。
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setHits([]);
      return;
    }
    let cancelled = false;
    setLoading(true);
    const timer = setTimeout(() => {
      searchCatalogModels(q)
        .then((list) => !cancelled && setHits(list))
        .catch((e) => !cancelled && setError(String(e)))
        .finally(() => !cancelled && setLoading(false));
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [query]);

  return (
    <Modal
      title={title}
      width={540}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
        </>
      }
    >
      <div className="relative mb-2">
        {loading ? (
          <LoaderCircle
            size={12}
            className="absolute left-2 top-1/2 -translate-y-1/2 animate-spin text-dim"
          />
        ) : (
          <Search size={12} className="absolute left-2 top-1/2 -translate-y-1/2 text-dim" />
        )}
        <Input
          autoFocus
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜模型名或 ID，如 deepseek-v4、claude-sonnet"
          className="py-1.5 pl-7 text-xs"
        />
      </div>
      <p className="mb-2 flex items-start gap-1.5 text-[11px] leading-relaxed text-dim">
        <TriangleAlert size={12} className="mt-0.5 shrink-0 text-tone-orange" />
        元数据决定上下文窗口、能力与定价。中转站的模型名往往与目录 id 不同，选对那一行
        比名字看着像更重要。
      </p>
      {error && <div className="mb-2 text-xs text-[var(--danger)]">{error}</div>}
      <div className="max-h-[45vh] space-y-1 overflow-y-auto pr-1">
        {hits.map((h) => (
          <button
            key={h.meta}
            onClick={() => onPick(h)}
            className="flex w-full items-center gap-2 rounded-md border border-edge bg-base px-2.5 py-1.5 text-left transition-colors hover:border-dim"
          >
            <span className="min-w-0 flex-1">
              <span className="block truncate text-xs text-ink">{h.name}</span>
              <span className="mono block truncate text-[11px] text-dim">
                {h.meta}
                {h.context_window ? ` · ${fmtTokens(h.context_window)}` : ""}
              </span>
            </span>
            {h.deprecated && <span className="shrink-0 text-[10px] text-tone-orange">已弃用</span>}
          </button>
        ))}
        {!loading && query.trim() && hits.length === 0 && !error && (
          <div className="py-6 text-center text-xs text-dim">目录里没有匹配的条目</div>
        )}
      </div>
    </Modal>
  );
}