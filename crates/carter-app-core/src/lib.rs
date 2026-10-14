//! carter-app 的应用核心。
//!
//! 刻意**不依赖 Tauri**：会话编排、事件桥、状态根都是纯 tokio + carter 的东西，
//! 能在没有 webview 的情况下测。`src-tauri` 只是薄薄一层命令与事件转发。
//!
//! * [`protocol`] —— App ↔ 前端 的 IPC 契约（改这里就要同步改 `src/lib/api.ts`）。
//! * [`shell`] —— 状态根：全部会话 + 事件总线 + 配置快照。
//! * [`session`] —— 会话运行器：一个会话一个 tokio 任务。
//! * [`view`] —— 会话历史 → 前端展示形态（不背 base64 字节）。

pub mod capture;
pub mod protocol;
pub mod session;
pub mod shell;
pub mod view;

pub use capture::{CaptureHook, CaptureStore, ExchangeRecord};
pub use protocol::{AppEvent, Command, SessionEvent, TodoView, UsageView};
pub use session::{start_session, SessionOptions, StartedSession};
pub use shell::{AppShell, SessionHandle};
pub use view::{BlockView, MessageView};

/// 该渠道声明不出去的服务端工具（协议没有对应物，或名字不认识）。
///
/// 放在核心而不是外壳里：外壳不该直接认识协议枚举。渠道列表要用它把
/// 「这个渠道的 web_search 不生效」显式标出来 —— 静默忽略会让用户以为开了。
pub fn unsupported_server_tools(cfg: &carter::config::ProviderConfig) -> Vec<String> {
    let pid = cfg.protocol();
    match pid.protocol() {
        Some(proto) => {
            llm_adapt_core::resolve_specs(proto, &cfg.server_tools, cfg.server_tool_max_uses).1
        }
        None => cfg.server_tools.clone(),
    }
}
