// 定时任务页：增删改查这张表。
//
// 与「对话里建」是同一个数据源（`~/.carter-app/schedules.json`）：模型调
// `schedule` 工具写进去的，刷新一下就在这页出现；这页改的，模型下一轮 `list`
// 也能看到。两边共用 `ScheduleStore`，没有第二份真相。
//
// ## 为什么一次性任务跑完不能直接拨回来
//
// 跑完的一次性任务 `enabled = false`，但 `at_ms` 还是那个过去的时间。
// 后端 `set_enabled` 重算下一次时对 `Once` 就是把 `at_ms` 原样写回 ——
// 于是一打开就会立刻再跑一遍。所以这类任务的开关在界面上是禁用的，
// 要重新用只能编辑时间（`save_schedule` 也会拒绝过去的一次性时间）。
//
// ## 为什么时间是本机时间的字符串
//
// 与 `schedule` 工具一致：界面上给人看的、给人改的都该是「2026-11-01 09:00」
// 这种本机时间。存盘用 epoch 毫秒，跨时区/夏令时由 `Date` 与后端的
// `chrono::Local` 各自算，不在这里抠。

import { useState } from "react";
import { Clock, Pencil, Play, Plus, Trash } from "lucide-react";

import {
  deleteSchedule,
  listSchedules,
  runSchedule,
  saveSchedule,
  toggleSchedule,
  type Schedule,
  type ScheduleKind,
} from "../../lib/api";
import { fullTime, relTime } from "../../lib/format";
import { openSession } from "../../lib/sessionOps";
import { toastError, toastOk } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { useModelCatalog } from "../../lib/modelLabel";
import { useStore } from "../../store";
import { Button, IconButton } from "../../components/ui/Button";
import { Field, Input, Segmented, Switch, Textarea } from "../../components/ui/Input";
import { Select } from "../../components/ui/Select";
import { Badge, Banner, Card, ConfirmDialog, Modal } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

type Mode = "once" | "interval" | "daily";

const pad = (n: number) => String(n).padStart(2, "0");

/** `YYYY-MM-DD HH:MM`，与后端 `schedules::fmt_local` 一个写法。 */
function fmtLocal(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}`
  );
}

/** `<input type="datetime-local">` 要的值：没有秒、没有时区。 */
function toInput(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "";
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}`
  );
}

/** 镜像后端 `ScheduleKind::label()`。两处算法一致，改一处要改另一处。 */
function kindLabel(kind: ScheduleKind): string {
  switch (kind.kind) {
    case "once":
      return `${fmtLocal(kind.at_ms)} 触发一次`;
    case "interval": {
      const m = kind.every_minutes;
      if (m % (24 * 60) === 0) return `每 ${m / (24 * 60)} 天`;
      if (m % 60 === 0) return `每 ${m / 60} 小时`;
      return `每 ${m} 分钟`;
    }
    case "daily":
      return `每天 ${pad(kind.hour)}:${pad(kind.minute)}`;
  }
}

/** 一次性任务跑过之后就没法再用了 —— 时间是过去的，拨开就会立刻重跑。 */
function isSpent(s: Schedule): boolean {
  return s.kind.kind === "once" && !s.enabled && s.kind.at_ms <= Date.now();
}

type Draft = {
  id: string;
  name: string;
  prompt: string;
  mode: Mode;
  atLocal: string;
  everyMinutes: string;
  dailyTime: string;
  cwd: string;
  /** 空串 = 用默认模型。 */
  model: string;
  enabled: boolean;
};

function draftOf(s: Schedule | null): Draft {
  if (!s) {
    // 新建：默认「明天这时候」，比空着好改。
    const tomorrow = Date.now() + 24 * 60 * 60 * 1000;
    return {
      id: `sch_${crypto.randomUUID()}`,
      name: "",
      prompt: "",
      mode: "daily",
      atLocal: toInput(tomorrow),
      everyMinutes: "60",
      dailyTime: "09:00",
      cwd: useStore.getState().cwd,
      model: "",
      enabled: true,
    };
  }
  const k = s.kind;
  return {
    id: s.id,
    name: s.name,
    prompt: s.prompt,
    mode: k.kind,
    atLocal: toInput(k.kind === "once" ? k.at_ms : Date.now() + 24 * 60 * 60 * 1000),
    everyMinutes: String(k.kind === "interval" ? k.every_minutes : 60),
    dailyTime:
      k.kind === "daily" ? `${pad(k.hour)}:${pad(k.minute)}` : "09:00",
    cwd: s.cwd,
    model: s.model ?? "",
    enabled: s.enabled,
  };
}

function ScheduleEditor({
  editing,
  onClose,
}: {
  editing: Schedule | null;
  onClose: (changed: boolean) => void;
}) {
  const [d, setD] = useState<Draft>(() => draftOf(editing));
  const [saving, setSaving] = useState(false);
  const models = useModelCatalog();

  const set = <K extends keyof Draft>(k: K, v: Draft[K]) =>
    setD((prev) => ({ ...prev, [k]: v }));

  // 客户端先挡一遍明显不对的输入。真正的校验在后端 —— 这里只是为了不让用户
  // 填完再吃一个错，两边的规则仍需保持一致。
  const everyMinutes = Number(d.everyMinutes);
  const daily = /^(\d{1,2}):(\d{2})$/.exec(d.dailyTime);
  const atMs = d.atLocal ? new Date(d.atLocal).getTime() : NaN;

  const kindOk =
    d.mode === "once"
      ? !Number.isNaN(atMs) && atMs > Date.now()
      : d.mode === "interval"
        ? Number.isInteger(everyMinutes) && everyMinutes >= 1 && everyMinutes <= 30 * 24 * 60
        : daily !== null && Number(daily[1]) <= 23 && Number(daily[2]) <= 59;

  const valid =
    d.name.trim() !== "" && d.prompt.trim() !== "" && d.cwd.trim() !== "" && kindOk;

  async function submit() {
    const kind: ScheduleKind =
      d.mode === "once"
        ? { kind: "once", at_ms: atMs }
        : d.mode === "interval"
          ? { kind: "interval", every_minutes: everyMinutes }
          : { kind: "daily", hour: Number(daily![1]), minute: Number(daily![2]) };

    const next: Schedule = {
      id: d.id,
      name: d.name.trim(),
      prompt: d.prompt.trim(),
      kind,
      cwd: d.cwd.trim(),
      model: d.model || null,
      enabled: d.enabled,
      // 这几个后端自己维护；新建时给个占位，后端会按 kind 重算 next_at。
      created_at: editing?.created_at ?? Date.now(),
      next_at: editing?.next_at ?? 0,
      last_fired_at: editing?.last_fired_at ?? null,
      last_session_id: editing?.last_session_id ?? null,
      runs: editing?.runs ?? 0,
      last_error: editing?.last_error ?? null,
    };

    setSaving(true);
    try {
      await saveSchedule(next);
      onClose(true);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal
      title={editing ? `编辑定时任务 · ${editing.name}` : "新建定时任务"}
      onClose={() => onClose(false)}
      width={560}
      footer={
        <>
          <Button onClick={() => onClose(false)} disabled={saving}>
            取消
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || saving}>
            {saving ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      <Field label="名字" hint="侧栏里那条对话会用它起标题">
        <Input
          value={d.name}
          onChange={(e) => set("name", e.target.value)}
          placeholder="例如：早间简报"
        />
      </Field>

      <Field label="到点发什么" hint="原样作为用户消息发进新对话">
        <Textarea
          rows={4}
          value={d.prompt}
          onChange={(e) => set("prompt", e.target.value)}
          placeholder="把今天的 git 提交和未合并的 PR 汇总一下。"
        />
      </Field>

      <Field label="节奏">
        <Segmented<Mode>
          value={d.mode}
          onChange={(m) => set("mode", m)}
          options={[
            { id: "once", label: "一次" },
            { id: "interval", label: "间隔" },
            { id: "daily", label: "每天" },
          ]}
        />
      </Field>

      {d.mode === "once" && (
        <Field label="触发时间" hint="本机时间，必须是将来">
          <Input
            type="datetime-local"
            value={d.atLocal}
            onChange={(e) => set("atLocal", e.target.value)}
          />
        </Field>
      )}

      {d.mode === "interval" && (
        <Field label="每隔多少分钟" hint="1 分钟 ~ 30 天">
          <Input
            type="number"
            min={1}
            max={30 * 24 * 60}
            value={d.everyMinutes}
            onChange={(e) => set("everyMinutes", e.target.value)}
          />
        </Field>
      )}

      {d.mode === "daily" && (
        <Field label="每天几点" hint="本机时间">
          <Input
            type="time"
            value={d.dailyTime}
            onChange={(e) => set("dailyTime", e.target.value)}
          />
        </Field>
      )}

      <Field label="工作目录" hint="新对话在这个目录里开">
        <Input
          mono
          value={d.cwd}
          onChange={(e) => set("cwd", e.target.value)}
          placeholder="C:\\work\\proj"
        />
      </Field>

      <Field label="模型" hint="留空用默认模型">
        <Select
          value={d.model}
          onChange={(v) => set("model", v)}
          options={[
            { value: "", label: "默认模型" },
            ...models.map((m) => ({ value: m.reference, label: m.label ?? m.reference })),
          ]}
        />
      </Field>

      <label className="mb-2 flex items-center gap-2 text-xs text-dim">
        <input
          type="checkbox"
          checked={d.enabled}
          onChange={(e) => set("enabled", e.target.checked)}
          className="h-3.5 w-3.5 accent-[var(--accent)]"
        />
        保存后立即启用
      </label>

      <Banner kind="info">
        触发时会<strong className="font-medium">新开一条对话</strong>，不会插进你正在聊的那条。
        触发时没人在场，所以那一轮被要求自己拿主意，不会停下来问你。
      </Banner>
    </Modal>
  );
}

export function SchedulesPage() {
  const state = useAsync(listSchedules, []);
  const [editing, setEditing] = useState<Schedule | null | undefined>(undefined);
  const [confirming, setConfirming] = useState<Schedule | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const run = (s: Schedule) => {
    setBusy(s.id);
    runSchedule(s.id)
      .then(() => {
        toastOk(`「${s.name}」已开新对话`);
        state.reload();
      })
      .catch(toastError)
      .finally(() => setBusy(null));
  };

  return (
    <>
      <div className="mb-4 flex items-center gap-2">
        <h1 className="text-base font-semibold">定时任务</h1>
        <Button
          variant="primary"
          size="sm"
          className="ml-auto"
          onClick={() => setEditing(null)}
        >
          <Plus size={13} />
          新建任务
        </Button>
      </div>

      <PageIntro>
        也可以直接在对话里说「每天九点帮我看看 CI」，模型会调{" "}
        <span className="mono">schedule</span> 工具建好，建完这里就能看到。
        数据存在 <span className="mono">~/.carter-app/schedules.json</span>。
      </PageIntro>

      <div className="space-y-2">
        <AsyncBody
          state={state}
          empty={
            <>
              还没有定时任务。
              <br />
              点右上角「新建任务」，或者在对话里直接说想让它几点做什么。
            </>
          }
        >
          {(list: Schedule[]) => (
            <>
              {list.map((s) => {
                const spent = isSpent(s);
                return (
                  <Card key={s.id} className="space-y-1.5">
                    <div className="flex items-center gap-2">
                      <Clock size={13} className="shrink-0 text-tone-blue" />
                      <span className="truncate text-sm font-medium text-ink">{s.name}</span>
                      <Badge>{kindLabel(s.kind)}</Badge>
                      {spent && <Badge tone="orange">已跑完</Badge>}
                      {!s.enabled && !spent && <Badge tone="orange">已暂停</Badge>}
                      <span className="ml-auto flex shrink-0 items-center gap-1">
                        <Switch
                          label={`${s.enabled ? "暂停" : "启用"} ${s.name}`}
                          checked={s.enabled}
                          disabled={busy === s.id}
                          onChange={(next) => {
                            setBusy(s.id);
                            toggleSchedule(s.id, next)
                              .then(() => state.reload())
                              .catch(toastError)
                              .finally(() => setBusy(null));
                          }}
                        />
                        <IconButton
                          title="立即运行一次"
                          disabled={busy === s.id}
                          onClick={() => run(s)}
                        >
                          <Play size={12} />
                        </IconButton>
                        <IconButton title="编辑" onClick={() => setEditing(s)}>
                          <Pencil size={12} />
                        </IconButton>
                        <IconButton title="删除" onClick={() => setConfirming(s)}>
                          <Trash size={12} />
                        </IconButton>
                      </span>
                    </div>

                    <div className="truncate text-[11px] text-dim" title={s.prompt}>
                      {s.prompt}
                    </div>

                    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-dim">
                      {s.enabled && !spent && (
                        <span title={fullTime(s.next_at)}>下次 {fmtLocal(s.next_at)}</span>
                      )}
                      {s.last_fired_at && (
                        <span title={fullTime(s.last_fired_at)}>
                          上次 {relTime(s.last_fired_at)}
                        </span>
                      )}
                      {s.last_session_id && (
                        <button
                          className="text-tone-blue hover:underline"
                          onClick={() => openSession(s.last_session_id!)}
                        >
                          打开上次那条对话
                        </button>
                      )}
                      <span className="mono">已跑 {s.runs} 次</span>
                      <span className="mono truncate">{s.cwd}</span>
                      {s.model && <span className="mono">{s.model}</span>}
                    </div>

                    {s.last_error && (
                      <Banner kind="error">
                        上次触发失败：{s.last_error}
                        <br />
                        <span className="text-dim">
                          定时器会按原节奏继续，下一次照常尝试。
                        </span>
                      </Banner>
                    )}
                  </Card>
                );
              })}
            </>
          )}
        </AsyncBody>
      </div>

      {editing !== undefined && (
        <ScheduleEditor
          editing={editing}
          onClose={(changed) => {
            setEditing(undefined);
            if (changed) state.reload();
          }}
        />
      )}

      {confirming && (
        <ConfirmDialog
          title="删除定时任务"
          danger
          confirmLabel="确认删除"
          message={
            <>
              会删掉「{confirming.name}」。已经跑出来的对话不受影响 ——
              删的只是这条节奏，不是它的历史。
            </>
          }
          onCancel={() => setConfirming(null)}
          onConfirm={() => {
            const id = confirming.id;
            setConfirming(null);
            deleteSchedule(id)
              .then(() => {
                toastOk("已删除");
                state.reload();
              })
              .catch(toastError);
          }}
        />
      )}
    </>
  );
}
