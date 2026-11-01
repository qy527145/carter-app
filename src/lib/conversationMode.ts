//! 对话呈现密度：详细 / 精简。
//
// 两种模式**只差界面**：工具照常执行、结果照常回灌给模型，区别只是中间过程在
// 对话里摊开成一张张卡片，还是折成一行可展开的摘要。所以它是一个纯粹的界面偏好，
// 存在 `~/.carter-app/settings.json` 里，随时可切、来回切不碰会话历史。
//
// 为什么要有「精简」：一轮长任务下来，对话里会插进十几张工具卡片。回看时用户想读
// 的是模型说了什么，而不是它敲了哪些命令 —— 那些属于排障时才翻的过程记录。
// 参考实现里 ChatGPT 的「工作模式 / 聊天模式」切的就是这一层。

import { setAppSettings, type ConversationMode } from "./api";
import { useStore } from "../store";

export function useConversationMode(): ConversationMode {
  // 设置回来之前先按详细模式渲染：那是最完整的一档，不会把内容藏掉。
  return useStore((s) => s.settings?.conversation_mode ?? "detailed");
}

/** 切换并落盘。先改界面（立刻生效），存不下只影响下次启动，不打断用户。 */
export function setConversationMode(mode: ConversationMode): void {
  const s = useStore.getState().settings;
  if (!s) return;
  const next = { ...s, conversation_mode: mode };
  useStore.getState().setSettings(next);
  void setAppSettings(next).catch(() => {});
}