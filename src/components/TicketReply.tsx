// 回答一张等人处理的工单：候选按钮 + 自己写一句。
//
// 助理面板的工单卡片与助理对话里的汇报卡片共用这一个 —— 两处答的都直接走
// `assistant_ticket_action` 的 reply，原话送进工单，不经过助理模型转述；后端顺手在
// 助理对话里记一笔「已答复」，所以从哪答，助理都知道。

import { useState } from "react";
import { assistantTicketAction, type AssistantTicket } from "../lib/api";
import { toastError, toastOk } from "../lib/toast";
import { Button } from "./ui/Button";
import { Input } from "./ui/Input";

export function TicketReply({ t }: { t: AssistantTicket }) {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);

  const send = (reply: string) => {
    if (!reply.trim() || busy) return;
    setBusy(true);
    assistantTicketAction(t.id, { type: "reply", text: reply.trim() })
      .then(() => {
        toastOk("已送进工单");
        setText("");
      })
      .catch(toastError)
      .finally(() => setBusy(false));
  };

  const options = t.status === "needs_user" ? (t.options ?? []) : [];

  return (
    <div className="space-y-1.5">
      {options.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {options.map((o) => (
            <Button key={o} size="sm" disabled={busy} onClick={() => send(o)} title={o}>
              <span className="max-w-[28rem] truncate">{o}</span>
            </Button>
          ))}
        </div>
      )}
      <div className="flex gap-2">
        <Input
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.nativeEvent.isComposing) send(text);
          }}
          placeholder={
            options.length > 0
              ? "都不合适？自己写一句"
              : t.status === "needs_user"
                ? "回答它的问题，它会接着干"
                : "补充要求，让它接着干"
          }
          className="text-xs"
        />
        <Button size="sm" disabled={busy || !text.trim()} onClick={() => send(text)}>
          发送
        </Button>
      </div>
    </div>
  );
}
