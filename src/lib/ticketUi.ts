// 工单在界面上的共用词条：状态怎么说、什么颜色、哪些在等人。
//
// 单独放一个文件，是因为现在有三处要画工单的状态：助理面板的工单卡片、侧栏里
// 那条工单对话的标记、对话头部的那枚徽章。三处各写一份词表，迟早出现「同一张工单
// 在面板上是『等你拍板』、在侧栏里是『needs_user』」—— 那不是设计，是漏改。
//
// **只管显示**。工单编号（`#3`）刻意不在这里出现：编号是助理对话里指代工单用的，
// 对话列表里的那条会话就是一条普通对话（标题就是用户派单时起的那个名字），
// 编号只留在面板那一侧。

import type { TicketStatus } from "./api";

export const TICKET_STATUS_LABEL: Record<TicketStatus, string> = {
  queued: "排队中",
  working: "进行中",
  ready: "待查看",
  blocked: "受阻",
  failed: "失败",
  needs_user: "等你拍板",
  unreported: "跑完未汇报",
  dropped: "无需处理",
  done: "已完成",
  stopped: "已叫停",
};

export type TicketTone = "blue" | "green" | "orange" | "violet" | "pink";

export const TICKET_STATUS_TONE: Record<TicketStatus, TicketTone | undefined> = {
  queued: undefined,
  working: "blue",
  ready: "green",
  blocked: "orange",
  failed: "pink",
  needs_user: "violet",
  unreported: "orange",
  dropped: undefined,
  done: undefined,
  stopped: undefined,
};

/** 等着人处理的几种状态（面板「等你处理」、汇报卡上的回答框都按它判）。 */
export const TICKET_NEEDS_PERSON: TicketStatus[] = [
  "ready",
  "needs_user",
  "blocked",
  "failed",
  "unreported",
];

/** 还没收尾的（排队中 / 进行中）。 */
export const TICKET_ACTIVE: TicketStatus[] = ["queued", "working"];

/** 侧栏与对话头部那枚标记上的一句话。 */
export function ticketMarkTitle(status: TicketStatus): string {
  return `工单对话 · ${TICKET_STATUS_LABEL[status]}`;
}
