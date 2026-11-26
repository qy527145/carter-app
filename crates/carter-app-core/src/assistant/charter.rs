//! 授权书：值守层能自己决定什么、什么必须请示人。
//!
//! ## 为什么是代码，不是提示词
//!
//! 「可以自己拍板」如果只写在提示词里，模型随时可以合理地认为某件事在授权内。
//! 所以授权在这里是**可枚举的动作类别 + 风险等级**，由 `supervise_judge` 在写入
//! 之前逐条校验；子工单的授权用 [`intersect`] 收窄，不可逆动作永远进不了默认授权。
//!
//! ## 默认授权从窄
//!
//! 新装的助理只有「改代码、跑检查、重试、调整方案、查看」这几类。推送、发消息、
//! 删除、花钱一律默认请示；用户明确授予也只到可逆动作为止，而且必须带期限。
//!
//! 存储上是 `assistant.json` 里的 `charters`，与规则的写法一致：一实体一对象。

use serde::{Deserialize, Serialize};

use crate::protocol::now_ms;

/// 一类可授予/可请示的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// 改代码、写文件（仓库内）。
    EditCode,
    /// 跑测试、lint、构建。
    RunChecks,
    /// 重试上一次失败的操作。
    Retry,
    /// 调整实现方案、任务范围（不改变目标）。
    AdjustPlan,
    /// 只读查看：读文件、跑只读命令、看 diff。
    Inspect,
    /// 加依赖。
    AddDependency,
    /// 改公共接口（跨模块契约）。
    ChangePublicApi,
    /// 改配置（项目配置、应用设置）。
    ConfigWrite,
    /// 推送。
    Push,
    /// 开 PR。
    OpenPr,
    /// 对外评论（PR、issue）。
    CommentExternal,
    /// 对外发消息（钉钉、邮件等）。
    SendMessage,
    /// 删除文件或数据。
    DeleteFiles,
    /// 花钱（购买、下单、付费 API）。
    SpendMoney,
}

/// 风险等级。只有 [`Risk::Low`] 与 [`Risk::Reversible`] 允许策略默认授予。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Reversible,
    Costly,
    Irreversible,
}

impl ActionClass {
    pub const ALL: [ActionClass; 14] = [
        Self::EditCode,
        Self::RunChecks,
        Self::Retry,
        Self::AdjustPlan,
        Self::Inspect,
        Self::AddDependency,
        Self::ChangePublicApi,
        Self::ConfigWrite,
        Self::Push,
        Self::OpenPr,
        Self::CommentExternal,
        Self::SendMessage,
        Self::DeleteFiles,
        Self::SpendMoney,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::EditCode => "edit_code",
            Self::RunChecks => "run_checks",
            Self::Retry => "retry",
            Self::AdjustPlan => "adjust_plan",
            Self::Inspect => "inspect",
            Self::AddDependency => "add_dependency",
            Self::ChangePublicApi => "change_public_api",
            Self::ConfigWrite => "config_write",
            Self::Push => "push",
            Self::OpenPr => "open_pr",
            Self::CommentExternal => "comment_external",
            Self::SendMessage => "send_message",
            Self::DeleteFiles => "delete_files",
            Self::SpendMoney => "spend_money",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::EditCode => "改代码",
            Self::RunChecks => "跑检查",
            Self::Retry => "重试",
            Self::AdjustPlan => "调整方案",
            Self::Inspect => "查看",
            Self::AddDependency => "加依赖",
            Self::ChangePublicApi => "改公共接口",
            Self::ConfigWrite => "改配置",
            Self::Push => "推送",
            Self::OpenPr => "开 PR",
            Self::CommentExternal => "对外评论",
            Self::SendMessage => "对外发消息",
            Self::DeleteFiles => "删除文件",
            Self::SpendMoney => "花钱",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|a| a.id() == s || a.label() == s)
    }

    /// 风险分类。这张表是本模块的核心，改它等于改产品底线。
    pub fn risk(self) -> Risk {
        match self {
            Self::Inspect | Self::RunChecks => Risk::Low,
            // 仓库内、可回滚。
            Self::EditCode | Self::Retry | Self::AdjustPlan => Risk::Reversible,
            // 可逆，但有成本：多一个依赖、多一处契约、多一次配置改动。
            Self::AddDependency | Self::ChangePublicApi | Self::ConfigWrite => Risk::Costly,
            // 出仓库了：发出去就收不回，或者根本不可逆。
            Self::Push
            | Self::OpenPr
            | Self::CommentExternal
            | Self::SendMessage
            | Self::DeleteFiles
            | Self::SpendMoney => Risk::Irreversible,
        }
    }

    /// 能不能靠「策略默认」授予。不可逆动作永远不能 —— 只能用户逐次或限期授权。
    pub fn default_grantable(self) -> bool {
        self.risk() <= Risk::Reversible
    }
}

/// 授权书管到哪儿。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Scope {
    Global,
    /// 某个项目目录（绝对路径）。
    Project(String),
    /// 某个观察源开出来的工单。
    Source(String),
}

impl Scope {
    pub fn label(&self) -> String {
        match self {
            Self::Global => "所有项目".into(),
            Self::Project(p) => p.clone(),
            Self::Source(s) => format!("来源 {s}"),
        }
    }

    pub fn applies(&self, cwd: &str, source_id: &str) -> bool {
        match self {
            Self::Global => true,
            Self::Project(p) => p == cwd,
            Self::Source(s) => s == source_id,
        }
    }

    /// 越具体的范围优先：项目/来源都优先于全局。
    fn specificity(&self) -> u8 {
        match self {
            Self::Global => 0,
            Self::Project(_) | Self::Source(_) => 1,
        }
    }
}

/// 请示的打扰级别。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notify {
    #[default]
    Immediate,
    /// 攒到简报里，不单独打断。
    Digest,
    /// 只记台账，不主动出声（用户自己翻）。
    Silent,
}

/// 谁给的授权。模型只能**建议**更窄的授权，不能凭空授予。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    /// 策略默认（安装时生成，或某一类固定的安全默认）。
    #[default]
    Default,
    /// 用户在面板或对话里亲手给的。
    User,
}

/// 一份授权书。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Charter {
    pub id: String,
    pub name: String,
    pub scope: Scope,
    /// 可以自己决定的动作。
    #[serde(default)]
    pub may: Vec<ActionClass>,
    /// 无论如何都要请示的动作（优先级高于 `may`）。
    #[serde(default)]
    pub must_ask: Vec<ActionClass>,
    /// 验证命令：汇报 `ready` 时可以重跑它们来核对。
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub notify: Notify,
    #[serde(default)]
    pub granted: Grant,
    /// 到期时间；到点后只剩默认授权。
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub note: String,
}

impl Charter {
    /// 新装/全新项目用的默认授权：只够做局部、可逆的事。
    pub fn default_for(scope: Scope) -> Self {
        Self {
            id: format!("ch_{}", now_ms()),
            name: match &scope {
                Scope::Global => "默认授权".into(),
                Scope::Project(p) => format!("项目授权 · {p}"),
                Scope::Source(s) => format!("来源授权 · {s}"),
            },
            scope,
            may: vec![
                ActionClass::Inspect,
                ActionClass::RunChecks,
                ActionClass::EditCode,
                ActionClass::Retry,
                ActionClass::AdjustPlan,
            ],
            must_ask: vec![
                ActionClass::AddDependency,
                ActionClass::ChangePublicApi,
                ActionClass::ConfigWrite,
                ActionClass::Push,
                ActionClass::OpenPr,
                ActionClass::CommentExternal,
                ActionClass::SendMessage,
                ActionClass::DeleteFiles,
                ActionClass::SpendMoney,
            ],
            checks: Vec::new(),
            notify: Notify::Immediate,
            granted: Grant::Default,
            expires_at: None,
            note: String::new(),
        }
    }

    /// 现在还在不在有效期内。
    pub fn live(&self, now: u64) -> bool {
        self.expires_at.is_none_or(|t| t > now)
    }

    /// 这类动作能不能自决。`must_ask` 永远优先；过期后只剩默认授权。
    pub fn allows(&self, action: ActionClass, now: u64) -> bool {
        if self.must_ask.contains(&action) {
            return false;
        }
        if !self.live(now) {
            return action.default_grantable() && self.may.contains(&action);
        }
        self.may.contains(&action)
    }

    /// 子工单能继承的授权：父授权 ∩ 请求 ∩ 默认可授予。
    pub fn intersect(&self, requested: &[ActionClass], now: u64) -> Vec<ActionClass> {
        requested
            .iter()
            .copied()
            .filter(|a| a.default_grantable() && self.allows(*a, now))
            .collect()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("授权书要有名字".into());
        }
        for a in &self.may {
            if matches!(a.risk(), Risk::Irreversible) && self.granted == Grant::Default {
                return Err(format!(
                    "「{}」不可逆，不能放进默认授权；只能由用户限期授予",
                    a.label()
                ));
            }
        }
        if self.granted == Grant::User
            && self
                .may
                .iter()
                .any(|a| matches!(a.risk(), Risk::Irreversible))
            && self.expires_at.is_none()
        {
            return Err("授予不可逆动作时必须带期限（expires_at）".into());
        }
        Ok(())
    }
}

/// 按作用域挑一份最具体的有效授权书。没有就用默认那份。
pub fn resolve<'a>(
    charters: &'a [Charter],
    cwd: &str,
    source_id: &str,
    now: u64,
) -> Option<&'a Charter> {
    charters
        .iter()
        .filter(|c| c.live(now) || c.granted == Grant::Default)
        .filter(|c| c.scope.applies(cwd, source_id))
        .max_by_key(|c| c.scope.specificity())
}

/// 确保总有一份能用的默认授权。
pub fn ensure_default(charters: &mut Vec<Charter>) {
    if charters.is_empty() {
        charters.push(Charter::default_for(Scope::Global));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irreversible_actions_are_never_default_grantable() {
        for a in [
            ActionClass::Push,
            ActionClass::OpenPr,
            ActionClass::CommentExternal,
            ActionClass::SendMessage,
            ActionClass::DeleteFiles,
            ActionClass::SpendMoney,
        ] {
            assert!(!a.default_grantable(), "{a:?}");
            assert_eq!(a.risk(), Risk::Irreversible);
        }
        assert!(ActionClass::EditCode.default_grantable());
        assert!(ActionClass::RunChecks.default_grantable());
    }

    #[test]
    fn the_default_charter_is_narrow() {
        let c = Charter::default_for(Scope::Global);
        assert!(c.allows(ActionClass::EditCode, 1));
        assert!(!c.allows(ActionClass::Push, 1));
        assert!(!c.allows(ActionClass::SendMessage, 1));
        assert!(!c.allows(ActionClass::AddDependency, 1));
        c.validate().unwrap();
    }

    #[test]
    fn must_ask_beats_may() {
        let mut c = Charter::default_for(Scope::Global);
        c.may.push(ActionClass::SendMessage);
        c.must_ask.push(ActionClass::SendMessage);
        assert!(!c.allows(ActionClass::SendMessage, 1));
    }

    #[test]
    fn an_expired_grant_falls_back_to_the_default_set() {
        let mut c = Charter::default_for(Scope::Global);
        c.may.push(ActionClass::Push);
        c.expires_at = Some(100);
        assert!(!c.allows(ActionClass::Push, 200));
        assert!(c.allows(ActionClass::EditCode, 200));
    }

    #[test]
    fn a_child_can_only_narrow() {
        let c = Charter::default_for(Scope::Global);
        let got = c.intersect(
            &[
                ActionClass::EditCode,
                ActionClass::Push,
                ActionClass::SendMessage,
            ],
            1,
        );
        assert_eq!(got, vec![ActionClass::EditCode]);
    }

    #[test]
    fn default_grant_with_irreversible_is_rejected() {
        let mut c = Charter::default_for(Scope::Global);
        c.may.push(ActionClass::Push);
        assert!(c.validate().unwrap_err().contains("不可逆"));
        c.granted = Grant::User;
        assert!(c.validate().unwrap_err().contains("期限"));
        c.expires_at = Some(now_ms() + 60_000);
        c.validate().unwrap();
    }

    #[test]
    fn the_most_specific_scope_wins() {
        let mut global = Charter::default_for(Scope::Global);
        global.id = "g".into();
        let mut project = Charter::default_for(Scope::Project("/p".into()));
        project.id = "p".into();
        project.may.push(ActionClass::Push);
        project.granted = Grant::User;
        project.expires_at = Some(u64::MAX);
        let list = vec![global, project];
        let got = resolve(&list, "/p", "", 1).unwrap();
        assert_eq!(got.id, "p");
        let got = resolve(&list, "/other", "", 1).unwrap();
        assert_eq!(got.id, "g");
    }

    #[test]
    fn an_empty_list_gets_a_default() {
        let mut v = Vec::new();
        ensure_default(&mut v);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].scope, Scope::Global);
    }
}
