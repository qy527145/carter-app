// 侧边聊天（/btw）：右边一个抽屉，与主对话并存。
//
// 三条设计取舍：
//
// 1. **抽屉，不是整屏。** 「侧边」的全部意义就是主对话还看得见 —— 主对话正在流式输出
//    的时候，用户得能看着它继续动，才敢往这边打字。整屏切换就退化成「另一个对话」了。
// 2. **自己一个精简输入框。** 不复用 `Composer.tsx`：那个带着 `@文件`、图片、模型
//    选择器、工作目录，是「这条对话的身份」那一层；侧边聊天是临时问一句，塞进去
//    是错的一层。
// 3. **z-30，比报文抽屉（z-40）低。** 两个都在右边，同时开会叠在一起；报文是
//    「看一眼就关」的临时视图，盖在上面更符合直觉。

import { useEffect, useRef, useState } from "react";
import { Globe, Send, Square, X } from "lucide-react";

import * as ops from "../lib/sessionOps";
import { useStore } from "../store";
import { TurnList } from "./ChatView";
import { IconButton } from "./ui/Button";

export function SidePanel() {
  const side = useStore((s) => s.side);
  const addSideUserTurn = useStore((s) => s.addSideUserTurn);
  const [text, setText] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const taRef = useRef<HTMLTextAreaElement>(null);

  // 贴底跟随，规则与主对话一致：用户往上翻时不要把他拽回来。
  useEffect(() => {
    const el = scrollRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [side?.turns, side?.streaming]);

  // 打开时把焦点放进输入框 —— 抽屉刚开还要再点一下才能打字，很别扭。
  useEffect(() => {
    if (side) taRef.current?.focus();
  }, [side?.sessionId]);

  if (!side) return null;

  const send = () => {
    const t = text.trim();
    if (!t || side.streaming) return;
    setText("");
    addSideUserTurn(t);
    ops.send(side.sessionId, t).catch((e) => useStore.getState().setSideNotice(String(e)));
  };

  const cancel = () => {
    ops.cancel(side.sessionId).catch(() => {
      /* 会话可能已经结束 */
    });
  };

  return (
    <div className="fixed inset-y-0 right-0 z-30 flex w-[min(30rem,46vw)] flex-col border-l border-edge bg-sidebar shadow-2xl">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-edge px-3">
        <Globe size={14} className="shrink-0 text-tone-violet" />
        <span className="text-sm text-ink">侧边聊天</span>
        <span className="truncate text-[11px] text-dim">
          继承主对话上下文 · {side.model} · 关掉即丢
        </span>
        <IconButton className="ml-auto" title="关闭（这条对话不会留下）" onClick={ops.closeSideChat}>
          <X size={14} />
        </IconButton>
      </div>

      {side.notice && (
        <div className="shrink-0 border-b border-edge px-3 py-2 text-xs text-tone-orange">
          {side.notice}
        </div>
      )}

      <div
        ref={scrollRef}
        onScroll={() => {
          const el = scrollRef.current;
          if (!el) return;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
        }}
        className="min-h-0 flex-1 overflow-y-auto px-3 py-4"
      >
        {/* 继承的上下文默认**折叠**：展开着会把侧边那几句对话顶到屏幕外，
            而用户打开抽屉要看的是自己刚问的那句。 */}
        {side.inheritedTurns.length > 0 && (
          <details className="mb-4 rounded-lg border border-edge bg-card/40">
            <summary className="cursor-pointer list-none px-3 py-1.5 text-[11px] text-dim">
              打开时主对话的上下文（{side.inheritedTurns.length} 条）· 提问时会读最新进展
            </summary>
            <div className="border-t border-edge px-3 py-3 opacity-70">
              <TurnList turns={side.inheritedTurns} streaming={false} />
            </div>
          </details>
        )}

        {side.turns.length === 0 && side.streaming && (
          <div className="text-sm text-dim">正在生成…</div>
        )}
        <TurnList turns={side.turns} streaming={side.streaming} />
      </div>

      <div className="shrink-0 border-t border-edge p-2.5">
        <div className="rounded-xl border border-edge bg-elevated">
          <textarea
            ref={taRef}
            rows={1}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                send();
              }
              if (e.key === "Escape") ops.closeSideChat();
            }}
            placeholder="随手问一句，不影响主对话"
            className="max-h-32 min-h-11 w-full resize-none bg-transparent px-3 py-2.5 text-sm leading-relaxed text-ink outline-none placeholder:text-dim"
          />
          <div className="flex items-center gap-2 px-2.5 pb-2">
            <span className="text-[11px] text-dim">
              只读参考主对话的历史；不连 MCP、不派生子 agent
            </span>
            <span className="flex-1" />
            {side.streaming ? (
              <button
                onClick={cancel}
                title="中断这一轮"
                className="inline-flex items-center gap-1.5 rounded-full border border-edge px-2.5 py-1 text-[11px] text-dim transition-colors hover:text-[var(--danger)]"
              >
                <Square size={10} />
                停止
              </button>
            ) : (
              <button
                onClick={send}
                disabled={!text.trim()}
                title="发送"
                className="rounded-full bg-accent p-1.5 text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-30"
              >
                <Send size={12} />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
