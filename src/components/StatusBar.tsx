// 底部状态栏：用量（缓存单列）与成本。
//
// 模型与工作目录**不在这里** —— 它们已经挪到输入框下方。同一个东西出现在两处时，
// 用户不知道该看哪个、也不知道哪一处能点。
//
// 缓存读写刻意与普通输入并列显示 —— prompt cache 命中是这个客户端要让人一眼看到
// 的指标（命中与否直接决定成本），藏进 tooltip 就失去意义了。

import { useStore } from "../store";

function Stat({ label, value }: { label: string; value: number }) {
  return (
    <span className="inline-flex items-baseline gap-1 whitespace-nowrap">
      <span className="text-dim">{label}</span>
      <span className="tabular-nums text-ink">{value.toLocaleString()}</span>
    </span>
  );
}

export function StatusBar() {
  const usage = useStore((s) => s.usage);
  const cost = useStore((s) => s.cost);
  const model = useStore((s) => s.model);

  return (
    <div className="flex h-7 shrink-0 items-center gap-3 overflow-hidden border-t border-edge bg-sidebar px-4 text-[11px]">
      {usage ? (
        <span className="flex shrink-0 items-center gap-3">
          <Stat label="输入" value={usage.input_tokens} />
          <Stat label="输出" value={usage.output_tokens} />
          {usage.thinking_tokens > 0 && <Stat label="思考" value={usage.thinking_tokens} />}
          {/* 缓存单列，且用 tone 色点出来 —— 这是最该被看见的一项。 */}
          <span className="inline-flex items-baseline gap-1 whitespace-nowrap">
            <span className="text-dim">缓存</span>
            <span className="tabular-nums text-tone-green">
              {usage.cache_read_tokens.toLocaleString()}
            </span>
            <span className="text-dim">/</span>
            <span className="tabular-nums text-dim">
              {usage.cache_write_tokens.toLocaleString()}
            </span>
          </span>
        </span>
      ) : (
        <span className="shrink-0 text-dim">暂无用量</span>
      )}

      <span className="shrink-0 tabular-nums text-ink" title="本轮累计成本（USD）">
        ${cost.toFixed(4)}
      </span>

      <span className="mono ml-auto min-w-0 truncate text-dim" title={model}>
        {model || "—"}
      </span>
    </div>
  );
}
