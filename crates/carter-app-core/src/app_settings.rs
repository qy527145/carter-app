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
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            disabled_tools: Vec::new(),
            sidebar_width: 260,
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
        };
        s.save_to(&path).unwrap();

        let back = AppSettings::load_from(&path);
        assert_eq!(back.theme, "light");
        assert_eq!(back.disabled_tools, vec!["bash".to_string()]);
        assert_eq!(back.sidebar_width, 300);

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
