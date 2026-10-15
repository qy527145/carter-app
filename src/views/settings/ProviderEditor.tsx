// 渠道编辑弹窗：协议、密钥、服务端工具、模型列表。
//
// ## 密钥的三态
//
// 弹窗**不回传**已存的密钥。打开时 `api_key` 输入框是空的，placeholder 显示
// 「已配置（留空则不改）」。于是「用户没碰密钥」这件事在提交的数据里是
// `null`，后端据此保持原样 —— 而不是把界面读到的值（可能是 `${VAR}`，
// 也可能是插值后的明文）再写回去。见 core 的 `config_edit.rs`。

import { useEffect, useMemo, useState } from "react";
import { CircleCheck, Plus, TriangleAlert, X } from "lucide-react";

import {
  checkServerTools,
  deleteProvider,
  getProviderDetail,
  saveProvider,
  type ModelDetail,
  type ModelDraft,
  type ProviderDetail,
  type ProviderDraft,
} from "../../lib/api";
import {
  PROTOCOLS,
  REASONING_EFFORTS,
  UNKNOWN_PROTOCOL_HINT,
  presetFor,
  reasoningIsCustom,
  reasoningText,
} from "../../lib/protocols";
import { toastError } from "../../lib/toast";
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
};

function blankModel(): ModelState {
  return { name: "", meta: "", api_name: "", reasoning: "", reasoningCustom: false };
}

function toState(m: ModelDetail): ModelState {
  return {
    name: m.name,
    meta: m.meta,
    api_name: m.api_name ?? "",
    reasoning: reasoningText(m.default_reasoning),
    reasoningCustom: reasoningIsCustom(m.default_reasoning),
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
  const [name, setName] = useState(editing ?? "");
  const [kind, setKind] = useState("anthropic");
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

  // 编辑时拉原始配置回填。
  useEffect(() => {
    if (!editing) {
      setModels([blankModel()]);
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

  const preset = presetFor(kind);
  const nameLocked = editing !== null;

  const valid = useMemo(() => {
    if (!name.trim()) return false;
    if (!kind.trim()) return false;
    // 模型行允许整行留空（用户点了「加一个」还没填），但不能有半填的。
    return models.every((m) => {
      const empty = !m.name.trim() && !m.meta.trim();
      return empty || (m.name.trim() !== "" && m.meta.trim() !== "");
    });
  }, [name, kind, models]);

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
      width={560}
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

      <Field
        label="渠道名"
        hint={
          nameLocked
            ? "改名要新建一个渠道再删掉旧的"
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
          onChange={setKind}
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

      <Field label="API Key" hint={loaded?.has_api_key ? "已配置（留空则不改）" : "可填 ${ENV_VAR}"}>
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
        <div className="mb-2 flex items-center gap-2">
          <Label>模型</Label>
          <span className="text-[11px] text-dim">
            块名会成为模型引用 <span className="mono">渠道名/块名</span>，meta 是 models.dev 的查找 id
          </span>
          <Button
            size="sm"
            className="ml-auto"
            onClick={() => setModels([...models, blankModel()])}
          >
            <Plus size={12} />
            添加
          </Button>
        </div>

        <div className="space-y-2">
          {models.map((m, i) => (
            <div key={i} className="rounded-lg border border-edge bg-base p-2.5">
              <div className="flex items-center gap-2">
                <Input
                  mono
                  value={m.name}
                  onChange={(e) =>
                    setModels(models.map((x, j) => (j === i ? { ...x, name: e.target.value } : x)))
                  }
                  placeholder="块名，如 sonnet"
                  className="py-1.5 text-xs"
                />
                <IconButton
                  title="删除这个模型"
                  onClick={() => setModels(models.filter((_, j) => j !== i))}
                >
                  <X size={12} />
                </IconButton>
              </div>
              <div className="mt-1.5">
                <Input
                  mono
                  value={m.meta}
                  onChange={(e) =>
                    setModels(models.map((x, j) => (j === i ? { ...x, meta: e.target.value } : x)))
                  }
                  placeholder="meta，如 anthropic/claude-sonnet-4-5"
                  className="py-1.5 text-xs"
                />
              </div>
              <div className="mt-1.5 flex gap-2">
                <Input
                  mono
                  value={m.api_name ?? ""}
                  onChange={(e) =>
                    setModels(models.map((x, j) => (j === i ? { ...x, api_name: e.target.value } : x)))
                  }
                  placeholder="下发给端点的名字（留空 = 用块名）"
                  className="flex-1 py-1.5 text-xs"
                />
                {m.reasoningCustom ? (
                  // 配置里是 `{ budget = 1024 }` 之类 —— Select 表达不了，只展示。
                  <span
                    className="w-32 shrink-0 rounded-md border border-dashed border-edge px-2 py-1.5 text-center text-[11px] text-dim"
                    title="这个取值界面改不了，去配置文件里改"
                  >
                    自定义
                  </span>
                ) : (
                  <Select
                    className="w-32"
                    value={m.reasoning}
                    onChange={(v) =>
                      setModels(models.map((x, j) => (j === i ? { ...x, reasoning: v } : x)))
                    }
                    options={REASONING_EFFORTS.map((r) => ({ value: r.value, label: r.label }))}
                  />
                )}
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="flex items-center gap-2 text-[11px] leading-relaxed text-dim">
        <CircleCheck size={12} className="shrink-0" />
        保存会即时热重载：正在跑的会话从**下一条消息**起用新配置；MCP server 是会话建立时连的，
        改了要新开一个会话。
      </div>

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
