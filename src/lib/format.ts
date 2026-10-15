// 显示用的格式化。集中在这里是为了让同一件事（时间、字节、token）在整个界面上
// 只有一种写法 —— 以前每个文件各写一遍 `toLocaleString`，格式各不相同。

/** 会话列表用的相对时间：刚刚 / 12 分钟前 / 昨天 / 03-14。 */
export function relTime(ms: number): string {
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "";

  const diff = Date.now() - ms;
  const min = Math.floor(diff / 60_000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;

  const today = new Date();
  const sameDay = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();

  if (sameDay(d, today)) {
    return d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  }
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  if (sameDay(d, yesterday)) {
    return `昨天 ${d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}`;
  }
  if (d.getFullYear() === today.getFullYear()) {
    return d.toLocaleDateString("zh-CN", { month: "2-digit", day: "2-digit" });
  }
  return d.toLocaleDateString("zh-CN");
}

/** 完整时间，用在 title 属性里。 */
export function fullTime(ms: number): string {
  const d = new Date(ms);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleString("zh-CN");
}

/** 会话列表按 今天 / 昨天 / 更早 分组。 */
export type TimeBucket = "今天" | "昨天" | "更早";

export function bucketOf(ms: number): TimeBucket {
  const d = new Date(ms);
  const today = new Date();
  const sameDay = (a: Date, b: Date) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();
  if (sameDay(d, today)) return "今天";
  const yesterday = new Date(today);
  yesterday.setDate(today.getDate() - 1);
  if (sameDay(d, yesterday)) return "昨天";
  return "更早";
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** 上下文窗口这类大数字：200000 → 200K。 */
export function fmtTokens(n: number): string {
  if (n >= 1000) {
    const k = n / 1000;
    return `${Number.isInteger(k) ? k : k.toFixed(1)}K`;
  }
  return String(n);
}

/** 定价是「每百万 token 的美元」。免费模型给 0，要显示成「免费」而不是 $0.000。 */
export function fmtPrice(usd: number | null | undefined): string {
  if (usd === null || usd === undefined) return "—";
  if (usd === 0) return "免费";
  return `$${usd < 1 ? usd.toFixed(3) : usd.toFixed(2)}`;
}

export function fmtMs(v: number | null): string {
  if (v === null) return "—";
  return v < 1000 ? `${v}ms` : `${(v / 1000).toFixed(2)}s`;
}

/** 能力标识 → 中文。未知的原样返回，至少不显示成 undefined。 */
export function capabilityLabel(cap: string): string {
  switch (cap) {
    case "tools":
      return "工具";
    case "vision":
      return "图片";
    case "thinking":
      return "思考";
    case "pdf":
      return "PDF";
    case "streaming":
      return "流式";
    case "prompt_cache":
      return "缓存";
    default:
      return cap;
  }
}
