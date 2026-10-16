// 从 models.dev 目录里挑模型。
//
// 官方渠道本该只填密钥就能用，卡住人的是 `meta`：它必须和 models.dev 的 `id`
// 一字不差，而 id 和展示名常常对不上（「DeepSeek V4.1 Flash」的 id 是
// `deepseek-flash`）。所以有目录的渠道一律让人「选」，手写 meta 只留给中转与自定义模型。

import { useEffect, useState } from "react";
import { RefreshCw } from "lucide-react";

import { listCatalogModels, refreshModelCatalog, type CatalogEntry } from "../../lib/api";
import { fmtTokens } from "../../lib/format";
import { toastError } from "../../lib/toast";
import { Button } from "../../components/ui/Button";
import { Banner } from "../../components/ui/Panel";
import { Select } from "../../components/ui/Select";

export type Catalog = {
  /** `null` = 这个协议没有对应的 models.dev 服务商，或者还没拉回来。 */
  entries: CatalogEntry[] | null;
  error: string | null;
  refreshing: boolean;
  refresh: () => void;
};

/** 拉某个 models.dev 服务商下的模型。`providerId` 为空串时什么都不做。 */
export function useCatalog(providerId: string): Catalog {
  const [entries, setEntries] = useState<CatalogEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    setEntries(null);
    setError(null);
    if (!providerId) return;
    let cancelled = false;
    listCatalogModels(providerId)
      .then((list) => !cancelled && setEntries(list))
      .catch((e) => !cancelled && setError(String(e)));
    return () => {
      cancelled = true;
    };
  }, [providerId, nonce]);

  const refresh = () => {
    setRefreshing(true);
    refreshModelCatalog()
      .then(() => setNonce((n) => n + 1))
      .catch(toastError)
      .finally(() => setRefreshing(false));
  };

  return { entries, error, refreshing, refresh };
}

/** 目录里第一个没弃用的模型 —— 新建官方渠道时的默认选择。 */
export function firstLive(entries: CatalogEntry[] | null): CatalogEntry | undefined {
  return entries?.find((e) => !e.deprecated) ?? entries?.[0];
}

/** 目录读不到（多半是还没有缓存）时的提示条，带一个就地刷新的按钮。 */
export function CatalogError({ catalog }: { catalog: Catalog }) {
  if (!catalog.error) return null;
  return (
    <div className="mb-3">
      <Banner
        kind="warn"
        action={
          <Button size="sm" onClick={catalog.refresh} disabled={catalog.refreshing}>
            <RefreshCw size={12} className={catalog.refreshing ? "animate-spin" : ""} />
            拉取目录
          </Button>
        }
      >
        读不到模型目录，只能手写 meta：{catalog.error}
      </Banner>
    </div>
  );
}

/**
 * 目录下拉。`value` 是当前 meta 的模型 id 部分；不在目录里时显示占位文本。
 * 弃用的模型只在当前正选着它时才列出来 —— 不推荐，但也不能让已有配置「消失」。
 */
export function CatalogPicker({
  entries,
  value,
  onPick,
  className = "",
}: {
  entries: CatalogEntry[];
  value: string;
  onPick: (e: CatalogEntry) => void;
  className?: string;
}) {
  const shown = entries.filter((e) => !e.deprecated || e.id === value);
  return (
    <Select
      className={className}
      value={value}
      placeholder="从目录选择模型…"
      onChange={(id) => {
        const hit = entries.find((e) => e.id === id);
        if (hit) onPick(hit);
      }}
      options={shown.map((e) => ({
        value: e.id,
        label: e.deprecated ? `${e.name}（已弃用）` : e.name,
        hint: [e.id, e.context_window ? fmtTokens(e.context_window) : null]
          .filter(Boolean)
          .join(" · "),
      }))}
    />
  );
}

/** 模型 id 里可能有 `.`（`gpt-4.1`），而块名只能用字母、数字、`-`、`_`。 */
export function blockNameFor(id: string): string {
  return id.replace(/[^A-Za-z0-9_-]/g, "-");
}

/** meta 里属于这个服务商的模型 id；不是这个服务商的返回空串。 */
export function catalogIdOf(meta: string, metaProvider: string): string {
  const prefix = `${metaProvider}/`;
  return metaProvider && meta.startsWith(prefix) ? meta.slice(prefix.length) : "";
}

/**
 * 从目录选中一个模型后，这一行该变成什么样。
 *
 * 块名只在「还是空的」或「是上一次自动填的」时才跟着换 —— 用户自己起的名字
 * （比如 `sonnet`）可能已经被 `agent.model` 引用着，悄悄改掉会让默认模型失效。
 * 块名和模型 id 不一样时补上 api_name，保证下发给端点的是真名。
 */
export function applyPick(
  row: { name: string; meta: string; api_name: string },
  metaProvider: string,
  e: CatalogEntry,
): { name: string; meta: string; api_name: string } {
  const prevId = catalogIdOf(row.meta, metaProvider);
  const auto = row.name.trim() === "" || (prevId !== "" && row.name === blockNameFor(prevId));
  const name = auto ? blockNameFor(e.id) : row.name;
  return { name, meta: `${metaProvider}/${e.id}`, api_name: name === e.id ? "" : e.id };
}
