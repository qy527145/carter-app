// 对话主区：消息列表 + 待办 + 提问面板 + 输入框。
//
// 列表**没有**做虚拟滚动。`@tanstack/react-virtual` 已经装好，但流式追加时每一帧
// 都在改末尾元素的高度，虚拟器要不断重测；在这个「跟着底部滚」的场景里，它带来
// 的复杂度（动态高度 + 保持贴底 + markdown 异步布局）比收益大。
// TODO: 长会话需要时再引入虚拟滚动（依赖已在 package.json）。
//
// 视觉上照参考实现的方向：**助手输出不套气泡也不带头像**，正文就是页面上的正文；
// 只有用户消息给一个气泡。工具与思考是嵌在其中的卡片 —— 它们是流程的一部分，
// 不该跟正文抢同一层视觉重量。

import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  Bot,
  Brain,
  ChevronRight,
  CircleCheck,
  CircleX,
  FileText,
  Globe,
  Info,
  LoaderCircle,
  ScanSearch,
  Send,
  Square,
  TriangleAlert,
  Wrench,
} from "lucide-react";

import { sendCommand } from "../lib/api";
import { useStore, type Item, type Turn } from "../store";
import { Button, IconButton } from "./ui/Button";
import { Input } from "./ui/Input";
import { Banner } from "./ui/Panel";

type ToolItem = Extract<Item, { kind: "tool" }>;
type ServerItem = Extract<Item, { kind: "server" }>;
type ImageItem = Extract<Item, { kind: "image" }>;
type AssistantTurnT = Extract<Turn, { role: "assistant" }>;

// ---------------------------------------------------------------------------
// 片段渲染
// ---------------------------------------------------------------------------

function MarkdownText({ text }: { text: string }) {
  return (
    <div className="carter-md">
      <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>
    </div>
  );
}

/** 内容块左侧那个小图标底衬。全应用唯一的彩色落点。 */
function Glyph({
  tone,
  children,
}: {
  tone: "blue" | "green" | "orange" | "violet" | "pink";
  children: React.ReactNode;
}) {
  // 类名必须是字面量：拼接出来的 Tailwind 扫描不到，颜色会静默失效。
  const cls = {
    blue: "bg-tone-blue-soft text-tone-blue",
    green: "bg-tone-green-soft text-tone-green",
    orange: "bg-tone-orange-soft text-tone-orange",
    violet: "bg-tone-violet-soft text-tone-violet",
    pink: "bg-tone-pink-soft text-tone-pink",
  }[tone];
  return (
    <span className={`flex size-5 shrink-0 items-center justify-center rounded ${cls}`}>
      {children}
    </span>
  );
}

function ThinkingBlock({ text, hasSignature }: { text: string; hasSignature?: boolean }) {
  return (
    <details className="group rounded-lg border border-edge bg-card/50">
      <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-1.5 text-xs text-dim transition-colors hover:text-ink">
        <ChevronRight size={12} className="transition-transform group-open:rotate-90" />
        <Glyph tone="violet">
          <Brain size={11} />
        </Glyph>
        <span>思考</span>
        {/* hasSignature 只在历史消息里带值；实时增量拿不到签名状态，不显示提示。 */}
        {hasSignature === false && (
          <span className="ml-1 text-[11px] text-tone-orange">
            无签名 · 历史消息无法原样回传
          </span>
        )}
      </summary>
      <div className="whitespace-pre-wrap border-t border-edge px-3 py-2 text-xs leading-relaxed text-dim">
        {text}
      </div>
    </details>
  );
}

function ToolCard({ item }: { item: ToolItem }) {
  const r = item.result;
  return (
    <div className="rounded-lg border border-edge bg-card px-3 py-2">
      <div className="flex items-center gap-2">
        <Glyph tone="blue">
          <Wrench size={11} />
        </Glyph>
        <span className="font-mono text-xs text-ink">{item.name}</span>
        {r ? (
          r.ok ? (
            <CircleCheck size={13} className="text-[var(--ok)]" />
          ) : (
            <CircleX size={13} className="text-[var(--danger)]" />
          )
        ) : (
          <LoaderCircle size={12} className="animate-spin text-dim" />
        )}
      </div>
      {item.argsPreview && (
        <div className="mt-1.5 truncate font-mono text-[11px] text-dim" title={item.argsPreview}>
          {item.argsPreview}
        </div>
      )}
      {r && (
        <div
          className={`mt-1 truncate text-xs ${
            r.ok ? "text-dim" : "text-[var(--danger)]"
          }`}
          title={r.summary}
        >
          {r.summary || "（无输出）"}
        </div>
      )}
    </div>
  );
}

/**
 * 服务端工具活动（web_search 等）。
 *
 * 刻意与本地工具卡片长得不一样：这类活动是**厂商在服务端跑完**的，本地没有执行过，
 * 用同一套样式会让人以为是我们自己调的工具。
 */
function ServerCard({ item }: { item: ServerItem }) {
  return (
    <div className="rounded-lg border border-dashed border-edge bg-card/50 px-3 py-2">
      <div className="flex items-center gap-2">
        <Glyph tone="orange">
          <Globe size={11} />
        </Glyph>
        <span className="font-mono text-xs text-ink">{item.rawType}</span>
        <span className="rounded border border-edge px-1.5 py-0.5 text-[10px] text-dim">
          服务端活动
        </span>
      </div>
      {item.summary && (
        <div className="mt-1 truncate text-xs text-dim" title={item.summary}>
          {item.summary}
        </div>
      )}
    </div>
  );
}

function ImageCard({ item }: { item: ImageItem }) {
  return (
    <div className="flex items-center gap-2 rounded-lg border border-edge bg-card px-3 py-2 text-xs text-dim">
      <Glyph tone="pink">
        <FileText size={11} />
      </Glyph>
      <span className="mono">{item.mediaType}</span>
      {/* 视图里只带 hash、不带字节，所以这里只给一个占位标识。 */}
      {item.hash && <span className="truncate text-dim/70">#{item.hash.slice(0, 12)}</span>}
    </div>
  );
}

function ItemView({ item, showCursor }: { item: Item; showCursor: boolean }) {
  switch (item.kind) {
    case "thinking":
      return <ThinkingBlock text={item.text} hasSignature={item.hasSignature} />;
    case "text":
      return (
        <div className="text-sm text-ink">
          <MarkdownText text={item.text} />
          {showCursor && <span className="carter-cursor">▍</span>}
        </div>
      );
    case "tool":
      return <ToolCard item={item} />;
    case "server":
      return <ServerCard item={item} />;
    case "image":
      return <ImageCard item={item} />;
    case "note":
      return <div className="whitespace-pre-wrap text-xs text-dim">{item.text}</div>;
  }
}

// ---------------------------------------------------------------------------
// 轮次
// ---------------------------------------------------------------------------

function AssistantTurn({
  turn: t,
  isLast,
  streaming,
}: {
  turn: AssistantTurnT;
  isLast: boolean;
  streaming: boolean;
}) {
  const openInspector = useStore((s) => s.openInspector);
  const items = t.items;
  return (
    <div className="group/msg min-w-0 space-y-2">
      {items.length === 0 && streaming && (
        <div className="flex items-center gap-2 text-sm text-dim">
          <span className="carter-dots">
            <span />
            <span />
            <span />
          </span>
          正在生成…
        </div>
      )}
      {items.map((item, i) => (
        <ItemView
          key={i}
          item={item}
          showCursor={streaming && isLast && i === items.length - 1 && item.kind === "text"}
        />
      ))}
      {/* 把这条回答对回产生它的那次 API 请求。历史消息没有轮次号（事件不回放），
          所以只有本轮之后的消息才拿得到这个入口。 */}
      {t.turn !== undefined && items.length > 0 && (
        <button
          onClick={() => openInspector(t.turn ?? null)}
          title={`查看第 ${t.turn} 轮发出的请求`}
          className="group/msg inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-dim opacity-0 transition-opacity hover:bg-card hover:text-ink group-hover/msg:opacity-100"
        >
          <ScanSearch size={12} />
          查看报文
        </button>
      )}
    </div>
  );
}

function UserTurn({ turn }: { turn: Extract<Turn, { role: "user" }> }) {
  return (
    <div className="flex justify-end">
      <div className="max-w-[75%] whitespace-pre-wrap break-words rounded-2xl bg-bubble px-3.5 py-2 text-sm leading-relaxed text-ink">
        {turn.text}
      </div>
    </div>
  );
}

function TurnView({
  turn,
  isLast,
  streaming,
}: {
  turn: Turn;
  isLast: boolean;
  streaming: boolean;
}) {
  if (turn.role === "user") return <UserTurn turn={turn} />;
  return <AssistantTurn turn={turn} isLast={isLast} streaming={streaming} />;
}

// ---------------------------------------------------------------------------
// 列表 + 贴底滚动
// ---------------------------------------------------------------------------

function MessageList() {
  const turns = useStore((s) => s.turns);
  const streaming = useStore((s) => s.streaming);
  const scrollRef = useRef<HTMLDivElement>(null);
  // 用户往上翻时不要把他拽回底部 —— 只在「本来就在底部」时跟随。
  const stick = useRef(true);

  useEffect(() => {
    const el = scrollRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [turns, streaming]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  };

  return (
    <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto px-4 py-6">
      <div className="mx-auto flex max-w-3xl flex-col gap-6">
        {turns.length === 0 && <EmptyState />}
        {turns.map((t, i) => (
          <TurnView key={i} turn={t} isLast={i === turns.length - 1} streaming={streaming} />
        ))}
      </div>
    </div>
  );
}

function EmptyState() {
  const model = useStore((s) => s.model);
  return (
    <div className="flex flex-col items-center justify-center py-20 text-center">
      <div className="mb-5 flex size-11 items-center justify-center rounded-xl bg-ink text-[var(--accent-contrast)]">
        <Bot size={22} />
      </div>
      <div className="mb-1.5 text-[22px] font-semibold tracking-tight">开始一段对话</div>
      <div className="text-sm text-dim">
        {model ? (
          <>
            当前模型 <span className="mono text-ink">{model}</span>
          </>
        ) : (
          "发条消息就能开始。"
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 待办 / 提示条 / 提问面板 / 输入框
// ---------------------------------------------------------------------------

function TodoPanel() {
  const todos = useStore((s) => s.todos);
  if (todos.length === 0) return null;
  return (
    <div className="border-t border-edge bg-sidebar px-4 py-2">
      <div className="mx-auto max-w-3xl space-y-1">
        {todos.map((t, i) => (
          <div key={i} className="flex items-center gap-2 text-xs">
            {t.status === "completed" ? (
              <CircleCheck size={13} className="shrink-0 text-[var(--ok)]" />
            ) : t.status === "in_progress" ? (
              <LoaderCircle size={13} className="shrink-0 animate-spin text-tone-blue" />
            ) : (
              <span className="inline-block size-3 shrink-0 rounded-full border border-edge" />
            )}
            <span
              className={
                t.status === "completed" ? "text-dim line-through" : "text-ink"
              }
            >
              {t.status === "in_progress" ? t.active_form : t.content}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}

function NoticeBanner() {
  const notice = useStore((s) => s.notice);
  const setNotice = useStore((s) => s.setNotice);
  if (!notice) return null;
  return (
    <div className="px-4 pt-3">
      <div className="mx-auto max-w-3xl">
        <Banner kind="warn" onClose={() => setNotice(null)}>
          <div className="flex items-start gap-2">
            <TriangleAlert size={14} className="mt-0.5 shrink-0" />
            <span className="whitespace-pre-wrap break-words">{notice}</span>
          </div>
        </Banner>
      </div>
    </div>
  );
}

function AskPanel() {
  const pendingAsk = useStore((s) => s.pendingAsk);
  const clearPendingAsk = useStore((s) => s.clearPendingAsk);
  const [free, setFree] = useState("");

  if (!pendingAsk) return null;

  const answer = (a: string) => {
    const sid = useStore.getState().sessionId;
    if (sid) {
      sendCommand(sid, { type: "answer_ask", id: pendingAsk.id, answer: a }).catch((e) =>
        useStore.getState().setNotice(String(e)),
      );
    }
    clearPendingAsk();
    setFree("");
  };

  return (
    <div className="px-4 pb-2">
      <div className="mx-auto max-w-3xl rounded-xl border border-edge bg-elevated p-3 shadow-soft">
        <div className="mb-2 flex items-center gap-2 text-xs text-dim">
          <Info size={13} />
          模型在等你回答
        </div>
        <div className="mb-2.5 text-sm text-ink">{pendingAsk.question}</div>
        {pendingAsk.options.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-2">
            {pendingAsk.options.map((opt) => (
              <Button key={opt} onClick={() => answer(opt)}>
                {opt}
              </Button>
            ))}
          </div>
        )}
        <div className="flex gap-2">
          <Input
            value={free}
            onChange={(e) => setFree(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && free.trim()) {
                e.preventDefault();
                answer(free.trim());
              }
            }}
            placeholder="或直接输入回答…"
            className="flex-1"
          />
          <Button variant="primary" disabled={!free.trim()} onClick={() => answer(free.trim())}>
            回答
          </Button>
        </div>
      </div>
    </div>
  );
}

function Composer() {
  const [text, setText] = useState("");
  const streaming = useStore((s) => s.streaming);
  const connected = useStore((s) => s.sessionId !== null);
  const pendingAsk = useStore((s) => s.pendingAsk);
  const model = useStore((s) => s.model);
  const addUserTurn = useStore((s) => s.addUserTurn);
  const setNotice = useStore((s) => s.setNotice);
  const taRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    taRef.current?.focus();
  }, []);

  const send = () => {
    const t = text.trim();
    const sid = useStore.getState().sessionId;
    if (!t || !sid || streaming) return;
    // 用户消息事件不存在，本地先落一条，后端随后发 turn_started 打开助手轮。
    addUserTurn(t);
    setText("");
    const el = taRef.current;
    if (el) el.style.height = "auto";
    sendCommand(sid, { type: "send_prompt", text: t }).catch((e) => setNotice(String(e)));
  };

  const cancel = () => {
    const sid = useStore.getState().sessionId;
    if (sid) sendCommand(sid, { type: "cancel" }).catch((e) => setNotice(String(e)));
  };

  const disabled = !connected || pendingAsk !== null;

  return (
    <div className="shrink-0 px-4 pb-4 pt-1">
      <div className="mx-auto max-w-3xl">
        <div className="rounded-2xl border border-edge bg-elevated shadow-soft transition-shadow focus-within:shadow-lift">
          <textarea
            ref={taRef}
            rows={1}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onInput={(e) => {
              const el = e.currentTarget;
              el.style.height = "auto";
              el.style.height = Math.min(el.scrollHeight, 200) + "px";
            }}
            onKeyDown={(e) => {
              // Enter 发送，Shift+Enter 换行。
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send();
              }
            }}
            disabled={disabled}
            placeholder={
              !connected
                ? "会话未就绪 —— 点「新对话」开始"
                : pendingAsk
                  ? "请先回答上面的问题"
                  : "输入消息，Enter 发送，Shift+Enter 换行"
            }
            className="max-h-48 min-h-14 w-full resize-none bg-transparent px-4 pb-1 pt-3.5 text-[15px] leading-relaxed text-ink outline-none placeholder:text-dim disabled:opacity-50"
          />
          <div className="flex items-center gap-2 px-2.5 pb-2">
            {/* 模型名放在输入框里而不是顶栏：它跟着「这条消息发给谁」走，
                和发送按钮属于同一组信息。 */}
            <span className="mono min-w-0 flex-1 truncate px-1.5 text-[11px] text-dim">
              {model || "—"}
            </span>
            {streaming ? (
              <button
                onClick={cancel}
                title="中断这一轮"
                className="inline-flex items-center gap-1.5 rounded-full border border-edge px-3 py-1.5 text-xs text-dim transition-colors hover:text-[var(--danger)]"
              >
                <Square size={11} />
                停止
              </button>
            ) : (
              <button
                onClick={send}
                disabled={disabled || !text.trim()}
                title="发送"
                className="rounded-full bg-accent p-2 text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-30"
              >
                <Send size={14} />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/** 顶部条：当前会话标题 + 打开报文面板的入口。 */
function ChatHeader() {
  const sessionId = useStore((s) => s.sessionId);
  const sessions = useStore((s) => s.sessions);
  const openInspector = useStore((s) => s.openInspector);
  const title = sessions.find((s) => s.id === sessionId)?.title;

  return (
    <div className="flex h-10 shrink-0 items-center gap-2 border-b border-edge px-4">
      <span className="truncate text-sm text-dim">{title || "新对话"}</span>
      <IconButton
        className="ml-auto"
        disabled={!sessionId}
        onClick={() => openInspector(null)}
        title="查看本次会话抓到的 HTTP 报文"
      >
        <ScanSearch size={15} />
      </IconButton>
    </div>
  );
}

export function ChatView() {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ChatHeader />
      <NoticeBanner />
      <MessageList />
      <TodoPanel />
      <AskPanel />
      <Composer />
    </div>
  );
}
