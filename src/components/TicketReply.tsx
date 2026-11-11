// 回答一张等人处理的工单：候选按钮 + 自己写一句 + 看过了就结束。
//
// 助理面板的工单卡片与助理对话里的汇报卡片共用这一个 —— 两处答的都直接走
// `assistant_ticket_action` 的 reply，原话送进工单，不经过助理模型转述；后端顺手在
// 助理对话里记一笔「已答复」，所以从哪答，助理都知道。

import { Check, EyeOff } from "lucide-react";
import { useState } from "react";
import { type AssistantTicket, assistantTicketAction } from "../lib/api";
import { isImeComposing } from "../lib/ime";
import { toastError, toastOk } from "../lib/toast";
import { Button } from "./ui/Button";
import { Input } from "./ui/Input";

/**
 * `withDone`：带上「看过了，结束」与「忽略」。面板卡片顶栏已经有结束按钮，不重复；
 * 助理对话的汇报卡片上没有别的入口，要带。
 */
export function TicketReply({ t, withDone = false }: { t: AssistantTicket; withDone?: boolean }) {
  const [text, setText] = useState("");
  // 点中的候选。点一下只是「选上」，还得按发送才算答 —— 这些候选就是给用户拍板的，
  // 误点一下就替人做了决定，收不回来。
  const [picked, setPicked] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const options = t.status === "needs_user" ? (t.options ?? []) : [];
  // 状态变了之后候选可能整组没了，留下的选中项也不算数。
  const choice = picked && options.includes(picked) ? picked : null;
  /** 发出去的就一条：选中的候选，或者自己写的那句话。 */
  const pending = choice ?? text.trim();

  const send = (reply: string) => {
    if (!reply.trim() || busy) return;
    setBusy(true);
    assistantTicketAction(t.id, { type: "reply", text: reply.trim() })
      .then(() => {
        toastOk("已送进工单");
        setText("");
        setPicked(null);
      })
      .catch(toastError)
      .finally(() => setBusy(false));
  };

  /** 再点一次取消选中；选候选和写字只发一个，所以点候选时把输入框清空。 */
  const pick = (o: string) => {
    setPicked((cur) => (cur === o ? null : o));
    setText("");
  };

  /** 输入框有字时清掉候选，二者只能发一个。 */
  const onType = (v: string) => {
    setText(v);
    if (v) setPicked(null);
  };

  const done = () => {
    if (busy) return;
    setBusy(true);
    assistantTicketAction(t.id, { type: "done" })
      .then(() => toastOk("已结束"))
      .catch(toastError)
      .finally(() => setBusy(false));
  };

  const ignore = () => {
    if (busy) return;
    setBusy(true);
    assistantTicketAction(t.id, { type: "ignore" })
      .then(() => toastOk("已忽略（工单没结束，面板上还在）"))
      .catch(toastError)
      .finally(() => setBusy(false));
  };

  return (
    <div className="space-y-1.5">
      {options.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {options.map((o) => (
            <Button
              key={o}
              size="sm"
              disabled={busy}
              variant={choice === o ? "primary" : "secondary"}
              onClick={() => pick(o)}
              title={o}
            >
              {choice === o && <Check size={11} className="shrink-0" />}
              <span className="max-w-[28rem] truncate">{o}</span>
            </Button>
          ))}
        </div>
      )}
      <div className="flex gap-2">
        <Input
          value={text}
          onChange={(e) => onType(e.target.value)}
          onKeyDown={(e) => {
            // 输入法组字中的回车只是上屏，不发送。
            if (isImeComposing(e)) return;
            if (e.key === "Enter") send(pending);
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
        <Button
          size="sm"
          variant={pending ? "primary" : "secondary"}
          disabled={busy || !pending}
          onClick={() => send(pending)}
        >
          发送
        </Button>
        {withDone && (
          <>
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={ignore}
              title="我知道了，先不回复 —— 这条汇报从助理对话里排掉。工单不结束，面板上照常显示"
            >
              <EyeOff size={12} />
              忽略
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={done}
              title="看过了，不用再做什么，结束这张工单"
            >
              <Check size={12} />
              结束
            </Button>
          </>
        )}
      </div>
    </div>
  );
}
