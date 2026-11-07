import {
  Check,
  ChevronRight,
  Copy,
  CornerUpLeft,
  GitBranch,
  ListChecks,
  Megaphone,
  MessageSquare,
  MessagesSquare,
  Radar,
  RotateCcw,
  ScanSearch,
  Target,
  Terminal,
} from "lucide-react";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import type { ReportTag, TurnTime } from "../../lib/api";
import { copyText } from "../../lib/clipboard";
import { useConversationMode } from "../../lib/conversationMode";
import { fmtDuration, fullTime, msgTime } from "../../lib/format";
import { parseContexts, stripContextTags } from "../../lib/promptContext";
import { ASSISTANT_CHAT_ID, jumpToTurn, openSession } from "../../lib/sessionOps";
import { TICKET_STATUS_LABEL } from "../../lib/ticketUi";
import { TOAST_MS } from "../../lib/toast";
import { type Item, type Turn, useStore } from "../../store";
import { MessageImages } from "../Composer";
import { ContextChips } from "../ContextChip";
import { TicketReply } from "../TicketReply";
import { hasSelection, type MenuEntry, useContextMenu } from "../ui/ContextMenu";
import { ConfirmDialog } from "../ui/Panel";
import { assistantActionSlots, forkTarget, mergeAssistantRuns } from "./changes";
import { type AssistantTurnT, ItemView, ProcessGroup, segmentsOf } from "./fragments";

// ---------------------------------------------------------------------------
// 轮次
// ---------------------------------------------------------------------------

/** 「还在跑」的跳动圆点。还什么都没出来时（`label`）多带一句字。 */
function GeneratingHint({ label = false }: { label?: boolean }) {
  return (
    <div className="flex h-5 items-center gap-2 text-sm text-dim" aria-label="正在生成">
      <span className="carter-dots">
        <span />
        <span />
        <span />
      </span>
      {label && "正在生成…"}
    </div>
  );
}

/**
 * 气泡下面那一行小按钮（复制 / 重试 / 报文）。
 *
 * 悬停才浮出来：静止时它不该跟正文抢注意力；但**始终占着位置**（`opacity-0`
 * 而不是不渲染），否则鼠标移上去的一瞬间整段对话会跳一下。
 * `focus-within` 让键盘也能把它叫出来 —— 透明不等于按不到。
 */
function ActionChip({
  icon,
  label,
  title,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  title?: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      title={title ?? label}
      className="carter-press inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-dim hover:bg-ink/5 hover:text-ink"
    >
      {icon}
      {label}
    </button>
  );
}

/** 操作行的共同外壳：悬停/聚焦时出现。 */
const ACTION_ROW =
  "flex flex-wrap items-center gap-1 opacity-0 transition-opacity duration-150 group-hover/msg:opacity-100 focus-within:opacity-100";

/**
 * 消息旁边那一小段时间。**常显**，不跟操作行一起悬停才出来 —— 翻一段长对话时
 * 要一眼看出「这两句隔了多久」，每条都去悬停一下就没法用了。
 *
 * `at` 缺省或为 0（老会话没有时刻记录）时什么都不画，不占位。
 */
function MsgTime({ at, extra }: { at?: number | null; extra?: string }) {
  if (!at) return null;
  const label = msgTime(at);
  if (!label) return null;
  return (
    <time
      dateTime={new Date(at).toISOString()}
      title={fullTime(at)}
      className="shrink-0 select-none text-[11px] tabular-nums text-dim"
    >
      {label}
      {extra && <span className="ml-1">· {extra}</span>}
    </time>
  );
}

function AssistantTurn({
  turn: t,
  time,
  isLast,
  liveTurn,
  streaming,
  awaitingUser,
  showPayload,
  cwd,
  onRetry,
  retryAsks,
  onFork,
}: {
  turn: AssistantTurnT;
  /** 这一轮的起止时刻。回答末尾显示答完的时刻与用时。 */
  time?: TurnTime;
  isLast: boolean;
  /** 会话还在跑**这**一轮（不只是「有东西在跑」）：过程分组靠它显示「正在 …」，
   *  也靠它保住末尾的「正在生成」—— 切页面回来时重建的那几轮不再谎报还在跑。 */
  liveTurn: boolean;
  streaming: boolean;
  awaitingUser: boolean;
  showPayload: boolean;
  /** 预览文件时用来解析相对路径。 */
  cwd: string;
  /** 重跑这一轮。不给就不画「重试」（侧边聊天、继承来的上下文）。 */
  onRetry?: () => void;
  /** 重试会顺带丢掉后面几轮对话 —— 点之前要先问一句。 */
  retryAsks?: boolean;
  /** 从这一轮之后的对话分叉出一条新会话。 */
  onFork?: () => void;
}) {
  const openInspector = useStore((s) => s.openInspector);
  const mode = useConversationMode();
  const menu = useContextMenu();
  const items = t.items;
  // 「复制」就地变对勾，2.5 秒后复位（与文件预览弹窗同一个节奏）。
  const [copied, setCopied] = useState(false);
  const [confirmingRetry, setConfirmingRetry] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), TOAST_MS);
    return () => clearTimeout(timer);
  }, [copied]);

  // 复制的是**正文**：思考与工具输出属于过程，不是用户想带走的那段话。
  const answer = items
    .filter((i): i is Extract<Item, { kind: "text" }> => i.kind === "text")
    .map((i) => i.text)
    .join("\n\n")
    .trim();

  // 重试 = 就地重写这条回答（提问与前面的工具往返都留着）。流式期间整排重试都由
  // `TurnList` 收掉（见那边的注释），这里只认「有没有回调」。
  const canRetry = onRetry !== undefined;
  // 一条动作都没有时整行不渲染：纯工具输出的回答末尾不该留一条看不见的空白。
  const showPayloadChip = showPayload && t.turn !== undefined;
  const actions = !!answer || canRetry || showPayloadChip || onFork !== undefined;
  // 答完的时刻 + 用时。还在写的那一轮不显示（时刻还没定）；只有收尾没有开头的
  // （`started_at` 为 0）只显示时刻，不瞎算用时。
  const done = streaming && isLast ? null : (time?.ended_at ?? null);
  const took =
    done && time?.started_at && done >= time.started_at
      ? `用时 ${fmtDuration(done - time.started_at)}`
      : undefined;

  const doCopy = async () => {
    // 成功才打勾；失败时 copyText 自己会 toast。
    if (await copyText(answer, null)) setCopied(true);
  };

  const onContextMenu = (e: React.MouseEvent) => {
    // 有选中文字时让系统菜单出来：用户要的多半是「复制选中的这一段」。
    if (hasSelection()) return;
    const entries: MenuEntry[] = [];
    if (answer) {
      entries.push({
        label: "复制回答",
        icon: <Copy size={12} />,
        onSelect: () => void copyText(answer),
      });
    }
    if (canRetry) {
      entries.push({
        label: "重新生成",
        icon: <RotateCcw size={12} />,
        onSelect: () => (retryAsks ? setConfirmingRetry(true) : onRetry?.()),
      });
    }
    if (onFork) {
      entries.push({
        label: "从这里分叉",
        icon: <GitBranch size={12} />,
        onSelect: onFork,
      });
    }
    if (showPayload && t.turn !== undefined) {
      entries.push({
        label: "查看本轮报文",
        icon: <ScanSearch size={12} />,
        onSelect: () => openInspector(t.turn ?? null),
      });
    }
    if (entries.length === 0) return;
    e.preventDefault();
    menu.open({ x: e.clientX, y: e.clientY }, entries);
  };

  // 精简模式只在渲染这一层换一种切法（见 `segmentsOf`），数据与动作都不变。
  const segs = mode === "compact" ? segmentsOf(items) : null;

  return (
    <div className="group/msg min-w-0 space-y-2" onContextMenu={onContextMenu}>
      {/* 自动继续的轮次：用户没说话，是长任务目标推起来的。画一条分隔说明来历 ——
          不标的话，界面上会看起来像模型自己莫名其妙又说了很长一段。 */}
      {t.auto && (
        <div className="flex items-center gap-2 text-[11px] text-dim">
          <span className="h-px flex-1 bg-edge" />
          <span className="inline-flex items-center gap-1">
            <Target size={11} />
            自动继续
          </span>
          <span className="h-px flex-1 bg-edge" />
        </div>
      )}
      {segs
        ? segs.map((seg, i) => {
            const last = i === segs.length - 1;
            if (seg.kind === "process") {
              return (
                // biome-ignore lint/suspicious/noArrayIndexKey: 分段按事件流顺序生成，没有独立 id
                <ProcessGroup key={i} items={seg.items} cwd={cwd} liveTurn={liveTurn} />
              );
            }
            // 「正在写」只算正在生成的那一段正文，与详细模式的判断口径一致。
            return (
              <ItemView
                // biome-ignore lint/suspicious/noArrayIndexKey: 同上，正文段可能文字完全相同
                key={i}
                item={seg.item}
                cwd={cwd}
                isLast={last}
                liveTurn={liveTurn}
                writing={streaming && isLast && last && seg.kind === "text"}
              />
            );
          })
        : items.map((item, i) => (
            <ItemView
              // biome-ignore lint/suspicious/noArrayIndexKey: 条目按事件流顺序生成，没有独立 id
              key={i}
              item={item}
              cwd={cwd}
              isLast={i === items.length - 1}
              liveTurn={liveTurn}
              writing={streaming && isLast && i === items.length - 1 && item.kind === "text"}
            />
          ))}
      {/* 挂在整轮末尾，不挂在某一段正文后面：思考、跑工具的时候最后一项不是正文，
          挂在正文上这几段就看不出还在跑。等用户回答提问时不算在跑。 */}
      {streaming && isLast && liveTurn && !awaitingUser && (
        <GeneratingHint label={items.length === 0} />
      )}
      {/* 回答下面那一行小动作。悬停才浮出来，但一直占着位置 —— 否则鼠标移上来
          的一瞬间整段对话会跳一下。一条动作都没有时整行不渲染（纯工具输出的回答
          不该在末尾留一条空白）。 */}
      {(actions || done) && (
        <div className="flex items-center gap-2">
          {/* 时间常显，在操作行**前面**：操作行静止时是透明的，时间不该跟着隐身。 */}
          <MsgTime at={done} extra={took} />
          {actions && (
            <div className={ACTION_ROW}>
              {answer && (
                <ActionChip
                  icon={
                    copied ? (
                      <Check size={12} className="carter-pop text-[var(--ok)]" />
                    ) : (
                      <Copy size={12} />
                    )
                  }
                  label={copied ? "已复制" : "复制"}
                  onClick={() => void doCopy()}
                />
              )}
              {canRetry && (
                <ActionChip
                  icon={<RotateCcw size={12} />}
                  label="重试"
                  title={
                    retryAsks
                      ? "沿用同样的上下文重写这条回复（会丢掉后面的对话）"
                      : "沿用同样的上下文重写这条回复"
                  }
                  onClick={() => (retryAsks ? setConfirmingRetry(true) : onRetry?.())}
                />
              )}
              {onFork && (
                <ActionChip
                  icon={<GitBranch size={12} />}
                  label="分叉"
                  title="带着到这条回答为止的对话分叉出一条新会话，原来这条不受影响"
                  onClick={onFork}
                />
              )}
              {/* 把这条回答对回产生它的那次 API 请求。轮次号对**历史消息**也成立
              （后端按同样口径算好，见 `view.rs`），所以切走再回来入口还在。
              侧边聊天不给（`showPayload=false`）：它不落盘、不抓包，按主对话的轮次号
              去开主对话的报文只会指错地方。 */}
              {showPayloadChip && (
                <ActionChip
                  icon={<ScanSearch size={12} />}
                  label="报文"
                  title={`查看第 ${t.turn} 轮发出的请求`}
                  onClick={() => openInspector(t.turn ?? null)}
                />
              )}
            </div>
          )}
        </div>
      )}
      {confirmingRetry && (
        <ConfirmDialog
          title="重新生成这条回复？"
          message={
            <>
              这条回复之后的对话会被丢掉，模型会拿写这条回复时的同一份上下文重写一遍。
              <br />
              <span className="text-dim">只回滚对话，已经改过的文件、跑过的命令不会撤销。</span>
            </>
          }
          confirmLabel="重新生成"
          onCancel={() => setConfirmingRetry(false)}
          onConfirm={() => {
            setConfirmingRetry(false);
            onRetry?.();
          }}
        />
      )}
    </div>
  );
}

function UserTurn({
  turn,
  time,
  onRetry,
  retryAsks,
  onFork,
}: {
  turn: Extract<Turn, { role: "user" }>;
  /** 这一轮的起止时刻。气泡下方显示发问的时刻。 */
  time?: TurnTime;
  /** 把这句话原样再问一遍（这一轮的回答及之后的对话丢掉）。 */
  onRetry?: () => void;
  /** 重试会顺带丢掉后面几轮对话 —— 点之前要先问一句。 */
  retryAsks?: boolean;
  /** 从这句话之前分叉出一条新会话，原话摆进输入框。 */
  onFork?: () => void;
}) {
  const menu = useContextMenu();
  const { contexts, rest } = useMemo(() => parseContexts(turn.text), [turn.text]);
  // 与回答那边同一套反馈：就地变对勾，2.5 秒后复位。
  const [copied, setCopied] = useState(false);
  const [confirmingRetry, setConfirmingRetry] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), TOAST_MS);
    return () => clearTimeout(timer);
  }, [copied]);

  const retry = () => (retryAsks ? setConfirmingRetry(true) : onRetry?.());

  // 这条会话是不是某张工单的执行会话：按会话 id 认（工单上记着它的执行会话），
  // 不读任务书里写了什么。
  const sessionId = useStore((s) => s.sessionId);
  const ticket = useStore((s) =>
    sessionId ? (s.assistant?.tickets.find((t) => t.session_id === sessionId) ?? null) : null,
  );

  const menuEntries = (): MenuEntry[] => {
    const entries: MenuEntry[] = [
      {
        label: "复制消息",
        icon: <Copy size={12} />,
        onSelect: () => void copyText(stripContextTags(turn.text)),
      },
    ];
    if (onRetry) {
      entries.push({ label: "重新提问", icon: <RotateCcw size={12} />, onSelect: retry });
    }
    if (onFork) {
      entries.push({
        label: "从这里分叉",
        icon: <GitBranch size={12} />,
        onSelect: onFork,
      });
    }
    // 这条会话是助理派出去的工单：给它一个入口跳回助理对话 —— 落在那张工单最近
    // 一条汇报上，而不是让用户在一长串汇报里自己翻。
    if (ticket !== null) {
      entries.push({
        label: "去助理对话看这条汇报",
        icon: <CornerUpLeft size={12} />,
        onSelect: () => {
          const reportTurn = useStore.getState().assistant?.report_turns?.[ticket.id];
          // 没有那条汇报的轮次（还没有汇报、或汇报在往期存档里）时就只是回到
          // 助理对话：给个 0 等于跳到一个不存在的轮次，人会觉得按钮坏了。
          jumpToTurn(ASSISTANT_CHAT_ID, reportTurn);
        },
      });
    }
    return entries;
  };

  return (
    <div
      className="group/msg flex flex-col items-end"
      onContextMenu={(e) => {
        if (hasSelection()) return;
        e.preventDefault();
        menu.open({ x: e.clientX, y: e.clientY }, menuEntries());
      }}
    >
      {/* 粘贴的图片在会话里存的是 `[img:...]` 引用 token，回看时要渲染回图。
          纯文本消息走 MessageImages 的文本分支，行为与直接输出一致。 */}
      <div className="max-w-[75%] break-words rounded-2xl bg-bubble px-3.5 py-2 text-sm leading-relaxed text-ink">
        {contexts.length > 0 && (
          <div className={rest ? "mb-1.5" : ""}>
            <ContextChips contexts={contexts} />
          </div>
        )}
        <MessageImages text={rest} />
      </div>
      <div className="mt-1 flex items-center justify-end gap-2">
        <div className={`${ACTION_ROW} justify-end`}>
          <ActionChip
            icon={
              copied ? (
                <Check size={12} className="carter-pop text-[var(--ok)]" />
              ) : (
                <Copy size={12} />
              )
            }
            label={copied ? "已复制" : "复制"}
            onClick={async () => {
              if (await copyText(stripContextTags(turn.text), null)) setCopied(true);
            }}
          />
          {onRetry && (
            <ActionChip
              icon={<RotateCcw size={12} />}
              label="重试"
              title={
                retryAsks ? "把这句话原样再问一遍（会丢掉后面的对话）" : "把这句话原样再问一遍"
              }
              onClick={retry}
            />
          )}
          {onFork && (
            <ActionChip
              icon={<GitBranch size={12} />}
              label="分叉"
              title="从这句话之前分叉出一条新会话，原话放进输入框（原来这条不受影响）"
              onClick={onFork}
            />
          )}
        </div>
        {/* 时间常显，放在操作行**外面**：操作行静止时是透明的。 */}
        <MsgTime at={time?.started_at} />
      </div>
      {confirmingRetry && (
        <ConfirmDialog
          title="重新问这句话？"
          message={
            <>
              这句之后的对话会被丢掉，模型会基于这句话重新回答。
              <br />
              <span className="text-dim">只回滚对话，已经改过的文件、跑过的命令不会撤销。</span>
            </>
          }
          confirmLabel="重新提问"
          onCancel={() => setConfirmingRetry(false)}
          onConfirm={() => {
            setConfirmingRetry(false);
            onRetry?.();
          }}
        />
      )}
    </div>
  );
}

/** 与后端 `assistant::REPORT_MARK` / `EVENT_MARK` 逐字对应。 */
const REPORT_MARK = "【助理汇报】";
const EVENT_MARK = "【新事件】";

/**
 * 助理送进来的一条消息：汇报（进助理对话）或观察源的新事件（进工单会话）。
 * 第一行是标题，后面是正文。靠左 —— 不是用户在说话。
 *
 * 这张卡属于哪张工单、能不能回答，**只看 `tag`** —— 后端记这条时按工单本身填好、
 * 跟着这一轮存下来的归属（见 core 的 `timeline::ReportTag`）。正文与标题只拿来显示，
 * 不从里面解析任何东西：那是给模型看的文字，写法随时会变，也可能带着外部内容。
 */
function ReportTurn({
  mark,
  text,
  tag,
  latest,
}: {
  mark: string;
  text: string;
  tag?: ReportTag | null;
  /** 这是这张工单最近的一次提问（旧提问上的回答框会答到早就不是这个问题的工单上）。 */
  latest?: boolean;
}) {
  const rest = text.slice(mark.length);
  const cut = rest.indexOf("\n");
  const title = cut < 0 ? rest : rest.slice(0, cut);
  let body = cut < 0 ? "" : rest.slice(cut + 1).trim();
  const ticket = useStore((s) =>
    tag ? s.assistant?.tickets.find((t) => t.id === tag.ticket_id) : undefined,
  );
  // 打开哪条对话：工单现在的执行会话优先（重开过会换），没有就用记这条时的那条。
  const sessionId = ticket?.session_id ?? tag?.session_id ?? null;
  // 回答框：是一次提问、是最近那次、工单还等着人、用户没划掉。
  const answerable =
    tag?.ask && latest && ticket && TICKET_WAITING.includes(ticket.status) && !ticket.ignored;
  // 候选已经画成按钮了，正文里那行给模型看的「候选：」不再重复。
  if (answerable && ticket.options?.length) body = body.replace(/\n?候选：.*$/, "").trim();
  // 派单是一条「我派了谁去做」，不是「出事了」。图标与语气都该轻一格：助理对话是
  // 用户唯一的入口，干活的记录也该在这里排成一条线，而不是只在报错时浮上来。
  const dispatch = tag?.dispatch === true;
  return (
    <div className="rounded-xl border border-edge bg-card px-3.5 py-2.5">
      <div
        className={`flex items-center gap-1.5 text-xs font-medium ${
          dispatch ? "text-dim" : "text-tone-blue"
        }`}
      >
        {mark === EVENT_MARK ? (
          <Radar size={12} className="shrink-0" />
        ) : dispatch ? (
          <ListChecks size={12} className="shrink-0" />
        ) : (
          <Megaphone size={12} className="shrink-0" />
        )}
        <span className="truncate">{title}</span>
        {sessionId ? (
          <button
            className="ml-auto flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-dim hover:bg-ink/5 hover:text-ink"
            onClick={() => openSession(sessionId)}
          >
            <MessageSquare size={11} />
            打开工单对话
          </button>
        ) : ticket ? (
          // 工单还没有对话可开：排队中（还没开工），或者已经结束了（会话收掉了）。
          // 按它此刻的状态说，别一律写「排队中」。它开工之后这张卡会自己长出按钮来 ——
          // 这里订阅着 store 里的工单，`session_id` 一出现就重渲染。
          <span
            title="这张工单还没有执行会话，开工之后这里会出现「打开工单对话」"
            className="ml-auto shrink-0 px-1.5 py-0.5 text-[11px] text-dim/70"
          >
            {TICKET_STATUS_LABEL[ticket.status]}
          </span>
        ) : null}
      </div>
      {body && <div className="mt-1 whitespace-pre-wrap break-words text-sm text-ink">{body}</div>}
      {answerable && (
        <div className="mt-2">
          <TicketReply t={ticket} withDone />
        </div>
      )}
    </div>
  );
}

/** 等人回答的工单状态（与面板「等你处理」里能回话的那几种一致）。 */
const TICKET_WAITING = ["ready", "needs_user", "blocked", "failed", "unreported"];

/** 每张工单最近一次提问在第几个位置 —— 只有那一条给回答框。只看归属，不看正文。 */
function latestAsks(turns: Turn[], times?: Record<number, TurnTime>): Set<number> {
  const seen = new Set<string>();
  const out = new Set<number>();
  for (let i = turns.length - 1; i >= 0; i--) {
    const t = turns[i];
    if (t.role !== "user" || t.turn === undefined) continue;
    const tag = times?.[t.turn]?.report;
    if (!tag?.ask || seen.has(tag.ticket_id)) continue;
    seen.add(tag.ticket_id);
    out.add(i);
  }
  return out;
}

/** 与后端 `tools::background::DONE_MARK` 逐字对应。 */
const DONE_MARK = "【后台命令】";

/**
 * 后台命令结束时系统送进来的通知：第一行是「shell_id · 已结束（退出码 N）」，
 * 后面是给模型看的说明与输出。不是用户说的话，画成一张可展开的卡片。
 */
function BackgroundDoneTurn({ text }: { text: string }) {
  const rest = text.slice(DONE_MARK.length);
  const cut = rest.indexOf("\n");
  const title = cut < 0 ? rest : rest.slice(0, cut);
  const body = cut < 0 ? "" : rest.slice(cut + 1).trim();
  const failed = !/退出码 0）/.test(title);
  return (
    <details className="group rounded-xl border border-edge bg-card px-3.5 py-2.5">
      <summary
        className={`flex cursor-pointer list-none items-center gap-1.5 text-xs font-medium ${
          failed ? "text-danger" : "text-ok"
        }`}
      >
        <Terminal size={12} className="shrink-0" />
        <span className="truncate">后台命令 {title}</span>
        <ChevronRight size={12} className="shrink-0 transition-transform group-open:rotate-90" />
      </summary>
      {body && (
        <pre className="mt-1.5 max-h-80 overflow-auto whitespace-pre-wrap break-words font-mono text-xs text-dim">
          {body}
        </pre>
      )}
    </details>
  );
}

/** 与后端 `peers::PEER_MARK` 逐字对应。 */
const PEER_MARK = "【跨会话】";

/**
 * 另一条会话发来的信（或回执）：第一行是「来自 xxx · m3」，后面是给模型看的说明与
 * `<来信>` 正文。界面只摊开正文，那段说明折在「原文」里。
 *
 * 每封信右上角有一个「打开对方会话」—— 与工单汇报卡上那个是同一个动作。它按
 * `<来信 session="…">` 里那个 id 跳，不按地址：地址（`carter-app-79d8`）会重名、
 * 改名之后就不认识了。
 */
function PeerTurn({ text }: { text: string }) {
  const rest = text.slice(PEER_MARK.length);
  const cut = rest.indexOf("\n");
  const title = cut < 0 ? rest : rest.slice(0, cut);
  const letters = [...text.matchAll(/<(来信|回执)([^>]*)>\n([\s\S]*?)\n<\/\1>/g)].map((m) => ({
    receipt: m[1] === "回执",
    from: /from="([^"]*)"/.exec(m[2])?.[1] ?? "",
    session: /session="([^"]*)"/.exec(m[2])?.[1] ?? "",
    body: m[3],
  }));
  return (
    <div className="rounded-xl border border-edge bg-card px-3.5 py-2.5">
      <div className="flex items-center gap-1.5 text-xs font-medium text-tone-blue">
        <MessagesSquare size={12} className="shrink-0" />
        <span className="truncate">跨会话 · {title}</span>
      </div>
      {letters.map((l, i) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: 跨会话来信按出现位置排列，同一封信可能重复
        <div key={i} className="mt-1.5">
          <div className="flex items-center gap-2">
            {letters.length > 1 && <span className="text-xs text-dim">{l.from}</span>}
            {l.session && (
              <button
                type="button"
                onClick={() => openSession(l.session)}
                className="ml-auto flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-dim hover:bg-ink/5 hover:text-ink"
              >
                <MessageSquare size={11} />
                打开{l.from || "对方"}会话
              </button>
            )}
          </div>
          <div
            className={`whitespace-pre-wrap break-words text-sm ${l.receipt ? "text-dim" : "text-ink"}`}
          >
            {l.body}
          </div>
        </div>
      ))}
      <details className="group mt-1.5">
        <summary className="flex cursor-pointer list-none items-center gap-1 text-xs text-dim">
          <ChevronRight size={11} className="shrink-0 transition-transform group-open:rotate-90" />
          模型看到的原文
        </summary>
        <pre className="mt-1 max-h-80 overflow-auto whitespace-pre-wrap break-words font-mono text-xs text-dim">
          {text}
        </pre>
      </details>
    </div>
  );
}

function TurnView({
  turn,
  time,
  isLast,
  liveTurn,
  streaming,
  awaitingUser,
  showPayload,
  cwd,
  onRetry,
  retryAsks,
  onFork,
  latestReport,
}: {
  turn: Turn;
  time?: TurnTime;
  isLast: boolean;
  /** 会话正在跑的就是这一轮（见 `TurnList` 的 `liveTurn`）。 */
  liveTurn: boolean;
  streaming: boolean;
  awaitingUser: boolean;
  showPayload: boolean;
  cwd: string;
  onRetry?: () => void;
  retryAsks?: boolean;
  onFork?: () => void;
  /** 这是某张工单最近的一条汇报（只有它给回答框）。 */
  latestReport?: boolean;
}) {
  // 包一层带上这一轮的编号：助理面板上卡片里的「跳转」按它找过来（见 `jumpToTurn`）。
  const wrap = (node: ReactNode) => (
    <div data-turn={turn.turn} className="scroll-mt-6">
      {node}
    </div>
  );
  if (turn.role === "user") {
    // 助理自己送进来的汇报：不是用户说的话，不画成用户气泡，也不给重试 / 分叉。
    if (turn.text.startsWith(REPORT_MARK))
      return wrap(
        <ReportTurn
          mark={REPORT_MARK}
          text={turn.text}
          tag={turn.turn !== undefined ? time?.report : undefined}
          latest={latestReport}
        />,
      );
    if (turn.text.startsWith(EVENT_MARK))
      return wrap(<ReportTurn mark={EVENT_MARK} text={turn.text} />);
    if (turn.text.startsWith(DONE_MARK)) return wrap(<BackgroundDoneTurn text={turn.text} />);
    if (turn.text.startsWith(PEER_MARK)) return wrap(<PeerTurn text={turn.text} />);
    return wrap(
      <UserTurn turn={turn} time={time} onRetry={onRetry} retryAsks={retryAsks} onFork={onFork} />,
    );
  }
  return wrap(
    <AssistantTurn
      turn={turn}
      time={time}
      isLast={isLast}
      liveTurn={liveTurn}
      streaming={streaming}
      awaitingUser={awaitingUser}
      showPayload={showPayload}
      cwd={cwd}
      onRetry={onRetry}
      retryAsks={retryAsks}
      onFork={onFork}
    />,
  );
}

/**
 * 一段对话的正文。**纯展示**（turns 从参数来，不读 store）—— 主对话与侧边聊天
 * 共用这一份：两边的事件词汇、item 形状完全相同，各画一遍必然会走样。
 */
export function TurnList({
  turns: raw,
  times,
  streaming,
  awaitingUser = false,
  showPayload = true,
  cwd = "",
  onRetry,
  onFork,
}: {
  turns: Turn[];
  /** 每一轮的起止时刻（轮次号 → 时刻）。不传 = 不显示时间（侧边聊天继承来的那段）。 */
  times?: Record<number, TurnTime>;
  streaming: boolean;
  /** 模型停下来等用户回答提问：还算这一轮，但不该再显示「正在生成」。 */
  awaitingUser?: boolean;
  /** 是否给助手轮画「查看报文」入口。侧边聊天传 false（它不抓包）。 */
  showPayload?: boolean;
  /** 预览工具卡片里的文件时用来解析相对路径。 */
  cwd?: string;
  /**
   * 重试。**只有真正跑着的那条会话才传** —— 侧边聊天、侧边聊天里那段继承来的
   * 上下文都不该往回改。
   *
   * `reply = true` 是回答上的重试：就地重写这条回答（`retry_reply`）；
   * `false` 是提问上的重试：把这句话原样再问一遍（`regenerate`）。
   */
  onRetry?: (turn: number, reply: boolean) => void;
  /**
   * 只保留第 `keepBefore` 轮之前的历史，分叉出一条新会话。`draft` 是提问上的分叉
   * 带过去的那句话，新会话里摆进输入框。
   */
  onFork?: (keepBefore: number, draft?: string) => void;
}) {
  const turns = useMemo(() => mergeAssistantRuns(raw), [raw]);
  // 一圈正在跑着的时候整排都不给改动：重试会插进队列、等这一轮跑完才生效，那时
  // 用户多半已经忘了自己点过它；分叉 / 编辑要关掉当前会话再开新的，正在跑的那一轮
  // 与排队里的消息会跟着没掉 —— 按钮看着只是「分一条出去」，不该有这个副作用。
  const live = !streaming;
  const slots = onRetry && live ? assistantActionSlots(turns) : [];
  const reports = useMemo(() => latestAsks(turns, times), [turns, times]);
  // 会话正在跑的是**最后那一轮**（一问一答成对）；不是最后几轮各算各的 —— 否则
  // 切走再回来时，历史里最后一张没结果的工具卡会被当成「还在跑」。
  const liveTurn = useMemo(() => {
    for (let i = turns.length - 1; i >= 0; i--) {
      if (turns[i].turn !== undefined) return turns[i].turn;
    }
    return undefined;
  }, [turns]);

  return (
    <div className="flex flex-col gap-6">
      {turns.map((t, i) => {
        const asks = slots[i];
        const turn = t.turn;
        return (
          <TurnView
            // biome-ignore lint/suspicious/noArrayIndexKey: 轮次按对话顺序只追加，序号就是轮次的身份
            key={i}
            turn={t}
            time={turn !== undefined ? times?.[turn] : undefined}
            isLast={i === turns.length - 1}
            liveTurn={i === turns.length - 1 && turn !== undefined && turn === liveTurn}
            streaming={streaming}
            awaitingUser={awaitingUser}
            showPayload={showPayload}
            cwd={cwd}
            // 没有轮次号（历史里那种极端竞态）就没法指名改哪一轮。
            onRetry={
              onRetry && live && turn !== undefined && (t.role === "user" || asks !== undefined)
                ? () => onRetry(turn, t.role === "assistant")
                : undefined
            }
            // 提问上的重试：这句之后还有回答以外的对话（下一句提问）才要先问一句。
            retryAsks={t.role === "user" ? turns.slice(i + 1).some((x) => x.role === "user") : asks}
            onFork={forkTarget(t, onFork, live)}
            latestReport={reports.has(i)}
          />
        );
      })}
      {/* 消息已发出、助手轮还没开始：和空的助手轮同一个位置，接上时不会跳。 */}
      {streaming && !awaitingUser && turns[turns.length - 1]?.role === "user" && (
        <GeneratingHint label />
      )}
    </div>
  );
}
