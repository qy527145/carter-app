// 助理汇报卡的标题解析（`ChatView` 的 `ReportTurn` 用）。
//
// 标题都是后端 `Ticket::label()` 拼的：`{状态} · #{编号} {标题}`，例如
// `等你拍板 · #3 修登录`。**每一张卡都带编号**，包括「已答复 / 已结束 / 已忽略 /
// 跑完未汇报」这些提示 —— 它们也是 `record` 进去的，同样走 `label()`。
//
// 所以这里要分清两件事：
//
// - 「这张卡说的是哪张工单」—— 任何卡都能答上来，答案是标题里的编号。界面拿它去
//   store 里查工单、取执行会话 id，卡片上的「打开工单对话」就是这么来的。
// - 「这张卡该不该给回答框」—— 只有**等着人回答**的那种汇报该给。已答复 / 已结束 /
//   已忽略 / 本来会接是事后记的一笔，不是提问。
//
// 曾经的错误是把这两件事混在一个函数里：给回答框的那套过滤规则被拿去解析归属，
// 结果这四类卡片一个都绑不到工单，按钮自然出不来。

/** 汇报标题里的工单编号：`等你拍板 · #3 修登录` → 3。没编号（提议、来信）→ null。 */
export function ticketNumberOf(title: string): number | null {
  const m = / · #(\d+)/.exec(title);
  return m ? Number(m[1]) : null;
}

/** 这些标题是「事后记的一笔」，不是一张等着人回答的汇报。与后端 `chat::report_number` 一致。 */
const NOTICES = ["已答复", "已结束", "已忽略", "本来会接"];

/** 这张卡是不是提示（而不是一次要人处理的汇报）。 */
export function isNotice(title: string): boolean {
  return NOTICES.some((p) => title.startsWith(p));
}

/** 要人处理的汇报的工单编号：提示类一律 null（它们不给回答框）。 */
export function reportNumberOf(title: string): number | null {
  return isNotice(title) ? null : ticketNumberOf(title);
}
