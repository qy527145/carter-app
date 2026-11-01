// JSON 树。
//
// 报文动辄几百 KB：排成一大段文本只是"没缩进"，想找 `tools[3].parameters` 还是得
// 自己数括号。所以做成一棵**真正可折叠**的树 —— 每个对象/数组收成一行摘要，
// 长字符串单独折叠。
//
// 只渲染展开的节点（收起时子树根本不挂载），所以默认深度给小一点也不会卡：
// 一条 200 个事件的流整棵收起来，也就 200 行。
//
// 没有引第三方 JSON 查看器：这类库维护状况普遍不好，需要的能力就这么多，
// 而配色要跟着本仓库的 token 走。

import { useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

import { fmtNumber } from "../../lib/format";

/** 长字符串（base64 图片、整段文件内容）先折起来，否则一行能把页面撑爆。 */
const STRING_PREVIEW = 160;

export function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isContainer(v: unknown): v is Record<string, unknown> | unknown[] {
  return isPlainObject(v) || Array.isArray(v);
}

function Primitive({ value }: { value: unknown }) {
  const [expanded, setExpanded] = useState(false);

  if (typeof value === "string") {
    const long = value.length > STRING_PREVIEW;
    return (
      <span className="break-all">
        <span className="text-tone-green">
          "{expanded || !long ? value : `${value.slice(0, STRING_PREVIEW)}…`}"
        </span>
        {long && (
          <button
            type="button"
            className="ml-1 text-[11px] text-dim underline hover:text-ink"
            onClick={() => setExpanded((v) => !v)}
          >
            {expanded ? "收起" : `展开（${fmtNumber(value.length)} 字符）`}
          </button>
        )}
      </span>
    );
  }
  if (typeof value === "number") return <span className="text-tone-orange">{String(value)}</span>;
  if (typeof value === "boolean") return <span className="text-tone-violet">{String(value)}</span>;
  if (value === null) return <span className="text-dim">null</span>;
  // undefined 不是合法 JSON，但值可能来自手写的结构，兜一下。
  return <span className="text-dim">{String(value)}</span>;
}

/** 收起状态下括号中间那段摘要。 */
function summarize(value: Record<string, unknown> | unknown[], keys: string[]): string {
  if (Array.isArray(value)) return `${value.length} 项`;
  const shown = keys.slice(0, 3).join(", ");
  return keys.length > 3 ? `${shown}, … ${keys.length} 个键` : shown;
}

/**
 * 缩进由深度算，而不是靠嵌套 padding 与负 margin 抵消 ——
 * 后者要精确知道每层的盒模型，改一处样式闭括号就和开括号对不齐了。
 */
const INDENT_EM = 1.1;
const indentOf = (depth: number) => `${depth * INDENT_EM}em`;

/** 叶子没有折叠箭头，但要留出与箭头同宽的空位，否则同层节点起始位置差一格。 */
function ChevronSpacer() {
  return <span className="mr-0.5 inline-block size-3 shrink-0" />;
}

function Node({
  name,
  value,
  depth,
  defaultDepth,
}: {
  name?: string;
  value: unknown;
  depth: number;
  defaultDepth: number;
}) {
  const container = isContainer(value);
  const keys = container ? Object.keys(value as object) : [];
  const [open, setOpen] = useState(depth < defaultDepth);

  const label =
    name !== undefined && (
      <>
        <span className="text-tone-blue">"{name}"</span>
        <span className="text-dim">: </span>
      </>
    );

  if (!container) {
    return (
      <div style={{ paddingLeft: indentOf(depth) }}>
        <ChevronSpacer />
        {label}
        <Primitive value={value} />
      </div>
    );
  }

  const [open_, close] = Array.isArray(value) ? ["[", "]"] : ["{", "}"];

  if (keys.length === 0) {
    return (
      <div style={{ paddingLeft: indentOf(depth) }}>
        <ChevronSpacer />
        {label}
        <span className="text-dim">
          {open_}
          {close}
        </span>
      </div>
    );
  }

  return (
    <div>
      <button
        type="button"
        style={{ paddingLeft: indentOf(depth) }}
        className="flex w-full items-start rounded text-left hover:bg-ink/5"
        onClick={() => setOpen((v) => !v)}
      >
        {open ? (
          <ChevronDown size={12} className="mr-0.5 mt-[3px] shrink-0 text-dim" />
        ) : (
          <ChevronRight size={12} className="mr-0.5 mt-[3px] shrink-0 text-dim" />
        )}
        <span className="min-w-0 break-all">
          {label}
          <span className="text-dim">{open_}</span>
          {!open && (
            <span className="text-dim">
              {summarize(value as Record<string, unknown> | unknown[], keys)} {close}
            </span>
          )}
        </span>
      </button>

      {open && (
        <>
          {keys.map((k) => (
            <Node
              key={k}
              // 数组下标不值得占一格引号，和对象长得一样反而更难分辨。
              name={Array.isArray(value) ? undefined : k}
              value={(value as Record<string, unknown>)[k]}
              depth={depth + 1}
              defaultDepth={defaultDepth}
            />
          ))}
          <div className="text-dim" style={{ paddingLeft: indentOf(depth) }}>
            {close}
          </div>
        </>
      )}
    </div>
  );
}

/**
 * 一棵 JSON 树。
 *
 * 展开/收起由「默认深度 + 重挂载令牌」控制：每个节点的展开状态是自己的局部
 * state，要一次性全部重排就换个 key 重挂载整棵树。令牌单独计数而不是拿 depth
 * 当 key —— 连点两次同一个按钮时 depth 不变，那样重置不了手工展开的节点。
 */
export function JsonTree({
  value,
  defaultDepth = 2,
  className = "",
}: {
  value: unknown;
  defaultDepth?: number;
  className?: string;
}) {
  const [depth, setDepth] = useState(defaultDepth);
  const [resetToken, setResetToken] = useState(0);
  const apply = (d: number) => {
    setDepth(d);
    setResetToken((t) => t + 1);
  };

  const collapsible = isContainer(value);

  return (
    <div className="space-y-1.5">
      {collapsible && (
        <div className="flex gap-1">
          {(
            [
              [1, "展开一层"],
              [2, "展开两层"],
              [Infinity, "全部展开"],
            ] as const
          ).map(([d, label]) => (
            <button
              key={label}
              type="button"
              onClick={() => apply(d)}
              className="rounded px-1.5 py-0.5 text-[11px] text-dim transition-colors hover:bg-ink/5 hover:text-ink"
            >
              {label}
            </button>
          ))}
        </div>
      )}
      <div
        key={resetToken}
        className={`overflow-auto rounded p-2 mono text-[11px] leading-relaxed text-ink ${className}`}
      >
        <Node value={value} depth={0} defaultDepth={depth} />
      </div>
    </div>
  );
}