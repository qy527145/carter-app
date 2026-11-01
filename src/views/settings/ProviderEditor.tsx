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
import { CircleCheck, TriangleAlert, X } from "lucide-react";

import {
  checkServerTools,
  deleteProvider,
  detectCatalogProvider,
  getProviderDetail,
  listChannels,
  listProviderTemplates,
  recommendedCatalogModels,
  saveProvider,
  type ProviderDetail,
  type ProviderDraft,
  type ProviderTemplate,
} from "../../lib/api";
import { PROTOCOLS, UNKNOWN_PROTOCOL_HINT, presetFor } from "../../lib/protocols";
import { useAsync } from "../../lib/useAsync";
import { toastError } from "../../lib/toast";
import { useCatalog } from "./CatalogPicker";
import {
  CatalogDialog,
  UpstreamFetchDialog,
} from "./ModelPickers";
import {
  ModelList,
  blankModel,
  rowFromEntry,
  rowFromUpstream,
  toDraft,
  toState,
  type ModelState,
} from "./ModelRows";
import { Button } from "../../components/ui/Button";
import { Field, Input, Switch } from "../../components/ui/Input";
import { Banner, ConfirmDialog, Modal } from "../../components/ui/Panel";
import { Select } from "../../components/ui/Select";

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

  // 旧配置里可能写着别名 kind（`deepseek` 之类）。下拉只列三种线协议，所以要把
  // 当前这个别名单独塞进去 —— 否则选择框会显示成占位符，看着像配置丢了。
  const alias = preset && preset.kind !== preset.protocol ? preset : undefined;
  const protocolOptions = useMemo(() => {
    const opts = PROTOCOLS.map((p) => ({
      value: p.kind,
      label: p.label,
      hint: p.kind,
    }));
    if (alias) {
      opts.push({
        value: alias.kind,
        label: `${alias.label}（别名）`,
        hint: `${alias.kind} → ${alias.protocol}`,
      });
    }
    return opts;
  }, [alias]);
  const catalog = useCatalog(catalogProvider ?? "");

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
            ? preset.kind === preset.protocol
              ? `carter 走 ${preset.protocol}`
              : `${preset.label} 是别名，carter 按 ${preset.protocol} 处理`
            : "carter 不认识这个 Kind，按 Chat Completions 处理"
        }
      >
        <Select
          value={kind}
          onChange={changeKind}
          options={protocolOptions}
        />
      </Field>
      {alias && (
        <div className="mb-3">
          <Banner kind="info">
            这个渠道的 <span className="mono">kind = {alias.kind}</span>{" "}
            是 carter 的别名，实际走{" "}
            <span className="mono">{alias.protocol}</span>。留着也能用；改成上面的线协议
            并填好地址，配置会更直白。
          </Banner>
        </div>
      )}
      {!preset && kind.trim() !== "" && (
        <div className="mb-3">
          <Banner kind="warn">
            <span className="flex items-start gap-2">
              <TriangleAlert size={13} className="icon-optical shrink-0" />
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
      <ModelList
        models={models}
        setModels={setModels}
        catalogProvider={catalogProvider}
        catalog={catalog}
        onFetchUpstream={() => setPicker("upstream")}
        onPickCatalog={() => setPicker("catalog")}
      />

      <div className="flex items-center gap-2 text-[11px] leading-relaxed text-dim">
        <CircleCheck size={12} className="shrink-0" />
        保存会即时热重载：正在跑的会话从**下一条消息**起用新配置；MCP server 是会话建立时连的，
        改了要新开一个会话。
      </div>

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