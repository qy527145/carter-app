// 场景模型：每一种「要调一次模型」的场合用哪个模型，没指定时怎么回落。
//
// 回落链与「实际会用」都由后端算（`scene_models.rs`）—— 运行时用的是同一份规则，
// 页面上看到的就是真会发生的，不在前端再推一遍。

import { ArrowRight, CircleAlert } from "lucide-react";
import { Fragment, useState } from "react";
import { Badge } from "../../components/ui/Panel";
import { Select } from "../../components/ui/Select";
import {
  getSceneModels,
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
  scene: "本场景",
  fast: "副模型",
  main: "主模型",
  session: "会话模型",
};

const STORAGE_HINT: Record<SceneRow["storage"], string> = {
  config: "存在 config.toml（agent.model），carter CLI 也用它",
  assistant: "存在助理设置里，与「接单规则」弹窗里的同一项",
  app: "存在应用设置里，只有本应用用",
};

/** 某一环此刻指向谁：给回落链上的每一格配一个具体的模型名。 */
function stepTarget(step: SceneStep, view: SceneModelsView, row: SceneRow): string | null {
  switch (step) {
    case "scene":
      return row.binding;
    case "fast":
      return view.fast;
    case "main":
      return view.main;
    case "session":
      return null;
  }
}

function Chain({ row, view }: { row: SceneRow; view: SceneModelsView }) {
  const models = useModelCatalog();
  // 普通对话没有回落：它的绑定就是主模型本身。
  const steps: SceneStep[] = row.id === "chat" ? ["main"] : ["scene", ...row.fallback];
  return (
    <div className="flex flex-wrap items-center gap-1 text-[11px]">
      {steps.map((s, i) => {
        const target = stepTarget(s, view, row);
        const active = s === row.effective_from;
        const empty = s !== "session" && !target;
        return (
          <Fragment key={s}>
            {i > 0 && <ArrowRight size={10} className="text-dim/60" />}
            <span
              className={`rounded px-1.5 py-0.5 ${
                active ? "bg-tone-blue-soft text-tone-blue" : "text-dim"
              } ${empty && !active ? "line-through decoration-dim/40" : ""}`}
              title={
                s === "session"
                  ? "发起这次调用的那条会话当前用的模型（可能中途切过）"
                  : target
                    ? target
                    : "未设置，跳过"
              }
            >
              {STEP_LABEL[s]}
              {target && s !== "scene" && (
                <span className="mono ml-1 opacity-80">{displayName(target, models)}</span>
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
  onChange: (modelRef: string | null) => void;
}) {
  const models = useModelCatalog();
  const isChat = row.id === "chat";
  const fallbackText = row.fallback.map((s) => STEP_LABEL[s]).join(" → ");
  const options = [
    ...(isChat ? [] : [{ value: "", label: `跟随默认（${fallbackText}）` }]),
    ...models.map((m) => ({
      value: m.reference,
      label: m.label ?? m.reference,
      hint: m.reference,
    })),
  ];
  const effective =
    row.effective_from === "session"
      ? "会话当前模型"
      : row.effective
        ? displayName(row.effective, models)
        : "—";

  return (
    <div className="rounded-xl border border-edge bg-card px-4 py-3">
      <div className="flex items-center gap-2">
        <span className="text-sm text-ink">{row.label}</span>
        <Badge title={STORAGE_HINT[row.storage]}>
          {row.storage === "config" ? "CLI 共用" : row.storage === "assistant" ? "助理" : "本应用"}
        </Badge>
        <span className="ml-auto w-72">
          <Select
            value={row.binding ?? ""}
            options={options}
            onChange={(v) => onChange(v || null)}
            placeholder="选择模型"
          />
        </span>
      </div>
      <div className="mt-1 text-xs text-dim">{row.description}</div>
      <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="text-[11px] text-dim">
          实际使用 <span className="text-ink">{effective}</span>
        </span>
        <Chain row={row} view={view} />
      </div>
      {row.binding_broken && (
        <div className="mt-2 flex items-start gap-1.5 rounded-md border border-[var(--warn-border)] bg-[var(--warn-bg)] px-2 py-1.5 text-[11px]">
          <CircleAlert size={12} className="icon-optical shrink-0 text-tone-orange" />
          <span>
            绑定的 <span className="mono">{row.binding}</span>{" "}
            已不在任何渠道里，运行时会跳过它往下回落。
          </span>
        </div>
      )}
    </div>
  );
}

export function ScenesPage() {
  const state = useAsync(getSceneModels, []);
  const setSettings = useStore((s) => s.setSettings);
  const bumpConfig = useStore((s) => s.bumpConfig);
  const [busy, setBusy] = useState(false);

  async function change(row: SceneRow, modelRef: string | null) {
    if ((row.binding ?? null) === modelRef) return;
    setBusy(true);
    try {
      const next = await setSceneModel(row.id, modelRef);
      // 不对齐的话，下一次在「外观」页改主题会把这里写的绑定抹掉（整体覆盖写入）。
      setSettings(next);
      if (row.id === "chat" && modelRef) {
        // 主模型变了：模型页、会话头部的标签都要重拉。正在看的这条会话也跟着换 ——
        // 与模型页「设为默认」一致。
        bumpConfig();
        const sid = useStore.getState().sessionId;
        if (sid) sendCommand(sid, { type: "set_model", model: modelRef }).catch(toastError);
      }
      toastOk(modelRef ? `${row.label} → ${modelRef}` : `${row.label} 已改回跟随默认`);
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
        每个场景先用自己绑定的模型；没绑定（或绑定的模型已被删除）就沿箭头往后找，
        高亮的那一格是此刻实际会用的。<span className="text-ink">副模型</span>与
        <span className="text-ink">主模型</span>在「模型」页设置；
        <span className="text-ink">会话模型</span>指发起调用的那条会话当前用的模型（可能中途切过）。
        改动从下一次调用起生效；助理对话会当场切换。
      </PageIntro>
      <div className={`space-y-2 ${busy ? "pointer-events-none opacity-70" : ""}`}>
        <AsyncBody state={state} empty="还没有配置模型">
          {(view: SceneModelsView) =>
            view.scenes.map((row) => (
              <SceneCard key={row.id} row={row} view={view} onChange={(m) => change(row, m)} />
            ))
          }
        </AsyncBody>
      </div>
    </>
  );
}
