// 模型在界面上的统一写法：`渠道/显示名`。
//
// 屏幕上出现模型的地方有六七处（会话列表、输入框下方、空状态、侧边聊天、搜索命中），
// 它们手上的原料却各不相同：会话文件里记的是**引用**（`ds/opus`），会话事件里带的是
// **真正发给上游的名字**（`claude-opus-5-5`），models.dev 目录里还有**展示名**
// （`Claude Opus 5.5`）。每处各挑一个来显示，就会出现「左边写 ds/deepseek-flash、
// 下面写 claude-opus-5-5」这种同一件事两种写法的界面。
//
// 所以定一条规则，全部走这里：
//
// * **平时**显示 `渠道/显示名`。显示名取自 models.dev 目录；名字对不上目录时退回
//   配置里的块名 —— 那时引用本身就已经是最好的写法了。
// * **悬停**展开另外两个名字：真正发出去的模型名、以及元数据对应的目录 id。
//   排查「为什么这个模型没按预期计费 / 能力不对」时要的正是这两个。

import { useEffect, useState } from "react";

import { listModels, type ModelRow } from "./api";
import { useStore } from "../store";

/** 引用拆成渠道与块名。没有 `/` 时整串当块名（老配置里可能是裸名字）。 */
export function splitRef(reference: string): { channel: string; name: string } {
  const i = reference.indexOf("/");
  return i < 0
    ? { channel: "", name: reference }
    : { channel: reference.slice(0, i), name: reference.slice(i + 1) };
}

/** 某个引用在配置里的那一行（拿得到展示名、实际名、meta）。 */
export function rowOf(models: ModelRow[], reference: string): ModelRow | undefined {
  return models.find((m) => m.reference === reference);
}

/**
 * 反过来找：只知道真正发给上游的名字时，它在配置里是哪一行。
 *
 * 会话事件（`ready` / `model_changed` / `turn_usage`）与侧边聊天拿到的都是 `api_name`，
 * 而那正是「分不出配的是哪个模型」的那种写法 —— 同一个名字完全可能挂在两个渠道上。
 * 同时命中多条时取排序后的第一条：这里要的是**稳定**，不是「更对」。
 */
export function resolveRef(models: ModelRow[], apiName: string): string {
  if (!apiName) return "";
  const hit = models.find((m) => m.api_name === apiName);
  return hit?.reference ?? apiName;
}

/** 展示写法：`渠道/显示名`。 */
export function displayName(
  reference: string,
  models: ModelRow[],
): string {
  const { channel, name } = splitRef(reference);
  const row = rowOf(models, reference);
  const label = row?.label ?? name;
  return channel ? `${channel}/${label}` : label;
}

/**
 * 悬停时的完整身份：实际请求名 + 元数据 id。
 *
 * 与展示名重复的那几项不列 —— 一张 tooltip 里三行全一样没有意义。
 */
export function modelTitle(reference: string, models: ModelRow[]): string {
  const display = displayName(reference, models);
  const row = rowOf(models, reference);
  const lines = [row ? `模型引用：${reference}` : display];
  const api = row?.api_name ?? splitRef(reference).name;
  if (api && api !== display) lines.push(`实际请求：${api}`);
  if (row?.meta) lines.push(`元数据：${row.meta}`);
  return lines.join("\n");
}

/** 搜索时要能按四种写法都找到：引用、展示名、实际名、meta。 */
export function modelSearchText(reference: string, models: ModelRow[]): string {
  const row = rowOf(models, reference);
  return [reference, row?.label, row?.api_name, row?.meta]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
}

// ---------------------------------------------------------------------------
// 目录缓存
// ---------------------------------------------------------------------------

/**
 * 进程内的一份模型目录。
 *
 * 会话列表里一次显示几十行，每行各发一次 `list_models` 会把侧栏拖垮 —— 而且后端
 * 每次都要把 models.dev 缓存那条 JSON 解析一遍。所以按「配置改过几次」缓存：
 * `config_changed` 之后配置真的变了，才重新拉。
 *
 * 没有做成 store 的字段：它是一份**只读**的派生数据，放 store 里会诱使人去改它，
 * 而唯一的事实来源是 `~/.carter/config.toml`。
 */
const catalog = {
  nonce: -1,
  loading: null as Promise<ModelRow[]> | null,
  rows: [] as ModelRow[],
  listeners: new Set<() => void>(),
};

function ensureCatalog(nonce: number): Promise<ModelRow[]> {
  if (catalog.nonce === nonce && catalog.loading) return catalog.loading;
  catalog.nonce = nonce;
  catalog.loading = listModels()
    .then((rows) => {
      catalog.rows = rows;
      catalog.listeners.forEach((f) => f());
      return rows;
    })
    .catch(() => {
      // 拉不到目录不是错误：模型标签退回显示引用本身，对话照常进行。
      catalog.rows = [];
      catalog.listeners.forEach((f) => f());
      return [];
    });
  return catalog.loading;
}

/** 订阅这份目录。`configNonce` 变了就重拉。 */
export function useModelCatalog(): ModelRow[] {
  const nonce = useStore((s) => s.configNonce);
  const [rows, setRows] = useState<ModelRow[]>(catalog.rows);

  useEffect(() => {
    let cancelled = false;
    const sync = () => !cancelled && setRows(catalog.rows);
    catalog.listeners.add(sync);
    void ensureCatalog(nonce);
    // 已有缓存时不必等 promise —— 直接对齐一次，避免首帧闪成引用写法。
    if (catalog.nonce === nonce && catalog.rows.length > 0) sync();
    return () => {
      cancelled = true;
      catalog.listeners.delete(sync);
    };
  }, [nonce]);

  return rows;
}