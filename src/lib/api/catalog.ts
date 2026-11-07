import { invoke } from "@tauri-apps/api/core";
import type { CatalogEntry } from "./assistant";

// ---------------------------------------------------------------------------
// 上游模型 / 元数据匹配 / 渠道模板
// ---------------------------------------------------------------------------

/** 上游一个模型名对到目录之后的结论。镜像 `catalog.rs` 的 `MetaMatch`。 */
export type MetaMatch = {
  /** 上游给的模型名，原样。 */
  name: string;
  /** `服务商/模型id`。推断不出来是 `null` —— 界面要让人手动绑。 */
  meta: string | null;
  /**
   * 推断的可靠度。`exact`（名字与目录 id 一致）/ `alias`（归一化或 canonical 对上）/
   * `reseller`（只有转售商条目）/ `none`（没对上）。
   */
  match_kind: "exact" | "alias" | "reseller" | "none";
  label: string | null;
  context_window: number | null;
  max_output_tokens: number | null;
  /** 能力标记，与 `ModelRow.capabilities` 同一套字符串。 */
  capabilities: string[];
  deprecated: boolean;
  /** 看起来像不像对话模型（embedding / 语音 / 生图是 `false`）。 */
  chat_capable: boolean;
};

/** 「从上游拉取模型」的结果。镜像 `upstream_models.rs` 的 `UpstreamModels`。 */
export type UpstreamModels = {
  /** 实际请求的 URL —— 出错时第一件要看的东西。 */
  url: string;
  /** 从 base_url 认出来的目录服务商（匹配元数据时的提示）。 */
  provider_hint: string | null;
  models: MetaMatch[];
};

/**
 * 向渠道的上游要模型清单（`GET {base_url}/v1/models`）并对好元数据。
 *
 * `baseUrl` 留空时用该 `kind` 的协议默认地址。`apiKey` 留空 = 沿用已存的密钥
 * （编辑已有渠道时就是这样：界面手里没有明文密钥）。
 */
export function fetchUpstreamModels(
  kind: string,
  baseUrl: string,
  apiKey: string,
  provider: string | null,
): Promise<UpstreamModels> {
  return invoke<UpstreamModels>("fetch_upstream_models", { kind, baseUrl, apiKey, provider });
}

/** 跨服务商搜目录条目（手动绑元数据用）。空查询返回空表。 */
export function searchCatalogModels(query: string): Promise<CatalogHit[]> {
  return invoke<CatalogHit[]>("search_catalog_models", { query });
}

/** 目录条目的可读摘要（镜像 `catalog.rs` 的 `MetaInfo`）。 */
export type MetaInfo = {
  /** 目录里的规范写法 —— 大小写写错时会纠正过来。 */
  meta: string;
  label: string;
  context_window: number | null;
  max_output_tokens: number | null;
  /** 能力标记，与 `ModelRow.capabilities` 同一套字符串。 */
  capabilities: string[];
  /** 每百万 token 的美元价。目录条目没录 `cost` 时是 `null` —— 那不是免费。 */
  pricing: {
    input: number;
    output: number;
    cache_read: number | null;
    cache_write: number | null;
  } | null;
  deprecated: boolean;
};

/**
 * 一批 meta 在目录里查得到什么。查不到的位置是 `null`（不是错误）——
 * 编辑框据此把那一行标成「未绑定」，而不是等保存时才炸。
 */
export function lookupMetas(metas: string[]): Promise<(MetaInfo | null)[]> {
  return invoke<(MetaInfo | null)[]>("lookup_metas", { metas });
}

export type CatalogHit = {
  /** `服务商/模型id`，可以直接写进 meta。 */
  meta: string;
  provider: string;
  id: string;
  name: string;
  context_window: number | null;
  max_output_tokens: number | null;
  /** 能力标记，与 `ModelRow.capabilities` 同一套字符串。 */
  capabilities: string[];
  deprecated: boolean;
};

/** 一个渠道模板（官方服务商的 kind / base_url / 目录 id）。 */
export type ProviderTemplate = {
  id: string;
  label: string;
  kind: string;
  protocol: string;
  base_url: string;
  name: string;
  catalog: string | null;
  note: string;
};

export function listProviderTemplates(): Promise<ProviderTemplate[]> {
  return invoke<ProviderTemplate[]>("list_provider_templates");
}

/**
 * base_url 对应 models.dev 的哪家（认不出来是 `null`）。
 *
 * 决定「模型能不能从目录直接预填」。与请求发去哪无关：请求永远走配置里的 base_url。
 */
export function detectCatalogProvider(baseUrl: string): Promise<string | null> {
  return invoke<string | null>("detect_catalog_provider", { baseUrl });
}

/**
 * 某个目录服务商下「值得预先填进渠道」的几个模型（同 family 只要最新的，弃用的
 * 与 embedding / 生图都不要）。还没有缓存时 reject。
 */
export function recommendedCatalogModels(providerId: string): Promise<CatalogEntry[]> {
  return invoke<CatalogEntry[]>("recommended_catalog_models", { providerId });
}
