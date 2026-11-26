// 值守面板：请示、授权书、台账、指标。
//
// 放在助理页最上面一块 —— 这是用户与「助理的另一半」唯一的直接接触面。
// 原则同整页：先列要人处理的，健康的状态只占一行数字。

import { ChevronRight } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Input, Segmented, Textarea } from "../../components/ui/Input";
import { Badge, Card, Collapse, Section } from "../../components/ui/Panel";
import {
  ACTION_CLASS_LABEL,
  type ActionClass,
  type AssistantView,
  answerAssistantEscalation,
  type Charter,
  deleteAssistantCharter,
  type Escalation,
  getAssistantLedger,
  IRREVERSIBLE_ACTIONS,
  type LedgerEntry,
  type SuperviseMode,
  saveAssistantCharter,
  saveAssistantSettings,
} from "../../lib/api";
import { fullTime } from "../../lib/format";
import { toastError, toastOk } from "../../lib/toast";

export function SuperviseSection({ a }: { a: AssistantView }) {
  const open = (a.escalations ?? []).filter((e) => e.status === "open");
  const m = a.metrics;
  return (
    <Section
      title="值守"
      action={
        <Badge tone={a.settings.supervisor?.mode === "assist" ? "green" : "blue"}>
          {a.settings.supervisor?.mode === "assist"
            ? "授权内自决"
            : a.settings.supervisor?.mode === "off"
              ? "已关闭"
              : "只写建议"}
        </Badge>
      }
    >
      {m && (
        <>
          <div className="mb-2 flex items-center gap-2">
            <span className="text-[11px] text-dim">值守方式</span>
            <Segmented<SuperviseMode>
              value={a.settings.supervisor?.mode ?? "observe"}
              onChange={(mode) => {
                saveAssistantSettings({
                  ...a.settings,
                  supervisor: { ...a.settings.supervisor, mode },
                }).catch(toastError);
              }}
              options={[
                { id: "off", label: "关闭" },
                { id: "observe", label: "只写建议" },
                { id: "assist", label: "授权内自决" },
              ]}
            />
            <span className="text-[10px] text-dim">
              只写建议时不向工单发指令，只把「我会这样定」记进台账
            </span>
          </div>
          <div className="mb-3 grid grid-cols-2 gap-2 sm:grid-cols-5">
            <Stat label="今天请示" value={m.escalations_today} />
            <Stat label="今天自决" value={m.directives_today} />
            <Stat
              label="自决率"
              value={m.self_resolved === null ? "—" : `${Math.round(m.self_resolved * 100)}%`}
            />
            <Stat label="未决请示" value={m.open_escalations} />
            <Stat label="在办工单" value={m.open_tickets} />
          </div>
        </>
      )}

      {open.length === 0 ? (
        <p className="mb-3 text-[12px] text-dim">
          没有等你拍板的事。工单的汇报先由值守消化，能自决的不会来打扰你。
        </p>
      ) : (
        <div className="mb-3 space-y-2">
          {open.map((e) => (
            <EscalationCard key={e.id} e={e} />
          ))}
        </div>
      )}

      <Charters list={a.charters ?? []} />
      <LedgerBox />
    </Section>
  );
}

function Stat({ label, value }: { label: string; value: number | string }) {
  return (
    <div className="rounded-lg bg-card px-3 py-2">
      <div className="text-[16px] font-semibold tabular-nums">{value}</div>
      <div className="text-[10px] text-dim">{label}</div>
    </div>
  );
}

function remaining(ms: number): string {
  const left = ms - Date.now();
  if (left <= 0) return "已过期";
  const min = Math.ceil(left / 60_000);
  if (min < 60) return `${min} 分钟后搁置`;
  return `${Math.ceil(min / 60)} 小时后搁置`;
}

function EscalationCard({ e }: { e: Escalation }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const send = (choice: string | null) => {
    const said = text.trim();
    if (!choice && !said) return;
    setBusy(true);
    answerAssistantEscalation(e.id, choice, said || null)
      .then(() => {
        setText("");
        toastOk("已经把你的批示送进工单");
      })
      .catch(toastError)
      .finally(() => setBusy(false));
  };
  return (
    <Card className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <Badge tone="orange">等你拍板</Badge>
        <span className="text-[13px] font-medium">{e.ticket_label}</span>
        <span className="ml-auto text-[11px] text-dim">{remaining(e.expires_at)}</span>
      </div>
      <p className="whitespace-pre-wrap text-[12px]">
        <span className="text-dim">发生了什么：</span>
        {e.brief.situation}
      </p>
      <p className="whitespace-pre-wrap text-[12px]">
        <span className="text-dim">我的建议：</span>
        <span className="font-medium">{e.brief.proposal}</span>
      </p>
      {e.brief.options.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {e.brief.options.map((o) => (
            <Button
              key={o.id}
              size="sm"
              variant={o.risky ? "secondary" : "primary"}
              disabled={busy}
              title={o.consequence}
              onClick={() => send(o.id)}
            >
              {o.label}
              {o.risky && <span className="ml-1 text-[10px] opacity-70">不可逆</span>}
            </Button>
          ))}
        </div>
      )}
      <p className="text-[11px] text-dim">你不管的话：{e.brief.if_silent}</p>
      <div className="flex items-start gap-2">
        <Textarea
          value={text}
          onChange={(ev) => setText(ev.target.value)}
          placeholder="或者直接说你的决定…"
          rows={2}
          className="flex-1"
        />
        <Button size="sm" disabled={busy || !text.trim()} onClick={() => send(null)}>
          按我说的办
        </Button>
      </div>
      {e.brief.refs.length > 0 && (
        <p className="text-[10px] text-dim">依据：{e.brief.refs.join(" · ")}</p>
      )}
    </Card>
  );
}

function Charters({ list }: { list: Charter[] }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="mt-1">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex items-center gap-1 text-[11px] text-dim hover:text-ink"
      >
        <ChevronRight size={11} className={`transition-transform ${open ? "rotate-90" : ""}`} />
        授权书
        <span className="opacity-70">值守能自己决定什么、什么必须问你</span>
      </button>
      <Collapse open={open}>
        <div className="mt-2 space-y-2">
          {list.map((c) => (
            <CharterCard key={c.id} c={c} />
          ))}
          <AddCharter />
        </div>
      </Collapse>
    </div>
  );
}

const DEFAULT_MAY: ActionClass[] = ["inspect", "run_checks", "edit_code", "retry", "adjust_plan"];

function scopeLabel(c: Charter): string {
  if (c.scope.kind === "global") return "所有项目";
  if (c.scope.kind === "project") return c.scope.value;
  return `来源 ${c.scope.value}`;
}

function CharterCard({ c }: { c: Charter }) {
  const [draft, setDraft] = useState<Charter>(c);
  const [checksText, setChecksText] = useState(c.checks.join("\n"));
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    setDraft(c);
    setChecksText(c.checks.join("\n"));
  }, [c]);
  const toggle = (a: ActionClass) => {
    const has = draft.may.includes(a);
    setDraft({
      ...draft,
      may: has ? draft.may.filter((x) => x !== a) : [...draft.may, a],
    });
  };
  const checks = checksText
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
  const dirty =
    JSON.stringify(draft.may) !== JSON.stringify(c.may) ||
    JSON.stringify(checks) !== JSON.stringify(c.checks);
  const save = () => {
    setBusy(true);
    saveAssistantCharter({ ...draft, checks })
      .then(() => toastOk("授权书已保存"))
      .catch(toastError)
      .finally(() => setBusy(false));
  };
  const remove = () => {
    setBusy(true);
    deleteAssistantCharter(c.id)
      .then(() => toastOk("已删除这份授权书"))
      .catch(toastError)
      .finally(() => setBusy(false));
  };
  return (
    <Card className="space-y-2">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-[12px] font-medium">{c.name}</span>
        <Badge>{scopeLabel(c)}</Badge>
        <Badge tone={c.granted === "user" ? "blue" : undefined}>
          {c.granted === "user" ? "你授予的" : "默认"}
        </Badge>
        {c.expires_at && <span className="text-[10px] text-dim">到 {fullTime(c.expires_at)}</span>}
        <span className="ml-auto flex gap-1">
          <Button size="sm" disabled={busy || !dirty} onClick={save}>
            保存
          </Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={remove}>
            删除
          </Button>
        </span>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {(Object.keys(ACTION_CLASS_LABEL) as ActionClass[]).map((a) => {
          const locked = IRREVERSIBLE_ACTIONS.includes(a);
          const on = draft.may.includes(a);
          return (
            <button
              key={a}
              type="button"
              disabled={locked}
              onClick={() => toggle(a)}
              title={locked ? "不可逆动作永远只能逐次请示" : undefined}
              className={`rounded px-1.5 py-0.5 text-[10px] ${
                locked
                  ? "cursor-not-allowed bg-ink/[0.03] text-dim/50"
                  : on
                    ? "bg-tone-blue/15 text-tone-blue"
                    : "bg-bubble text-dim hover:text-ink"
              }`}
            >
              {ACTION_CLASS_LABEL[a]}
            </button>
          );
        })}
      </div>
      <p className="text-[10px] text-dim">
        灰掉的不可逆动作（推送、发消息、删除、花钱）永远只能逐次请示，不会因为授权书而放开。
      </p>
      <Input
        value={checksText}
        onChange={(e) => setChecksText(e.target.value)}
        placeholder="验证命令，一行一条（如 cargo test / bun run check）；值守核对证据时会跑"
        mono
        className="text-[11px]"
      />
    </Card>
  );
}

function AddCharter() {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const add = () => {
    const scope: Charter["scope"] = path.trim()
      ? { kind: "project", value: path.trim() }
      : { kind: "global" };
    setBusy(true);
    saveAssistantCharter({
      id: "",
      name: name.trim() || "新授权书",
      scope,
      may: DEFAULT_MAY,
      must_ask: IRREVERSIBLE_ACTIONS,
      checks: [],
      notify: "immediate",
      granted: "default",
      expires_at: null,
      note: "",
    })
      .then(() => {
        setOpen(false);
        setName("");
        setPath("");
        toastOk("已新建授权书");
      })
      .catch(toastError)
      .finally(() => setBusy(false));
  };
  if (!open) {
    return (
      <Button size="sm" variant="ghost" onClick={() => setOpen(true)}>
        + 新建授权书
      </Button>
    );
  }
  return (
    <Card className="space-y-2">
      <Input
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder="名字，如「这个仓库的日常维护」"
      />
      <Input
        value={path}
        onChange={(e) => setPath(e.target.value)}
        placeholder="项目绝对路径（留空 = 所有项目）"
        mono
      />
      <div className="flex gap-2">
        <Button size="sm" disabled={busy} onClick={add}>
          新建
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
          取消
        </Button>
      </div>
    </Card>
  );
}

function LedgerBox() {
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<LedgerEntry[] | null>(null);
  useEffect(() => {
    if (!open || rows) return;
    getAssistantLedger(null, 60).then(setRows).catch(toastError);
  }, [open, rows]);
  return (
    <div className="mt-3">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="flex items-center gap-1 text-[11px] text-dim hover:text-ink"
      >
        <ChevronRight size={11} className={`transition-transform ${open ? "rotate-90" : ""}`} />
        决策台账
      </button>
      <Collapse open={open}>
        <div className="mt-2 space-y-1">
          {!rows && <p className="text-[11px] text-dim">读取中…</p>}
          {rows?.length === 0 && <p className="text-[11px] text-dim">还没有记录。</p>}
          {rows?.map((e) => (
            <div key={e.id} className="flex gap-2 rounded bg-card px-2 py-1 text-[11px]">
              <span className="w-14 shrink-0 text-dim">{fullTime(e.at).slice(11)}</span>
              <Badge>{e.kind}</Badge>
              <span className="shrink-0 text-dim">{e.actor}</span>
              <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-dim">
                {JSON.stringify(e.data)}
              </span>
            </div>
          ))}
        </div>
      </Collapse>
    </div>
  );
}
