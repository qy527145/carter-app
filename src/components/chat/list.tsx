import { ArrowDown, Bot, Eraser, Radar } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { getAssistantEpisode, newAssistantEpisode } from "../../lib/api";
import { fullTime } from "../../lib/format";
import { displayName, modelTitle, useModelCatalog } from "../../lib/modelLabel";
import { ASSISTANT_CHAT_ID, fork, regenerate, retryReply } from "../../lib/sessionOps";
import { toastError } from "../../lib/toast";
import { historyToTurns, type Turn, useStore } from "../../store";
import { ConfirmDialog } from "../ui/Panel";
import { CursorLine } from "./fragments";
import { TurnList } from "./turns";

// ---------------------------------------------------------------------------
// 列表 + 贴底滚动
// ---------------------------------------------------------------------------

const SETTLE_MS = 1500;

export function MessageList() {
  const stored = useStore((s) => s.turns);
  const times = useStore((s) => s.times);
  const generating = useStore((s) => s.streaming);
  const awaitingUser = useStore((s) => s.pendingForm !== null);
  // 直发的那条先画成气泡（见 `QueuedPrompt.direct`）。`turn_started` 到了换成真的
  // 那条，两者下标相同，React 就地复用，看不出替换。
  const direct = useStore((s) => s.queued.find((q) => q.direct));
  const turns = useMemo<Turn[]>(
    () => (direct ? [...stored, { role: "user", text: direct.text }] : stored),
    [stored, direct],
  );
  const streaming = generating || direct !== undefined;
  const cwd = useStore((s) => s.cwd);
  const sessionId = useStore((s) => s.sessionId);
  const hasEpisodes = useStore((s) => (s.assistant?.episodes.length ?? 0) > 0);
  const scrollRef = useRef<HTMLDivElement>(null);
  // 用户往上翻时不要把他拽回底部 —— 只在「本来就在底部」时跟随。
  const stick = useRef(true);
  // 离底部够远时浮出「回到最新」—— 翻回去看了一眼，不该靠拖滚动条找回来。
  const [away, setAway] = useState(false);
  const contentRef = useRef<HTMLDivElement>(null);
  // 刚进一条会话后的「落定期」：内容还在陆续变高，要一路贴着底部。
  const settleUntil = useRef(0);

  // biome-ignore lint/correctness/useExhaustiveDependencies: 贴底跟随只看 turns / streaming 这两下变化，正文不直接读它们
  useEffect(() => {
    const el = scrollRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [turns, streaming]);

  // 换会话：直接落到底部，并复位「跟随」—— 上一条会话里翻到中间的状态不该带过来。
  useEffect(() => {
    stick.current = true;
    setAway(false);
    settleUntil.current = performance.now() + SETTLE_MS;
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
    if (sessionId && !window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      contentRef.current?.animate(
        [
          { opacity: 0, transform: "translateY(6px)" },
          { opacity: 1, transform: "none" },
        ],
        { duration: 240, easing: "cubic-bezier(0.23, 1, 0.32, 1)" },
      );
    }
  }, [sessionId]);

  // 助理对话的往期原话、图片都是后来才长出来的，且画在本期**上面**：只在换会话那一刻
  // 滚一次，内容一长出来就又停在中间了。只在落定期内跟随 —— 平时用户展开一张工具卡片，
  // 不该被拽去底部。
  useEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    const ro = new ResizeObserver(() => {
      const el = scrollRef.current;
      if (el && stick.current && performance.now() < settleUntil.current) {
        el.scrollTop = el.scrollHeight;
      }
    });
    ro.observe(content);
    return () => ro.disconnect();
  }, []);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
    stick.current = gap < 48;
    setAway(gap > 400);
  };

  const toBottom = () => {
    const el = scrollRef.current;
    if (!el) return;
    stick.current = true;
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" });
  };

  return (
    <div className="relative flex min-h-0 flex-1 flex-col">
      {/* 「跳到某一轮」的落点。放在滚动容器**同一层**：它要动这块滚动条，
          也得能压住「贴底跟随」（见组件里的说明）。 */}
      <TouchFocus stick={stick} />
      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="min-h-0 flex-1 overflow-y-auto px-4 py-6 [overflow-anchor:none]"
      >
        <div ref={contentRef} className="mx-auto max-w-3xl">
          {sessionId === ASSISTANT_CHAT_ID && (
            <PastEpisodes
              onGrow={() => {
                settleUntil.current = performance.now() + SETTLE_MS;
              }}
            />
          )}
          {turns.length === 0 ? (
            // 换会话的空档不画空状态：否则每次切换都先闪一下欢迎页。
            sessionId === null ? null : sessionId === ASSISTANT_CHAT_ID ? (
              hasEpisodes ? null : (
                <AssistantEmptyState />
              )
            ) : (
              <EmptyState />
            )
          ) : (
            <TurnList
              turns={turns}
              times={times}
              streaming={streaming}
              awaitingUser={awaitingUser}
              cwd={cwd}
              // 重试与分叉都作用在**这条会话**上 —— 侧边聊天那份 TurnList 一概不传。
              onRetry={
                sessionId
                  ? (turn, reply) =>
                      reply ? retryReply(sessionId, turn) : regenerate(sessionId, turn)
                  : undefined
              }
              onFork={sessionId ? (keep, draft) => void fork(sessionId, keep, draft) : undefined}
            />
          )}
        </div>
      </div>
      {/* 「回到最新」：贴着输入框上沿居中浮着。生成中且不在底部时带一个点 ——
          告诉人下面还在出字。不用时淡出并让出点击。 */}
      <button
        onClick={toBottom}
        tabIndex={away ? 0 : -1}
        aria-hidden={!away}
        title="回到最新"
        className={`carter-press absolute bottom-3 left-1/2 z-10 flex -translate-x-1/2 items-center gap-1.5 rounded-full bg-elevated px-3 py-1.5 text-xs text-dim shadow-lift ring-1 ring-edge transition-[opacity,translate] duration-200 ease-[var(--ease-out)] hover:text-ink ${
          away ? "translate-y-0 opacity-100" : "pointer-events-none translate-y-2 opacity-0"
        }`}
      >
        {streaming && <span className="size-1.5 animate-pulse rounded-full bg-tone-blue" />}
        <ArrowDown size={12} />
        回到最新
      </button>
    </div>
  );
}

const ASSISTANT_EXAMPLES = [
  "帮我盯着主干 CI，挂了就看看原因，能修就修",
  "有人给我提 PR 时告诉我，先别动手",
  "现在在盯什么？有什么要我处理的？",
];

/** 一开始带出最近几期的原话；更早的点「再往前看」。 */
const EPISODES_SHOWN = 2;

/**
 * 助理对话的往期原话，画在本期上面，每期末尾一条波浪线（游标）。
 *
 * 切期之后模型只看得到纪要与记忆，但人还要能往回翻 —— 原话在往期存档里，
 * 这里只读地画出来，不给重试、分叉这些改历史的入口。
 */
function PastEpisodes({ onGrow }: { onGrow: () => void }) {
  const episodes = useStore((s) => s.assistant?.episodes) ?? [];
  const [shown, setShown] = useState(EPISODES_SHOWN);
  const [loaded, setLoaded] = useState<Record<number, Turn[]>>({});
  const visible = episodes.slice(-shown);
  const missing = visible.filter((e) => !(e.number in loaded)).map((e) => e.number);
  const key = missing.join(",");

  // biome-ignore lint/correctness/useExhaustiveDependencies: 只在「缺哪几期」变化时拉一次；missing 每次渲染都是新数组，进依赖会反复重拉
  useEffect(() => {
    if (!key) return;
    let alive = true;
    Promise.all(
      missing.map((n) =>
        getAssistantEpisode(n)
          .then((h) => [n, historyToTurns(h)] as const)
          .catch(() => [n, [] as Turn[]] as const),
      ),
    ).then((got) => {
      if (!alive) return;
      setLoaded((p) => ({ ...p, ...Object.fromEntries(got) }));
      onGrow();
    });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  if (episodes.length === 0) return null;
  return (
    <div className="mb-6 flex flex-col gap-6">
      {episodes.length > shown && (
        <button
          onClick={() => setShown((n) => n + EPISODES_SHOWN)}
          className="self-center rounded-md px-2 py-1 text-xs text-dim hover:bg-ink/5 hover:text-ink"
        >
          再往前看（还有 {episodes.length - shown} 期）
        </button>
      )}
      {visible.map((e) => (
        <div key={e.number} className="flex flex-col gap-6">
          {loaded[e.number]?.length ? (
            <div className="opacity-70">
              <TurnList turns={loaded[e.number]} streaming={false} showPayload={false} />
            </div>
          ) : null}
          <CursorLine
            label={`第 ${e.number} 期到此 · 以上不再进上下文 · ${e.summary.split("\n")[0]}`}
            title={`${fullTime(e.started_at)} ~ ${fullTime(e.ended_at)}\n${e.summary}`}
          />
        </div>
      ))}
    </div>
  );
}

/** 助理对话的开场：说清它是什么，给几句能直接发的话。点一下填进输入框。 */
function AssistantEmptyState() {
  const setDraft = useStore((s) => s.setComposerDraft);
  return (
    <div className="flex flex-col items-center py-20 text-center">
      <div className="mb-5 flex size-11 items-center justify-center rounded-xl bg-ink text-[var(--accent-contrast)]">
        <Radar size={22} />
      </div>
      <div className="mb-1.5 text-[22px] font-semibold tracking-tight">卡特助理</div>
      <div className="mb-5 text-sm text-dim">
        告诉我要盯什么、什么事可以替你做。有事我会回到这里找你。
      </div>
      <div className="flex w-full max-w-md flex-col gap-1.5">
        {ASSISTANT_EXAMPLES.map((x) => (
          <button
            key={x}
            onClick={() => setDraft(x)}
            className="rounded-lg border border-edge px-3 py-2 text-left text-sm text-dim transition-colors hover:bg-ink/5 hover:text-ink"
          >
            {x}
          </button>
        ))}
      </div>
    </div>
  );
}

/** 助理对话的顶部条：它是谁、现在什么状态、要人处理几件，开新一期，以及去面板的入口。 */
export function AssistantHeader() {
  const a = useStore((s) => s.assistant);
  const setView = useStore((s) => s.setView);
  const [confirmNew, setConfirmNew] = useState(false);
  const running = a?.tickets.filter((t) => t.status === "working").length ?? 0;
  const facts = a
    ? [
        a.settings.paused ? "已暂停" : a.settings.mode === "auto" ? "自动接单" : "只观察",
        `盯着 ${a.sources.filter((s) => s.enabled).length} 个观察源`,
        ...(running > 0 ? [`${running} 张工单在跑`] : []),
      ]
    : [];
  return (
    <div className="flex h-11 shrink-0 items-center gap-2 border-b border-edge px-4">
      <Radar size={14} className="shrink-0 text-tone-blue" />
      <span className="shrink-0 text-sm font-medium text-ink">卡特助理</span>
      <span className="truncate text-xs text-dim">{facts.join(" · ")}</span>
      <button
        onClick={() => setConfirmNew(true)}
        title="之前的对话整理成纪要，不再进上下文"
        className="ml-auto flex shrink-0 items-center gap-1 rounded-md px-2 py-1 text-xs text-dim transition-colors hover:bg-ink/5 hover:text-ink"
      >
        <Eraser size={12} />
        开新一期
      </button>
      <button
        onClick={() => setView("assistant")}
        className="shrink-0 rounded-md px-2 py-1 text-xs text-dim transition-colors hover:bg-ink/5 hover:text-ink"
      >
        {a && a.attention > 0 ? `面板 · ${a.attention} 件待处理` : "面板"}
      </button>
      {confirmNew && (
        <ConfirmDialog
          title="开新一期对话"
          confirmLabel="开新一期"
          message={
            <>
              之前的对话整理成一条纪要，你说过的偏好记进长期记忆，原话存进往期存档（面板「往期纪要」里能翻）。
              之后助理只看纪要与记忆，不再带着原话。正在说话的话，等这一轮说完再切。
            </>
          }
          onCancel={() => setConfirmNew(false)}
          onConfirm={() => {
            setConfirmNew(false);
            newAssistantEpisode().catch(toastError);
          }}
        />
      )}
    </div>
  );
}

function EmptyState() {
  const modelRef = useStore((s) => s.modelRef);
  const models = useModelCatalog();
  return (
    <div className="flex flex-col items-center justify-center py-20 text-center">
      <div className="mb-5 flex size-11 items-center justify-center rounded-xl bg-ink text-[var(--accent-contrast)]">
        <Bot size={22} />
      </div>
      <div className="mb-1.5 text-[22px] font-semibold tracking-tight">开始一段对话</div>
      <div className="text-sm text-dim">
        {modelRef ? (
          <>
            当前模型 {/* 悬停给出实际请求名与 meta —— 与输入框下方那处同一个写法。 */}
            <span className="mono text-ink" title={modelTitle(modelRef, models)}>
              {displayName(modelRef, models)}
            </span>
          </>
        ) : (
          "发条消息就能开始。"
        )}
      </div>
    </div>
  );
}

function TouchFocus({ stick }: { stick: React.RefObject<boolean> }) {
  const focus = useStore((s) => s.focusTurn);
  const sessionId = useStore((s) => s.sessionId);
  const turns = useStore((s) => s.turns);
  const clear = useStore((s) => s.clearTurnFocus);
  // 已经落位过的**请求** id。同一请求只跳一次，之后的重渲染不再动滚动条。
  const done = useRef<number | null>(null);
  // 请求到达的时刻：超过 DEADLINE 还找不到那一轮就放弃。
  const since = useRef(0);
  // biome-ignore lint/correctness/useExhaustiveDependencies: 这里要的正是「turns 一变就再试一次」—— 那一轮是随历史异步画出来的，effect 本身不读它
  useEffect(() => {
    if (!focus || focus.sessionId !== sessionId) return;
    if (done.current === focus.id) return;
    if (since.current === 0) since.current = performance.now();
    const el = document.querySelector<HTMLElement>(`[data-turn="${focus.turn}"]`);
    if (!el) {
      if (performance.now() - since.current > 10_000) {
        since.current = 0;
        clear();
      }
      return;
    }
    done.current = focus.id;
    since.current = 0;
    // 别让贴底逻辑把刚跳过去的位置抢回去。
    stick.current = false;
    el.scrollIntoView({ block: "center", behavior: "smooth" });
    el.animate(
      [
        { boxShadow: "inset 0 0 0 2px color-mix(in oklab, var(--accent) 45%, transparent)" },
        { boxShadow: "inset 0 0 0 2px transparent" },
      ],
      { duration: 1400, easing: "ease-out" },
    );
    clear();
  }, [focus, sessionId, turns, clear, stick]);
  return null;
}
