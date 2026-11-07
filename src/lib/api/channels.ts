import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// 渠道 / 模型 / 协议
// ---------------------------------------------------------------------------

export type ChannelRow = {
  name: string;
  /** 配置里写的 kind，可能是别名或兼容写法。 */
  kind: string;
  /** 实际走的线协议 —— 与 kind 不同时说明走了别名映射。 */
  protocol: string;
  base_url: string;
  has_api_key: boolean;
  /** 该渠道声明了、但其协议不认的服务端工具（会被静默忽略）。 */
  unsupported_server_tools: string[];
  /** 该协议确实支持的服务端工具。 */
  supported_server_tools: string[];
  models: string[];
  is_default: boolean;
};

export type ModelDetail = {
  name: string;
  meta: string;
  api_name: string | null;
  /**
   * **原样的 TOML 值**：字符串（`"medium"`）或表（`{ budget = 1024 }`）。
   * 表形态界面表达不了，编辑时要原样放过 —— 见 `lib/protocols.ts`。
   */
  default_reasoning: unknown;
};

export type ProviderDetail = {
  name: string;
  kind: string;
  /** **原始文本**，可能还是 `${ANTHROPIC_API_KEY}` 这种引用。 */
  api_key: string;
  has_api_key: boolean;
  base_url: string;
  server_tools: string[];
  server_tool_max_uses: number | null;
  models: ModelDetail[];
};

export type ModelDraft = {
  name: string;
  meta: string;
  api_name?: string | null;
  default_reasoning?: string | null;
};

export type ProviderDraft = {
  name: string;
  kind: string;
  /**
   * **`null` = 保持原样。** 界面不回传已存的密钥，于是「不碰它」这个意图
   * 在协议层就是明确的。传空串则是「清掉」。
   */
  api_key: string | null;
  base_url: string | null;
  server_tools: string[];
  server_tool_max_uses: number | null;
  /** 整表对齐：不在这个列表里的模型会被删掉。 */
  models: ModelDraft[];
  /** 编辑时的原渠道名；与 `name` 不同即改名。 */
  rename_from?: string | null;
};

/** 全部模型引用 + 从 models.dev 解出来的元数据。 */
export type ModelRow = {
  /** `provider/model`。 */
  reference: string;
  provider: string;
  name: string;
  api_name: string;
  meta: string;
  /**
   * models.dev 目录里的展示名（「Claude Opus 5.5」）。界面上的模型标签用它 ——
   * 用户配的是 `opus`、请求里发的是 `claude-opus-5-5`，都不如这个名字好认。
   * 没绑 meta 或目录里查不到时是 `null`，那时退回显示引用本身。
   */
  label: string | null;
  is_default: boolean;
  /** 是不是副模型（`agent.fast_model`）：压缩上下文、生成标题用的那个。 */
  is_fast: boolean;
  protocol: string;
  /** 配置里给这个模型指定的默认推理档位，原样（字符串或表）。 */
  default_reasoning: unknown;
  context_window: number | null;
  max_output_tokens: number | null;
  capabilities: string[];
  pricing: {
    input: number;
    output: number;
    cache_read: number | null;
    cache_write: number | null;
  } | null;
  /** 解析元数据失败的原因。有值时上面几栏是空的，但这一行仍然有效。 */
  error: string | null;
};

export function listChannels(): Promise<ChannelRow[]> {
  return invoke<ChannelRow[]>("list_channels");
}

export function getProviderDetail(name: string): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("get_provider_detail", { name });
}

/**
 * 某个协议能吃下哪些服务端工具，返回 `[生效的, 不生效的]`。
 *
 * 直接问底座而不是在前端维护一张协议 → 工具名的表：那张表会和实现悄悄漂移，
 * 而这里给出的答案永远是真的。
 */
export function checkServerTools(kind: string, tools: string[]): Promise<[string[], string[]]> {
  return invoke<[string[], string[]]>("check_server_tools", { kind, tools });
}

export function saveProvider(draft: ProviderDraft): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("save_provider", { draft });
}

export function deleteProvider(name: string): Promise<void> {
  return invoke<void>("delete_provider", { name });
}

/** 新增或覆盖一个模型（按块名 upsert），返回写回后的渠道详情。 */
export function saveModel(provider: string, model: ModelDraft): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("save_model", { provider, model });
}

export function deleteModel(provider: string, model: string): Promise<ProviderDetail> {
  return invoke<ProviderDetail>("delete_model", { provider, model });
}

export function listModels(): Promise<ModelRow[]> {
  return invoke<ModelRow[]>("list_models");
}

/**
 * 设副模型（压缩上下文、生成标题用）。传 `null` 清除、回落主模型。
 */
export function setFastModel(modelRef: string | null): Promise<void> {
  return invoke<void>("set_fast_model", { modelRef });
}

/** 自动压缩：开关 + 触发比例（占上下文窗口的比例，0.3–0.95）。 */
export interface CompactionSettings {
  enabled: boolean;
  ratio: number;
}

export function getCompaction(): Promise<CompactionSettings> {
  return invoke<CompactionSettings>("get_compaction");
}

export function setCompaction(enabled: boolean, ratio: number): Promise<void> {
  return invoke<void>("set_compaction", { enabled, ratio });
}
