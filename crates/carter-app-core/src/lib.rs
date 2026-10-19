//! carter-app 的应用核心。
//!
//! 刻意**不依赖 Tauri**：会话编排、事件桥、状态根都是纯 tokio + carter 的东西，
//! 能在没有 webview 的情况下测。`src-tauri` 只是薄薄一层命令与事件转发。
//!
//! * [`protocol`] —— App ↔ 前端 的 IPC 契约（改这里就要同步改 `src/lib/api.ts`）。
//! * [`shell`] —— 状态根：全部会话 + 事件总线 + 配置快照。
//! * [`session`] —— 会话运行器：一个会话一个 tokio 任务。
//! * [`view`] —— 会话历史 → 前端展示形态（不背 base64 字节）。
//! * [`config_edit`] —— 界面里改 `~/.carter/config.toml`（保真写回）。
//! * [`app_settings`] —— 应用私有的界面设置（主题、工具开关）。
//! * [`sessions`] —— 对话管理：重命名 / fork / 删除 / 搜索 / 导出。
//! * [`tools`] —— 在 carter 的装配结果之上套一层工具开关。
//! * [`skills`] —— 技能：列举 / 查看 / 启停 / 新建 / 删除。
//! * [`catalog`] —— 全部模型引用摊平成一张带元数据的表。
//! * [`input`] —— 输入框里的斜杠命令与 `@文件` 引用。
//! * [`images`] —— 粘贴进来的图片（复用 carter 的内容寻址图片库）。
//! * [`inspect`] —— 抓到的报文解回 IR，供报文面板按语义渲染。
//! * [`title`] —— 首轮之后用副模型起会话标题（含「别思考」的那层包装）。
//! * [`platform`] —— 操作系统与 shell 探测（bash 工具的跨平台基础）。

pub mod app_settings;
pub mod capture;
pub mod catalog;
pub mod config_edit;
pub mod goal;
pub mod images;
pub mod inspect;
pub mod input;
pub mod paths;
pub mod platform;
pub mod protocol;
pub mod session;
pub mod sessions;
pub mod shell;
pub mod side;
pub mod skills;
pub mod title;
pub mod tools;
pub mod view;

pub use app_settings::AppSettings;
pub use capture::{CaptureHook, CaptureStore, ExchangeRecord};
pub use catalog::{CatalogEntry, ModelRow};
pub use config_edit::{McpDraft, ModelDraft, ProviderDetail, ProviderDraft};
pub use goal::{Goal, GoalStatus};
pub use input::{CommandRow, FileKind};
pub use protocol::{AppEvent, Command, GoalView, SessionEvent, TodoView, UsageView};
pub use session::{start_session, SessionOptions, StartedSession};
pub use sessions::{SearchHit, SessionRow};
pub use shell::{AppShell, SessionHandle};
pub use side::{start_side_session, StartedSide};
pub use skills::SkillRow;
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

/// 某个协议能吃下哪些服务端工具。
///
/// 编辑弹窗在用户勾选时就要把「这几个在当前协议下不生效」标出来，而不是等他保存
/// 完再看渠道列表。让前端自己维护一张协议 → 工具名的表是很诱人的，但那张表会和
/// 底座的实现悄悄漂移；这里直接问底座，答案永远是真的。
///
/// 返回 `(生效的, 不生效的)`。
pub fn check_server_tools(kind: &str, tools: &[String]) -> (Vec<String>, Vec<String>) {
    // 只需要 `kind` 与 `server_tools` 两个字段 —— 其余给空值，`protocol()` 只看 kind。
    let cfg = carter::config::ProviderConfig {
        kind: kind.to_string(),
        api_key: None,
        base_url: None,
        models: Default::default(),
        server_tools: tools.to_vec(),
        server_tool_specs: Vec::new(),
        server_tool_max_uses: None,
    };
    let unsupported = unsupported_server_tools(&cfg);
    let ok = tools
        .iter()
        .filter(|t| !unsupported.contains(t))
        .cloned()
        .collect();
    (ok, unsupported)
}
