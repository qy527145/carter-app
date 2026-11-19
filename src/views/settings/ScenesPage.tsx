// 场景模型：每一种「要调一次模型」的场合用哪个模型。
//
// 取值本质上是两种：**指针**（主模型 / 副模型，跟着它们走）或**具体模型**（钉死）。
// 回落链与「实际使用」都由后端算（`scene_models.rs`）—— 运行时用的是同一份规则，
// 页面上看到的就是真会发生的，不在前端再推一遍。

import { ArrowRight, RotateCcw } from "lucide-react";
import { Fragment, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Badge } from "../../components/ui/Panel";
import { Select } from "../../components/ui/Select";
import {
  getSceneModels,
  SCENE_FAST,
  SCENE_MAIN,
  type SceneModelsView,
  type SceneRow,
  type SceneStep,
  sendCommand,
  setSceneModel,
} from "../../lib/api";
import { displayName, useModelCatalog } from "../../lib/modelLabel";
import { toastError, toastOk } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { useStore } from "../../store";
import { AsyncBody, PageIntro } from "./AsyncBody";

const STEP_LABEL: Record<SceneStep, string> = {
  scene: "指定模型",
  fast: "副模型",
  main: "主模型",
  session: "会话模型",
};

const POINTER_LABEL: Record<string, string> = {
  [SCENE_MAIN]: "主模型",
  [SCENE_FAST]: "副模型",
};

const STORAGE_HINT: Record<SceneRow["storage"], string> = {
  config: "存在 config.toml（agent.model），carter CLI 也用它",
  assistant: "存在助理设置里，与「接单规则」弹窗里的同一项",
  app: "存在应用设置里，只有本应用用",
};

/** 回落链：每一格是哪一环、指向谁；高亮的是此刻实际生效的那一格。 */
function Chain({ row }: { row: SceneRow }) {
  const models = useModelCatalog();
  return (
    <div className="flex flex-wrap items-center gap-1 text-[11px]">
      {row.chain.map((l, i) => {
        const active = i === row.effective;
        // 没设（副模型未配置）或在生效格之前、却没被用上（模型已删除）：划掉。
        const skipped =
          (l.step !== "session" && !l.model) || (row.effective !== null && i < row.effective);
        return (
          <Fragment key={l.step}>
            {i > 0 && <ArrowRight size={10} className="text-dim/60" />}
            <span
              className={`rounded px-1.5 py-0.5 ${
                active ? "bg-tone-blue-soft text-tone-blue" : "text-dim"
              } ${skipped ? "line-through decoration-dim/50" : ""}`}
              title={
                l.step === "session"
                  ? "发起这次调用的那条会话当前用的模型（可能中途切过）"
                  : !l.model
                    ? "未设置，跳过"
                    : skipped
                      ? `${l.model} 已不在任何渠道里，跳过`
                      : l.model
              }
            >
              {STEP_LABEL[l.step]}
              {l.model && (
                <span className="mono ml-1 opacity-80">{displayName(l.model, models)}</span>
              )}
            </span>
          </Fragment>
        );
      })}
    </div>
  );
}

function SceneCard({
  row,
  view,
  onChange,
}: {
  row: SceneRow;
  view: SceneModelsView;
  onChange: (value: string | null) => void;
}) {
  const models = useModelCatalog();
  const isChat = row.id === "chat";
  const tag = (v: string) => (v === row.default_choice ? "（默认）" : "");
  const options = [
    // 普通对话就是主模型本身，指针对它没有意义。
    ...(isChat
      ? []
      : [
          {
            value: SCENE_MAIN,
            label: `主模型${tag(SCENE_MAIN)}`,
            hint: displayName(view.main, models),
          },
          {
            value: SCENE_FAST,
            label: `副模型${tag(SCENE_FAST)}`,
            hint: view.fast ? displayName(view.fast, models) : "未设置",
          },
        ]),
    ...models.map((m) => ({
      value: m.reference,
      label: m.label ?? m.reference,
      hint: m.reference,
    })),
  ];
  const isDefault = row.default_choice === null || row.choice === row.default_choice;
  const eff = row.effective === null ? null : row.chain[row.effective];
  const effective = !eff
    ? "无可用模型"
    : eff.step === "session"
      ? "会话当前模型"
      : eff.model
        ? displayName(eff.model, models)
        : "—";

  return (
    <div className="rounded-xl border border-edge bg-card px-4 py-3">
      <div className="flex items-center gap-2">
        <span className="text-sm text-ink">{row.label}</span>
        <Badge title={STORAGE_HINT[row.storage]}>
          {row.storage === "config" ? "CLI 共用" : row.storage === "assistant" ? "助理" : "本应用"}
        </Badge>
        {row.default_choice && (
          <span className="text-[11px] text-dim">
            默认：{POINTER_LABEL[row.default_choice] ?? row.default_choice}
          </span>
        )}
        <span className="ml-auto flex items-center gap-1.5">
          {!isDefault && (
            <Button size="sm" variant="ghost" onClick={() => onChange(null)} title="恢复默认">
              <RotateCcw size={12} />
              恢复默认
            </Button>
          )}
          <span className="w-64">
            <Select value={row.choice} options={options} onChange={(v) => onChange(v)} />
          </span>
        </span>
      </div>
      <div className="mt-1 text-xs text-dim">{row.description}</div>
      <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="text-[11px] text-dim">
          实际使用 <span className="text-ink">{effective}</span>
        </span>
        <Chain row={row} />
      </div>
    </div>
  );
}

export function ScenesPage() {
  const state = useAsync(getSceneModels, []);
  const setSettings = useStore((s) => s.setSettings);
  const bumpConfig = useStore((s) => s.bumpConfig);
  const models = useModelCatalog();
  const [busy, setBusy] = useState(false);

  async function change(row: SceneRow, value: string | null) {
    if (value === row.choice) return;
    setBusy(true);
    try {
      const next = await setSceneModel(row.id, value);
      // 不对齐的话，下一次在「外观」页改主题会把这里写的取值抹掉（整体覆盖写入）。
      setSettings(next);
      if (row.id === "chat" && value) {
        // 主模型变了：模型页、会话头部的标签都要重拉。正在看的这条会话也跟着换 ——
        // 与模型页「设为默认」一致。
        bumpConfig();
        const sid = useStore.getState().sessionId;
        if (sid) sendCommand(sid, { type: "set_model", model: value }).catch(toastError);
      }
      const shown = value ? (POINTER_LABEL[value] ?? displayName(value, models)) : "默认";
      toastOk(`${row.label} → ${shown}`);
      state.reload();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className="mb-4 flex items-center gap-2">
        <h1 className="text-[16px] font-semibold">场景模型</h1>
      </div>
      <PageIntro>
        每个场景可以指向<span className="text-ink">主模型</span>或
        <span className="text-ink">副模型</span>（跟着它们变，在「模型」页设置），
        也可以直接钉死一个具体模型。用不上时沿箭头往后找，高亮的那一格是此刻实际会用的；
        <span className="text-ink">会话模型</span>指发起调用的那条会话当前用的模型（可能中途切过）。
        改动从下一次调用起生效，助理对话会当场切换。
      </PageIntro>
      <div className={`space-y-2 ${busy ? "pointer-events-none opacity-70" : ""}`}>
        <AsyncBody state={state} empty="还没有配置模型">
          {(view: SceneModelsView) =>
            view.scenes.map((row) => (
              <SceneCard key={row.id} row={row} view={view} onChange={(v) => change(row, v)} />
            ))
          }
        </AsyncBody>
      </div>
    </>
  );
}
