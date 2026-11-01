// 渠道编辑框里的「模型」那一栏。
//
// 一行 = 配置里的一段 `[providers.X.models.Y]`。界面上它其实只回答两件事：
//
// 1. **这个模型叫什么**：显示名（块名，`渠道/块名` 就是模型引用的写法，界面各处
//    显示的是它）与实际模型名（真正发给上游的名字，留空 = 显示名）。
// 2. **它是什么规格**：靠 `meta` 绑到 models.dev 目录，展开成上下文窗口、能力
//    （思考 / 工具 / 图片 …）与价格。绑 meta 的全部意义就是这几项 —— 只显示一个
//    展示名等于没绑。
//
// ## 为什么收起
//
// 一个渠道动辄七八个模型，每个都摊开四个输入框会让人找不到主线：用户来这一栏
// 通常是想确认「加了哪些、各自什么规格」，改名字是少数时候。所以默认收起，只摆
// 摘要；新建的行和没绑元数据的行默认展开，因为它们本来就是要填的。
//
// ## 与「模型」页的分工
//
// 这里改的是单个渠道下的模型定义（含实际模型名）；模型页是全部渠道摊平后看
// 规格、设默认 / 副模型。两处读的是同一段配置。

import { useState } from "react";
import {
  ChevronDown,
  ChevronRight,
  CloudDownload,
  ListChecks,
  Plus,
  Search,
  TriangleAlert,
  X,
} from "lucide-react";

import type { CatalogEntry, MetaInfo, ModelDetail, ModelDraft } from "../../lib/api";
import { REASONING_EFFORTS, reasoningIsCustom, reasoningText } from "../../lib/protocols";
import { Button, IconButton } from "../../components/ui/Button";
import { Input, Label } from "../../components/ui/Input";
import { Badge } from "../../components/ui/Panel";
import { Select } from "../../components/ui/Select";
import {
  CatalogError,
  applyPick,
  blockNameFor,
  type Catalog,
} from "./CatalogPicker";
import { MetaFacts, MetaSearchDialog, useMetaInfo } from "./ModelPickers";

/**
 * 模型行的编辑态。
 *
 * `reasoningCustom` 是给配置里手写的 `default_reasoning = { budget = 1024 }` 用的：
 * 那种写法界面表达不了，只能原样留着（提交时传 `null` = 不动那个键），
 * 而不是把它摊平成一个既写不回去、又看着像合法的字符串。
 *
 * `open` 是**这个行的展开状态**，不是配置字段。
 */
export type ModelState = {
  name: string;
  meta: string;
  api_name: string;
  reasoning: string;
  reasoningCustom: boolean;
  open: boolean;
};

export function blankModel(): ModelState {
  return {
    name: "",
    meta: "",
    api_name: "",
    reasoning: "",
    reasoningCustom: false,
    // 新加的行就是要填的，直接展开。
    open: true,
  };
}

export function toState(m: ModelDetail): ModelState {
  return {
    name: m.name,
    meta: m.meta,
    api_name: m.api_name ?? "",
    reasoning: reasoningText(m.default_reasoning),
    reasoningCustom: reasoningIsCustom(m.default_reasoning),
    open: false,
  };
}

export function toDraft(m: ModelState): ModelDraft {
  return {
    name: m.name.trim(),
    meta: m.meta.trim(),
    api_name: m.api_name.trim() || null,
    // 自定义取值传 null —— 那是「别动这个键」的意思。
    default_reasoning: m.reasoningCustom ? null : m.reasoning,
  };
}

/** 目录条目 → 一行模型。块名取目录 id（`.` 换成 `-`），api_name 补上真名。 */
export function rowFromEntry(provider: string, e: CatalogEntry): ModelState {
  const picked = applyPick({ name: "", meta: "", api_name: "" }, provider, e);
  return { ...blankModel(), ...picked, open: false };
}

/** 上游模型名 → 一行模型。块名要合法，真正下发用的名字放进 api_name。 */
export function rowFromUpstream(name: string, meta: string | null): ModelState {
  const block = blockNameFor(name);
  return {
    ...blankModel(),
    name: block,
    api_name: block === name ? "" : name,
    meta: meta ?? "",
    // 没匹配到元数据就展开，让人一眼看到要补什么。
    open: !meta,
  };
}

/**
 * 模型列表：表头那排按钮 + 一行行模型。
 *
 * 状态（models）由调用方持有 —— 保存是渠道编辑框底部那一个按钮的事，这里只管渲染
 * 与就地编辑。
 */
export function ModelList({
  models,
  setModels,
  catalogProvider,
  catalog,
  onFetchUpstream,
  onPickCatalog,
}: {
  models: ModelState[];
  setModels: React.Dispatch<React.SetStateAction<ModelState[]>>;
  /** 认出来的 models.dev 服务商（`null` = 认不出，只能手写 meta）。 */
  catalogProvider: string | null;
  catalog: Catalog;
  onFetchUpstream: () => void;
  onPickCatalog: () => void;
}) {
  // 一次问一整批 meta，避免每行一次 IPC 让弹窗打开时闪一串「加载中」。
  const metaMap = useMetaInfo(models.map((m) => m.meta.trim()).filter(Boolean));
  /** 正在给哪一行搜元数据（`null` = 搜索框关着）。 */
  const [binding, setBinding] = useState<number | null>(null);

  const patch = (i: number, p: Partial<ModelState>) =>
    setModels((ms) => ms.map((x, j) => (j === i ? { ...x, ...p } : x)));
  const remove = (i: number) => setModels((ms) => ms.filter((_, j) => j !== i));

  /** 有块名但没绑元数据的行。它们存不进配置 —— carter 解析模型要靠 meta。 */
  const unbound = models.filter((m) => m.name.trim() && !m.meta.trim());

  return (
    <div className="mb-3 mt-5 border-t border-edge pt-4">
      <div className="mb-2 flex flex-wrap items-center gap-2">
        <Label>模型</Label>
        <span className="text-[11px] text-dim">
          {catalogProvider ? (
            <>
              元数据按 <span className="mono">{catalogProvider}</span> 匹配
            </>
          ) : (
            <>模型引用写作 <span className="mono">渠道名/显示名</span></>
          )}
        </span>
        <div className="ml-auto flex items-center gap-1.5">
          <Button size="sm" onClick={onFetchUpstream}>
            <CloudDownload size={12} />
            从上游拉取
          </Button>
          {catalogProvider && (
            <Button size="sm" onClick={onPickCatalog}>
              <ListChecks size={12} />
              从目录选
            </Button>
          )}
          <Button size="sm" onClick={() => setModels((ms) => [...ms, blankModel()])}>
            <Plus size={12} />
            添加
          </Button>
        </div>
      </div>

      <CatalogError catalog={catalog} />

      {unbound.length > 0 && (
        <div className="mb-2 flex items-start gap-2 rounded-lg border border-[var(--warn-border)] bg-[var(--warn-bg)] px-3 py-2 text-xs leading-relaxed">
          <TriangleAlert size={13} className="icon-optical shrink-0 text-tone-orange" />
          <span>
            有 {unbound.length} 个模型没绑元数据（
            <span className="mono">{unbound.map((m) => m.name).join("、")}</span>
            ）。carter 要靠 meta 解析模型，保存前得给它们各绑一个 —— 名字对不上目录时，
            用行尾的「绑元数据」搜一个。
          </span>
        </div>
      )}

      {models.length === 0 ? (
        <div className="rounded-lg border border-dashed border-edge px-3 py-6 text-center text-xs text-dim">
          还没有模型。用「从上游拉取」问渠道自己要，或者从目录里选。
        </div>
      ) : (
        <div className="space-y-2">
          {models.map((m, i) => (
            <ModelRow
              key={i}
              m={m}
              info={m.meta.trim() ? metaMap[m.meta.trim()] : undefined}
              onPatch={(p) => patch(i, p)}
              onRemove={() => remove(i)}
              onBind={() => setBinding(i)}
            />
          ))}
        </div>
      )}

      {binding !== null && models[binding] && (
        <MetaSearchDialog
          title="给这一行绑元数据"
          initial={models[binding].name || models[binding].api_name}
          onClose={() => setBinding(null)}
          onPick={(hit) => {
            // 绑元数据**只换 meta**：实际名说的是「发什么给上游」，与元数据无关。
            // 只有显示名还空着时才顺手填一个，让人看得出这行是哪个模型。
            patch(binding, {
              name: models[binding].name.trim() || blockNameFor(hit.id),
              meta: hit.meta,
            });
            setBinding(null);
          }}
        />
      )}
    </div>
  );
}

/** 一行模型：收起时是摘要，展开时是编辑框。 */
function ModelRow({
  m,
  info,
  onPatch,
  onRemove,
  onBind,
}: {
  m: ModelState;
  /** 目录摘要。`undefined` = 还在查；`null` = 查不到。 */
  info: MetaInfo | null | undefined;
  onPatch: (p: Partial<ModelState>) => void;
  onRemove: () => void;
  onBind: () => void;
}) {
  const name = m.name.trim();
  const apiName = m.api_name.trim() || name;
  // 标题优先用目录里的展示名（「Claude Haiku 4.5」比 `claude-haiku-4-5` 好读；
  // 名字对不上目录时才退回实际模型名）。
  const title = info?.label ?? (apiName || "新模型");
  const bound = m.meta.trim() !== "";
  const broken = bound && info === null;
  // 一行都还没填的空壳不该报警 —— 那个「未绑元数据」说的是「填了名字却没绑」。
  const missing = !bound && name !== "";

  return (
    <div
      className={`rounded-lg border bg-base transition-colors ${
        missing || broken ? "border-[var(--warn-border)]" : "border-edge"
      }`}
    >
      <div className="flex items-start gap-1.5 px-2 py-2">
        <button
          type="button"
          onClick={() => onPatch({ open: !m.open })}
          aria-label={m.open ? "收起这一行" : "展开编辑这一行"}
          className="icon-optical shrink-0 rounded p-0.5 text-dim transition-colors hover:text-ink"
        >
          {m.open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        </button>

        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <span className={`truncate text-xs font-medium ${name ? "text-ink" : "text-dim"}`}>
              {title}
            </span>
            {missing && <Badge tone="orange">未绑元数据</Badge>}
            {broken && <Badge tone="orange">meta 查不到</Badge>}
            {info?.deprecated && <Badge tone="orange">已弃用</Badge>}
            {m.reasoningCustom && <Badge>自定义推理</Badge>}
          </div>

          {/* 引用名 → 实际模型名 → meta。三者都是名字，放在同一行里对照着看。 */}
          <div className="mono mt-0.5 truncate text-[11px] text-dim" title={m.meta || undefined}>
            <span className="text-ink">{name || "（未命名）"}</span>
            {apiName && apiName !== name && <span> → {apiName}</span>}
            {bound && <span className="ml-1.5">· {m.meta.trim()}</span>}
          </div>

          {/* 空壳行展开着就是要填的，别再叠一句「未绑定」；收起状态才需要提示。 */}
          {(bound || (!m.open && name)) && (
            <div className="mt-1">
              <MetaFacts meta={m.meta} info={info} hideLabel />
            </div>
          )}
        </div>

        <div className="flex shrink-0 items-center gap-0.5">
          <button
            type="button"
            onClick={onBind}
            className="flex items-center gap-1 rounded px-1.5 py-1 text-[11px] text-dim transition-colors hover:text-ink"
            title="在 models.dev 目录里搜一个元数据绑上来"
          >
            <Search size={10} />
            {bound ? "改绑定" : "绑元数据"}
          </button>
          <IconButton title="删除这个模型" onClick={onRemove}>
            <X size={12} />
          </IconButton>
        </div>
      </div>

      {m.open && (
        <div className="space-y-2.5 border-t border-edge px-2.5 py-2.5">
          <div className="grid grid-cols-2 gap-2">
            <label className="block">
              <span className="text-[11px] text-dim">显示名（引用名）</span>
              <Input
                mono
                value={m.name}
                onChange={(e) => onPatch({ name: e.target.value })}
                placeholder="如 haiku"
                className="mt-0.5 py-1 text-xs"
              />
              <span className="mono mt-0.5 block truncate text-[10px] text-dim">
                {name ? `模型引用：渠道名/${name}` : "只能用字母、数字、-、_"}
              </span>
            </label>
            <label className="block">
              <span className="text-[11px] text-dim">实际模型名</span>
              <Input
                mono
                value={m.api_name}
                onChange={(e) => onPatch({ api_name: e.target.value })}
                placeholder={name || "留空 = 显示名"}
                className="mt-0.5 py-1 text-xs"
              />
              <span className="mt-0.5 block text-[10px] text-dim">真正发给上游的名字</span>
            </label>
          </div>

          <label className="block">
            <span className="flex items-baseline justify-between gap-2">
              <span className="text-[11px] text-dim">元数据 meta</span>
              <span className="text-[10px] text-dim">models.dev 的查找 id</span>
            </span>
            <Input
              mono
              value={m.meta}
              onChange={(e) => onPatch({ meta: e.target.value })}
              placeholder="服务商/模型id，如 anthropic/claude-sonnet-4-5"
              className="mt-0.5 py-1 text-xs"
            />
          </label>

          <div className="flex items-center gap-2">
            <span className="shrink-0 text-[11px] text-dim">默认推理</span>
            {m.reasoningCustom ? (
              <span
                className="rounded-md border border-dashed border-edge px-2 py-0.5 text-[11px] text-dim"
                title="这个取值界面改不了，去配置文件里改"
              >
                配置里是自定义取值
              </span>
            ) : (
              <Select
                className="w-40"
                value={m.reasoning}
                onChange={(v) => onPatch({ reasoning: v })}
                options={REASONING_EFFORTS.map((r) => ({ value: r.value, label: r.label }))}
              />
            )}
            <span className="text-[10px] text-dim">留空 = 不写这个键</span>
          </div>
        </div>
      )}
    </div>
  );
}