// 常驻助理面板。
//
// 照原型（dsh-superagent）的面板原则排：**先列要人处理的，健康的状态不占地方。**
//
//   1. 等你处理 —— 待查看 / 等拍板 / 受阻 / 失败 / 跑完未汇报的工单，观察模式下「本来会接」
//      的那几条，连续失败的观察源。全都没有时这一节整个不显示。
//   2. 进行中 —— 排队与在跑的工单。
//   3. 观察源 —— 盯着什么、多久看一次。
//   4. 接单规则与工作约定 —— 什么事可以替你做、做到什么程度。
//   5. 记录 —— 最近的判定（含忽略的），折起来；以及已结束的工单。
//
// 数据整份来自 store 里的 `assistant`（`assistant_changed` 时重拉），这页不自己存副本。

import {
  Check,
  ChevronRight,
  CircleAlert,
  CornerUpLeft,
  Eye,
  ListChecks,
  MessageSquare,
  Pause,
  Pencil,
  Play,
  Plus,
  Radar,
  RefreshCw,
  RotateCcw,
  Square,
  Trash,
  X,
} from "lucide-react";
import { type ReactNode, useEffect, useState } from "react";
import { TicketReply } from "../../components/TicketReply";
import { Button, IconButton } from "../../components/ui/Button";
import { Field, Input, Segmented, Switch, Textarea } from "../../components/ui/Input";
import {
  Badge,
  Banner,
  Card,
  Collapse,
  ConfirmDialog,
  EmptyState,
  Loading,
  Modal,
  Section,
} from "../../components/ui/Panel";
import { Select } from "../../components/ui/Select";
import {
  type AssistantDecision,
  type AssistantEpisode,
  type AssistantMemory,
  type AssistantRule,
  type AssistantSettings,
  type AssistantSource,
  type AssistantTicket,
  approveAssistantDecision,
  assistantTicketAction,
  deleteAssistantSource,
  dismissAssistantDecision,
  getAssistantEpisode,
  pollAssistantSource,
  reassessAssistantDecision,
  saveAssistantMemory,
  saveAssistantSettings,
  saveAssistantSource,
  type TicketStatus,
  testAssistantCommand,
  type Verdict,
} from "../../lib/api";
import { fullTime, relTime } from "../../lib/format";
import { useModelCatalog } from "../../lib/modelLabel";
import {
  ASSISTANT_CHAT_ID,
  jumpToTurn,
  openAssistantChat,
  openSession,
  refreshAssistant,
} from "../../lib/sessionOps";
import { toastError, toastOk } from "../../lib/toast";
import { useStore } from "../../store";
import { PageIntro } from "../settings/AsyncBody";

// ---------------------------------------------------------------------------
// 词表
// ---------------------------------------------------------------------------

const STATUS_LABEL: Record<TicketStatus, string> = {
  queued: "排队中",
  working: "进行中",
  ready: "待查看",
  blocked: "受阻",
  failed: "失败",
  needs_user: "等你拍板",
  unreported: "跑完未汇报",
  dropped: "无需处理",
  done: "已完成",
  stopped: "已叫停",
};

type Tone = "blue" | "green" | "orange" | "violet" | "pink";

const STATUS_TONE: Record<TicketStatus, Tone | undefined> = {
  queued: undefined,
  working: "blue",
  ready: "green",
  blocked: "orange",
  failed: "pink",
  needs_user: "violet",
  unreported: "orange",
  dropped: undefined,
  done: undefined,
  stopped: undefined,
};

const NEEDS_PERSON: TicketStatus[] = ["ready", "needs_user", "blocked", "failed", "unreported"];
const ACTIVE: TicketStatus[] = ["queued", "working"];

const VERDICT_LABEL: Record<Verdict, string> = {
  accept: "已接单",
  would_accept: "本来会接",
  attach: "送进工单",
  ignore: "忽略",
  unavailable: "评估失败",
};

/** 与后端 `source::FAILURES_TO_SHOW` 一致。 */
const FAILURES_TO_SHOW = 3;

function minutesLabel(m: number): string {
  if (m % 60 === 0) return `每 ${m / 60} 小时`;
  return `每 ${m} 分钟`;
}

/** 一行标题 + 点开的内容。 */
function Disclosure({ title, children }: { title: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button
        onClick={() => setOpen((o) => !o)}
        className="flex items-center gap-1 text-[11px] text-dim hover:text-ink"
      >
        <ChevronRight size={11} className={`transition-transform ${open ? "rotate-90" : ""}`} />
        {title}
      </button>
      <Collapse open={open}>{children}</Collapse>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 页面
// ---------------------------------------------------------------------------

export function AssistantPage() {
  const a = useStore((s) => s.assistant);

  // 打开面板时拉一次：事件只在变化时来，界面刚挂上时不一定有最新的。
  useEffect(() => {
    void refreshAssistant();
  }, []);

  if (!a) {
    return (
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-3xl px-6 py-6">
          <Loading />
        </div>
      </div>
    );
  }

  const needs = a.tickets
    .filter((t) => NEEDS_PERSON.includes(t.status))
    .sort((x, y) => y.updated_at - x.updated_at);
  const approvals = a.decisions.filter(
    (d) => d.verdict === "would_accept" && !d.ticket_id && !d.dismissed,
  );
  const broken = a.sources.filter((s) => s.enabled && s.state.failures >= FAILURES_TO_SHOW);
  const active = a.tickets
    .filter((t) => ACTIVE.includes(t.status))
    .sort((x, y) => x.created_at - y.created_at);
  const ended = a.tickets
    .filter((t) => !NEEDS_PERSON.includes(t.status) && !ACTIVE.includes(t.status))
    .sort((x, y) => y.updated_at - x.updated_at);
  const nothingWaits = needs.length + approvals.length + broken.length === 0;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto max-w-3xl px-6 py-6">
        <Header settings={a.settings} />

        <PageIntro>
          助理按你配的观察源定时检查（跑一条命令，看输出里新出现的行），按接单规则判断要不要替你开工。
          开了工就在后台开一条对话做完，只在需要你的时候发系统通知。关掉窗口它也在托盘里接着盯；
          数据存在 <span className="mono">~/.carter-app/assistant.json</span>。
        </PageIntro>

        <FirstRun
          hasSources={a.sources.length > 0}
          hasRules={a.settings.rules.some((r) => r.enabled)}
        />

        {!nothingWaits && (
          <Section title="等你处理">
            <div className="space-y-2">
              {broken.map((s) => (
                <Banner key={s.id} kind="error">
                  观察源「{s.name}」连续 {s.state.failures} 次检查失败：{s.state.last_error}
                </Banner>
              ))}
              {approvals.map((d) => (
                <ApprovalCard key={d.id} d={d} />
              ))}
              {needs.map((t) => (
                <TicketCard key={t.id} t={t} />
              ))}
            </div>
          </Section>
        )}

        {active.length > 0 && (
          <Section title="进行中">
            <div className="space-y-2">
              {active.map((t) => (
                <TicketCard key={t.id} t={t} />
              ))}
            </div>
          </Section>
        )}

        <SourcesSection sources={a.sources} busy={a.busy} />
        <RulesSection settings={a.settings} />
        <MemorySection memories={a.memories ?? []} episodes={a.episodes ?? []} />
        <HistorySection decisions={a.decisions} ended={ended} />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 顶栏：模式与暂停
// ---------------------------------------------------------------------------

function Header({ settings }: { settings: AssistantSettings }) {
  const [busy, setBusy] = useState(false);
  const save = (patch: Partial<AssistantSettings>) => {
    setBusy(true);
    saveAssistantSettings({ ...settings, ...patch })
      .catch(toastError)
      .finally(() => setBusy(false));
  };
  return (
    <div className="mb-4 flex items-center gap-2">
      <Radar size={16} className="text-tone-blue" />
      <h1 className="text-[16px] font-semibold">常驻助理</h1>
      {settings.paused && <Badge tone="orange">已暂停</Badge>}
      <span className="ml-auto flex items-center gap-2">
        <Button size="sm" onClick={() => void openAssistantChat()}>
          和助理聊
        </Button>
        <Segmented<"observe" | "auto">
          value={settings.mode}
          onChange={(mode) => save({ mode })}
          options={[
            { id: "observe", label: "只观察" },
            { id: "auto", label: "自动接单" },
          ]}
        />
        <Button size="sm" disabled={busy} onClick={() => save({ paused: !settings.paused })}>
          {settings.paused ? <Play size={12} /> : <Pause size={12} />}
          {settings.paused ? "恢复" : "暂停"}
        </Button>
      </span>
    </div>
  );
}

/** 什么都还没配时的引导。配齐之后不再出现。 */
function FirstRun({ hasSources, hasRules }: { hasSources: boolean; hasRules: boolean }) {
  if (hasSources && hasRules) return null;
  return (
    <Banner kind="info">
      <div className="space-y-1">
        <div className="font-medium text-ink">三步让它开始干活</div>
        <div className={hasSources ? "line-through opacity-60" : ""}>
          1. 加一个观察源：一条每行输出一件事的命令，比如 GitHub 通知、主干 CI 状态、钉钉待办。
        </div>
        <div className={hasRules ? "line-through opacity-60" : ""}>
          2. 写一条接单规则：用一句话说清什么样的事可以替你做。
        </div>
        <div>3. 先在「只观察」下跑几天，看它判的「本来会接」对不对，再切到「自动接单」。</div>
      </div>
    </Banner>
  );
}

// ---------------------------------------------------------------------------
// 工单
// ---------------------------------------------------------------------------

function TicketCard({ t }: { t: AssistantTicket }) {
  const [busy, setBusy] = useState(false);
  const [confirmStop, setConfirmStop] = useState(false);
  // 这张工单最近一条汇报在助理对话的第几轮：卡片上「去对话」直接落在那儿，
  // 不用在一长串汇报里自己翻。
  const reportTurn = useStore((s) => s.assistant?.report_turns?.[t.id]);

  const act = (action: Parameters<typeof assistantTicketAction>[1], ok?: string) => {
    setBusy(true);
    assistantTicketAction(t.id, action)
      .then(() => {
        if (ok) toastOk(ok);
      })
      .catch(toastError)
      .finally(() => setBusy(false));
  };

  const waiting = NEEDS_PERSON.includes(t.status);
  const canRetry = ["blocked", "failed", "unreported"].includes(t.status);
  const live = !["done", "stopped", "dropped"].includes(t.status);

  return (
    <Card className="space-y-2">
      <div className="flex items-center gap-2">
        <ListChecks size={13} className="shrink-0 text-dim" />
        <span className="mono shrink-0 text-xs text-dim">#{t.number}</span>
        <span className="truncate text-sm font-medium text-ink" title={t.title}>
          {t.title}
        </span>
        <Badge tone={STATUS_TONE[t.status]}>{STATUS_LABEL[t.status]}</Badge>
        {t.pending.length > 0 && t.status !== "queued" && (
          <Badge title="等着送进执行会话的消息">{t.pending.length} 条待送</Badge>
        )}
        <span className="ml-auto flex shrink-0 items-center gap-1">
          {t.session_id && (
            <IconButton
              title="打开工单对话"
              onClick={() => t.session_id && openSession(t.session_id)}
            >
              <MessageSquare size={12} />
            </IconButton>
          )}
          {reportTurn !== undefined && (
            <IconButton
              title="跳到助理对话里的这条汇报"
              onClick={() => jumpToTurn(ASSISTANT_CHAT_ID, reportTurn)}
            >
              <CornerUpLeft size={12} />
            </IconButton>
          )}
          {canRetry && (
            <IconButton
              title="从停下的地方再来一次"
              disabled={busy}
              onClick={() => act({ type: "retry" })}
            >
              <RotateCcw size={12} />
            </IconButton>
          )}
          {waiting && (
            <IconButton
              title="看过了，结束"
              disabled={busy}
              onClick={() => act({ type: "done" }, "已结束")}
            >
              <Check size={12} />
            </IconButton>
          )}
          {live && (
            <IconButton title="叫停" disabled={busy} onClick={() => setConfirmStop(true)}>
              <Square size={12} />
            </IconButton>
          )}
        </span>
      </div>

      {t.question && (
        <div className="rounded-md bg-tone-violet-soft px-3 py-2 text-xs leading-relaxed text-ink">
          {t.question}
        </div>
      )}
      {t.summary && (
        <div className="whitespace-pre-wrap text-xs leading-relaxed text-ink/80">{t.summary}</div>
      )}

      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[11px] text-dim">
        {t.rule ? <span>规则「{t.rule}」</span> : null}
        <span>来源「{t.source_name}」</span>
        <span title={fullTime(t.updated_at)}>{relTime(t.updated_at)}</span>
        <span className="mono truncate">{t.cwd}</span>
      </div>

      {waiting && <TicketReply t={t} />}

      <Disclosure title="任务与事件">
        <div className="space-y-2 pt-1 text-[11px] leading-relaxed text-dim">
          <div className="whitespace-pre-wrap text-ink/80">{t.task}</div>
          <ul className="mono space-y-0.5">
            {t.events.map((e, i) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: 事件日志只追加，同一句话可以重复出现
              <li key={i} className="break-all">
                {e}
              </li>
            ))}
          </ul>
        </div>
      </Disclosure>

      {confirmStop && (
        <ConfirmDialog
          title={`叫停 #${t.number}`}
          danger
          confirmLabel="叫停"
          message={
            <>
              正在跑的这一轮会被打断，之后来源里与它相关的新事件也不再送进来。
              执行对话本身留着，可以点开接着聊。
            </>
          }
          onCancel={() => setConfirmStop(false)}
          onConfirm={() => {
            setConfirmStop(false);
            act({ type: "stop" }, "已叫停");
          }}
        />
      )}
    </Card>
  );
}

/** 观察模式下「本来会接」的一条：批准就开工，不用就划掉。 */
function ApprovalCard({ d }: { d: AssistantDecision }) {
  const [busy, setBusy] = useState(false);
  return (
    <Card className="space-y-1.5">
      <div className="flex items-center gap-2">
        <Eye size={13} className="shrink-0 text-tone-blue" />
        <span className="truncate text-sm font-medium text-ink">{d.title ?? "未命名"}</span>
        <Badge tone="blue">本来会接</Badge>
        <span className="ml-auto flex shrink-0 items-center gap-1">
          <Button
            size="sm"
            variant="primary"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              approveAssistantDecision(d.id)
                .then(() => toastOk("已开工"))
                .catch(toastError)
                .finally(() => setBusy(false));
            }}
          >
            <Play size={12} />
            开工
          </Button>
          <IconButton
            title="不用"
            disabled={busy}
            onClick={() => dismissAssistantDecision(d.id).catch(toastError)}
          >
            <X size={12} />
          </IconButton>
        </span>
      </div>
      <div className="text-xs leading-relaxed text-ink/80">{d.task}</div>
      <div className="text-[11px] text-dim">
        规则「{d.rule}」· 来源「{d.source_name}」· {relTime(d.at)} · {d.reason}
      </div>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// 观察源
// ---------------------------------------------------------------------------

function SourcesSection({ sources, busy }: { sources: AssistantSource[]; busy: string[] }) {
  const [editing, setEditing] = useState<AssistantSource | null | undefined>(undefined);
  const [confirming, setConfirming] = useState<AssistantSource | null>(null);
  const [polling, setPolling] = useState<string | null>(null);

  const poll = (s: AssistantSource) => {
    setPolling(s.id);
    pollAssistantSource(s.id)
      .then((n) =>
        toastOk(
          s.state.baselined
            ? n > 0
              ? `「${s.name}」有 ${n} 条新事件，已交给评估`
              : `「${s.name}」没有新事件`
            : `「${s.name}」已记下现状，之后出现的新行才算事件`,
        ),
      )
      .catch(toastError)
      .finally(() => setPolling(null));
  };

  return (
    <Section
      title="观察源"
      action={
        <Button size="sm" onClick={() => setEditing(null)}>
          <Plus size={12} />
          添加
        </Button>
      }
    >
      {sources.length === 0 ? (
        <EmptyState>
          还没有观察源。加一条命令，它的输出里每出现一行新的，就是一件可能要处理的事。
        </EmptyState>
      ) : (
        <div className="space-y-2">
          {sources.map((s) => {
            const checking = busy.includes(s.id) || polling === s.id;
            return (
              <Card key={s.id} className="space-y-1">
                <div className="flex items-center gap-2">
                  <span className="truncate text-sm font-medium text-ink">{s.name}</span>
                  <Badge>{minutesLabel(s.every_minutes)}</Badge>
                  {!s.enabled && <Badge tone="orange">已停用</Badge>}
                  {checking && <Badge tone="blue">检查中</Badge>}
                  <span className="ml-auto flex shrink-0 items-center gap-1">
                    <Switch
                      label={`${s.enabled ? "停用" : "启用"} ${s.name}`}
                      checked={s.enabled}
                      onChange={(enabled) =>
                        saveAssistantSource({ ...s, enabled }).catch(toastError)
                      }
                    />
                    <IconButton title="立即检查" disabled={checking} onClick={() => poll(s)}>
                      <RefreshCw size={12} />
                    </IconButton>
                    <IconButton title="编辑" onClick={() => setEditing(s)}>
                      <Pencil size={12} />
                    </IconButton>
                    <IconButton title="删除" onClick={() => setConfirming(s)}>
                      <Trash size={12} />
                    </IconButton>
                  </span>
                </div>
                <div className="mono truncate text-[11px] text-dim" title={s.command}>
                  $ {s.command}
                </div>
                <div className="flex flex-wrap gap-x-3 text-[11px] text-dim">
                  {s.state.last_polled_at ? (
                    <span title={fullTime(s.state.last_polled_at)}>
                      上次检查 {relTime(s.state.last_polled_at)}
                      {s.state.last_new > 0 && `，${s.state.last_new} 条新事件`}
                    </span>
                  ) : (
                    <span>还没检查过</span>
                  )}
                  {!s.state.baselined && s.state.last_polled_at === null && (
                    <span>第一次检查只记下现状</span>
                  )}
                  <span className="mono truncate">{s.cwd}</span>
                </div>
                {s.state.last_error && s.state.failures < FAILURES_TO_SHOW && (
                  <div className="text-[11px] text-[var(--danger)]">
                    上次失败：{s.state.last_error}
                  </div>
                )}
              </Card>
            );
          })}
        </div>
      )}

      {editing !== undefined && (
        <SourceEditor editing={editing} onClose={() => setEditing(undefined)} />
      )}
      {confirming && (
        <ConfirmDialog
          title="删除观察源"
          danger
          confirmLabel="删除"
          message={<>不再检查「{confirming.name}」。已经开出来的工单不受影响。</>}
          onCancel={() => setConfirming(null)}
          onConfirm={() => {
            const id = confirming.id;
            setConfirming(null);
            deleteAssistantSource(id).catch(toastError);
          }}
        />
      )}
    </Section>
  );
}

/** 编辑框里的示例。每条都是「一行一件事」的写法 —— 按行比对靠的就是这个。 */
const EXAMPLES: { label: string; name: string; command: string; minutes: number }[] = [
  {
    label: "GitHub 通知",
    name: "GitHub 通知",
    command:
      "gh api notifications --jq '.[] | \"\\(.repository.full_name) [\\(.subject.type)] \\(.subject.title) — \\(.reason) \\(.updated_at)\"'",
    minutes: 5,
  },
  {
    label: "主干 CI",
    name: "主干 CI 失败",
    command:
      'gh run list --branch main --limit 10 --json databaseId,conclusion,displayTitle --jq \'.[] | select(.conclusion=="failure") | "CI 失败 #\\(.databaseId) \\(.displayTitle)"\'',
    minutes: 10,
  },
  {
    label: "指给我的 PR 评审",
    name: "待我评审的 PR",
    command:
      "gh search prs --review-requested=@me --state=open --json repository,number,title,updatedAt --jq '.[] | \"\\(.repository.nameWithOwner)#\\(.number) \\(.title) \\(.updatedAt)\"'",
    minutes: 15,
  },
];

function SourceEditor({
  editing,
  onClose,
}: {
  editing: AssistantSource | null;
  onClose: () => void;
}) {
  const cwd = useStore((s) => s.cwd);
  const [d, setD] = useState<AssistantSource>(
    () =>
      editing ?? {
        id: "",
        name: "",
        command: "",
        cwd,
        every_minutes: 10,
        enabled: true,
        state: {
          baselined: false,
          next_at: 0,
          last_polled_at: null,
          last_new: 0,
          last_error: null,
          failures: 0,
        },
      },
  );
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [output, setOutput] = useState<{ ok: boolean; text: string } | null>(null);

  const set = <K extends keyof AssistantSource>(k: K, v: AssistantSource[K]) =>
    setD((p) => ({ ...p, [k]: v }));
  const valid =
    d.name.trim() !== "" &&
    d.command.trim() !== "" &&
    d.cwd.trim() !== "" &&
    d.every_minutes >= 1 &&
    d.every_minutes <= 24 * 60;

  const test = () => {
    setTesting(true);
    testAssistantCommand(d.command, d.cwd)
      .then((text) => setOutput({ ok: true, text: text.trim() || "（没有输出）" }))
      .catch((e) => setOutput({ ok: false, text: String(e) }))
      .finally(() => setTesting(false));
  };

  return (
    <Modal
      title={editing ? `编辑观察源 · ${editing.name}` : "添加观察源"}
      onClose={onClose}
      width={620}
      footer={
        <>
          <Button onClick={onClose} disabled={saving}>
            取消
          </Button>
          <Button
            variant="primary"
            disabled={!valid || saving}
            onClick={() => {
              setSaving(true);
              saveAssistantSource(d)
                .then(onClose)
                .catch(toastError)
                .finally(() => setSaving(false));
            }}
          >
            {saving ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      {!editing && (
        <div className="mb-3 flex flex-wrap items-center gap-1.5 text-[11px] text-dim">
          从示例开始：
          {EXAMPLES.map((x) => (
            <button
              key={x.label}
              className="rounded bg-ink/5 px-1.5 py-0.5 hover:bg-ink/10 hover:text-ink"
              onClick={() =>
                setD((p) => ({ ...p, name: x.name, command: x.command, every_minutes: x.minutes }))
              }
            >
              {x.label}
            </button>
          ))}
        </div>
      )}
      <Field label="名字">
        <Input
          value={d.name}
          onChange={(e) => set("name", e.target.value)}
          placeholder="例如：GitHub 通知"
        />
      </Field>
      <Field label="命令" hint="每行输出一件事。新出现的行会交给评估；第一次检查只记下现状。">
        <Textarea
          mono
          rows={3}
          value={d.command}
          onChange={(e) => set("command", e.target.value)}
        />
      </Field>
      <Field label="工作目录" hint="命令在这里跑；这个来源开出来的工单也在这里干活">
        <Input mono value={d.cwd} onChange={(e) => set("cwd", e.target.value)} />
      </Field>
      <Field label="多久检查一次（分钟）" hint="1 分钟 ~ 24 小时">
        <Input
          type="number"
          min={1}
          max={24 * 60}
          value={String(d.every_minutes)}
          onChange={(e) => set("every_minutes", Number(e.target.value))}
        />
      </Field>
      <div className="mb-2 flex items-center gap-2">
        <Button size="sm" disabled={testing || !d.command.trim()} onClick={test}>
          <Play size={12} />
          {testing ? "运行中…" : "试运行"}
        </Button>
        <span className="text-[11px] text-dim">只跑一下看输出，不记录、不评估</span>
      </div>
      {output && (
        <pre
          className={`mono max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-md bg-ink/5 p-2 text-[11px] ${
            output.ok ? "text-ink/80" : "text-[var(--danger)]"
          }`}
        >
          {output.text}
        </pre>
      )}
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// 规则与约定
// ---------------------------------------------------------------------------

function RulesSection({ settings }: { settings: AssistantSettings }) {
  const [editing, setEditing] = useState(false);
  const enabled = settings.rules.filter((r) => r.enabled);
  return (
    <Section
      title="接单规则"
      action={
        <Button size="sm" onClick={() => setEditing(true)}>
          <Pencil size={12} />
          编辑
        </Button>
      }
    >
      {settings.rules.length === 0 ? (
        <EmptyState>
          还没有规则 —— 没有规则时助理只记录、从不评估。用一句话写清什么样的事可以替你做，
          例如「主干 CI 失败时，定位原因并在新分支上修好，不推送」。
        </EmptyState>
      ) : (
        <div className="space-y-1.5">
          {settings.rules.map((r) => (
            <Card key={r.id} className={r.enabled ? "" : "opacity-50"}>
              <div className="flex items-center gap-2 text-sm font-medium text-ink">
                {r.name}
                {!r.enabled && <Badge>停用</Badge>}
              </div>
              <div className="mt-0.5 text-xs leading-relaxed text-dim">{r.instruction}</div>
            </Card>
          ))}
          <div className="px-1 text-[11px] text-dim">
            {enabled.length} 条启用 · 同时最多 {settings.max_running} 张工单在跑 ·{" "}
            {settings.mode === "auto" ? "命中就开工" : "只记录「本来会接」，等你批准"}
          </div>
        </div>
      )}
      {editing && <SettingsEditor settings={settings} onClose={() => setEditing(false)} />}
    </Section>
  );
}

function SettingsEditor({
  settings,
  onClose,
}: {
  settings: AssistantSettings;
  onClose: () => void;
}) {
  const [d, setD] = useState<AssistantSettings>(settings);
  const [saving, setSaving] = useState(false);
  const models = useModelCatalog();
  const modelOptions = [
    { value: "", label: "默认" },
    ...models.map((m) => ({ value: m.reference, label: m.label ?? m.reference })),
  ];

  const setRule = (i: number, patch: Partial<AssistantRule>) =>
    setD((p) => ({ ...p, rules: p.rules.map((r, j) => (j === i ? { ...r, ...patch } : r)) }));

  return (
    <Modal
      title="接单规则与工作约定"
      onClose={onClose}
      width={640}
      footer={
        <>
          <Button onClick={onClose} disabled={saving}>
            取消
          </Button>
          <Button
            variant="primary"
            disabled={saving}
            onClick={() => {
              setSaving(true);
              saveAssistantSettings(d)
                .then(onClose)
                .catch(toastError)
                .finally(() => setSaving(false));
            }}
          >
            {saving ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      <div className="mb-4 space-y-2">
        {d.rules.map((r, i) => (
          <div key={r.id || i} className="space-y-1.5 rounded-md bg-ink/[0.03] p-2">
            <div className="flex items-center gap-2">
              <Input
                value={r.name}
                onChange={(e) => setRule(i, { name: e.target.value })}
                placeholder="规则名（评估按名字引用）"
                className="text-xs"
              />
              <Switch
                label={r.enabled ? "停用" : "启用"}
                checked={r.enabled}
                onChange={(enabled) => setRule(i, { enabled })}
              />
              <IconButton
                title="删除这条"
                onClick={() => setD((p) => ({ ...p, rules: p.rules.filter((_, j) => j !== i) }))}
              >
                <Trash size={12} />
              </IconButton>
            </div>
            <Textarea
              rows={2}
              value={r.instruction}
              onChange={(e) => setRule(i, { instruction: e.target.value })}
              placeholder="什么样的事可以做、做到什么程度。例如：主干 CI 失败时，定位原因，在新分支上修好并跑通测试，不推送。"
              className="text-xs"
            />
          </div>
        ))}
        <Button
          size="sm"
          onClick={() =>
            setD((p) => ({
              ...p,
              rules: [...p.rules, { id: "", name: "", instruction: "", enabled: true }],
            }))
          }
        >
          <Plus size={12} />
          加一条规则
        </Button>
      </div>

      <Field
        label="工作约定"
        hint="每张工单的执行对话都会读到：做到什么程度停、怎么交付、什么不能做"
      >
        <Textarea
          rows={5}
          value={d.scope}
          onChange={(e) => setD((p) => ({ ...p, scope: e.target.value }))}
        />
      </Field>
      <div className="grid grid-cols-3 gap-3">
        <Field label="同时运行">
          <Input
            type="number"
            min={1}
            max={8}
            value={String(d.max_running)}
            onChange={(e) => setD((p) => ({ ...p, max_running: Number(e.target.value) }))}
          />
        </Field>
        <Field label="评估模型" hint="默认用副模型">
          <Select
            value={d.eval_model ?? ""}
            onChange={(v) => setD((p) => ({ ...p, eval_model: v || null }))}
            options={modelOptions}
          />
        </Field>
        <Field label="执行模型">
          <Select
            value={d.work_model ?? ""}
            onChange={(v) => setD((p) => ({ ...p, work_model: v || null }))}
            options={modelOptions}
          />
        </Field>
      </div>
      <Banner kind="info">
        评估只读事件、不跑工具，宁可漏接也不错接；拿不准时它会忽略，等后续事件说清楚。
        规则是写给模型看的文字，工具权限与「不能问人」这些限制由应用强制，规则改不了。
      </Banner>
    </Modal>
  );
}

// ---------------------------------------------------------------------------
// 记录
// ---------------------------------------------------------------------------

/**
 * 助理对话的长期记忆与往期纪要（见后端 `assistant/memory.rs`）。
 *
 * 记忆是模型每轮都会读的东西，必须能看见、能改、能删 —— 否则一条记错的「用户希望……」
 * 会悄悄影响以后所有的判断。每条带着出处（用户哪天说的哪句话），删之前能判断。
 */
function MemorySection({
  memories,
  episodes,
}: {
  memories: AssistantMemory[];
  episodes: AssistantEpisode[];
}) {
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  const add = () => {
    const t = draft.trim();
    if (!t) return;
    saveAssistantMemory(null, t)
      .then(() => {
        setDraft("");
        setAdding(false);
      })
      .catch(toastError);
  };
  return (
    <Section
      title="长期记忆"
      action={
        <Button size="sm" onClick={() => setAdding((x) => !x)}>
          <Plus size={12} />
          添加
        </Button>
      }
    >
      <p className="mb-2 text-[11px] text-dim">
        助理对话每天（或空闲几小时后）整理成一期：原话进往期存档，只把你说过的偏好与约定记在这里，
        每轮都会带给模型。也可以在对话里直接说「记住……」。
      </p>
      {adding && (
        <div className="mb-2 flex gap-2">
          <Input
            autoFocus
            value={draft}
            placeholder="例如：周末别打扰我，线上故障除外"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") add();
              if (e.key === "Escape") setAdding(false);
            }}
          />
          <Button size="sm" variant="primary" disabled={!draft.trim()} onClick={add}>
            保存
          </Button>
        </div>
      )}
      {memories.length === 0 && !adding ? (
        <div className="text-xs text-dim">还没有。</div>
      ) : (
        <div className="space-y-1">
          {memories.map((m) => (
            <MemoryRow key={m.id} m={m} />
          ))}
        </div>
      )}
      {episodes.length > 0 && (
        <div className="mt-3">
          <Disclosure title={`往期纪要（${episodes.length} 期）`}>
            <div className="space-y-1.5 pt-2">
              {[...episodes].reverse().map((e) => (
                <EpisodeRow key={e.number} e={e} />
              ))}
            </div>
          </Disclosure>
        </div>
      )}
    </Section>
  );
}

function MemoryRow({ m }: { m: AssistantMemory }) {
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(m.text);
  const [confirm, setConfirm] = useState(false);
  const save = () => {
    const t = text.trim();
    if (!t || t === m.text) {
      setEditing(false);
      return;
    }
    saveAssistantMemory(m.id, t)
      .then(() => setEditing(false))
      .catch(toastError);
  };
  return (
    <div className="group flex items-start gap-2 rounded-md px-2 py-1.5 hover:bg-ink/[0.03]">
      <div className="min-w-0 flex-1">
        {editing ? (
          <Input
            autoFocus
            value={text}
            onChange={(e) => setText(e.target.value)}
            onBlur={save}
            onKeyDown={(e) => {
              if (e.key === "Enter") save();
              if (e.key === "Escape") {
                setText(m.text);
                setEditing(false);
              }
            }}
          />
        ) : (
          <div className="text-sm text-ink">{m.text}</div>
        )}
        <div className="mt-0.5 truncate text-[11px] text-dim" title={m.source}>
          {m.source}
        </div>
      </div>
      <span className="flex shrink-0 gap-0.5 opacity-0 transition-opacity group-hover:opacity-100">
        <IconButton title="改" onClick={() => setEditing(true)}>
          <Pencil size={12} />
        </IconButton>
        <IconButton title="删掉" onClick={() => setConfirm(true)}>
          <Trash size={12} />
        </IconButton>
      </span>
      {confirm && (
        <ConfirmDialog
          title="删掉这条记忆"
          danger
          confirmLabel="删掉"
          message={<>助理之后就不再按「{m.text}」行事了。</>}
          onCancel={() => setConfirm(false)}
          onConfirm={() => {
            setConfirm(false);
            saveAssistantMemory(m.id, "").catch(toastError);
          }}
        />
      )}
    </div>
  );
}

/** 一期的纪要；点开看那一期的原话（往期存档）。 */
function EpisodeRow({ e }: { e: AssistantEpisode }) {
  const [open, setOpen] = useState(false);
  const [lines, setLines] = useState<{ who: string; text: string }[] | null>(null);
  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (next && lines === null) {
      getAssistantEpisode(e.number)
        .then((msgs) =>
          setLines(
            msgs
              .map((m) => ({
                who: m.role === "user" ? "你" : "助理",
                text: m.blocks
                  .filter((b) => b.kind === "text")
                  .map((b) => (b as { text: string }).text)
                  .join("\n")
                  .trim(),
              }))
              .filter((l) => l.text)
              .map((l) =>
                l.text.startsWith("【助理汇报】") || l.text.startsWith("【新事件】")
                  ? { who: "汇报", text: l.text }
                  : l,
              ),
          ),
        )
        .catch((err) => {
          setLines([]);
          toastError(err);
        });
    }
  };
  return (
    <div className="rounded-md px-2 py-1.5 text-[11px] hover:bg-ink/[0.03]">
      <button onClick={toggle} className="flex w-full items-start gap-2 text-left">
        <ChevronRight
          size={11}
          className={`mt-0.5 shrink-0 text-dim transition-transform ${open ? "rotate-90" : ""}`}
        />
        <span
          className="shrink-0 text-dim"
          title={`${fullTime(e.started_at)} ~ ${fullTime(e.ended_at)}`}
        >
          第 {e.number} 期 · {relTime(e.ended_at)}
        </span>
        <span className="min-w-0 flex-1 whitespace-pre-wrap text-ink/80">{e.summary}</span>
      </button>
      <Collapse open={open}>
        <div className="mt-1.5 max-h-80 space-y-1 overflow-y-auto border-l border-edge pl-3">
          {lines === null ? (
            <Loading />
          ) : lines.length === 0 ? (
            <div className="text-dim">这一期的存档读不出来。</div>
          ) : (
            lines.map((l, i) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: 存档是只读的一串对话，按位置区分
              <div key={i} className="whitespace-pre-wrap break-words">
                <span className="text-dim">{l.who}：</span>
                <span className="text-ink/80">{l.text}</span>
              </div>
            ))
          )}
        </div>
      </Collapse>
    </div>
  );
}

function HistorySection({
  decisions,
  ended,
}: {
  decisions: AssistantDecision[];
  ended: AssistantTicket[];
}) {
  if (decisions.length === 0 && ended.length === 0) return null;
  return (
    <Section title="记录">
      {ended.length > 0 && (
        <Disclosure title={`已结束的工单（${ended.length}）`}>
          <div className="space-y-2 pt-2">
            {ended.map((t) => (
              <TicketCard key={t.id} t={t} />
            ))}
          </div>
        </Disclosure>
      )}
      {decisions.length > 0 && (
        <Disclosure title={`最近的判定（${decisions.length}）`}>
          <div className="space-y-1 pt-2">
            {decisions.map((d) => (
              <DecisionRow key={d.id} d={d} />
            ))}
          </div>
        </Disclosure>
      )}
    </Section>
  );
}

function DecisionRow({ d }: { d: AssistantDecision }) {
  const [busy, setBusy] = useState(false);
  const tone: Tone | undefined =
    d.verdict === "accept"
      ? "green"
      : d.verdict === "would_accept"
        ? "blue"
        : d.verdict === "attach"
          ? "violet"
          : d.verdict === "unavailable"
            ? "pink"
            : undefined;
  return (
    <div className="rounded-md px-2 py-1.5 text-[11px] hover:bg-ink/[0.03]">
      <div className="flex items-center gap-2">
        <Badge tone={tone}>{VERDICT_LABEL[d.verdict]}</Badge>
        <span className="truncate text-ink/80" title={d.events.join("\n")}>
          {d.events[0]}
          {d.events.length > 1 && ` 等 ${d.events.length} 条`}
        </span>
        <span className="ml-auto shrink-0 text-dim" title={fullTime(d.at)}>
          {relTime(d.at)}
        </span>
        {(d.verdict === "unavailable" || d.verdict === "ignore") && (
          <IconButton
            title="重新评估"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              reassessAssistantDecision(d.id)
                .then((r) => toastOk(`重新评估：${VERDICT_LABEL[r.verdict]}`))
                .catch(toastError)
                .finally(() => setBusy(false));
            }}
          >
            <RotateCcw size={11} />
          </IconButton>
        )}
      </div>
      <div className="mt-0.5 flex gap-1 text-dim">
        {d.verdict === "unavailable" && <CircleAlert size={11} className="mt-0.5 shrink-0" />}
        <span>
          {d.source_name}
          {d.rule && ` · 规则「${d.rule}」`} · {d.reason}
        </span>
      </div>
    </div>
  );
}
