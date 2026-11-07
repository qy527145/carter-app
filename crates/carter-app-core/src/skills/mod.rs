//! 技能管理：列举、查看、启停、新建、删除。
//!
//! ## 单个技能的启停为什么是搬目录
//!
//! carter 的 `parse_skill_md` 只认 frontmatter 里的 `name` / `description` /
//! `allowed-tools` 三个键，多写一个 `enabled: false` 会被当成未知键**静默忽略** ——
//! 技能照样被加载。配置文件里唯一的开关是全局的 `[skills].enabled`。
//!
//! 于是单技能启停只能落在文件系统上：扫描的是 `<root>/<名称>/SKILL.md`，
//! 把目录挪到隔壁的 `<root>/skills.disabled/` 就自然消失了。可逆、不丢内容，
//! 而且用户在文件管理器里也看得懂发生了什么。
//!
//! ## 扫描路径：并集，项目 → 用户 → 外部根
//!
//! 技能从三类地方来，按**并集**用：系统提示里的技能目录与 `skill` 工具都认这些根。
//!
//! * **项目级** `<cwd>/.carter/skills`：从会话 cwd 往上找到最近的一个，自动参与，
//!   不需要任何设置。它排在**最前面** —— 同一个项目里，项目自己钉死的版本比用户
//!   个人的默认更具体。
//! * **用户级** `~/.carter/skills`：本应用自己的目录。
//! * **外部根**：界面上能加**额外的扫描父目录**（Claude Code 的 `~/.claude/skills`、
//!   Codex 的 `~/.codex/skills`，或任意自定义路径）。
//!
//! 按名字去重、**先出现者胜**。项目级与用户级都是本应用的目录，可写（启停仍是搬目录）；
//! 外部根属于别的工具，只读。
//!
//! ## 外部技能为什么只读
//!
//! `~/.claude/skills` 是 Claude Code 自己的目录。本应用对那里的技能做任何写操作
//! （搬目录、删除、覆盖 SKILL.md）都是在别的工具的地盘上动土，而那边可能正指着同一个
//! 文件。所以外部技能只能**看**和**按名字忽略**，界面上的编辑 / 删除按钮直接不出现。
//!
//! 项目级与用户级都是本应用的目录，写操作一视同仁：编辑、删除、停用（搬到同级的
//! `skills.disabled/`）。区别只在于命令行看到的永远是用户级那一份 —— 项目级只对本应用
//! 的会话成立。
//!
//! 忽略名单（[`AppSettings::ignored_skills`]）**只作用于外部**：本应用自己的技能已经有
//! 开关机制（搬目录，文件管理器里看得见），两套混在一起就会出现「文件明明在
//! `~/.carter/skills/pdf/`、技能却不加载」这种隐形状态 —— 正是本模块一直在躲的那类错。
//! 项目级尤其不能进忽略名单：它按名字生效，而同一个名字在不同项目里是不同的技能。
//!
//! ## 列表为什么不去重、并集为什么去重
//!
//! 并集是给模型看的：同一个名字出现两次，模型会以为有两个技能。列表是给人看的：把被
//! 遮蔽的那一行藏起来，用户就看不到「我 Codex 里那个同名技能被谁挡住了」—— 而那恰恰是
//! 最需要解释的状态。所以 [`list`] 列出每个根扫到的每一行，用 `effective` /
//! `shadowed_by` 说明谁真正生效。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use carter::skills::{SkillMeta, load, parse_skill_md};

use crate::app_settings::AppSettings;
use crate::paths;

/// 一个技能根属于哪一类。类别决定可写性与优先级，界面也按它分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    /// `<cwd>/.carter/skills`：跟着会话走，可写。
    Project,
    /// `~/.carter/skills`：本应用自己的目录，可写。
    Local,
    /// 用户加进来的其它工具目录，只读。
    External,
}

mod edit;
mod roots;
mod scan;

#[cfg(test)]
mod tests;

// 子模块之间互用的内部符号（`discover_at` / `winners` / `same_dir` …）都在 `scan`：
// glob 进父作用域，其它子模块的 `use super::*` 才看得见。
use scan::*;

// 对外 API：调用方仍然只认 `skills::xxx`。
pub use edit::{SkillDraft, delete, save, set_enabled};
pub use roots::{SkillPreset, SkillRootRow, add_root, presets, remove_root, roots};
pub use scan::{Scan, SkillRow, find_body, list, project_roots, read, union_metas};
