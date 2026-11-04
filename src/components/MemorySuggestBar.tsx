// 记忆建议：轮末副模型从用户的话里提炼出的候选记忆，等用户点一下确认。
//
// 放在输入框上方、与目标条同一处：用户刚说完一句话、目光就在这里，「要不要记住」
// 这件事挨着那句话问最自然。不弹窗 —— 它不该打断正在读回答的人，不理它也没关系。
//
// 记错一条的代价是之后每一轮都被它带偏，所以这里只给候选，写不写由人决定
// （见 core 的 `memory_suggest.rs`）。

import { Brain, Check, X } from "lucide-react";
import type { MemoryCandidate } from "../lib/api";
import * as api from "../lib/api";
import { toastError, toastOk } from "../lib/toast";
import { useStore } from "../store";
import { IconButton } from "./ui/Button";

/** 写到哪里 —— 用户要知道这条记住之后在哪些地方生效。 */
function where(c: MemoryCandidate): string {
  if (c.kind === "profile") return "个人偏好";
  return c.scope === "user" ? "所有项目" : "本项目";
}

export function MemorySuggestBar() {
  const items = useStore((s) => s.memorySuggestions);
  const cwd = useStore((s) => s.cwd);
  const drop = useStore((s) => s.dropMemorySuggestion);

  if (items.length === 0) return null;

  const accept = async (c: MemoryCandidate) => {
    try {
      toastOk(await api.acceptMemory(cwd, c));
      drop(c.content);
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <div className="shrink-0 px-4 pt-2">
      <div className="mx-auto max-w-3xl rounded-xl bg-tone-blue-soft px-3 py-2">
        <div className="flex items-center gap-2 text-[11px] text-dim">
          <Brain size={13} className="icon-optical shrink-0 text-tone-blue" />
          <span className="text-tone-blue">要记住吗？</span>
          <span>记住后，之后的对话都会照着做</span>
        </div>
        {items.map((c) => (
          <div key={c.content} className="mt-1 flex items-start gap-2">
            <span className="mt-0.5 shrink-0 rounded bg-base px-1.5 text-[10px] text-dim">
              {where(c)}
            </span>
            <div className="min-w-0 flex-1 whitespace-pre-wrap break-words text-sm text-ink">
              {c.content}
            </div>
            <div className="flex shrink-0 items-center gap-0.5">
              <IconButton title="记住" onClick={() => void accept(c)}>
                <Check size={13} />
              </IconButton>
              <IconButton title="不用了" onClick={() => drop(c.content)}>
                <X size={12} />
              </IconButton>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
