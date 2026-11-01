// 输入框：斜杠命令、@ 文件/目录、粘贴图片、模型选择、工作目录。
//
// ## 弹出菜单的位置
//
// `/` 与 `@` 菜单都往上弹（`bottom-full`），因为它们出现在光标附近、而输入框
// 永远贴着窗口底部 —— 往下弹会被窗口裁掉。
//
// ## 图片为什么单独放一个列表，而不是塞进文本
//
// `[img:hash.ext]` 是 carter 在消息文本里认的引用形态（会话文件因此保持纯文本）。
// 但把这一串 token 明晃晃地摆在输入框里给人看是没有意义的 —— 用户粘的是图，
// 不是 hash。所以图片存在 `attachments` 里、显示成缩略图，发送时才拼到文本末尾。
// 历史消息里那些 token 由 `MessageImages` 渲染回图片。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Folder, ImagePlus, ListOrdered, LoaderCircle, Send, Square, X, Zap } from "lucide-react";

import {
  listCommands,
  listFiles,
  listModels,
  readImage,
  saveImage,
  sendCommand,
  type CommandRow,
} from "../lib/api";
import { toastError } from "../lib/toast";
import * as ops from "../lib/sessionOps";
import { useStore, type QueuedPrompt } from "../store";

// ---------------------------------------------------------------------------
// 弹出菜单
// ---------------------------------------------------------------------------

type MenuItem = {
  /** 选中后要插进文本的东西（`/name ` 或 `@path `）。 */
  insert: string;
  label: string;
  hint?: string;
  onPick?: () => void;
};

function Popup({
  title,
  items,
  active,
  onPick,
}: {
  title: string;
  items: MenuItem[];
  active: number;
  onPick: (item: MenuItem) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  // 键盘上下移动时把选中项滚进视野，否则超过一屏就看不见自己在选什么。
  useEffect(() => {
    ref.current?.querySelector<HTMLElement>('[data-active="true"]')?.scrollIntoView({
      block: "nearest",
    });
  }, [active]);

  if (items.length === 0) return null;
  return (
    <div
      ref={ref}
      className="absolute bottom-full left-0 z-30 mb-2 max-h-64 w-[22rem] max-w-full overflow-y-auto rounded-lg bg-card py-1 shadow-lift"
    >
      <div className="px-3 pb-1 pt-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">
        {title}
      </div>
      {items.map((it, i) => (
        <button
          key={`${it.insert}-${i}`}
          data-active={i === active}
          // 用 pointerdown 而不是 click：click 之前输入框会先失焦，
          // 而失焦会关掉菜单，于是这一下永远点不中。
          onPointerDown={(e) => {
            e.preventDefault();
            onPick(it);
          }}
          className={`flex w-full items-center gap-2 px-3 py-1.5 text-left transition-colors ${
            i === active ? "bg-base" : "hover:bg-ink/5"
          }`}
        >
          <span className="mono min-w-0 flex-1 truncate text-xs text-ink">{it.label}</span>
          {it.hint && (
            <span className="min-w-0 max-w-[55%] shrink-0 truncate text-[11px] text-dim">
              {it.hint}
            </span>
          )}
        </button>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 图片附件
// ---------------------------------------------------------------------------

/** 一张已粘贴图片的缩略图。 */
function Thumb({ token, onRemove }: { token: string; onRemove: () => void }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    readImage(token)
      .then((u) => !cancelled && setUrl(u))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [token]);

  return (
    <div className="group relative size-14 shrink-0 overflow-hidden rounded-md bg-base">
      {url ? (
        <img src={url} alt="已粘贴的图片" className="size-full object-cover" />
      ) : (
        <div className="flex size-full items-center justify-center">
          <ImagePlus size={14} className="text-dim" />
        </div>
      )}
      <button
        onClick={onRemove}
        title="移除这张图"
        className="absolute right-0.5 top-0.5 rounded bg-black/60 p-0.5 text-white opacity-0 transition-opacity group-hover:opacity-100"
      >
        <X size={11} />
      </button>
    </div>
  );
}

/**
 * 把文本里的 `[img:...]` 引用渲染成图片。
 *
 * 用在**历史消息**里：粘贴的图存的是引用 token，会话文件是纯文本，
 * 回看时得把它换回图给人看。非图片的文本原样输出。
 */
export function MessageImages({ text }: { text: string }) {
  const parts = useMemo(() => {
    const out: { kind: "text" | "img"; value: string }[] = [];
    const re = /\[img:([0-9a-fA-F]+\.[A-Za-z0-9]+)\]/g;
    let last = 0;
    for (let m = re.exec(text); m; m = re.exec(text)) {
      if (m.index > last) out.push({ kind: "text", value: text.slice(last, m.index) });
      out.push({ kind: "img", value: m[0] });
      last = m.index + m[0].length;
    }
    if (last < text.length) out.push({ kind: "text", value: text.slice(last) });
    return out;
  }, [text]);

  if (parts.length === 0) return null;
  return (
    <>
      {parts.map((p, i) =>
        p.kind === "text" ? (
          <span key={i} className="whitespace-pre-wrap break-words">
            {p.value}
          </span>
        ) : (
          <InlineImage key={i} token={p.value} />
        ),
      )}
    </>
  );
}

function InlineImage({ token }: { token: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    readImage(token)
      .then((u) => !cancelled && setUrl(u))
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [token]);

  if (!url) {
    return (
      <span className="mono my-1 block rounded border border-dashed border-edge px-2 py-1 text-[11px] text-dim">
        {token}（读不到图片文件）
      </span>
    );
  }
  return (
    <img
      src={url}
      alt="粘贴的图片"
      className="my-1.5 block max-h-64 max-w-full rounded-lg object-contain"
    />
  );
}

// ---------------------------------------------------------------------------
// 输入框
// ---------------------------------------------------------------------------

/**
 * 光标前若是一个 `@引用`，返回 `@` 的位置与之后的查询词；否则 null。
 *
 * 从光标往回找最近的一个 `@`，中间不能有空白 —— 有空白说明那个 `@` 已经是
 * 上一个词的事了；`@` 本身也要在词首（邮箱里的 `@` 不算）。
 */
function atRefAt(text: string, caret: number): { start: number; query: string } | null {
  const upto = text.slice(0, caret);
  const at = upto.lastIndexOf("@");
  if (at < 0) return null;
  const query = upto.slice(at + 1);
  if (/\s/.test(query)) return null;
  const prev = at > 0 ? upto[at - 1] : " ";
  if (!/\s/.test(prev)) return null;
  return { start: at, query };
}

/** 光标前的 `@` 查询词（菜单用它来筛列表）；不在引用里就返回 null。 */
function fileQueryAt(text: string, caret: number): string | null {
  return atRefAt(text, caret)?.query ?? null;
}

/**
 * 选中菜单项时，要替换掉的「正在输入的词」从哪儿开始。
 *
 * 菜单是边打边筛的，选中时不能把结果**追加**在查询词后面 —— 否则会插成
 * `@src/@src/components/` 或 `/goal/goal`。`@` 引用从 `@` 原位替换（插入的
 * snippet 自带 `@`），斜杠命令从行首替换。
 */
function tokenStartAt(text: string, caret: number): number {
  const ref = atRefAt(text, caret);
  if (ref) return ref.start;
  const upto = text.slice(0, caret);
  if (upto.startsWith("/") && !/\s/.test(upto.slice(1))) return 0;
  return caret;
}

export function Composer() {
  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<string[]>([]);
  const [pasting, setPasting] = useState(false);

  const [commands, setCommands] = useState<CommandRow[]>([]);
  const [files, setFiles] = useState<string[]>([]);
  const [fileQuery, setFileQuery] = useState<string | null>(null);
  const [active, setActive] = useState(0);

  const streaming = useStore((s) => s.streaming);
  const sessionId = useStore((s) => s.sessionId);
  const cwd = useStore((s) => s.cwd);
  const model = useStore((s) => s.model);
  const pendingAsk = useStore((s) => s.pendingForm);
  const addUserTurn = useStore((s) => s.addUserTurn);
  const setNotice = useStore((s) => s.setNotice);
  const queued = useStore((s) => s.queued);
  const enqueuePrompt = useStore((s) => s.enqueuePrompt);

  const taRef = useRef<HTMLTextAreaElement>(null);

  const connected = sessionId !== null;
  const disabled = !connected || pendingAsk !== null;

  // 命令列表按 cwd 取一次。自定义命令来自磁盘，cwd 变了要重取。
  useEffect(() => {
    listCommands(cwd || null)
      .then(setCommands)
      .catch(() => setCommands([]));
  }, [cwd]);

  // `@` 的文件搜索：按 250ms 防抖，边打边搜。
  useEffect(() => {
    if (fileQuery === null) {
      setFiles([]);
      return;
    }
    let cancelled = false;
    const t = setTimeout(() => {
      listFiles(cwd || null, fileQuery, 50)
        .then((r) => !cancelled && setFiles(r))
        .catch(() => !cancelled && setFiles([]));
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [fileQuery, cwd]);

  // -------------------------------------------------------------------------
  // 菜单：由「光标前是什么」决定，而不是由某个开关状态决定 ——
  // 这样用户在句中打 `@` 也能触发，而开始打字后菜单自己就没了。
  // -------------------------------------------------------------------------

  const slashItems: MenuItem[] = useMemo(() => {
    if (!text.startsWith("/") || text.includes(" ")) return [];
    const q = text.slice(1).toLowerCase();
    return commands
      .filter((c) => c.name.toLowerCase().includes(q))
      .map((c) => ({
        insert: `/${c.name} `,
        label: `/${c.name}`,
        hint: c.description || c.argument_hint || "",
      }));
  }, [text, commands]);

  const fileItems: MenuItem[] = useMemo(() => {
    if (fileQuery === null) return [];
    return files.map((f) => {
      // 目录（后端用尾随 `/` 标记）选中后**不补空格**：菜单继续停在那个目录里，
      // 用户可以接着选下一层。文件才补空格、结束这次引用。
      const isDir = f.endsWith("/");
      return {
        insert: isDir ? `@${f}` : `@${f} `,
        label: f,
        hint: isDir ? "目录" : undefined,
      };
    });
  }, [fileQuery, files]);

  const menu = fileQuery !== null ? "file" : slashItems.length > 0 ? "slash" : null;
  const items = menu === "file" ? fileItems : slashItems;

  useEffect(() => setActive(0), [menu, items.length]);

  /** 文本变化时顺便重新判断光标前是不是 `@`。 */
  const onTextChange = (next: string, caret: number) => {
    setText(next);
    setFileQuery(fileQueryAt(next, caret));
  };

  const insert = useCallback(
    (snippet: string) => {
      const ta = taRef.current;
      if (!ta) {
        setText((t) => t + snippet);
        return;
      }
      const caret = ta.selectionStart;
      const end = ta.selectionEnd;
      // 正在输入的那段路径/命令名要换掉，而不是在后面追加。
      const start = tokenStartAt(text, caret);
      const next = text.slice(0, start) + snippet + text.slice(end);
      const pos = start + snippet.length;
      setText(next);
      // 插入的是 `@dir/`（目录）时菜单继续打开，查询词就是新目录 —— 多级引用
      // 可以一层层往下点。文件引用带尾随空格，fileQueryAt 会自然返回 null。
      setFileQuery(fileQueryAt(next, pos));
      requestAnimationFrame(() => {
        ta.focus();
        ta.setSelectionRange(pos, pos);
      });
    },
    [text],
  );

  const onPick = useCallback(
    (item: MenuItem) => {
      if (item.onPick) {
        item.onPick();
        setText("");
        setFileQuery(null);
        return;
      }
      if (item.insert.startsWith("/")) {
        // 内置命令就地执行，不进文本 —— 它不是给模型看的内容。
        const name = item.insert.slice(1).trim();
        const result = runBuiltin(name, "", { setNotice, addUserTurn });
        if (result === "done") {
          setText("");
          setFileQuery(null);
          return;
        }
        if (result === "needs-args") {
          // 点击菜单里的 `/goal` 与手打一样：把命令名摆好等用户补目标正文。
          setText(`/${name} `);
          setFileQuery(null);
          focusEnd(taRef.current, name.length + 2);
          return;
        }
        // 自定义命令：把 `/名字 ` 留在输入框里让用户补参数，
        // 真正的展开在发送时由后端做（见 core 的 expand_input）。
      }
      insert(item.insert);
    },
    [insert, setNotice, addUserTurn],
  );

  // -------------------------------------------------------------------------
  // 粘贴图片
  // -------------------------------------------------------------------------

  const onPaste = useCallback(
    async (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
      const images = Array.from(e.clipboardData.items).filter((it) =>
        it.type.startsWith("image/"),
      );
      if (images.length === 0) return; // 普通文本粘贴，走默认行为
      e.preventDefault();
      setPasting(true);
      try {
        for (const it of images) {
          const file = it.getAsFile();
          if (!file) continue;
          const buf = await file.arrayBuffer();
          const b64 = arrayBufferToBase64(buf);
          const ext = (it.type.split("/")[1] || "png").replace("jpeg", "jpg");
          const token = await saveImage(b64, ext);
          setAttachments((a) => (a.includes(token) ? a : [...a, token]));
        }
      } catch (err) {
        toastError(err);
      } finally {
        setPasting(false);
      }
    },
    [],
  );

  // -------------------------------------------------------------------------
  // 发送 / 取消
  // -------------------------------------------------------------------------

  /**
   * 发送。`interject` = 插队：中断当前这一轮，让这条消息下一个跑。
   *
   * 这里**不**直接往 `turns` 里塞用户气泡 —— 后端可能正在跑别的消息，气泡提前
   * 出现会把它下面那一轮的流式输出截断成两段。真正的插入时机是 `turn_started`
   * 带着 `prompt_id` 回来的时候（见 store 的 `applyEvent`）。
   */
  const send = (interject = false) => {
    const t = text.trim();
    // 打在输入框里的内置命令：**本地执行**，不进对话文本、也不发给模型。
    //
    // 之前只有从菜单里**点**才会执行内置命令，手打 `/help` 再回车是把字面量
    // `/help` 当消息发给模型 —— 模型只能一头雾水地问你想干什么。
    // 这条检查放在最前面，也是为了 `/goal pause` 这类命令在**生成中**照样能生效
    // （会话那边对 cancel / 作答 / goal 是即时处理的），否则目标模式跑起来就停不下了。
    const cmd = builtinOf(t);
    if (cmd) {
      const result = runBuiltin(cmd.name, cmd.args, { setNotice, addUserTurn });
      if (result === "done") {
        clearInput();
        return;
      }
      if (result === "needs-args") {
        // 命令想让你把话说完（`/goal` 后面接目标正文）。把命令名摆好、光标放末尾，
        // 而不是把用户已经打的那几个字发出去。
        setText(`/${cmd.name} `);
        setFileQuery(null);
        focusEnd(taRef.current, cmd.name.length + 2);
        return;
      }
    }
    if ((!t && attachments.length === 0) || !sessionId || disabled) return;
    // 图片引用拼在文本末尾：carter 在消息文本里认 `[img:...]` 这个形态。
    const payload = [t, ...attachments].filter(Boolean).join("\n");
    const id = nextPromptId();
    // 挂进「排队中」：模型写这一轮时用户还能接着说，后端的队列保证顺序。
    enqueuePrompt({ id, text: payload });
    setText("");
    setAttachments([]);
    setFileQuery(null);
    const el = taRef.current;
    if (el) el.style.height = "auto";
    ops.send(sessionId, payload, { id, interject }).catch((e) => setNotice(String(e)));
  };

  const clearInput = () => {
    setText("");
    setAttachments([]);
    setFileQuery(null);
    const el = taRef.current;
    if (el) el.style.height = "auto";
  };

  const cancel = () => {
    if (sessionId) ops.cancel(sessionId).catch((e) => setNotice(String(e)));
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (menu && items.length > 0) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setActive((i) => (i + 1) % items.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setActive((i) => (i - 1 + items.length) % items.length);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        onPick(items[active]);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setFileQuery(null);
        setText("");
        return;
      }
    }
    // Enter 发送，Shift+Enter 换行。生成中按 Cmd/Ctrl+Enter = 插队：
    // 中断这一轮，先处理这条。
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send(e.metaKey || e.ctrlKey);
    }
  };

  const canSend = (text.trim() !== "" || attachments.length > 0) && !disabled;

  return (
    <div className="shrink-0 px-4 pb-4 pt-1">
      <div className="relative mx-auto max-w-3xl">
        {/* 输入卡片：没有描边，全身分只靠底色的明度差（elevated 比 base 暗一档）。
            焦点是两圈咬合环，见 index.css 的 .focus-ring-bite —— 比「阴影变大」
            明确得多，而且在暗色下也成立（暗色里阴影本来就看不见）。 */}
        <div className="focus-ring-bite rounded-2xl bg-elevated shadow-soft">
          {attachments.length > 0 && (
            <div className="flex flex-wrap gap-2 px-3.5 pt-3">
              {attachments.map((t) => (
                <Thumb
                  key={t}
                  token={t}
                  onRemove={() => setAttachments((a) => a.filter((x) => x !== t))}
                />
              ))}
            </div>
          )}

          <QueueStrip queued={queued} sessionId={sessionId} />

          <textarea
            ref={taRef}
            rows={1}
            value={text}
            onChange={(e) => onTextChange(e.target.value, e.target.selectionStart)}
            onPaste={onPaste}
            onKeyDown={onKeyDown}
            disabled={disabled}
            placeholder={
              !connected
                ? "会话未就绪 —— 点「新对话」开始"
                : pendingAsk
                  ? "请先回答上面的问题"
                  : streaming
                    ? "可以接着说：Enter 排队，⌘/Ctrl+Enter 插队"
                    : "输入消息   / 命令   @ 引用文件/目录   可直接粘贴图片"
            }
            className="max-h-48 min-h-14 w-full resize-none bg-transparent px-4 pb-1 pt-3.5 text-[15px] leading-relaxed text-ink outline-none placeholder:text-dim disabled:opacity-50"
          />

          {/* 输入框下的一行：模型、工作目录、发送。 */}
          <div className="flex items-center gap-2 px-2.5 pb-2">
            <ModelPicker model={model} />
            <CwdPicker cwd={cwd} />
            {pasting && <LoaderCircle size={13} className="animate-spin text-dim" />}
            <span className="flex-1" />
            {streaming && (
              <button
                onClick={cancel}
                title="中断这一轮（正在跑的命令会被立刻结束）"
                className="inline-flex items-center gap-1.5 rounded-full bg-bubble px-3 py-1.5 text-xs text-dim transition-colors hover:text-[var(--danger)]"
              >
                <Square size={11} />
                停止
              </button>
            )}
            {/* 生成中**也**能发：默认排队，⌘/Ctrl+Enter 或按钮提示里的插队方式
                会中断当前轮。把发送键藏起来是之前「只能干等」的原因。 */}
            <button
              onClick={() => send(false)}
              disabled={!canSend}
              title={streaming ? "排队发送（⌘/Ctrl+Enter 插队）" : "发送"}
              className="rounded-full bg-accent p-2 text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-30"
            >
              <Send size={14} />
            </button>
          </div>
        </div>

        {menu === "slash" && (
          <Popup title="命令" items={slashItems} active={active} onPick={onPick} />
        )}
        {menu === "file" && (
          <Popup title="引用文件 / 目录" items={fileItems} active={active} onPick={onPick} />
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 排队中的消息
// ---------------------------------------------------------------------------

/**
 * 已发出、还没轮到它跑的消息。
 *
 * 它显示在输入框**里面**而不是对话流里：对话流里出现的东西必须是「模型已经看到
 * 的」，否则用户会以为模型正在回复一条它根本还没收到的消息。轮到了才挪进去
 * （`turn_started` 带 `prompt_id` 回来，见 store 的 `applyEvent`）。
 */
function QueueStrip({
  queued,
  sessionId,
}: {
  queued: QueuedPrompt[];
  sessionId: string | null;
}) {
  if (queued.length === 0) return null;
  return (
    <div className="border-b border-edge/70 px-3.5 py-2">
      <div className="mb-1 flex items-center gap-1.5 text-[10px] text-dim">
        <ListOrdered size={11} />
        排队中 {queued.length} 条 · 这一轮结束后依次发送
      </div>
      <ul className="space-y-0.5">
        {queued.map((q) => (
          <li
            key={q.id}
            className="group flex items-center gap-2 rounded px-1 py-0.5 hover:bg-ink/5"
          >
            <span className="min-w-0 flex-1 truncate text-xs text-dim" title={q.text}>
              {q.text.split("\n")[0]}
            </span>
            <button
              onClick={() => sessionId && ops.promote(sessionId, q.id)}
              title="插队：中断当前这一轮，先处理这条"
              className="inline-flex shrink-0 items-center gap-0.5 rounded px-1.5 py-0.5 text-[11px] text-dim opacity-0 transition-opacity hover:bg-elevated hover:text-ink group-hover:opacity-100"
            >
              <Zap size={10} />
              插队
            </button>
            <button
              onClick={() => sessionId && ops.dropQueued(sessionId, q.id)}
              title="撤回这条（还没开始跑）"
              className="shrink-0 rounded p-0.5 text-dim opacity-0 transition-opacity hover:text-[var(--danger)] group-hover:opacity-100"
            >
              <X size={12} />
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** 排队消息的编号。只在本进程内有效，`turn_started` 原样带回即可。 */
let promptSeq = Date.now();
function nextPromptId(): number {
  promptSeq += 1;
  return promptSeq;
}

// ---------------------------------------------------------------------------
// 模型选择 / 工作目录
// ---------------------------------------------------------------------------

function ModelPicker({ model }: { model: string }) {
  const [open, setOpen] = useState(false);
  const [models, setModels] = useState<
    { reference: string; provider: string; api_name: string }[]
  >([]);
  const sessionId = useStore((s) => s.sessionId);
  const setNotice = useStore((s) => s.setNotice);

  useEffect(() => {
    if (!open) return;
    listModels()
      .then((rows) =>
        setModels(rows.map((r) => ({ reference: r.reference, provider: r.provider, api_name: r.api_name }))),
      )
      .catch(() => setModels([]));
  }, [open]);

  const pick = (ref: string) => {
    setOpen(false);
    if (!sessionId) return;
    sendCommand(sessionId, { type: "set_model", model: ref }).catch((e) =>
      setNotice(String(e)),
    );
  };

  // 按渠道分组，一个渠道一段。
  const groups = useMemo(() => {
    const m = new Map<string, typeof models>();
    for (const x of models) {
      const arr = m.get(x.provider) ?? [];
      arr.push(x);
      m.set(x.provider, arr);
    }
    return [...m.entries()];
  }, [models]);

  return (
    // `relative` 是菜单的定位基准。之前菜单挂在一个 `fixed inset-0` 的遮罩里面，
    // 于是 `left-4` 是**离窗口左边 4px**、`bottom-[5.5rem]` 是离窗口底部 ——
    // 菜单就从窗口左下角弹出来了，跟按钮没有任何位置关系。
    <div className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        title={model ? `当前模型 ${model}，点击切换` : "选择模型"}
        className="mono max-w-[14rem] truncate rounded-md px-1.5 py-1 text-[11px] text-dim transition-colors hover:bg-ink/5 hover:text-ink"
      >
        {model || "选择模型"}
      </button>

      {open && (
        <>
          {/* 只负责「点别处关掉」的全屏遮罩，不参与定位。它在 DOM 上先于菜单，
              所以菜单（z-50 > z-40）盖在它上面，点击仍然落得到菜单项。 */}
          <div className="fixed inset-0 z-40" onClick={() => setOpen(false)} />
          <ModelMenu groups={groups} current={model} onPick={pick} />
        </>
      )}
    </div>
  );
}

function ModelMenu({
  groups,
  current,
  onPick,
}: {
  groups: [string, { reference: string; api_name: string }[]][];
  current: string;
  onPick: (ref: string) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  return (
    <div
      ref={ref}
      onClick={(e) => e.stopPropagation()}
      className="absolute bottom-full left-0 z-50 mb-2 max-h-80 w-72 overflow-y-auto rounded-lg bg-card py-1 shadow-lift"
    >
      {groups.length === 0 && (
        <div className="px-3 py-2 text-xs text-dim">没有可用的模型，先去「设置 → 渠道」配一个。</div>
      )}
      {groups.map(([provider, list]) => (
        <div key={provider}>
          <div className="px-3 pb-0.5 pt-2 text-[10px] font-medium uppercase tracking-wider text-dim">
            {provider}
          </div>
          {list.map((m) => (
            <button
              key={m.reference}
              onClick={() => onPick(m.reference)}
              className={`flex w-full items-center gap-2 px-3 py-1.5 text-left transition-colors hover:bg-ink/5 ${
                m.reference === current ? "text-ink" : "text-dim"
              }`}
            >
              <span className="w-3 shrink-0">{m.reference === current ? "✓" : ""}</span>
              <span className="mono min-w-0 flex-1 truncate text-xs">{m.api_name}</span>
              <span className="mono shrink-0 text-[10px] text-dim">{m.reference}</span>
            </button>
          ))}
        </div>
      ))}
    </div>
  );
}

function CwdPicker({ cwd }: { cwd: string }) {
  const [busy, setBusy] = useState(false);

  async function pick() {
    setBusy(true);
    try {
      const dir = await openDialog({ directory: true, multiple: false, title: "选择工作目录" });
      if (typeof dir !== "string" || !dir) return;
      if (dir === cwd) return;
      // 会话是按工作目录分文件存的，换目录必然是新会话 —— 这不是可以「就地切换」
      // 的东西。所以这里明确说清楚发生了什么，而不是让用户以为只是换了个标签。
      await ops.startInDirectory(dir);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <button
      onClick={pick}
      disabled={busy}
      title={cwd ? `${cwd}\n点击切换（会新开一个会话）` : "选择工作目录"}
      className="flex min-w-0 max-w-[20rem] items-center gap-1.5 rounded-md px-1.5 py-1 text-[11px] text-dim transition-colors hover:bg-ink/5 hover:text-ink disabled:opacity-50"
    >
      {busy ? <LoaderCircle size={11} className="animate-spin" /> : <Folder size={11} className="shrink-0" />}
      <span className="mono truncate">{cwd ? shortenPath(cwd) : "选择目录"}</span>
    </button>
  );
}

/**
 * 路径过长时只留尾两段，但**保留盘符**。
 *
 * 之前是 `…/Users/xuqiao`，把 `C:\Users\xuqiao` 的盘符抹掉了 —— 看着像个 Unix
 * 路径，于是和 bash 里 `pwd` 的输出（`/c/Users/xuqiao`）对不上，让人以为两处用的
 * 不是同一个目录。显示成 `C:\…\xuqiao` 就没有这个歧义了。
 *
 * 完整路径始终在 `title` 里。
 */
function shortenPath(p: string): string {
  // 短路径直接全显示 —— 这最常见，也最没有歧义。
  if (p.length <= 30) return p;
  const win = /^[A-Za-z]:[\\/]/.test(p);
  const sep = win ? "\\" : "/";
  const parts = p.split(/[\\/]/).filter(Boolean);
  if (parts.length <= 3) return p;
  const tail = parts.slice(-2).join(sep);
  return win ? `${parts[0]}${sep}…${sep}${tail}` : `…${sep}${tail}`;
}

// ---------------------------------------------------------------------------
// 内置命令的执行
// ---------------------------------------------------------------------------

/**
 * 本应用真能执行的内置命令名。
 *
 * 与后端 `crates/carter-app-core/src/input.rs::GUI_BUILTINS` 是同一份名单 ——
 * 那边决定菜单里显示什么，这边决定打出来能不能执行。多一个少一个都会让
 * 「菜单里有、打了没用」或反过来。
 */
const BUILTIN_NAMES = new Set([
  "new",
  "model",
  "cwd",
  "fork",
  "btw",
  "side",
  "goal",
  "export",
  "settings",
  "help",
]);

/**
 * 把 `/name args` 拆出来，`name` 不是内置命令就返回 null。
 *
 * 只认第一个词，且必须是名单里的名字：`/usr/bin/env` 这种路径、以及用户放在
 * `~/.carter/commands/` 里的自定义命令名都不该在这里被拦下来（它们要么该发给模型，
 * 要么该由后端展开）。
 */
function builtinOf(text: string): { name: string; args: string } | null {
  if (!text.startsWith("/")) return null;
  const rest = text.slice(1);
  const cut = rest.search(/\s/);
  const name = (cut < 0 ? rest : rest.slice(0, cut)).toLowerCase();
  if (!BUILTIN_NAMES.has(name)) return null;
  return { name, args: cut < 0 ? "" : rest.slice(cut + 1).trim() };
}

/** 执行一个内置命令要用到的几件事（都是 Composer 里的局部函数）。 */
type BuiltinCtx = {
  setNotice: (m: string | null) => void;
  addUserTurn: (t: string) => void;
};

/**
 * 内置命令的执行结果。三态而不是 `boolean`：`/goal` 不带参数时要把命令名摆回
 * 输入框让用户接着打目标正文，那既不是「执行完了」也不是「不认识这条命令」。
 */
type BuiltinResult = "done" | "needs-args" | "unhandled";

/**
 * 执行一个内置命令。
 *
 * 这些命令**不进对话文本** —— 它们不是给模型看的内容。名字与说明来自后端的
 * `GUI_BUILTINS`，那边只列本应用真做得到的，所以这里必须每一项都有着落。
 */
function runBuiltin(name: string, args: string, ctx: BuiltinCtx): BuiltinResult {
  switch (name) {
    case "new":
      ops.newSession();
      return "done";
    case "fork": {
      const id = useStore.getState().sessionId;
      if (id) void ops.fork(id);
      return "done";
    }
    case "export": {
      const id = useStore.getState().sessionId;
      if (id) void ops.export_(id, "markdown");
      return "done";
    }
    case "settings":
      useStore.getState().setView("settings");
      return "done";
    case "model":
      ctx.setNotice("点输入框下方的模型名即可切换");
      return "done";
    case "cwd":
      ctx.setNotice("点输入框下方的工作目录即可切换（会新开一个会话）");
      return "done";
    case "btw":
    case "side":
      // 带上参数的话，那几句话就是侧边聊天的第一问。
      void ops.openSideChat(args);
      return "done";
    case "goal":
      return runGoal(args, ctx);
    case "help":
      ctx.addUserTurn(HELP_TEXT);
      return "done";
    default:
      return "unhandled";
  }
}

/**
 * `/goal` 的几种形态。
 *
 * **命令不进对话文本**：目标由会话任务处理（改状态、落盘、决定要不要自动继续），
 * 它不是给模型看的一句话。假发一条用户气泡会让人以为「我说了这句」，而历史里并没有。
 */
function runGoal(args: string, ctx: BuiltinCtx): BuiltinResult {
  const id = useStore.getState().sessionId;
  if (!id) {
    ctx.setNotice("会话还没就绪");
    return "done";
  }
  const a = args.trim();
  if (!a) {
    // 无参数 = 查看。已有目标时目标条就在输入框上方，这里只需说清怎么改；
    // 还没有的话把输入框切回 `/goal `，让用户接着打目标正文。
    if (useStore.getState().goal) {
      ctx.setNotice("目标在上方那条里。用 `/goal <新的目标>` 改写正文，或点目标条上的按钮。");
      return "done";
    }
    return "needs-args";
  }

  switch (a.toLowerCase()) {
    case "clear":
    case "cancel":
      ops.goal(id, { type: "clear" });
      return "done";
    case "pause":
      ops.goal(id, { type: "pause" });
      return "done";
    case "resume":
    case "continue":
      ops.goal(id, { type: "resume" });
      return "done";
    case "done":
    case "complete":
      ops.goal(id, { type: "complete" });
      return "done";
    default:
      // 其余一律当目标正文。想设一个正文恰好叫 `pause` 的目标？点目标条上的
      // 编辑按钮 —— 留一个例外比在这里猜「用户是不是想暂停」划算。
      ops.goal(id, { type: "set", objective: a });
      return "done";
  }
}

/** 把光标放到末尾（插完一段文字之后用）。 */
function focusEnd(ta: HTMLTextAreaElement | null, pos: number) {
  if (!ta) return;
  requestAnimationFrame(() => {
    ta.focus();
    ta.setSelectionRange(pos, pos);
  });
}

const HELP_TEXT = [
  "可用命令：",
  "/new       开启新对话",
  "/model     切换模型（也可以点输入框下方的模型名）",
  "/cwd       切换工作目录（也可以点输入框下方路径）",
  "/fork      从当前会话派生新会话",
  "/btw       开一个不影响主对话的侧边聊天（可带上第一句：/btw 这个函数干嘛的）",
  "/goal      长任务目标：模型停下而目标没做完时会自动继续",
  "           /goal <目标> 设定    /goal pause | resume | done | clear",
  "/export    导出当前会话到下载目录",
  "/settings  打开设置",
  "/help      显示这段说明",
  "",
  "输入 / 开头会弹出命令菜单，@ 开头会弹出文件/目录引用菜单（目录可以一层层往下选），图片可以直接粘贴。",
  "另外 ~/.carter/commands/ 里的自定义命令（Markdown）也会出现在菜单里。",
].join("\n");

// ---------------------------------------------------------------------------

function arrayBufferToBase64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  // 分块拼接：一次 apply 几十万元素会爆栈。
  let binary = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(binary);
}
