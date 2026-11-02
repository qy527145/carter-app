//! `skill` 工具的**多根**实现：按优先级在若干个技能目录里找。
//!
//! ## 为什么不用 carter 的 `SkillTool`
//!
//! 它的 `skills_dir` 是构造期写死的**单根**（carter `skills/skill_tool.rs`）。技能页加了
//! 额外的扫描路径之后，系统提示里会列出那些根里的技能，而模型一调 `skill` 却只会在
//! 本地目录里找、报 `unknown skill` —— 模型能看见、能选中、但用不了的技能，比一开始
//! 就不列出来更坏。
//!
//! ## 为什么不是「注册两个工具」
//!
//! 工具池里出现两个同名的 `skill` 是硬错误：`ToolRegistry::specs()` 会把两条都下发给
//! 上游，Anthropic 直接 400。所以 `tools/mod.rs` 走的是**换实现**那条路（与 `bash` /
//! 文件工具并列），池子里始终只有一个 `skill`。
//!
//! `name()` 必须仍然返回 `"skill"`：carter 的并发安全名单是按**名字**判的，改名会让这个
//! 工具掉出并发批次。

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::skills::{self, Scan};

pub struct MultiRootSkill {
    scan: Scan,
}

impl MultiRootSkill {
    pub fn new(scan: Scan) -> Self {
        Self { scan }
    }
}

#[async_trait::async_trait]
impl Tool for MultiRootSkill {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "按名加载一个 Skill 的完整说明（SKILL.md 正文）。返回的正文即该能力的执行指引，加载后据此完成任务。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Skill 名（见系统提示里的 Skills 目录）" }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let name = match args.get("name").and_then(Value::as_str) {
            Some(n) if !n.is_empty() => n,
            _ => return ToolResult::err("missing or non-string argument: name"),
        };
        match skills::find_body(&self.scan, name) {
            Some(body) => ToolResult::ok(body),
            // 被停用 / 被忽略的技能也走这条。刻意**不**区分「不存在」与「已停用」：
            // 模型对「该技能已禁用」无从处理，只会去猜自己哪个参数写错了，然后在
            // 同一轮里反复重试。它该做的事与名字根本不存在时完全一样 —— 换个办法。
            None => ToolResult::err(format!("unknown skill: {name}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "carter-app-multi-skill-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_skill(root: &std::path::Path, name: &str, body: &str) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} 的说明\n---\n{body}\n"),
        )
        .unwrap();
    }

    /// 一个临时目录里的 `Scan`：`local/` 是本地启用目录，`ext/` 是外部根。
    fn scan_at(dir: &std::path::Path, ignored: &[&str]) -> Scan {
        let local = dir.join("skills");
        std::fs::create_dir_all(&local).unwrap();
        Scan {
            project_enabled: None,
            project_disabled: None,
            local_enabled: local,
            local_disabled: dir.join("skills.disabled"),
            externals: vec![dir.join("ext")],
            ignored: ignored.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// 本地没有、外部根里有 → 从外部根加载。
    #[tokio::test]
    async fn a_skill_only_present_in_an_external_root_is_loadable() {
        let dir = tmp_dir("ext");
        write_skill(&dir.join("ext"), "excel", "MAGIC-EXT");

        let t = MultiRootSkill::new(scan_at(&dir, &[]));
        let res = t.execute(json!({"name": "excel"})).await;
        assert!(res.ok, "{}", res.content);
        assert!(res.content.contains("MAGIC-EXT"));
        assert_eq!(t.name(), "skill", "名字必须不变：并发名单按名字判");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 同名时本地赢 —— 与系统提示里列的那一份同源。
    #[tokio::test]
    async fn the_local_copy_wins_over_the_external_one() {
        let dir = tmp_dir("local-wins");
        write_skill(&dir.join("skills"), "pdf", "MAGIC-LOCAL");
        write_skill(&dir.join("ext"), "pdf", "MAGIC-EXT");

        let t = MultiRootSkill::new(scan_at(&dir, &[]));
        let res = t.execute(json!({"name": "pdf"})).await;
        assert!(res.content.contains("MAGIC-LOCAL"), "{}", res.content);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 被忽略的外部技能加载不到，且报的是「不认识这个名字」。
    #[tokio::test]
    async fn an_ignored_skill_reports_as_unknown() {
        let dir = tmp_dir("ignored");
        write_skill(&dir.join("ext"), "excel", "MAGIC-EXT");

        let t = MultiRootSkill::new(scan_at(&dir, &["excel"]));
        let res = t.execute(json!({"name": "excel"})).await;
        assert!(!res.ok);
        assert_eq!(res.content, "unknown skill: excel");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// 参数缺失不该 panic，也不该当成「空名字」去查。
    #[tokio::test]
    async fn a_missing_name_is_an_error_not_a_lookup() {
        let t = MultiRootSkill::new(Scan {
            project_enabled: None,
            project_disabled: None,
            local_enabled: std::path::PathBuf::from("/definitely/not/here/skills"),
            local_disabled: std::path::PathBuf::from("/definitely/not/here/skills.disabled"),
            externals: vec![],
            ignored: vec![],
        });
        for args in [json!({}), json!({"name": 3})] {
            let res = t.execute(args.clone()).await;
            assert!(
                !res.ok && res.content.contains("name"),
                "{args} → {}",
                res.content
            );
        }
    }
}
