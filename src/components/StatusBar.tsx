// 底部状态栏：模型、用量（缓存单列）、成本、工作目录。
//
// 缓存读写刻意与普通输入并列显示 —— prompt cache 命中是这个客户端要让人一眼看到
// 的指标（命中与否直接决定成本），藏进 tooltip 就失去意义了。

import { Cpu, Folder } from "lucide-react";

import { useStore } from "../store";

function Stat({ label, value }: { label: string; value: number }) {
  return (
    <span className="inline-flex items-baseline gap-1">
      <span className="text-slate-500">{label}</span>
      <span className="tabular-nums text-slate-300">{value.toLocaleString()}</span>
    </span>
  );
}

export function StatusBar() {
  const model = useStore((s) => s.model);
  const usage = useStore((s) => s.usage);
  const cost = useStore((s) => s.cost);
  const cwd = useStore((s) => s.cwd);

  return (
    <div className="flex items-center gap-3 overflow-hidden border-t border-slate-800 bg-slate-900/70 px-4 py-1.5 text-xs">
      <span className="inline-flex shrink-0 items-center gap-1.5 text-slate-300">
        <Cpu size={13} className="text-slate-500" />
        <span className="max-w-[16rem] truncate">{model || "—"}</span>
      </span>

      <span className="text-slate-700">|</span>

      {usage ? (
        <span className="flex shrink-0 items-center gap-3">
          <Stat label="输入" value={usage.input_tokens} />
          <Stat label="输出" value={usage.output_tokens} />
          {usage.thinking_tokens > 0 && <Stat label="思考" value={usage.thinking_tokens} />}
          <Stat label="缓存读" value={usage.cache_read_tokens} />
          <Stat label="缓存写" value={usage.cache_write_tokens} />
        </span>
      ) : (
        <span className="shrink-0 text-slate-600">暂无用量</span>
      )}

      <span className="text-slate-700">|</span>

      <span className="shrink-0 tabular-nums text-emerald-300" title="本轮累计成本（USD）">
        ${cost.toFixed(4)}
      </span>

      <span className="ml-auto flex min-w-0 items-center gap-1.5 text-slate-500">
        <Folder size={13} className="shrink-0" />
        <span className="truncate" title={cwd}>
          {cwd || "—"}
        </span>
      </span>
    </div>
  );
}
