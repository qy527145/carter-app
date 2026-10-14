// 对话主区：消息列表 + 待办 + 提问面板 + 输入框。
//
// 列表**没有**做虚拟滚动。`@tanstack/react-virtual` 已经装好，但流式追加时每一帧
// 都在改末尾元素的高度，虚拟器要不断重测；在这个「跟着底部滚」的场景里，它带来
// 的复杂度（动态高度 + 保持贴底 + markdown 异步布局）比收益大。
// TODO: 长会话需要时再引入虚拟滚动（依赖已在 package.json）。

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
  Send,
  Square,
  TriangleAlert,
  Wrench,
  X,
} from "lucide-react";

import { sendCommand } from "../lib/api";
import { useStore, type Item, type Turn } from "../store";
import { Button } from "./ui/Button";
import { Input } from "./ui/Input";

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

function ThinkingBlock({ text, hasSignature }: { text: string; hasSignature?: boolean }) {
  return (
    <details className="group rounded-lg border border-slate-700/70 bg-slate-800/30">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 px-3 py-1.5 text-xs text-slate-400 hover:text-slate-200">
        <ChevronRight size={13} className="transition-transform group-open:rotate-90" />
        <Brain size={13} />
        <span>思考</span>
        {/* hasSignature 只在历史消息里带值；实时增量拿不到签名状态，不显示提示。 */}
        {hasSignature === false && (
          <span className="ml-1 text-[11px] text-amber-500/80">
            无签名 · 历史消息无法原样回传
          </span>
        )}
      </summary>
      <div className="whitespace-pre-wrap border-t border-slate-700/70 px-3 py-2 text-xs leading-relaxed text-slate-400">
        {text}
      </div>
    </details>
  );
}

function ToolCard({ item }: { item: ToolItem }) {
  const r = item.result;
  return (
    <div className="rounded-lg border border-slate-700 bg-slate-800/40 px-3 py-2">
      <div className="flex items-center gap-2 text-sm">
        <Wrench size={14} className="text-slate-400" />
        <span className="font-medium text-slate-200">{item.name}</span>
        {r ? (
          r.ok ? (
            <CircleCheck size={14} className="text-emerald-400" />
          ) : (
            <CircleX size={14} className="text-red-400" />
          )
        ) : (
          <LoaderCircle size={13} className="animate-spin text-slate-500" />
        )}
      </div>
      {item.argsPreview && (
        <div
          className="mt-1.5 truncate font-mono text-[11px] text-slate-500"
          title={item.argsPreview}
        >
          {item.argsPreview}
        </div>
      )}
      {r && (
        <div
          className={`mt-1 truncate text-xs ${r.ok ? "text-slate-400" : "text-red-300"}`}
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
    <div className="rounded-lg border border-indigo-800/60 bg-indigo-950/30 px-3 py-2">
      <div className="flex items-center gap-2 text-sm">
        <Globe size={14} className="text-indigo-300" />
        <span className="font-mono text-xs text-indigo-200">{item.rawType}</span>
        <span className="rounded bg-indigo-900/60 px-1.5 py-0.5 text-[10px] text-indigo-300">
          服务端活动
        </span>
      </div>
      {item.summary && (
        <div className="mt-1 truncate text-xs text-indigo-200/80" title={item.summary}>
          {item.summary}
        </div>
      )}
    </div>
  );
}

function ImageCard({ item }: { item: ImageItem }) {
  return (
    <div className="flex items-center gap-2 rounded-lg border border-slate-700 bg-slate-800/40 px-3 py-2 text-xs text-slate-400">
      <FileText size={14} />
      <span className="font-mono">{item.mediaType}</span>
      {/* 视图里只带 hash、不带字节，所以这里只给一个占位标识。 */}
      {item.hash && <span className="truncate text-slate-600">#{item.hash.slice(0, 12)}</span>}
    </div>
  );
}

function ItemView({ item, showCursor }: { item: Item; showCursor: boolean }) {
  switch (item.kind) {
    case "thinking":
      return <ThinkingBlock text={item.text} hasSignature={item.hasSignature} />;
    case "text":
      return (
        <div className="text-slate-100">
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
      return (
        <div className="whitespace-pre-wrap text-xs text-slate-500">{item.text}</div>
      );
  }
}

// ---------------------------------------------------------------------------
// 轮次
// ---------------------------------------------------------------------------

function AssistantTurn({
  turn,
  isLast,
  streaming,
}: {
  turn: AssistantTurnT;
  isLast: boolean;
  streaming: boolean;
}) {
  const items = turn.items;
  return (
    <div className="flex gap-3">
      <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-slate-700 text-slate-300">
        <Bot size={15} />
      </div>
      <div className="min-w-0 flex-1 space-y-2 pt-0.5">
        {items.length === 0 && streaming && (
          <div className="flex items-center gap-2 text-sm text-slate-500">
            <LoaderCircle size={14} className="animate-spin" />
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
      </div>
    </div>
  );
}

function UserTurn({ turn }: { turn: Extract<Turn, { role: "user" }> }) {
  return (
    <div className="flex justify-end">
      <div className="max-w-[85%] whitespace-pre-wrap break-words rounded-2xl rounded-br-sm bg-sky-600 px-3.5 py-2 text-sm text-white">
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
    <div
      ref={scrollRef}
      onScroll={onScroll}
      className="flex-1 overflow-y-auto px-4 py-5"
    >
      <div className="mx-auto flex max-w-3xl flex-col gap-5">
        {turns.length === 0 && (
          <div className="mt-16 text-center text-sm text-slate-600">
            发条消息开始对话吧。
          </div>
        )}
        {turns.map((t, i) => (
          <TurnView key={i} turn={t} isLast={i === turns.length - 1} streaming={streaming} />
        ))}
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
    <div className="border-t border-slate-800 bg-slate-900/60 px-4 py-2">
      <div className="mx-auto max-w-3xl space-y-1">
        {todos.map((t, i) => (
          <div key={i} className="flex items-center gap-2 text-xs">
            {t.status === "completed" ? (
              <CircleCheck size={13} className="text-emerald-400" />
            ) : t.status === "in_progress" ? (
              <LoaderCircle size={13} className="animate-spin text-sky-400" />
            ) : (
              <span className="inline-block h-3 w-3 rounded-full border border-slate-600" />
            )}
            <span className={t.status === "completed" ? "text-slate-500 line-through" : "text-slate-300"}>
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
    <div className="flex items-start gap-2 border-b border-amber-900/50 bg-amber-950/40 px-4 py-2 text-sm text-amber-200">
      <TriangleAlert size={15} className="mt-0.5 shrink-0" />
      <span className="flex-1 whitespace-pre-wrap break-words">{notice}</span>
      <button
        onClick={() => setNotice(null)}
        className="shrink-0 rounded p-0.5 hover:bg-amber-900/40"
        aria-label="关闭"
      >
        <X size={14} />
      </button>
    </div>
  );
}

function AskPanel() {
  const pendingAsk = useStore((s) => s.pendingAsk);
  const clearPendingAsk = useStore((s) => s.clearPendingAsk);
  const setNotice = useStore((s) => s.setNotice);
  const [free, setFree] = useState("");

  if (!pendingAsk) return null;

  const answer = (a: string) => {
    const sid = useStore.getState().sessionId;
    if (sid) {
      sendCommand(sid, { type: "answer_ask", id: pendingAsk.id, answer: a }).catch((e) =>
        setNotice(String(e)),
      );
    }
    clearPendingAsk();
    setFree("");
  };

  return (
    <div className="border-t border-sky-900/50 bg-sky-950/30 px-4 py-3">
      <div className="mx-auto max-w-3xl space-y-2">
        <div className="flex items-center gap-2 text-xs text-sky-300">
          <Info size={14} />
          模型在等你回答
        </div>
        <div className="text-sm text-slate-100">{pendingAsk.question}</div>
        {pendingAsk.options.length > 0 && (
          <div className="flex flex-wrap gap-2">
            {pendingAsk.options.map((opt) => (
              <Button key={opt} variant="outline" onClick={() => answer(opt)}>
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
          <Button disabled={!free.trim()} onClick={() => answer(free.trim())}>
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
    <div className="border-t border-slate-800 bg-slate-900/60 px-4 py-3">
      <div className="mx-auto flex max-w-3xl items-end gap-2">
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
            !connected ? "正在连接会话…" : pendingAsk ? "请先回答上面的问题" : "输入消息，Enter 发送，Shift+Enter 换行"
          }
          className="flex-1 resize-none rounded-lg border border-slate-700 bg-slate-950/60 px-3 py-2 text-sm text-slate-100 placeholder:text-slate-500 outline-none focus:border-sky-600 disabled:opacity-50"
        />
        {streaming ? (
          <Button variant="danger" onClick={cancel} className="h-9">
            <Square size={14} />
            取消
          </Button>
        ) : (
          <Button onClick={send} disabled={disabled || !text.trim()} className="h-9">
            <Send size={14} />
            发送
          </Button>
        )}
      </div>
    </div>
  );
}

export function ChatView() {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <NoticeBanner />
      <MessageList />
      <TodoPanel />
      <AskPanel />
      <Composer />
    </div>
  );
}
