// 对话主区。按职责拆在 `chat/` 下：
//
// * `fragments` —— 正文片段（markdown / 思考 / 图片 / 过程组）与 `CursorLine`；
// * `changes`   —— 「本轮动过的文件」面板及其计算；
// * `turns`     —— 一轮消息的渲染（助手 / 用户 / 汇报 / 跨会话 / 后台命令）；
// * `list`      —— 消息列表 + 贴底滚动 + 空状态；
// * `chrome`    —— 头部、待办、提示条、输入区与 `ChatView` 本体。
//
// 这里只做再导出，外部一律从 `components/ChatView` 引。

export { ChangesPanel } from "./chat/changes";
export { ChatView } from "./chat/chrome";
export { CursorLine } from "./chat/fragments";
export { TurnList } from "./chat/turns";
