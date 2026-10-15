//! 技能管理：列举、查看、启停、新建、删除。
//!
//! ## 单个技能的启停为什么是搬目录
//!
//! carter 的 `parse_skill_md` 只认 frontmatter 里的 `name` / `description` /
//! `allowed-tools` 三个键，多写一个 `enabled: false` 会被当成未知键**静默忽略** ——
//! 技能照样被加载。配置文件里唯一的开关是全局的 `[skills].enabled`。
//!
//! 于是单技能启停只能落在文件系统上：`discover` 扫的是 `<root>/skills/*/SKILL.md`，
//! 把目录挪到隔壁的 `<root>/skills.disabled/` 就自然消失了。可逆、不丢内容，
//! 而且用户在文件管理器里也看得懂发生了什么。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use carter::skills::{SkillMeta, discover, load, parse_skill_md};

fn enabled_dir() -> PathBuf {
    carter::config::paths::skills_dir()
}

/// `<root>/skills.disabled` —— 与启用目录平级。
fn disabled_dir() -> PathBuf {
    enabled_dir().with_file_name("skills.disabled")
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub enabled: bool,
    pub path: String,
}

fn row(dir: &Path, m: &SkillMeta, enabled: bool) -> SkillRow {
    SkillRow {
        name: m.name.clone(),
        description: m.description.clone(),
        allowed_tools: m.allowed_tools.clone(),
        enabled,
        path: dir.join(&m.name).join("SKILL.md").to_string_lossy().to_string(),
    }
}

/// 列举全部技能：启用的与停用的放在一起，用 `enabled` 区分。
pub fn list() -> Vec<SkillRow> {
    let mut rows: Vec<SkillRow> = discover(&enabled_dir())
        .iter()
        .map(|m| row(&enabled_dir(), m, true))
        .collect();
    rows.extend(
        discover(&disabled_dir())
            .iter()
            .map(|m| row(&disabled_dir(), m, false)),
    );
    rows.sort_by(|a, b| b.enabled.cmp(&a.enabled).then(a.name.cmp(&b.name)));
    rows
}

/// 技能正文（不含 frontmatter）。用于预览与编辑。
pub fn read(name: &str) -> Result<String, String> {
    for (dir, _) in [(enabled_dir(), true), (disabled_dir(), false)] {
        if let Some(s) = load(&dir, name) {
            return Ok(s.body);
        }
    }
    Err(format!("找不到技能 `{name}`"))
}

fn move_skill(name: &str, to_enabled: bool) -> Result<(), String> {
    let (from, to) = if to_enabled {
        (disabled_dir(), enabled_dir())
    } else {
        (enabled_dir(), disabled_dir())
    };
    let src = from.join(name);
    if !src.is_dir() {
        // 已经在目标那一侧了 —— 界面重复点开关时会走到这里，不是错误。
        if to.join(name).is_dir() {
            return Ok(());
        }
        return Err(format!("找不到技能目录 `{}`", src.display()));
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
    }
    std::fs::create_dir_all(&to).map_err(|e| format!("建目录失败：{e}"))?;
    std::fs::rename(&src, to.join(name)).map_err(|e| format!("移动技能目录失败：{e}"))
}

pub fn set_enabled(name: &str, enabled: bool) -> Result<(), String> {
    move_skill(name, enabled)
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillDraft {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub body: String,
}

/// 把草稿渲染成 `SKILL.md`。
///
/// 用 carter 手写 frontmatter 解析器认的那三个键，写法照它的解析规则来：
/// 键值一行一个，`allowed-tools` 用逗号分隔。
fn render(d: &SkillDraft) -> String {
    let mut s = String::from("---\n");
    s.push_str(&format!("name: {}\n", d.name));
    if !d.description.trim().is_empty() {
        // frontmatter 是逐行解析的，换行会把后面几行变成「未知键」。
        s.push_str(&format!(
            "description: {}\n",
            d.description.trim().replace(['\n', '\r'], " ")
        ));
    }
    if !d.allowed_tools.is_empty() {
        s.push_str(&format!("allowed-tools: {}\n", d.allowed_tools.join(", ")));
    }
    s.push_str("---\n\n");
    s.push_str(d.body.trim_end());
    s.push('\n');
    s
}

/// 新建或覆盖一个技能。新技能默认落在启用目录。
pub fn save(d: &SkillDraft) -> Result<(), String> {
    let name = d.name.trim();
    if name.is_empty() {
        return Err("技能名不能为空".into());
    }
    // 技能名会变成目录名，挡住路径穿越与 Windows 上的非法字符。
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("技能名 `{name}` 只能用字母、数字、`-`、`_`"));
    }

    let text = render(d);
    // 先验证渲染结果能被 carter 解析回来 —— 否则会写下一个「存在但永不加载」
    // 的技能，用户完全看不出问题在哪。
    if parse_skill_md(&text).is_none() {
        return Err("生成的 SKILL.md 无法被解析，请检查名称与描述".into());
    }

    // 已存在的话写在它当前所在的目录，别把它从「停用」意外变成「启用」。
    let dir = if enabled_dir().join(name).is_dir() {
        enabled_dir()
    } else if disabled_dir().join(name).is_dir() {
        disabled_dir()
    } else {
        enabled_dir()
    };
    let target = dir.join(name);
    std::fs::create_dir_all(&target).map_err(|e| format!("建目录失败：{e}"))?;
    std::fs::write(target.join("SKILL.md"), text).map_err(|e| format!("写 SKILL.md 失败：{e}"))
}

/// 删除技能（目录整个删掉，不可恢复）。
pub fn delete(name: &str) -> Result<(), String> {
    for dir in [enabled_dir(), disabled_dir()] {
        let p = dir.join(name);
        if p.is_dir() {
            return std::fs::remove_dir_all(&p).map_err(|e| format!("删除失败：{e}"));
        }
    }
    Err(format!("找不到技能 `{name}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(name: &str) -> SkillDraft {
        SkillDraft {
            name: name.into(),
            description: "把 PDF 拆成文本".into(),
            allowed_tools: vec!["read_file".into(), "bash".into()],
            body: "# 用法\n\n先 read_file，再 bash。".into(),
        }
    }

    /// 渲染出来的东西必须能被 carter 自己的解析器读回来 ——
    /// 否则会写出一个「文件在、技能却永远不出现」的技能。
    #[test]
    fn rendered_frontmatter_is_parsed_back_by_carter() {
        let text = render(&draft("pdf"));
        let (meta, body) = parse_skill_md(&text).expect("渲染结果应当可解析");
        assert_eq!(meta.name, "pdf");
        assert_eq!(meta.description, "把 PDF 拆成文本");
        assert_eq!(meta.allowed_tools, vec!["read_file", "bash"]);
        assert!(body.contains("先 read_file"));
    }

    /// 描述里的换行会把 frontmatter 后面几行变成「未知键」，必须被压平。
    #[test]
    fn newlines_in_the_description_do_not_break_the_frontmatter() {
        let mut d = draft("x");
        d.description = "第一行\n第二行".into();
        let (meta, _) = parse_skill_md(&render(&d)).expect("压平后应当可解析");
        assert_eq!(meta.description, "第一行 第二行");
    }

    #[test]
    fn names_that_would_escape_the_skills_dir_are_rejected() {
        for bad in ["../evil", "a/b", "a b", ""] {
            let mut d = draft(bad);
            d.name = bad.into();
            assert!(save(&d).is_err(), "`{bad}` 应当被拒绝");
        }
    }
}
