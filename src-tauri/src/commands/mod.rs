//! Tauri 命令，按领域分文件。每个文件只做三件事：取参数、转给 `carter-app-core`、
//! 把错误转成字符串。注册名单在 `lib.rs` 的 `generate_handler!` 里。

pub(crate) mod app;
pub(crate) mod assistant;
pub(crate) mod channels;
pub(crate) mod connectors;
pub(crate) mod input;
pub(crate) mod inspect;
pub(crate) mod mcp;
pub(crate) mod schedules;
pub(crate) mod session;
pub(crate) mod skills;
pub(crate) mod team;
pub(crate) mod tools;
pub(crate) mod update;

pub(crate) use app::*;
pub(crate) use assistant::*;
pub(crate) use channels::*;
pub(crate) use connectors::*;
pub(crate) use input::*;
pub(crate) use inspect::*;
pub(crate) use mcp::*;
pub(crate) use schedules::*;
pub(crate) use session::*;
pub(crate) use skills::*;
pub(crate) use team::*;
pub(crate) use tools::*;
pub(crate) use update::*;
