// 模型页：把全部渠道下的 `provider/model` 摊平成一张表。
//
// 与「渠道」页的分工：渠道页管的是「怎么连上游」，模型页管的是「有哪些模型、
// 它们各自是什么规格、默认用哪个」。同一个模型定义在两处都能看到，但只有这里
// 会把 models.dev 的元数据解出来 —— 上下文窗口、能力、定价。

import { useState } from "react";
import { CircleAlert, Download, Pencil, Plus, Star, Zap } from "lucide-react";

import {
  getCompaction,
  listModels,
  setCompaction,
  refreshModelCatalog,
  setDefaultModel,
  setFastModel,
  type CompactionSettings,
  type ModelRow,
} from "../../lib/api";
import { capabilityLabel, fmtPrice, fmtTokens } from "../../lib/format";
import { reasoningDisplay } from "../../lib/protocols";
import { toastError, toastOk } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { Button, IconButton } from "../../components/ui/Button";
import { Segmented, Switch } from "../../components/ui/Input";
import { Badge } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";
import { ModelEditor } from "./ModelEditor";

function ModelCard({
  m,
  onSetDefault,
  onSetFast,
  onEdit,
}: {
  m: ModelRow;
  onSetDefault: () => void;
  onSetFast: () => void;
  onEdit: () => void;
}) {
  return (
    <div
      className={`rounded-xl border px-4 py-3 ${
        m.is_default ? "border-dim bg-base" : "border-edge bg-card"
      }`}
    >
      <div className="flex items-center gap-2">
        <span className="mono text-sm text-ink">{m.reference}</span>
        {m.is_default && (
          <span className="inline-flex items-center gap-1 text-[10px] text-tone-blue">
            <Star size={10} />
            默认
          </span>
        )}
        {m.is_fast && (
          <span className="inline-flex items-center gap-1 text-[10px] text-tone-violet">
            <Zap size={10} />
            副模型
          </span>
        )}
        <span className="ml-auto flex items-center gap-1">
          <Badge>{m.protocol}</Badge>
          <IconButton title="编辑这个模型" onClick={onEdit}>
            <Pencil size={12} />
          </IconButton>
        </span>
      </div>

      <div className="mono mt-1 truncate text-[11px] text-dim" title={m.meta}>
        {m.meta || "(未设 meta)"}
        {m.api_name !== m.name && <span className="ml-2">→ {m.api_name}</span>}
      </div>

      {m.error ? (
        // 单行解析失败不能让整页失败：把它挂在这一行上，其余行照常显示。
        <div className="mt-2 flex items-start gap-1.5 rounded-md border border-[var(--warn-border)] bg-[var(--warn-bg)] px-2 py-1.5 text-[11px] leading-relaxed">
          <CircleAlert size={12} className="icon-optical shrink-0 text-tone-orange" />
          <span>{m.error}</span>
        </div>
      ) : (
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-dim">
          <span>
            上下文 <span className="tabular-nums text-ink">{fmtTokens(m.context_window ?? 0)}</span>
          </span>
          <span>
            最大输出{" "}
            <span className="tabular-nums text-ink">{fmtTokens(m.max_output_tokens ?? 0)}</span>
          </span>
          {m.pricing && (
            <span>
              输入 <span className="text-ink">{fmtPrice(m.pricing.input)}</span> · 输出{" "}
              <span className="text-ink">{fmtPrice(m.pricing.output)}</span>
              <span className="text-dim/70"> / 百万 token</span>
            </span>
          )}
          {m.default_reasoning != null && (
            <span>
              推理 <span className="mono text-ink">{reasoningDisplay(m.default_reasoning)}</span>
            </span>
          )}
          {m.capabilities.length > 0 && (
            <span className="flex flex-wrap gap-1">
              {m.capabilities.map((c) => (
                <Badge key={c}>{capabilityLabel(c)}</Badge>
              ))}
            </span>
          )}
        </div>
      )}

      <div className="mt-2 flex flex-wrap items-center gap-2">
        {!m.is_default && (
          <Button size="sm" onClick={onSetDefault} title="新对话默认用它">
            设为默认
          </Button>
        )}
        {!m.is_fast && (
          <Button
            size="sm"
            onClick={onSetFast}
            title="压缩上下文、生成标题、总结用它（要便宜、要快）"
          >
            设为副模型
          </Button>
        )}
        {m.is_fast && (
          <Button size="sm" onClick={() => onSetFast()} title="清除后回落主模型">
            清除副模型
          </Button>
        )}
      </div>
    </div>
  );
}

/** 自动压缩的触发点（占上下文窗口的比例）。给几档就够，没必要做成滑杆。 */
const RATIOS = ["0.6", "0.7", "0.75", "0.8", "0.9"] as const;

/**
 * 自动压缩：开关 + 触发比例。
 *
 * 放在模型页是因为它和「副模型」是一回事的两半 —— 压缩用的就是副模型；
 * 对话输入框右下角的上下文圆环点开说明里也指到这里。
 */
function CompactionRow() {
  const state = useAsync(getCompaction, []);
  const cur = state.data;

  function save(next: CompactionSettings) {
    setCompaction(next.enabled, next.ratio)
      .then(() => state.reload())
      .catch(toastError);
  }

  if (!cur) return null;
  // 配置里写了不在档位上的值（手改的）时也要能显示：补成一档。
  const ratio = String(cur.ratio);
  const options = (RATIOS as readonly string[]).includes(ratio)
    ? RATIOS.map((r) => ({ id: r as string, label: `${Math.round(Number(r) * 100)}%` }))
    : [...RATIOS, ratio]
        .sort((a, b) => Number(a) - Number(b))
        .map((r) => ({ id: r as string, label: `${Math.round(Number(r) * 100)}%` }));

  return (
    <div className="mb-4 rounded-xl border border-edge bg-card px-4 py-3">
      <div className="flex items-center gap-3">
        <span className="text-sm text-ink">自动压缩上下文</span>
        <span className="text-xs text-dim">
          占用超过阈值时，把较早的对话换成摘要（手动随时可用 /compact）
        </span>
        <span className="ml-auto">
          <Switch
            label="自动压缩上下文"
            checked={cur.enabled}
            onChange={(enabled) => save({ ...cur, enabled })}
          />
        </span>
      </div>
      {cur.enabled && (
        <div className="mt-2 flex items-center gap-3">
          <span className="text-xs text-dim">触发阈值</span>
          <Segmented
            value={ratio}
            options={options}
            onChange={(r) => save({ ...cur, ratio: Number(r) })}
          />
        </div>
      )}
    </div>
  );
}

export function ModelsPage() {
  const state = useAsync(listModels, []);
  const [refreshing, setRefreshing] = useState(false);
  // `undefined` = 弹窗关着；`null` = 新增；有值 = 编辑那一条。
  const [editing, setEditing] = useState<ModelRow | null | undefined>(undefined);

  async function refresh() {
    setRefreshing(true);
    try {
      const n = await refreshModelCatalog();
      toastOk(`已更新模型元数据（${n} 条）`);
      state.reload();
    } catch (e) {
      toastError(e);
    } finally {
      setRefreshing(false);
    }
  }

  return (
    <>
      <div className="mb-4 flex items-center gap-2">
        <h1 className="text-base font-semibold">模型</h1>
        <Button
          variant="primary"
          size="sm"
          className="ml-auto"
          onClick={() => setEditing(null)}
        >
          <Plus size={13} />
          添加模型
        </Button>
        <Button size="sm" onClick={refresh} disabled={refreshing}>
          <Download size={13} />
          {refreshing ? "拉取中…" : "刷新元数据"}
        </Button>
      </div>

      <PageIntro>
        上下文窗口、能力与定价来自 models.dev 的本地缓存。缓存不在、或某条{" "}
        <span className="mono">meta</span> 查不到时，只有那一行会标出原因 —— 其余照常显示。
        <br />
        <span className="text-ink">主模型</span>（<span className="mono">agent.model</span>）
        是新对话默认用的；<span className="text-ink">副模型</span>（
        <span className="mono">agent.fast_model</span>）只用于压缩上下文、生成标题、总结 ——
        那些活儿要的是便宜和快，不必用最强的模型。副模型没设时回落主模型。
      </PageIntro>

      <CompactionRow />

      <div className="space-y-2">
        <AsyncBody
          state={state}
          empty={
            <>
              还没有模型。
              <br />
              先到「渠道」页给某个渠道加上模型。
            </>
          }
        >
          {(models: ModelRow[]) => (
            <>
              {models.map((m) => (
                <ModelCard
                  key={m.reference}
                  m={m}
                  onEdit={() => setEditing(m)}
                  onSetDefault={() => {
                    setDefaultModel(m.reference)
                      .then(() => {
                        toastOk(`默认模型已设为 ${m.reference}`);
                        state.reload();
                      })
                      .catch(toastError);
                  }}
                  onSetFast={() => {
                    // 已经是副模型 → 再点一次是清除（按钮文案已经说明）。
                    const next = m.is_fast ? null : m.reference;
                    setFastModel(next)
                      .then(() => {
                        toastOk(next ? `副模型已设为 ${next}` : "副模型已清除");
                        state.reload();
                      })
                      .catch(toastError);
                  }}
                />
              ))}
            </>
          )}
        </AsyncBody>
      </div>

      {editing !== undefined && (
        <ModelEditor
          editing={editing}
          onClose={(changed) => {
            setEditing(undefined);
            if (changed) state.reload();
          }}
        />
      )}
    </>
  );
}
