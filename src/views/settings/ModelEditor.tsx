// 单个模型的编辑弹窗。
//
// 与「渠道」页那个大编辑框的分工：那边一次管一个渠道的全部模型，这边只管一个，
// 用的人是「我已经知道要加哪个模型」而不是「我要从头配一个渠道」。
// 两者写的是同一段配置（`[providers.X.models.Y]`），只是入口不同。
//
// 所以这里的字段是同一个模型行在渠道编辑框里的那几栏：块名、下发名、meta、
// 推理档位。meta 一样可以从目录里选、也可以在目录里搜出来绑上。
//
// 「这个渠道是哪家官方」由渠道列表里的 base_url 决定（而不是只看 kind）：
// 把一个 kind=anthropic 的渠道指到中转站上时，用 anthropic 的目录会把名字逐个对错。

import { useEffect, useMemo, useState } from "react";
import { Search } from "lucide-react";

import {
  deleteModel,
  detectCatalogProvider,
  listChannels,
  saveModel,
  type ChannelRow,
  type ModelRow,
} from "../../lib/api";
import {
  REASONING_EFFORTS,
  presetFor,
  reasoningIsCustom,
  reasoningText,
} from "../../lib/protocols";
import { toastError } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { Button } from "../../components/ui/Button";
import { Field, Input } from "../../components/ui/Input";
import { ConfirmDialog, Modal } from "../../components/ui/Panel";
import { Select } from "../../components/ui/Select";
import {
  CatalogError,
  CatalogPicker,
  applyPick,
  blockNameFor,
  catalogIdOf,
  useCatalog,
} from "./CatalogPicker";
import { MetaFacts, MetaSearchDialog, useMetaInfo } from "./ModelPickers";

export function ModelEditor({
  /** 给了就是在编辑这一条；`null` 是新增。 */
  editing,
  onClose,
}: {
  editing: ModelRow | null;
  onClose: (changed: boolean) => void;
}) {
  const channels = useAsync(listChannels, []);

  const [provider, setProvider] = useState(editing?.provider ?? "");
  const [name, setName] = useState(editing?.name ?? "");
  const [meta, setMeta] = useState(editing?.meta ?? "");
  const [apiName, setApiName] = useState(editing?.api_name ?? "");
  const [reasoning, setReasoning] = useState(reasoningText(editing?.default_reasoning));
  // 配置里手写的 `{ budget = 1024 }` 界面表达不了 —— 只展示、不改写。
  const reasoningCustom = reasoningIsCustom(editing?.default_reasoning);
  // 编辑时块名是这条记录的身份，改了就是另一个模型 —— 挡住。
  const nameLocked = editing !== null;

  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [searching, setSearching] = useState(false);

  // 渠道列表可能还没拉回来（或者编辑的渠道刚被删掉），用编辑目标兜底一个空壳，
  // 否则下拉会是空的、而当前值又不在选项里。
  const options = useMemo<ChannelRow[]>(() => {
    const list = channels.data ?? [];
    if (provider && !list.some((c) => c.name === provider)) {
      return [
        {
          name: provider,
          kind: "",
          protocol: "",
          base_url: "",
          has_api_key: false,
          unsupported_server_tools: [],
          supported_server_tools: [],
          models: [],
          is_default: false,
        },
        ...list,
      ];
    }
    return list;
  }, [channels.data, provider]);

  const channel = options.find((c) => c.name === provider);
  const providerKind = channel?.kind ?? "";

  // 「这个渠道对应目录里的哪家」：有 base_url 就以它为准（认不出就是没有目录），
  // 没填地址才回落到协议预设 —— 与渠道编辑框同一条规则。
  const [catalogProvider, setCatalogProvider] = useState<string | null>(null);
  useEffect(() => {
    const baseUrl = channel?.base_url?.trim() ?? "";
    if (!baseUrl) {
      setCatalogProvider(presetFor(providerKind)?.metaProvider || null);
      return;
    }
    let cancelled = false;
    detectCatalogProvider(baseUrl)
      .then((p) => !cancelled && setCatalogProvider(p))
      .catch(() => !cancelled && setCatalogProvider(null));
    return () => {
      cancelled = true;
    };
  }, [channel?.base_url, providerKind]);

  const catalog = useCatalog(catalogProvider ?? "");
  const pickedId = catalogIdOf(meta, catalogProvider ?? "");
  const infos = useMetaInfo(meta.trim() ? [meta.trim()] : []);
  const info = meta.trim() ? infos[meta.trim()] : undefined;

  const valid = provider.trim() !== "" && name.trim() !== "" && meta.trim() !== "";

  async function submit() {
    setSaving(true);
    try {
      await saveModel(provider, {
        name: name.trim(),
        meta: meta.trim(),
        api_name: apiName.trim() || null,
        // 自定义取值传 null = 别动这个键；否则空串 = 删掉它。
        default_reasoning: reasoningCustom ? null : reasoning,
      });
      onClose(true);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  async function doDelete() {
    setSaving(true);
    try {
      await deleteModel(provider, name);
      onClose(true);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal
      title={editing ? `编辑模型 · ${editing.reference}` : "添加模型"}
      onClose={() => onClose(false)}
      width={480}
      footer={
        <>
          {editing && (
            <Button
              variant="danger"
              className="mr-auto"
              onClick={() => setConfirmDelete(true)}
              disabled={saving}
            >
              删除模型
            </Button>
          )}
          <Button onClick={() => onClose(false)} disabled={saving}>
            取消
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || saving}>
            {saving ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      <Field label="渠道" hint="模型必须挂在某个渠道下">
        <Select
          value={provider}
          onChange={setProvider}
          disabled={nameLocked}
          placeholder="选一个渠道"
          options={options.map((c) => ({ value: c.name, label: c.name }))}
        />
      </Field>

      <CatalogError catalog={catalog} />
      {catalog.entries && (
        <Field label="从目录选择" hint="选中后自动填好下面三项">
          <CatalogPicker
            entries={catalog.entries}
            value={catalog.entries.some((e) => e.id === pickedId) ? pickedId : ""}
            onPick={(e) => {
              const next = applyPick({ name, meta, api_name: apiName }, catalogProvider ?? "", e);
              // 编辑时块名是这条记录的身份，不能跟着换 —— 那就靠 api_name 指到真名上。
              const block = nameLocked ? name : next.name;
              setName(block);
              setMeta(next.meta);
              setApiName(block === e.id ? "" : e.id);
            }}
          />
        </Field>
      )}

      <Field label="块名" hint="模型引用会是 渠道名/块名，只能用字母、数字、-、_">
        <Input
          mono
          value={name}
          onChange={(e) => setName(e.target.value)}
          disabled={nameLocked}
          placeholder="例如 sonnet"
        />
      </Field>

      <Field
        label="meta"
        hint={
          <button
            onClick={() => setSearching(true)}
            className="inline-flex items-center gap-1 text-dim transition-colors hover:text-ink"
          >
            <Search size={10} />
            在目录里搜
          </button>
        }
      >
        <Input
          mono
          value={meta}
          onChange={(e) => setMeta(e.target.value)}
          placeholder="例如 anthropic/claude-sonnet-4-5"
        />
      </Field>
      <div className="-mt-2 mb-3">
        <MetaFacts meta={meta} info={info} />
      </div>

      <Field label="下发名 api_name" hint="留空 = 用块名">
        <Input
          mono
          value={apiName}
          onChange={(e) => setApiName(e.target.value)}
          placeholder="真正发给上游的模型名"
        />
      </Field>

      <Field
        label="默认推理档位"
        hint={reasoningCustom ? "配置里是自定义取值，界面改不了" : "留空 = 不写这个键"}
      >
        {reasoningCustom ? (
          <div className="rounded-md border border-dashed border-edge px-3 py-2 text-sm text-dim">
            配置里写的是{" "}
            <span className="mono">{JSON.stringify(editing?.default_reasoning)}</span>
            ，这种形态界面表达不了 —— 要改请编辑配置文件。
          </div>
        ) : (
          <Select
            value={reasoning}
            onChange={setReasoning}
            options={REASONING_EFFORTS.map((r) => ({ value: r.value, label: r.label }))}
          />
        )}
      </Field>

      {/* 顺手把现有渠道下有哪些模型列出来 —— 免得在同一个块名上重复添加。 */}
      {editing === null && provider && (
        <ExistingModels models={options.find((c) => c.name === provider)?.models ?? []} />
      )}

      {searching && (
        <MetaSearchDialog
          title="在目录里搜一个 meta"
          initial={name || apiName}
          onClose={() => setSearching(false)}
          onPick={(hit) => {
            // 只换 meta（以及空着的块名）。下发名是「发什么给上游」，与元数据无关 ——
            // 手动搜 meta 的场合恰恰是名字对不上，这时候拿目录 id 去覆盖它最容易错。
            setName((n) => n.trim() || blockNameFor(hit.id));
            setMeta(hit.meta);
            setSearching(false);
          }}
        />
      )}

      {confirmDelete && editing && (
        <ConfirmDialog
          title="删除模型"
          danger
          confirmLabel="确认删除"
          message={
            <>
              将从配置里移除 <span className="mono text-ink">{editing.reference}</span>。
              <strong className="font-medium">不可恢复</strong>。
              {editing.is_default && (
                <>
                  <br />
                  它当前是默认模型，删掉之后新会话会因为没有默认模型而起不来 ——
                  记得另选一个。
                </>
              )}
            </>
          }
          onCancel={() => setConfirmDelete(false)}
          onConfirm={() => {
            setConfirmDelete(false);
            void doDelete();
          }}
        />
      )}
    </Modal>
  );
}

/** 当前渠道下已有的模型名。纯提示，不可点。 */
function ExistingModels({ models }: { models: string[] }) {
  if (models.length === 0) return null;
  return (
    <div className="mt-1 flex flex-wrap items-center gap-1 text-[11px] text-dim">
      <span>该渠道下已有：</span>
      {models.map((n) => (
        <span key={n} className="mono rounded bg-bubble px-1.5 py-0.5">
          {n}
        </span>
      ))}
    </div>
  );
}