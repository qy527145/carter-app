//! 应用私有的界面设置。
//!
//! 与 `carter::config::Config`（`~/.carter/config.toml`）分开存，因为这里的东西
//! **carter CLI 不认**。
//!
//! 最典型的是工具开关：carter 0.1.4 的 `ToolRegistry` 是「全部常开」，
//! `Config` 里也没有任何开关字段。本应用在装配工具池之后自己按这份名单过滤一遍，
//! 于是「关掉 bash」只对本应用生效。界面上必须把这句话说出来 —— 悄悄只做一半，
//! 用户以为 CLI 也关掉了，是比不提供这个功能更坏的结果。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// `system` | `light` | `dark`。
    pub theme: String,
    /// 不注册给模型的本地工具名。空 = 全部启用。
    pub disabled_tools: Vec<String>,
    /// 侧栏宽度（像素）。
    pub sidebar_width: u32,
    /// 额外的技能扫描**父目录**（规范化后的绝对路径）。
    ///
    /// 技能从 `~/.carter/skills` 与这些目录**取并集**，本地优先。加进来的典型是
    /// Claude Code（`~/.claude/skills`）与 Codex（`~/.codex/skills`）的默认位置。
    ///
    /// 存**绝对路径**而不是 `~` 原文：展开发生在「加入」那一刻，之后这个字符串
    /// 就是纯粹的数据，不需要在读的每一处都记得展开一次（漏一处就是「路径明明对、
    /// 技能却不见了」）。
    pub skill_roots: Vec<String>,
    /// 按名字忽略的技能。**只作用于外部扫描路径**。
    ///
    /// 为什么不能拿它管本地技能：本地技能已经有自己的开关机制（目录在
    /// `skills/` 与 `skills.disabled/` 之间搬，文件管理器里看得见）。两套机制混在
    /// 一起，就会出现「文件明明在 `~/.carter/skills/pdf/`、技能却不加载」这种隐形
    /// 状态 —— 那正是 `skills.rs` 模块头一直在骂的那类错误。
    ///
    /// 为什么需要它：外部技能目录属于别的工具（改不得、也删不得），而并集会把那边
    /// 的技能一次性全塞进每一轮请求的系统提示。想挑几个用，就只有「按名字忽略」
    /// 这一条路可走。
    pub ignored_skills: Vec<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            disabled_tools: Vec::new(),
            sidebar_width: 260,
            skill_roots: Vec::new(),
            ignored_skills: Vec::new(),
        }
    }
}

impl AppSettings {
    pub fn path() -> PathBuf {
        paths::settings_path()
    }

    /// 读设置。文件不存在、或内容坏了，都回落默认值而不是报错 ——
    /// 一份坏掉的界面配置不该让应用起不来。
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    /// 指定路径读取。测试用 —— 否则每个测试都要改进程级环境变量，
    /// 而并行跑测试时那是共享状态，会互相打架。
    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&raw).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("写 {} 失败：{e}", path.display()))
    }

    /// 某个工具是否启用。
    pub fn tool_enabled(&self, name: &str) -> bool {
        !self.disabled_tools.iter().any(|d| d == name)
    }

    /// 额外的技能扫描根，按加入顺序（= 优先级）。
    pub fn skill_root_paths(&self) -> Vec<PathBuf> {
        self.skill_roots.iter().map(PathBuf::from).collect()
    }

    /// 这个（外部）技能名是否被忽略。
    ///
    /// 大小写敏感：技能名来自 SKILL.md 的 frontmatter，是作者写定的标识，不是路径。
    pub fn skill_ignored(&self, name: &str) -> bool {
        self.ignored_skills.iter().any(|n| n == name)
    }

    /// 记下 / 撤销「忽略这个外部技能名」。返回是否真的改动了。
    ///
    /// 用 `retain` + `push` 而不是 toggle：调用方传来的是**目标状态**，重复调用
    /// （界面重复点、并发写）必须是幂等的。
    pub fn set_skill_ignored(&mut self, name: &str, ignored: bool) -> bool {
        let had = self.skill_ignored(name);
        if ignored == had {
            return false;
        }
        if ignored {
            self.ignored_skills.push(name.to_string());
        } else {
            self.ignored_skills.retain(|n| n != name);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试用自己的临时目录，不碰进程级环境变量 ——
    /// cargo 并行跑测试，共享一个 `CARTER_APP_HOME` 会互相覆盖。
    fn tmp_path(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("carter-app-settings-{tag}-{}-{n}", std::process::id()))
            .join("settings.json")
    }

    /// `serde(default)` 让**缺字段**的老设置文件也能读进来（新增设置项时不会
    /// 让用户的旧文件变成「坏文件」而回落默认值、把已有关掉）。
    #[test]
    fn missing_fields_fall_back_per_field_not_wholesale() {
        let s: AppSettings = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();
        assert_eq!(s.theme, "light");
        assert_eq!(s.sidebar_width, 260, "没写的字段取默认值");
        assert!(s.disabled_tools.is_empty());
        assert!(s.skill_roots.is_empty(), "新增的扫描路径字段要能缺省");
        assert!(s.ignored_skills.is_empty());
    }

    /// 忽略名单按**目标状态**写，重复调用幂等 —— 界面重复点、或者别的页面
    /// 并发写的时候，不能把同一个名字塞进去两次。
    #[test]
    fn ignoring_a_skill_is_idempotent_and_reversible() {
        let mut s = AppSettings::default();
        assert!(s.set_skill_ignored("pdf", true));
        assert!(!s.set_skill_ignored("pdf", true), "第二次不该算改动");
        assert_eq!(s.ignored_skills, vec!["pdf".to_string()]);
        assert!(s.skill_ignored("pdf"));

        assert!(s.set_skill_ignored("pdf", false));
        assert!(!s.set_skill_ignored("pdf", false));
        assert!(!s.skill_ignored("pdf"));
        assert!(s.ignored_skills.is_empty());
    }

    #[test]
    fn skill_root_paths_keeps_the_configured_order() {
        let s = AppSettings {
            skill_roots: vec!["C:\\a".into(), "C:\\b".into()],
            ..Default::default()
        };
        assert_eq!(
            s.skill_root_paths(),
            vec![PathBuf::from("C:\\a"), PathBuf::from("C:\\b")],
            "顺序就是优先级，不能被重排"
        );
    }

    #[test]
    fn tool_enabled_is_the_negation_of_the_disabled_list() {
        let s = AppSettings {
            disabled_tools: vec!["bash".into(), "write_file".into()],
            ..Default::default()
        };
        assert!(!s.tool_enabled("bash"));
        assert!(!s.tool_enabled("write_file"));
        assert!(s.tool_enabled("read_file"));
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = tmp_path("roundtrip");
        let s = AppSettings {
            theme: "light".into(),
            disabled_tools: vec!["bash".into()],
            sidebar_width: 300,
            skill_roots: vec!["C:\\scans\\claude".into()],
            ignored_skills: vec!["pdf".into()],
        };
        s.save_to(&path).unwrap();

        let back = AppSettings::load_from(&path);
        assert_eq!(back.theme, "light");
        assert_eq!(back.disabled_tools, vec!["bash".to_string()]);
        assert_eq!(back.sidebar_width, 300);
        assert_eq!(back.skill_roots, vec!["C:\\scans\\claude".to_string()]);
        assert!(back.skill_ignored("pdf"));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_file_yields_defaults() {
        let path = tmp_path("missing");
        let s = AppSettings::load_from(&path);
        assert_eq!(s.theme, "dark");
        assert!(s.disabled_tools.is_empty());
    }

    #[test]
    fn a_corrupt_file_falls_back_to_defaults() {
        let path = tmp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ this is not json").unwrap();

        assert_eq!(
            AppSettings::load_from(&path).theme,
            "dark",
            "坏文件回落默认值，而不是 panic"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
