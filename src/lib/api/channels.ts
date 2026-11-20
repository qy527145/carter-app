import { invoke } from "@tauri-apps/api/core";
import type { AppSettings } from "./app";

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
  /** 是不是副模型（`agent.fast_model`）：辅助场景默认指向的那个，见场景模型。 */
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
 * 设副模型（辅助场景默认指向它）。传 `null` 清除。
 */
export function setFastModel(modelRef: string | null): Promise<void> {
  return invoke<void>("set_fast_model", { modelRef });
}

// ---------------------------------------------------------------------------
// 场景模型（镜像 `scene_models.rs`）
// ---------------------------------------------------------------------------

/** 回落链上的一环：场景自己的绑定 / 副模型 / 主模型 / 发起调用的那条会话的模型。 */
export type SceneStep = "scene" | "fast" | "main" | "session";

/** 取值里的指针记号。具体模型一律是 `渠道/模型`。 */
export const SCENE_MAIN = "@main";
export const SCENE_FAST = "@fast";

/** 回落链上的一格。`model` 为 `null` = 没设，或这一格是「会话模型」。 */
export interface SceneLink {
  step: SceneStep;
  model: string | null;
}

export interface SceneRow {
  id: string;
  label: string;
  description: string;
  /** 此刻的取值：指针 `@main` / `@fast`，或具体的 `渠道/模型`。 */
  choice: string;
  /** 场景默认的取值；普通对话是 `null`（它就是主模型本身）。 */
  default_choice: string | null;
  /** 回落链，从取值开始。 */
  chain: SceneLink[];
  /** 链上实际生效的那一格；`null` = 一个都用不上。 */
  effective: number | null;
  /** 绑定存在哪：`config`（CLI 也认）| `assistant`（助理设置）| `app`（仅本应用）。 */
  storage: "config" | "assistant" | "app";
}

export interface SceneModelsView {
  main: string;
  fast: string | null;
  scenes: SceneRow[];
}

export function getSceneModels(): Promise<SceneModelsView> {
  return invoke<SceneModelsView>("scene_models");
}

/**
 * 改一个场景的取值（指针或具体模型），`null` = 恢复默认。回传新的 `AppSettings`，
 * 调用方要 `setSettings` 对齐（`set_app_settings` 是整体覆盖写入）。
 */
export function setSceneModel(scene: string, value: string | null): Promise<AppSettings> {
  return invoke<AppSettings>("set_scene_model", { scene, modelRef: value });
}

/** 自动压缩：开关 + 触发比例（占上下文窗口的比例，0.3–0.95）。 */
export interface CompactionSettings {
  enabled: boolean;
  ratio: number;
}

/** 前台命令超时之后：转到后台继续跑，或结束它。 */
export type OnTimeout = "background" | "kill";

/** 前台 bash 命令的超时策略（`agent.bash_timeout_secs` / `agent.bash_on_timeout`，carter CLI 也认）。 */
export interface BashTimeoutSettings {
  secs: number;
  on_timeout: OnTimeout;
}

export function getBashTimeout(): Promise<BashTimeoutSettings> {
  return invoke<BashTimeoutSettings>("get_bash_timeout");
}

export function setBashTimeout(secs: number, onTimeout: OnTimeout): Promise<void> {
  return invoke<void>("set_bash_timeout", { secs, onTimeout });
}

export function getCompaction(): Promise<CompactionSettings> {
  return invoke<CompactionSettings>("get_compaction");
}

export function setCompaction(enabled: boolean, ratio: number): Promise<void> {
  return invoke<void>("set_compaction", { enabled, ratio });
}

/**
 * 单次请求的最大输出 tokens（`agent.max_output_tokens`）。
 *
 * 这是**本机配的值**，不是模型本身的输出上限（后者见模型目录的 `max_output_tokens`）。
 * `null` = 配置里没写，请求不带这个字段、由渠道决定。
 */
export function getMaxOutputTokens(): Promise<number | null> {
  return invoke<number | null>("get_max_output_tokens");
}

export function setMaxOutputTokens(tokens: number): Promise<void> {
  return invoke<void>("set_max_output_tokens", { tokens });
}
