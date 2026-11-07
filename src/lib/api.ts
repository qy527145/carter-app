//! App ↔ 前端的 IPC 契约（TypeScript 侧）。
//!
//! 这是 `crates/carter-app-core/src/{protocol,config_edit,app_settings,sessions,
//! skills,catalog}.rs` 与 `src-tauri/src/lib.rs` 的逐字段镜像 —— Rust 侧改了这里
//! 就必须同步改，反之亦然。
//!
//! ## 两种命名约定，别混
//!
//! 1. **命令的参数名**走 camelCase：Tauri v2 会把 `sessionId` 自动转成 Rust 的
//!    `session_id`。这是 Tauri 的行为。
//! 2. **参数里的结构体字段**走 snake_case：那部分是 serde 直接反序列化的，
//!    Tauri 不参与。所以 `ProviderDraft` 里是 `api_key` 而不是 `apiKey`。
//!
//! 界面上这两种写法挨在一起（`save_provider({ draft: { api_key, … } })`），
//! 记混了只会得到一个「字段缺失」的运行时错误，所以特意写在这里。

/// 后端广播事件用的通道名。
///
/// 这是字符串字面量，改名会**静默**破坏订阅（前端再也收不到任何事件，也不报错）。
/// 与 `src-tauri/src/lib.rs` 的 `CARTER_EVENT` 必须一致。
export const EVENT_NAME = "carter://event";

export * from "./api/app";
export * from "./api/assistant";
export * from "./api/catalog";
export * from "./api/channels";
export * from "./api/connectors";
export * from "./api/input";
export * from "./api/inspect";
export * from "./api/mcp";
export * from "./api/schedules";
export * from "./api/session";
export * from "./api/skills";
export * from "./api/team";
export * from "./api/tools";
// 按领域拆在 `api/` 下，这里只做再导出 —— 外部一律从 `lib/api` 引，不直接引子文件。
// 子文件之间可以互相 import 类型，但**不要**互相调用命令封装。
export * from "./api/types";
export * from "./api/update";
