//! 外部动作识别：把一条命令或一次工具调用归到授权书的动作类别里。
//!
//! ## 它拦不住一切，也不假装能
//!
//! `bash` 里可以写脚本、可以 `eval`，任何模式匹配都能被绕过。所以这一层不做「沙箱」，
//! 做两件能做的事：
//!
//! 1. **常见动作能认出来**：推送、开 PR、对外评论 / 发消息、删文件、加依赖。
//! 2. **认出来的高风险动作写台账**：谁在什么时候、以什么命令做了；有没有对应的
//!    用户授权。没有就唤起值守，让人知道。
//!
//! 真正的边界在工具表与授权校验里（`assistant_decide`）；这里补的是「工单自己用
//! bash 绕过去」这条现实路径，最坏情况也能留下证据。

use crate::assistant::charter::ActionClass;
use serde_json::Value;

/// 一条命令涉及的动作。只返回能认出来的；认不出的当作无风险。
pub fn classify_command(command: &str) -> Vec<ActionClass> {
    let c = command.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut add = |a: ActionClass| {
        if !out.contains(&a) {
            out.push(a);
        }
    };
    // 推送 / PR / 评论：出仓库的动作。
    if c.contains("git push") || c.contains("git -c") && c.contains(" push ") {
        add(ActionClass::Push);
    }
    if c.contains("gh pr create") || c.contains("gh pr edit") {
        add(ActionClass::OpenPr);
    }
    if c.contains("gh pr comment")
        || c.contains("gh issue comment")
        || c.contains("gh pr review")
        || c.contains("gh issue close")
    {
        add(ActionClass::CommentExternal);
    }
    // 对外发消息：钉钉 CLI、邮件、curl 带请求体。
    if (c.contains("dws ") && (c.contains(" send") || c.contains("message send")))
        || c.contains("sendmail ")
        || c.contains("mail -s")
        || c.contains("lark-cli")
        || (c.contains("curl ")
            && (c.contains(" -x post") || c.contains(" -d ") || c.contains(" --data")))
        || (c.contains("curl ") && c.contains(" -x put"))
    {
        add(ActionClass::SendMessage);
    }
    if c.contains("rm -rf")
        || c.contains("rm -r ")
        || c.contains("git clean -fd")
        || c.contains("drop table")
    {
        add(ActionClass::DeleteFiles);
    }
    if c.contains("cargo add ")
        || c.contains("cargo install ")
        || c.contains("bun add ")
        || c.contains("bun install ")
        || c.contains("npm install ")
        || c.contains("pnpm add ")
        || c.contains("yarn add ")
        || c.contains("pip install ")
    {
        add(ActionClass::AddDependency);
    }
    if c.contains("git config ") || c.contains("defaults write ") {
        add(ActionClass::ConfigWrite);
    }
    out
}

/// 一次工具调用涉及的动作。文件写入算改代码；bash 走命令分类。
pub fn classify_tool(name: &str, args: &Value) -> Vec<ActionClass> {
    match name {
        "bash" | "bash_output" => args
            .get("command")
            .and_then(Value::as_str)
            .map(classify_command)
            .unwrap_or_default(),
        "write_file" | "edit_file" | "multi_edit" | "apply_patch" => vec![ActionClass::EditCode],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obvious_external_actions_are_recognised() {
        assert!(classify_command("git push origin main").contains(&ActionClass::Push));
        assert!(classify_command("gh pr create --fill").contains(&ActionClass::OpenPr));
        assert!(
            classify_command("gh pr comment 12 --body ok").contains(&ActionClass::CommentExternal)
        );
        assert!(
            classify_command("dws chat message send --text hi").contains(&ActionClass::SendMessage)
        );
        assert!(
            classify_command("curl -X POST https://x -d '{}'").contains(&ActionClass::SendMessage)
        );
        assert!(classify_command("rm -rf build/").contains(&ActionClass::DeleteFiles));
        assert!(classify_command("bun add zod").contains(&ActionClass::AddDependency));
    }

    #[test]
    fn ordinary_work_is_not_flagged() {
        assert!(classify_command("cargo test").is_empty());
        assert!(classify_command("ls -la").is_empty());
        assert!(classify_command("git status --porcelain").is_empty());
        assert!(classify_command("git log --oneline").is_empty());
    }

    #[test]
    fn tools_are_classified_by_name() {
        assert_eq!(
            classify_tool("write_file", &serde_json::json!({"path": "a"})),
            vec![ActionClass::EditCode]
        );
        assert!(
            classify_tool("bash", &serde_json::json!({"command": "git push"}))
                .contains(&ActionClass::Push)
        );
    }
}
