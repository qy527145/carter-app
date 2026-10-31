// 渠道编辑弹窗：预设、协议、密钥、服务端工具、模型列表。
//
// ## 渠道模板
//
// 新建时先给一排「这是哪家」（Anthropic / DeepSeek / Kimi …）。选一家就把协议、
// base_url、模型列表**一起填好** —— 用户只剩填密钥。模板表在后端
// （`carter-app-core/src/templates.rs`），因为它要和 models.dev 的服务商 id 对齐。
//
// ## 模型从哪来
//
// 三条路，对应 `ModelPickers.tsx` 里的三个对话框：
// 1. **从上游拉**（`GET /v1/models`）：中转站与大多数官方都支持，拉回来的是上游
//    真实模型名，后端顺手对好了元数据。
// 2. **从目录选**：base_url 认得出是哪家官方时，模型名与目录 id 一一对应，
//    不用联网问。
// 3. **手动绑**：名字对不上时，跨服务商搜一个 meta 绑到那一行上。
//
// `meta` 是 models.dev 的查找 id，决定上下文窗口、能力与定价。没有它模型照样能跑，
// 只是这几栏空着 —— 所以界面上「未绑定」是提示，不是错误。
//
// ## 密钥的三态
//
// 弹窗**不回传**已存的密钥。打开时 `api_key` 输入框是空的，placeholder 显示
// 「已配置（留空则不改）」。于是「用户没碰密钥」这件事在提交的数据里是
// `null`，后端据此保持原样 —— 而不是把界面读到的值（可能是 `${VAR}`，
// 也可能是插值后的明文）再写回去。见 core 的 `config_edit.rs`。

import { useEffect, useMemo, useState } from "react";
import {
  CircleCheck,
  CloudDownload,
  ListChecks,
  Plus,
  Search,
  TriangleAlert,
  X,
} from "lucide-react";

import {
  checkServerTools,
  deleteProvider,
  detectCatalogProvider,
  getProviderDetail,
  listChannels,
  listProviderTemplates,
  recommendedCatalogModels,
  saveProvider,
  type CatalogEntry,
  type ModelDetail,
  type ModelDraft,
  type ProviderDetail,
  type ProviderDraft,
  type ProviderTemplate,
} from "../../lib/api";
import {
  PROTOCOLS,
  REASONING_EFFORTS,
  UNKNOWN_PROTOCOL_HINT,
  presetFor,
  reasoningIsCustom,
  reasoningText,
} from "../../lib/protocols";
import { useAsync } from "../../lib/useAsync";
import { toastError } from "../../lib/toast";
import { CatalogError, applyPick, blockNameFor, useCatalog } from "./CatalogPicker";
import {
  CatalogDialog,
  MetaSearchDialog,
  UpstreamFetchDialog,
  metaSummary,
  useMetaInfo,
} from "./ModelPickers";
import { Button, IconButton } from "../../components/ui/Button";
import { Field, Input, Label, Switch } from "../../components/ui/Input";
import { Banner, ConfirmDialog, Modal } from "../../components/ui/Panel";
import { Select } from "../../components/ui/Select";

/**
 * 模型行的编辑态。
 *
 * `reasoningCustom` 是给配置里手写的 `default_reasoning = { budget = 1024 }` 用的：
 * 那种写法界面表达不了，只能原样留着（提交时传 `null` = 不动那个键），
 * 而不是把它摊平成一个既写不回去、又看着像合法的字符串。
 */
type ModelState = {
  name: string;
  meta: string;
  api_name: string;
  reasoning: string;
  reasoningCustom: boolean;
  /** 展开手写的 meta / api_name 输入框。默认收起，只显示绑定状态。 */
  manual: boolean;
};

function blankModel(): ModelState {
  return { name: "", meta: "", api_name: "", reasoning: "", reasoningCustom: false, manual: false };
}

function toState(m: ModelDetail): ModelState {
  return {
    name: m.name,
    meta: m.meta,
    api_name: m.api_name ?? "",
    reasoning: reasoningText(m.default_reasoning),
    reasoningCustom: reasoningIsCustom(m.default_reasoning),
    manual: false,
  };
}

function toDraft(m: ModelState): ModelDraft {
  return {
    name: m.name.trim(),
    meta: m.meta.trim(),
    api_name: m.api_name.trim() || null,
    // 自定义取值传 null —— 那是「别动这个键」的意思。
    default_reasoning: m.reasoningCustom ? null : m.reasoning,
  };
}

/** 目录条目 → 一行模型。块名取目录 id（`.` 换成 `-`），api_name 补上真名。 */
function rowFromEntry(provider: string, e: CatalogEntry): ModelState {
  const picked = applyPick({ name: "", meta: "", api_name: "" }, provider, e);
  return { ...blankModel(), ...picked };
}

/** 上游模型名 → 一行模型。块名要合法，真正下发用的名字放进 api_name。 */
function rowFromUpstream(name: string, meta: string | null): ModelState {
  const block = blockNameFor(name);
  return {
    ...blankModel(),
    name: block,
    api_name: block === name ? "" : name,
    meta: meta ?? "",
    // 没匹配到元数据就展开手写框，让人一眼看到要补什么。
    manual: !meta,
  };
}

export function ProviderEditor({
  /** `null` = 新建。 */
  editing,
  onClose,
}: {
  editing: string | null;
  onClose: (changed: boolean) => void;
}) {
  const [loaded, setLoaded] = useState<ProviderDetail | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  // 表单。新建时先给一份空壳。
  // 新建时渠道名先跟着默认协议走，选别的协议会跟着换（见 changeKind）。
  const [name, setName] = useState(editing ?? "anthropic");
  const [kind, setKind] = useState("anthropic");
  // 新建时要防重名：保存是按名字 upsert 的，撞上已有渠道会把它整个覆盖掉。
  const [existing, setExisting] = useState<string[]>([]);
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  // 留空 = 不改；想真的清掉已存的密钥得明说。两种意图不能共用一个空串。
  const [clearKey, setClearKey] = useState(false);
  const [serverTools, setServerTools] = useState<string[]>([]);
  const [maxUses, setMaxUses] = useState("");
  const [models, setModels] = useState<ModelState[]>([blankModel()]);
  const [customTool, setCustomTool] = useState("");

  const [unsupported, setUnsupported] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  /** 用户点过的模板（只用于高亮）。 */
  const [templateId, setTemplateId] = useState<string | null>(null);
  /** 当前 baseUrl 对应 models.dev 的哪家 —— 模型能不能从目录里选就看它。 */
  const [catalogProvider, setCatalogProvider] = useState<string | null>(null);
  /** 正在给哪一行选元数据（`null` = 没开搜索框）。 */
  const [metaEditing, setMetaEditing] = useState<number | null>(null);
  /** 打开「从上游拉取」/「从目录多选」。 */
  const [picker, setPicker] = useState<"upstream" | "catalog" | null>(null);

  const templates = useAsync(listProviderTemplates, []);

  // 编辑时拉原始配置回填。
  useEffect(() => {
    if (!editing) {
      setModels([blankModel()]);
      listChannels()
        .then((cs) => setExisting(cs.map((c) => c.name)))
        .catch(() => setExisting([]));
      return;
    }
    let cancelled = false;
    getProviderDetail(editing)
      .then((d) => {
        if (cancelled) return;
        setLoaded(d);
        setKind(d.kind);
        setBaseUrl(d.base_url);
        setServerTools(d.server_tools);
        setMaxUses(d.server_tool_max_uses ? String(d.server_tool_max_uses) : "");
        setModels(d.models.length > 0 ? d.models.map(toState) : [blankModel()]);
        // api_key 刻意不回填：留空即「不改」。
      })
      .catch((e) => !cancelled && setLoadError(String(e)));
    return () => {
      cancelled = true;
    };
  }, [editing]);

  // 勾了哪些工具、当前协议认不认。直接问后端 —— 前端自己维护这张表会漂移。
  useEffect(() => {
    let cancelled = false;
    checkServerTools(kind, serverTools)
      .then(([, bad]) => !cancelled && setUnsupported(bad))
      .catch(() => !cancelled && setUnsupported([]));
    return () => {
      cancelled = true;
    };
  }, [kind, serverTools]);

  // 认「这个地址是哪家」。地址留空时回落到协议预设的目录（官方渠道不填地址也认得出）；
  // 填了地址就必须以地址为准 —— 用户完全可能把 kind=anthropic 指到中转站上，
  // 那时用 anthropic 的目录会把中转站的模型名一个个对错。
  useEffect(() => {
    if (!baseUrl.trim()) {
      setCatalogProvider(presetFor(kind)?.metaProvider || null);
      return;
    }
    let cancelled = false;
    // 每敲一个字母打一次 IPC 没必要，也避免请求乱序覆盖。
    const timer = setTimeout(() => {
      detectCatalogProvider(baseUrl)
        .then((p) => !cancelled && setCatalogProvider(p))
        .catch(() => !cancelled && setCatalogProvider(null));
    }, 400);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [baseUrl, kind]);

  const preset = presetFor(kind);
  const nameLocked = editing !== null;
  const catalog = useCatalog(catalogProvider ?? "");

  const metaMap = useMetaInfo(models.map((m) => m.meta).filter(Boolean));

  const patchModel = (i: number, patch: Partial<ModelState>) =>
    setModels((ms) => ms.map((x, j) => (j === i ? { ...x, ...patch } : x)));

  const addRows = (rows: ModelState[]) =>
    setModels((ms) => {
      // 空壳行会被下面的过滤去掉；同名块也不再重复加 —— 重复的块名在 TOML 里是覆盖。
      // `fresh` 内部也要去重：上游可能同时给出 `a.b` 与 `a-b`，块名归一后撞车。
      const taken = new Set(ms.map((m) => m.name.trim()).filter(Boolean));
      const fresh = rows.filter((r) => {
        const k = r.name.trim();
        if (!k || taken.has(k)) return false;
        taken.add(k);
        return true;
      });
      const kept = ms.filter((m) => m.name.trim() || m.meta.trim());
      return [...kept, ...fresh];
    });

  /**
   * 套用一个模板：协议、地址、渠道名一起填好，再把目录里的推荐模型填进列表 ——
   * 用户只剩填密钥。模板行只在新建时出现，所以这里不用管编辑中的渠道。
   *
   * 渠道名只在「还是空的」或「还是上一个模板的默认名」时才换：用户自己起过名字
   * （甚至已经改成了 `ds`）不该被覆盖。
   */
  const applyTemplate = async (t: ProviderTemplate) => {
    setTemplateId(t.id);
    const autoName = name.trim() === "" || templates.data?.some((x) => x.name === name);
    if (autoName) setName(t.name);
    setKind(t.kind);
    setBaseUrl(t.base_url);

    if (!t.catalog) {
      setModels([blankModel()]);
      return;
    }
    try {
      const entries = await recommendedCatalogModels(t.catalog);
      setModels(
        entries.length > 0 ? entries.map((e) => rowFromEntry(t.catalog!, e)) : [blankModel()],
      );
    } catch {
      // 目录还没拉过、或者缓存坏了：留一行空的，用户可以用「从上游拉取」问上游。
      setModels([blankModel()]);
    }
  };

  // 新建官方渠道：目录回来了、用户还没动过任何一行 —— 预选目录里最新的那个。
  // 模板没覆盖到的（比如手动选了 kind = anthropic）走这条路。
  useEffect(() => {
    if (nameLocked || !catalog.entries || templateId) return;
    const first = catalog.entries.find((e) => !e.deprecated);
    if (!first) return;
    setModels((ms) =>
      ms.length === 1 && !ms[0].name.trim() && !ms[0].meta.trim()
        ? [rowFromEntry(catalogProvider ?? "", first)]
        : ms,
    );
  }, [catalog.entries, catalogProvider, nameLocked, templateId]);

  // 新建时渠道名跟着协议走、换了协议就把目录预填的模型清掉。
  // 有一行是用户手写的就整个不动。
  const changeKind = (k: string) => {
    if (!nameLocked && (name.trim() === "" || name === kind)) setName(k);
    setTemplateId(null);
    if (!nameLocked) {
      setModels((ms) =>
        ms.every((m) => !m.name.trim() && !m.meta.trim()) ? [blankModel()] : ms,
      );
    }
    setKind(k);
  };

  const duplicate = !nameLocked && existing.includes(name.trim());

  /** 有块名但没绑元数据的行。它们存不进配置 —— carter 解析模型要靠 meta。 */
  const unbound = models.filter((m) => m.name.trim() && !m.meta.trim());

  const valid = useMemo(() => {
    if (!name.trim() || duplicate) return false;
    if (!kind.trim()) return false;
    // 模型行允许整行留空（用户点了「添加」还没填），但不能有半填的。
    return models.every((m) => {
      const empty = !m.name.trim() && !m.meta.trim();
      return empty || (m.name.trim() !== "" && m.meta.trim() !== "");
    });
  }, [name, kind, models, duplicate]);

  const addTool = (t: string) => {
    const v = t.trim();
    if (v && !serverTools.includes(v)) setServerTools([...serverTools, v]);
  };

  async function submit() {
    setSaving(true);
    try {
      const draft: ProviderDraft = {
        name: name.trim(),
        kind: kind.trim(),
        // 三态：明确要清 → 空串；敲了东西 → 那个值；什么都没动 → null（保持原样）。
        api_key: clearKey ? "" : apiKey === "" ? null : apiKey,
        // 空 base_url 就是「删掉这个键」，carter 会回落到协议的默认地址。
        base_url: baseUrl.trim(),
        server_tools: serverTools,
        server_tool_max_uses: maxUses.trim() ? Number(maxUses.replace(/\D/g, "")) : null,
        models: models.filter((m) => m.name.trim() && m.meta.trim()).map(toDraft),
      };
      await saveProvider(draft);
      onClose(true);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  async function doDelete() {
    if (!editing) return;
    setSaving(true);
    try {
      await deleteProvider(editing);
      onClose(true);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  // 已知的工具名（当前协议的快选 + 各协议常见名字的并集），供 chip 快选。
  const knownTools = useMemo(() => {
    const set = new Set<string>();
    for (const p of PROTOCOLS) for (const t of p.serverTools) set.add(t);
    for (const t of preset?.serverTools ?? []) set.add(t);
    return [...set];
  }, [preset]);

  return (
    <Modal
      title={editing ? `编辑渠道 · ${editing}` : "新建渠道"}
      onClose={() => onClose(false)}
      width={620}
      footer={
        <>
          {editing && (
            <Button
              variant="danger"
              className="mr-auto"
              onClick={() => setConfirmDelete(true)}
              disabled={saving}
            >
              删除渠道
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
      {loadError && (
        <div className="mb-3">
          <Banner kind="error">{loadError}</Banner>
        </div>
      )}

      {/* 预设 ------------------------------------------------------------ */}
      {!nameLocked && (
        <Field label="预设" hint="选一家，协议与地址自动填好，模型也会预填">
          <div className="flex flex-wrap gap-1.5">
            {(templates.data ?? []).map((t) => (
              <button
                key={t.id}
                type="button"
                title={t.note}
                onClick={() => void applyTemplate(t)}
                className={`rounded-md border px-2 py-1 text-xs transition-colors ${
                  templateId === t.id
                    ? "border-dim bg-base text-ink"
                    : "border-edge text-dim hover:border-dim hover:text-ink"
                }`}
              >
                {t.label}
              </button>
            ))}
            <button
              type="button"
              onClick={() => {
                setTemplateId(null);
                setName("");
                setKind("openai_chat");
                setBaseUrl("");
                setModels([blankModel()]);
              }}
              className={`rounded-md border border-dashed px-2 py-1 text-xs transition-colors ${
                templateId === null
                  ? "border-dim text-ink"
                  : "border-edge text-dim hover:border-dim hover:text-ink"
              }`}
            >
              自定义 / 中转站
            </button>
          </div>
          <div className="mt-1.5 text-[11px] leading-relaxed text-dim">
            {(templates.data ?? []).find((t) => t.id === templateId)?.note ??
              "中转站（new-api / one-api 等）选「自定义」，填上地址后点「从上游拉取模型」。"}
          </div>
        </Field>
      )}

      <Field
        label="渠道名"
        hint={
          nameLocked
            ? "改名要新建一个渠道再删掉旧的"
            : duplicate
              ? <span className="text-[var(--danger)]">已有同名渠道，换一个名字或去编辑它</span>
              : "只能用字母、数字、-、_（它会变成配置里的表键）"
        }
      >
        <Input
          mono
          value={name}
          onChange={(e) => setName(e.target.value)}
          disabled={nameLocked}
          placeholder="例如 anthropic、my-proxy"
        />
      </Field>

      <Field
        label="协议"
        hint={
          preset
            ? `走 ${preset.protocol}`
            : "carter 不认识这个 Kind，按 Chat Completions 处理"
        }
      >
        <Select
          value={kind}
          onChange={changeKind}
          options={PROTOCOLS.map((p) => ({
            value: p.kind,
            label: p.label,
            hint: p.kind,
          }))}
        />
      </Field>
      {!preset && kind.trim() !== "" && (
        <div className="mb-3">
          <Banner kind="warn">
            <span className="flex items-start gap-2">
              <TriangleAlert size={13} className="mt-0.5 shrink-0" />
              {UNKNOWN_PROTOCOL_HINT}
            </span>
          </Banner>
        </div>
      )}

      <Field
        label="Base URL"
        hint={preset?.defaultBaseUrl ? `留空用 ${preset.defaultBaseUrl}` : "这个协议必须自己填"}
      >
        <Input
          mono
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
          placeholder={preset?.defaultBaseUrl || "https://…"}
        />
      </Field>

      <Field
        label="API Key"
        hint={
          loaded?.has_api_key
            ? "已配置（留空则不改）"
            : catalogProvider
              ? "官方渠道填上密钥就能用；也可填 ${ENV_VAR}"
              : "可填 ${ENV_VAR}"
        }
      >
        <Input
          mono
          type="password"
          value={clearKey ? "" : apiKey}
          onChange={(e) => {
            setClearKey(false);
            setApiKey(e.target.value);
          }}
          disabled={clearKey}
          placeholder={loaded?.has_api_key ? "••••••（留空则不改）" : "${MY_API_KEY} 或直接填密钥"}
        />
      </Field>
      {loaded?.has_api_key && (
        <div className="-mt-2 mb-3 flex items-center gap-2 text-[11px] text-dim">
          <span className="min-w-0 flex-1 truncate">
            当前存的是 <span className="mono">{loaded.api_key || "（空）"}</span>
            ，留空就不会动它。
          </span>
          <button
            onClick={() => setClearKey((c) => !c)}
            className={`shrink-0 rounded border px-1.5 py-0.5 transition-colors ${
              clearKey
                ? "border-[var(--error-border)] text-[var(--danger)]"
                : "border-edge hover:text-ink"
            }`}
          >
            {clearKey ? "将清空（点此撤销）" : "清空"}
          </button>
        </div>
      )}

      <Field
        label="服务端工具"
        hint={unsupported.length > 0 ? "标红的不生效" : "厂商在服务端执行，本地不跑"}
      >
        <div className="flex flex-wrap gap-1.5">
          {serverTools.length === 0 && (
            <span className="text-xs text-dim">未声明任何服务端工具</span>
          )}
          {serverTools.map((t) => {
            const bad = unsupported.includes(t);
            return (
              <span
                key={t}
                className={`inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-[11px] ${
                  bad
                    ? "border-[var(--error-border)] bg-[var(--error-bg)] text-[var(--danger)]"
                    : "border-edge text-ink"
                }`}
              >
                <span className="mono">{t}</span>
                {bad && <span title="当前协议没有对应物，请求时会被丢掉">不生效</span>}
                <button
                  onClick={() => setServerTools(serverTools.filter((x) => x !== t))}
                  aria-label={`移除 ${t}`}
                  className="text-dim transition-colors hover:text-ink"
                >
                  <X size={10} />
                </button>
              </span>
            );
          })}
        </div>
        <div className="mt-2 flex flex-wrap gap-1">
          {knownTools
            .filter((t) => !serverTools.includes(t))
            .map((t) => (
              <button
                key={t}
                onClick={() => addTool(t)}
                className="rounded border border-dashed border-edge px-1.5 py-0.5 text-[11px] text-dim transition-colors hover:border-dim hover:text-ink"
              >
                + {t}
              </button>
            ))}
        </div>
        <div className="mt-2 flex gap-2">
          <Input
            mono
            value={customTool}
            onChange={(e) => setCustomTool(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                addTool(customTool);
                setCustomTool("");
              }
            }}
            placeholder="自定义工具名，回车添加"
            className="flex-1 py-1.5 text-xs"
          />
        </div>
      </Field>

      <Field label="服务端工具调用次数上限" hint="留空 = 协议默认">
        <Input
          mono
          value={maxUses}
          onChange={(e) => setMaxUses(e.target.value.replace(/\D/g, ""))}
          placeholder="例如 5"
        />
      </Field>

      {/* 模型列表 --------------------------------------------------------- */}
      <div className="mb-3 mt-5 border-t border-edge pt-4">
        <div className="mb-2 flex flex-wrap items-center gap-2">
          <Label>模型</Label>
          <span className="text-[11px] text-dim">
            {catalogProvider ? (
              <>
                元数据按 <span className="mono">{catalogProvider}</span> 匹配
              </>
            ) : (
              <>块名会成为模型引用 <span className="mono">渠道名/块名</span>，meta 是 models.dev 的查找 id</>
            )}
          </span>
          <div className="ml-auto flex items-center gap-1.5">
            <Button size="sm" onClick={() => setPicker("upstream")}>
              <CloudDownload size={12} />
              从上游拉取
            </Button>
            {catalogProvider && (
              <Button size="sm" onClick={() => setPicker("catalog")}>
                <ListChecks size={12} />
                从目录选
              </Button>
            )}
            <Button size="sm" onClick={() => setModels([...models, blankModel()])}>
              <Plus size={12} />
              添加
            </Button>
          </div>
        </div>

        <CatalogError catalog={catalog} />

        {unbound.length > 0 && (
          <div className="mb-2 flex items-start gap-2 rounded-lg border border-[var(--warn-border)] bg-[var(--warn-bg)] px-3 py-2 text-xs leading-relaxed">
            <TriangleAlert size={13} className="mt-0.5 shrink-0 text-tone-orange" />
            <span>
              有 {unbound.length} 个模型没绑元数据（
              <span className="mono">{unbound.map((m) => m.name).join("、")}</span>
              ）。carter 要靠 meta 解析模型，保存前得给它们各绑一个 —— 名字对不上目录时，
              用行尾的「绑元数据」搜一个。
            </span>
          </div>
        )}

        <div className="space-y-2">
          {models.map((m, i) => {
            const info = m.meta.trim() ? metaMap[m.meta.trim()] : undefined;
            const summary = metaSummary(m.meta, info);
            return (
              <div key={i} className="rounded-lg border border-edge bg-base p-2.5">
                <div className="flex items-center gap-2">
                  <Input
                    mono
                    value={m.name}
                    onChange={(e) => patchModel(i, { name: e.target.value })}
                    placeholder="块名，如 sonnet"
                    className="py-1.5 text-xs"
                  />
                  <Input
                    mono
                    value={m.api_name}
                    onChange={(e) => patchModel(i, { api_name: e.target.value })}
                    placeholder="下发名（留空 = 块名）"
                    title="真正发给上游的模型名"
                    className="py-1.5 text-xs"
                  />
                  <IconButton
                    title="删除这个模型"
                    onClick={() => setModels(models.filter((_, j) => j !== i))}
                  >
                    <X size={12} />
                  </IconButton>
                </div>

                <div className="mt-1.5 flex items-center gap-2 text-[11px]">
                  <span
                    className={`min-w-0 flex-1 truncate ${summary.warn ? "text-tone-orange" : "text-dim"}`}
                    title={m.meta}
                  >
                    {summary.text}
                    {m.meta.trim() && <span className="mono ml-1.5 text-dim">{m.meta}</span>}
                  </span>
                  <button
                    onClick={() => setMetaEditing(i)}
                    className="flex shrink-0 items-center gap-1 text-dim transition-colors hover:text-ink"
                    title="在 models.dev 目录里搜一个元数据绑上来"
                  >
                    <Search size={10} />
                    {m.meta.trim() ? "改绑定" : "绑元数据"}
                  </button>
                  <button
                    onClick={() => patchModel(i, { manual: !m.manual })}
                    className="shrink-0 text-dim transition-colors hover:text-ink"
                    title="直接手写 meta 字符串"
                  >
                    {m.manual ? "收起" : "手写"}
                  </button>
                  {m.reasoningCustom ? (
                    <span
                      className="w-24 shrink-0 rounded-md border border-dashed border-edge px-2 py-0.5 text-center text-dim"
                      title="这个取值界面改不了，去配置文件里改"
                    >
                      自定义
                    </span>
                  ) : (
                    <Select
                      className="w-24 shrink-0"
                      value={m.reasoning}
                      onChange={(v) => patchModel(i, { reasoning: v })}
                      options={REASONING_EFFORTS.map((r) => ({ value: r.value, label: r.label }))}
                    />
                  )}
                </div>

                {m.manual && (
                  <div className="mt-1.5">
                    <Input
                      mono
                      value={m.meta}
                      onChange={(e) => patchModel(i, { meta: e.target.value })}
                      placeholder="meta，如 anthropic/claude-sonnet-4-5"
                      className="py-1.5 text-xs"
                    />
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>

      <div className="flex items-center gap-2 text-[11px] leading-relaxed text-dim">
        <CircleCheck size={12} className="shrink-0" />
        保存会即时热重载：正在跑的会话从**下一条消息**起用新配置；MCP server 是会话建立时连的，
        改了要新开一个会话。
      </div>

      {metaEditing !== null && models[metaEditing] && (
        <MetaSearchDialog
          title="给这一行绑元数据"
          initial={models[metaEditing].name || models[metaEditing].api_name}
          onClose={() => setMetaEditing(null)}
          onPick={(hit) => {
            // 绑元数据**只换 meta**：下发名与块名说的是「发什么给上游」，与元数据无关。
            // 只有块名还空着时才顺手填一个，让人看得出这行是哪个模型。
            patchModel(metaEditing, {
              name: models[metaEditing].name.trim() || blockNameFor(hit.id),
              meta: hit.meta,
              manual: false,
            });
            setMetaEditing(null);
          }}
        />
      )}

      {picker === "upstream" && (
        <UpstreamFetchDialog
          kind={kind}
          baseUrl={baseUrl}
          apiKey={apiKey}
          provider={editing}
          onClose={() => setPicker(null)}
          onApply={(rows) => {
            addRows(rows.map((r) => rowFromUpstream(r.name, r.meta)));
            setPicker(null);
          }}
        />
      )}

      {picker === "catalog" && catalogProvider && (
        <CatalogDialog
          providerId={catalogProvider}
          onClose={() => setPicker(null)}
          onApply={(entries) => {
            addRows(entries.map((e) => rowFromEntry(catalogProvider, e)));
            setPicker(null);
          }}
        />
      )}

      {confirmDelete && editing && (
        <ConfirmDialog
          title="删除渠道"
          danger
          confirmLabel="确认删除"
          message={
            <>
              将从 <span className="mono">config.toml</span> 里移除{" "}
              <span className="mono text-ink">[providers.{editing}]</span> 整段，
              包括它的全部模型定义。<strong className="font-medium">不可恢复</strong>。
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

/** 一个布尔开关行（MCP 用）。 */
export function SwitchRow({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-center gap-3">
      <div className="min-w-0 flex-1">
        <div className="text-sm text-ink">{label}</div>
        {hint && <div className="text-[11px] text-dim">{hint}</div>}
      </div>
      <Switch checked={checked} onChange={onChange} label={label} />
    </div>
  );
}