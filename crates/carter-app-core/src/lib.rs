//! carter-app 的应用核心。
//!
//! 刻意**不依赖 Tauri**：会话编排、事件桥、状态根都是纯 tokio + carter 的东西，
//! 能在没有 webview 的情况下测。`src-tauri` 只是薄薄一层命令与事件转发。
//!
//! * [`protocol`] —— App ↔ 前端 的 IPC 契约（改这里就要同步改 `src/lib/api.ts`）。
//! * [`shell`] —— 状态根：全部会话 + 事件总线 + 配置快照。
//! * [`session`] —— 会话运行器：一个会话一个 tokio 任务。
//! * [`view`] —— 会话历史 → 前端展示形态（不背 base64 字节）。

pub mod protocol;
pub mod session;
pub mod shell;
pub mod view;

pub use protocol::{AppEvent, Command, SessionEvent, TodoView, UsageView};
pub use session::{start_session, SessionOptions, StartedSession};
pub use shell::{AppShell, SessionHandle};
pub use view::{BlockView, MessageView};
