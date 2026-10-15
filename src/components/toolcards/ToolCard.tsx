// 工具卡片：按工具名分派到不同的呈现。
//
// ## 为什么不是一种卡片
//
// 之前所有工具共用一个模板（名字 + 一行参数 + 一行结果），因为它们拿到的数据
// 就只有那么点 —— 一个截断的参数串和结果的第一行。补齐数据之后（见 core 的
// `tools/record.rs`），能做的事就多了：
//
// * `bash` 要看到**命令本身和它的输出**，那是排障时唯一想看的东西；
// * `todo_write` 是一张清单，不是一段文本；
// * `edit_file` / `write_file` 关心的是**动了哪个文件**；
// * `grep` / `glob` 关心命中数量与文件列表；
// * `read_file` 关心路径与开头几行。
//
// ## 折叠的默认状态
//
// 长输出默认折叠，但**失败的一定展开** —— 出错时用户的第一反应是看输出，
// 让他再点一次是多余的。同理，`bash` 的命令行永远可见（折叠起来的话，
// 一行结果放在那里没有任何意义）。
//
// 结果正文从 `exit_code=0` 这类脚手架里**解出真正的内容**再显示：那是回灌给
// 模型的格式，不是给人看的格式。

import { useState } from "react";
import {
  Check,
  ChevronRight,
  CircleCheck,
  CircleX,
  FileText,
  Folder,
  Globe,
  ListChecks,
  LoaderCircle,
  Pencil,
  Search,
  Terminal,
  Wrench,
  X,
} from "lucide-react";

import type { Item } from "../../store";

export type ToolItem = Extract<Item, { kind: "tool" }>;

// ---------------------------------------------------------------------------
// 共用外壳
// ---------------------------------------------------------------------------

/** 全应用唯一的彩色落点：内容块左侧的小图标底衬。类名必须字面量，否则扫不到。 */
const GLYPH = {
  blue: "bg-tone-blue-soft text-tone-blue",
  green: "bg-tone-green-soft text-tone-green",
  orange: "bg-tone-orange-soft text-tone-orange",
  violet: "bg-tone-violet-soft text-tone-violet",
  pink: "bg-tone-pink-soft text-tone-pink",
} as const;

export function Glyph({
  tone,
  children,
}: {
  tone: keyof typeof GLYPH;
  children: React.ReactNode;
}) {
  return (
    <span className={`flex size-5 shrink-0 items-center justify-center rounded ${GLYPH[tone]}`}>
      {children}
    </span>
  );
}

function Card({
  tone,
  icon,
  title,
  subtitle,
  status,
  preview,
  children,
  defaultOpen = false,
  forceOpen = false,
  collapsible = true,
}: {
  tone: keyof typeof GLYPH;
  icon: React.ReactNode;
  title: string;
  subtitle?: string;
  status: React.ReactNode;
  /** 折叠时**也**显示的内容。命令、路径这类「不看就没意义」的东西放这里。 */
  preview?: React.ReactNode;
  /** 可折叠的正文（输出、清单…）。 */
  children?: React.ReactNode;
  defaultOpen?: boolean;
  /** 失败时强制展开。 */
  forceOpen?: boolean;
  collapsible?: boolean;
}) {
  // `defaultOpen` 只是**初始**值。流式过程中每个 todo_write 到达时都曾是「最后一个」，
  // 于是它们各自以展开状态挂载、之后不再收回去 —— 一轮下来同一份清单展开好几遍。
  // 所以这里让「展开/收起」跟着 `defaultOpen` 走，只在用户手动点过之后才由用户说了算。
  const [manual, setManual] = useState<boolean | null>(null);
  const open = manual ?? defaultOpen;
  const expanded = forceOpen || open;
  const hasBody = children != null;

  const head = (
    <>
      <Glyph tone={tone}>{icon}</Glyph>
      <span className="mono shrink-0 text-xs text-ink">{title}</span>
      {subtitle && (
        <span className="mono min-w-0 flex-1 truncate text-[11px] text-dim" title={subtitle}>
          {subtitle}
        </span>
      )}
      {!subtitle && <span className="flex-1" />}
      {collapsible && hasBody && (
        <ChevronRight
          size={11}
          className={`shrink-0 text-dim transition-transform ${expanded ? "rotate-90" : ""}`}
        />
      )}
      {status}
    </>
  );

  return (
    <div className="overflow-hidden rounded-lg border border-edge bg-card">
      {collapsible && hasBody ? (
        <button
          onClick={() => setManual(!expanded)}
          className="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors hover:bg-base"
        >
          {head}
        </button>
      ) : (
        <div className="flex items-center gap-2 px-3 py-2">{head}</div>
      )}
      {preview != null && (
        <div className="border-t border-edge bg-base px-3 py-2">{preview}</div>
      )}
      {expanded && hasBody && (
        <div className="border-t border-edge bg-base px-3 py-2">{children}</div>
      )}
    </div>
  );
}

function Status({ item }: { item: ToolItem }) {
  const r = item.result;
  if (!r) return <LoaderCircle size={12} className="shrink-0 animate-spin text-dim" />;
  return (
    <span className="flex shrink-0 items-center gap-1.5">
      {r.elapsedMs >= 1000 && (
        <span className="tabular-nums text-[10px] text-dim">
          {(r.elapsedMs / 1000).toFixed(1)}s
        </span>
      )}
      {r.ok ? (
        <CircleCheck size={13} className="text-[var(--ok)]" />
      ) : (
        <CircleX size={13} className="text-[var(--danger)]" />
      )}
    </span>
  );
}

/** 结果正文。等宽、可滚动、保留原始换行 —— 输出是给眼睛扫的。 */
function Output({ text }: { text: string }) {
  return (
    <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-all text-[11px] leading-relaxed text-ink">
      {text}
    </pre>
  );
}

/** 失败时把结果直白地顶出来（红色），不等用户展开。 */
function ErrorLine({ text }: { text: string }) {
  return (
    <div className="mono whitespace-pre-wrap break-all text-[11px] leading-relaxed text-[var(--danger)]">
      {text}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 各工具的取值辅助
// ---------------------------------------------------------------------------

function str(args: unknown, key: string): string | null {
  if (args && typeof args === "object" && key in args) {
    const v = (args as Record<string, unknown>)[key];
    if (typeof v === "string") return v;
    if (typeof v === "number" || typeof v === "boolean") return String(v);
  }
  return null;
}

/** 从 `exit_code=0\n<正文>` 里解出正文。bash 的结果是回灌给模型的格式。 */
function bashBody(content: string): { code: string | null; body: string } {
  const m = content.match(/^exit_code=(-?\d+)\n?/);
  if (!m) return { code: null, body: content };
  return { code: m[1], body: content.slice(m[0].length) };
}

/** 相对路径缩短到末两段，完整路径放 title。 */
function tail(p: string, n = 2): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length <= n ? p : `…/${parts.slice(-n).join("/")}`;
}

// ---------------------------------------------------------------------------
// 各个工具
// ---------------------------------------------------------------------------

function BashCard({ item }: { item: ToolItem }) {
  const command = str(item.args, "command") ?? "";
  const r = item.result;
  const parsed = r ? bashBody(r.content) : null;

  return (
    <Card
      tone="blue"
      icon={<Terminal size={11} />}
      title="bash"
      status={<Status item={item} />}
      // 命令放 preview：折叠起来的话，这张卡片就只剩一行输出，看不出在跑什么。
      preview={
        <div className="mono flex gap-2">
          <span className="shrink-0 select-none text-dim">$</span>
          <span className="min-w-0 whitespace-pre-wrap break-all text-ink">{command}</span>
        </div>
      }
      defaultOpen={false}
      forceOpen={r ? !r.ok : false}
    >
      {parsed && (
        <>
          {parsed.code !== null && !r?.ok && (
            <div className="mb-1 text-[10px] text-[var(--danger)]">退出码 {parsed.code}</div>
          )}
          {parsed.body.trim() ? (
            <Output text={parsed.body} />
          ) : (
            <span className="text-[11px] text-dim">（无输出）</span>
          )}
        </>
      )}
      {!r && <span className="text-[11px] text-dim">运行中…</span>}
    </Card>
  );
}

/** `todo_write` 的清单。这是它唯一值得看的东西，不该退化成一段 JSON。 */
function TodoCard({ item, isLast = true }: { item: ToolItem; isLast?: boolean }) {
  const todos = todosOf(item);
  const done = todos.filter((t) => t.status === "completed").length;
  const running = todos.some((t) => t.status === "in_progress");

  return (
    <Card
      tone="violet"
      icon={<ListChecks size={11} />}
      title="todo_write"
      subtitle={todos.length ? `${done}/${todos.length} 已完成` : undefined}
      status={<Status item={item} />}
      // 一轮里模型常调好几次 todo_write（每次都推进一步）。全部展开的话，
      // 同一份清单会在对话里重复出现好几遍 —— 只有最后一次是当下状态。
      defaultOpen={isLast}
    >
      {todos.length === 0 ? (
        <div className="text-[11px] text-dim">（空清单）</div>
      ) : (
        <ul className="space-y-1">
          {todos.map((t, i) => (
            <li key={i} className="flex items-start gap-2 text-[12px] leading-relaxed">
              {t.status === "completed" ? (
                <CircleCheck size={13} className="mt-0.5 shrink-0 text-[var(--ok)]" />
              ) : t.status === "in_progress" ? (
                <LoaderCircle
                  size={13}
                  className={`mt-0.5 shrink-0 text-tone-blue ${running ? "animate-spin" : ""}`}
                />
              ) : (
                <span className="mt-0.5 inline-block size-3 shrink-0 rounded-full border border-edge" />
              )}
              <span
                className={
                  t.status === "completed" ? "text-dim line-through" : "text-ink"
                }
              >
                {t.status === "in_progress" && t.active_form ? t.active_form : t.content}
              </span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

type Todo = { content: string; active_form?: string; status: string };

/** 从参数或结果里取 todo 列表。两者都试 —— 结果里那份才是最终态。 */
function todosOf(item: ToolItem): Todo[] {
  const fromArgs = (item.args as { todos?: unknown })?.todos;
  if (Array.isArray(fromArgs)) {
    return fromArgs.filter(isTodo);
  }
  return [];
}

function isTodo(v: unknown): v is Todo {
  return (
    !!v &&
    typeof v === "object" &&
    typeof (v as Todo).content === "string" &&
    typeof (v as Todo).status === "string"
  );
}

function FileCard({
  item,
  tone,
  icon,
  title,
}: {
  item: ToolItem;
  tone: keyof typeof GLYPH;
  icon: React.ReactNode;
  title: string;
}) {
  const path = str(item.args, "path") ?? str(item.args, "file_path") ?? "";
  const r = item.result;
  // `write_file` 的结果只有一句「wrote N bytes to X」—— 那正是要说的事，
  // 但它前面还有一行路径，显示出来意义不大。
  const body = r ? r.content.replace(/^exit_code=\d+\n/, "") : "";

  return (
    <Card
      tone={tone}
      icon={icon}
      title={title}
      subtitle={path ? tail(path) : undefined}
      status={<Status item={item} />}
      defaultOpen={false}
      forceOpen={r ? !r.ok : false}
    >
      <div className="mono mb-1 break-all text-[11px] text-dim" title={path}>
        {path}
      </div>
      {body && <Output text={body} />}
    </Card>
  );
}

/** `edit_file`：把 old/new 并列出来。改了什么比「成功」重要得多。 */
function EditCard({ item }: { item: ToolItem }) {
  const path = str(item.args, "path") ?? str(item.args, "file_path") ?? "";
  const oldStr = str(item.args, "old_string") ?? "";
  const newStr = str(item.args, "new_string") ?? "";
  const r = item.result;

  return (
    <Card
      tone="orange"
      icon={<Pencil size={11} />}
      title="edit_file"
      subtitle={path ? tail(path) : undefined}
      status={<Status item={item} />}
      defaultOpen={!item.result}
      forceOpen={r ? !r.ok : false}
    >
      <div className="mono mb-2 break-all text-[11px] text-dim" title={path}>
        {path}
      </div>
      {oldStr === "" && newStr === "" ? (
        <Output text={r?.content ?? ""} />
      ) : (
        <div className="space-y-1.5">
          <div className="rounded border border-edge bg-[var(--error-bg)] px-2 py-1">
            <div className="mb-0.5 text-[10px] text-dim">替换前</div>
            <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-all text-[11px] leading-relaxed text-ink">
              {oldStr || "（空）"}
            </pre>
          </div>
          <div className="rounded border border-edge bg-[var(--ok)]/10 px-2 py-1">
            <div className="mb-0.5 text-[10px] text-dim">替换后</div>
            <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-all text-[11px] leading-relaxed text-ink">
              {newStr || "（空）"}
            </pre>
          </div>
          {r && !r.ok && <ErrorLine text={r.content} />}
        </div>
      )}
    </Card>
  );
}

/** `grep` / `glob`：核心信息是「命中多少、命中哪些」。 */
function SearchCard({
  item,
  tone,
  icon,
  title,
}: {
  item: ToolItem;
  tone: keyof typeof GLYPH;
  icon: React.ReactNode;
  title: string;
}) {
  const pattern = str(item.args, "pattern") ?? "";
  const scope = str(item.args, "path") ?? ".";
  const r = item.result;
  const body = r?.content ?? "";
  const lines = body.split("\n").filter((l) => l.trim() !== "");
  const empty = !body.trim() || /^\(.*no .*(match|result)/i.test(body.trim());

  return (
    <Card
      tone={tone}
      icon={icon}
      title={title}
      subtitle={`${pattern}${pattern ? " · " : ""}${scope}`}
      status={
        <>
          {r && !empty && (
            <span className="shrink-0 tabular-nums text-[10px] text-dim">{lines.length} 行</span>
          )}
          <Status item={item} />
        </>
      }
      defaultOpen={false}
      forceOpen={r ? !r.ok : false}
    >
      {empty ? (
        <div className="text-[11px] text-dim">没有命中。</div>
      ) : (
        <Output text={body} />
      )}
    </Card>
  );
}

function ReadCard({ item }: { item: ToolItem }) {
  const path = str(item.args, "path") ?? str(item.args, "file_path") ?? "";
  const r = item.result;
  const body = r?.content ?? "";
  const lineCount = body ? body.split("\n").length : 0;

  return (
    <Card
      tone="green"
      icon={<FileText size={11} />}
      title="read_file"
      subtitle={path ? tail(path) : undefined}
      status={
        <>
          {lineCount > 0 && (
            <span className="shrink-0 tabular-nums text-[10px] text-dim">{lineCount} 行</span>
          )}
          <Status item={item} />
        </>
      }
      defaultOpen={false}
      forceOpen={r ? !r.ok : false}
    >
      <div className="mono mb-1 break-all text-[11px] text-dim" title={path}>
        {path}
      </div>
      {body && <Output text={body} />}
    </Card>
  );
}

function TaskCard({ item }: { item: ToolItem }) {
  const prompt = str(item.args, "prompt") ?? str(item.args, "prompts") ?? "";
  const r = item.result;
  return (
    <Card
      tone="violet"
      icon={<Wrench size={11} />}
      title="task"
      subtitle="子 agent"
      status={<Status item={item} />}
      defaultOpen={!item.result}
      forceOpen={r ? !r.ok : false}
    >
      {prompt && (
        <div className="mb-2 whitespace-pre-wrap break-words text-[11px] leading-relaxed text-dim">
          {prompt}
        </div>
      )}
      {r && <Output text={r.content} />}
    </Card>
  );
}

/** 兜底：未知的工具也不能什么都不显示。参数原样摊开。 */
function GenericCard({ item }: { item: ToolItem }) {
  const r = item.result;
  const hasArgs = item.args != null && typeof item.args === "object";
  return (
    <Card
      tone="blue"
      icon={<Wrench size={11} />}
      title={item.name}
      subtitle={hasArgs ? summarize(item.args) : undefined}
      status={<Status item={item} />}
      defaultOpen={false}
      forceOpen={r ? !r.ok : false}
    >
      {hasArgs && (
        <pre className="mb-2 max-h-40 overflow-auto whitespace-pre-wrap break-all text-[11px] leading-relaxed text-dim">
          {JSON.stringify(item.args, null, 2)}
        </pre>
      )}
      {r && <Output text={r.content} />}
    </Card>
  );
}

/** 一行摘要，给卡片标题右边用。 */
function summarize(args: unknown): string {
  try {
    const s = JSON.stringify(args);
    return s.length > 90 ? `${s.slice(0, 90)}…` : s;
  } catch {
    return "";
  }
}

// ---------------------------------------------------------------------------
// 分派
// ---------------------------------------------------------------------------

type CardProps = { item: ToolItem; isLast?: boolean };

const DISPATCH: Record<string, (p: CardProps) => React.ReactElement> = {
  bash: BashCard,
  todo_write: TodoCard,
  glob: (p) => <SearchCard {...p} tone="blue" icon={<Folder size={11} />} title="glob" />,
  grep: (p) => <SearchCard {...p} tone="green" icon={<Search size={11} />} title="grep" />,
  read_file: ReadCard,
  edit_file: EditCard,
  write_file: (p) => (
    <FileCard {...p} tone="orange" icon={<Pencil size={11} />} title="write_file" />
  ),
  save_memory: (p) => (
    <FileCard {...p} tone="pink" icon={<FileText size={11} />} title="save_memory" />
  ),
  task: TaskCard,
  skill: (p) => (
    <FileCard {...p} tone="violet" icon={<Wrench size={11} />} title="skill" />
  ),
};

export function ToolCard({ item, isLast }: CardProps) {
  const Impl = DISPATCH[item.name] ?? GenericCard;
  return <Impl item={item} isLast={isLast} />;
}

/** 服务端工具活动（web_search 等）。本地没执行过，长得必须和本地工具不一样。 */
export function ServerCard({
  item,
}: {
  item: Extract<Item, { kind: "server" }>;
}) {
  return (
    <div className="rounded-lg border border-dashed border-edge bg-card/50 px-3 py-2">
      <div className="flex items-center gap-2">
        <Glyph tone="orange">
          <Globe size={11} />
        </Glyph>
        <span className="mono text-xs text-ink">{item.rawType}</span>
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

/** 小图标，给别处复用。 */
export const ToolIcons = { Check, X };
