//! 团队协作：目标 / 任务 / 看板，存在项目里的 `.carter/` 下，靠 git 同步。
//!
//! ## 为什么这些不放 `~/.carter-app/`
//!
//! 应用私有的东西（报文、界面设置、定时任务）按方案 D8 落在 `~/.carter-app/`。
//! 团队数据反过来：它**必须**跟着仓库走，否则第二个人 clone 下来什么都没有。
//! 所以它们在项目里，进 git，跟代码一起 review。
//!
//! ## 三个模块的分工
//!
//! * [`model`] —— 纯数据：目标、任务、看板、id、排序插值。没有 IO。
//! * [`store`] —— `.carter/` 的读写：一实体一文件、原子写、派生视图。
//! * [`git`] —— git 通道：快进同步、远端、轮末提交。
//! * [`events`] —— 事件通道：评论 / @ / 广播 / 指派，存在独立的 `refs/carter/events` 上
//!   （协议见 `docs/team-protocol.md`）。
//! * [`members`] —— 成员名单：`members/<id>.json`，一人一文件，git 邮箱只用来认人。
//! * [`inbox`] —— 从事件流里挑出「跟我有关」的那些，界面与观察员共用。
//! * [`init`] —— 初始化：`git init` + 建骨架 + 首次提交 + 自动建同步观察源。
//! * [`skills`] —— 内置技能：教 agent 用 `team` 工具读写团队目录。
//! * [`validate`] —— 语义校验：坏文件在哪、该怎么改。写路径与 `team` 工具都走它。
//!
//! ## 与 Bibrain 的关系
//!
//! 结构参考 BiBrain 的 `goal.json` / `task-graph.json`，但把它那套「单文件 +
//! 网关投影表 + 中心发号」换成了「一实体一文件 + git 即真相 + 时间戳 id」。
//! 每处改动的理由写在 [`model`] 模块头。
//!
//! ## 对话不进 git
//!
//! BiBrain 把会话按 `sessions/<工号>/<目标>/<会话>/` 归档进团队仓库，于是要维护
//! 「对话 ↔ 成果物 ↔ 任务」三张关联表。这里**不这么做**：会话仍由 carter 存在
//! `~/.carter/sessions/`，团队目录里只记目标与任务。要说明「这件事是怎么做出来的」，
//! 写进任务的 `description` 或一条评论 —— 一段文字比一张关联表更能表达清楚，
//! 而关联表一旦漏填就再也补不回来。

pub mod events;
pub mod git;
pub mod inbox;
pub mod init;
pub mod members;
pub mod model;
pub mod skills;
pub mod store;
pub mod tool;
pub mod validate;

pub use init::{InitOutcome, initialize};
pub use model::{
    Board, Category, CategoryFile, Counts, Goal, GoalStatus, GoalView, Identity, Kind, Priority,
    Task, TaskStatus, TaskView, TeamMeta, between, new_id, valid_id,
};
pub use store::{Loaded, Store};
pub use tool::TeamTool;
pub use validate::{Issue, Report};
