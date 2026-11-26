//! 值守层能用的工具：查看、验证、判断、请示、收尾、派子工单、记经验。
//!
//! ## 能力边界
//!
//! 值守**只读不写**：没有 bash、没有文件工具。它能看到工单卡片、台账、git 改动，
//! 能按授权书重跑检查；但真正动手仍由工单做。这条边界由工具表保证 ——
//! `tools/mod.rs` 给值守会话只挂本模块的工具。
//!
//! ## 判断的落点
//!
//! `supervise_judge` 是唯一能改变工单状态的写工具，它在写入前逐条校验授权：
//!
//! * 动作必须在授权书的 `may` 里；
//! * 不可逆动作永远拒绝（不管授权书写了什么）；
//! * 观察模式下只记建议、不发送；
//! * 往返次数用尽时强制升级。
//!
//! 这些检查不能只写在提示词里 —— 模型可以合理地认为某件事在授权内。

use std::sync::Arc;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::assistant::charter::{self, ActionClass, Grant, Scope};
use crate::assistant::escalation::{self, Choice};
use crate::assistant::ledger::{Actor, AuthRef, LedgerEntry, LedgerKind};
use crate::assistant::runtime::{Wake, deliver, launch_queued, push_escalation};
use crate::assistant::{AssistantStore, Budget, SuperviseMode, Ticket, TicketStatus, verify};
use crate::protocol::now_ms;
use crate::shell::AppShell;

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Inspect,
    Verify,
    Judge,
    Escalate,
    Close,
    Dispatch,
    Remember,
    StandDown,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::Inspect,
        Kind::Verify,
        Kind::Judge,
        Kind::Escalate,
        Kind::Close,
        Kind::Dispatch,
        Kind::Remember,
        Kind::StandDown,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Inspect => "supervise_inspect",
            Self::Verify => "supervise_verify",
            Self::Judge => "supervise_judge",
            Self::Escalate => "supervise_escalate",
            Self::Close => "supervise_close",
            Self::Dispatch => "supervise_dispatch",
            Self::Remember => "supervise_remember",
            Self::StandDown => "supervise_stand_down",
        }
    }
}

pub struct SupervisorTool {
    shell: Arc<AppShell>,
    kind: Kind,
}

impl SupervisorTool {
    pub fn new(shell: Arc<AppShell>, kind: Kind) -> Self {
        Self { shell, kind }
    }
}

fn field(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn strings(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn resolve(store: &AssistantStore, key: &str) -> Option<Ticket> {
    let key = key.trim();
    let number = key.trim_start_matches('#').parse::<u32>().ok();
    store.read(|s| match number {
        Some(n) => s.tickets.iter().find(|t| t.number == n).cloned(),
        None => s
            .tickets
            .iter()
            .find(|t| t.id == key)
            .or_else(|| s.tickets.iter().find(|t| t.title.trim() == key))
            .cloned(),
    })
}

fn miss(store: &AssistantStore, key: &str) -> String {
    let live: Vec<String> = store.read(|s| {
        s.tickets
            .iter()
            .filter(|t| !t.status.ended())
            .map(|t| format!("{}（{}）", t.label(), t.status.label()))
            .collect()
    });
    format!(
        "找不到工单 `{key}`。现在有的：{}",
        if live.is_empty() {
            "（没有进行中的工单）".to_string()
        } else {
            live.join("、")
        }
    )
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[async_trait::async_trait]
impl Tool for SupervisorTool {
    fn name(&self) -> &str {
        self.kind.name()
    }

    fn description(&self) -> &str {
        match self.kind {
            Kind::Inspect => {
                "查看一张工单的现状：任务书、状态、预算、证据、台账、git 改动。只读。\
                 判断之前先看一遍，不要只信汇报。"
            }
            Kind::Verify => {
                "按工单授权书里的验证命令重跑检查，并把工作目录的 git 改动一起收进证据。\
                 返回一句摘要；证据会写回工单。"
            }
            Kind::Judge => {
                "在授权内替工单做决定，并把指令送进去。actions 是这条指令涉及的动作类别\
                 （edit_code / run_checks / retry / adjust_plan / inspect / add_dependency / \
                 change_public_api / config_write）；任何一类不在授权书里都会被拒绝，\
                 不可逆动作（push / send_message / delete_files 等）永远拒绝 —— 那种必须用 \
                 supervise_escalate 问人。观察模式下只记建议，不发送。"
            }
            Kind::Escalate => {
                "把一件事交给人拍板。四段必填：situation（发生了什么）、proposal（你的建议）、\
                 if_silent（用户不管你会怎么办）、options（候选走法，0–4 个）。\
                 只在授权外、不可逆、或你判断不了时用；能自决就自决。"
            }
            Kind::Close => {
                "给一张工单收尾：done（有结果且用户知道就行）或 dropped（核实后无需处理）。\
                 理由写进台账。"
            }
            Kind::Dispatch => {
                "派一张子工单。新工单的授权自动收窄为父工单授权的子集，不可逆动作永远不继承。\
                 必须给验收标准。"
            }
            Kind::Remember => {
                "记一条项目经验（推断，置信度不高）。只记可复用的事实，不记一次性信息；\
                 不能靠它给自己授权。"
            }
            Kind::StandDown => "本轮没有别的事就调用它结束，不要输出给用户看的文字。",
        }
    }

    fn parameters(&self) -> Value {
        match self.kind {
            Kind::Inspect => json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string", "description": "工单编号，如 `#3` 或 `3`" },
                    "what": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["status", "diff", "ledger"] },
                        "description": "看哪些，默认全看"
                    }
                },
                "required": ["ticket"]
            }),
            Kind::Verify => json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string" }
                },
                "required": ["ticket"]
            }),
            Kind::Judge => json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string" },
                    "directive": { "type": "string", "description": "给工单的具体指令，写清做什么、验收标准" },
                    "actions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "涉及的动作类别，至少一个"
                    },
                    "basis": { "type": "string", "description": "依据：哪份授权、哪条证据、哪条用户原话" }
                },
                "required": ["ticket", "directive", "actions", "basis"]
            }),
            Kind::Escalate => json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string", "description": "关联工单，可空" },
                    "situation": { "type": "string", "description": "发生了什么（2–3 句，含关键证据）" },
                    "proposal": { "type": "string", "description": "我建议怎么做、为什么" },
                    "options": {
                        "type": "array",
                        "maxItems": 4,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "label": { "type": "string" },
                                "consequence": { "type": "string" },
                                "risky": { "type": "boolean" },
                                "actions": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "选这条授权哪些动作（如 push / send_message）；用户答复后写进工单的逐次授权"
                                }
                            },
                            "required": ["id", "label"]
                        }
                    },
                    "if_silent": { "type": "string", "description": "用户不管的话你会怎么办" },
                    "expires_minutes": { "type": "integer", "description": "多久过期，默认 30" },
                    "topic": { "type": "string", "description": "话题键，用于去重；不给就用建议的前 40 字" }
                },
                "required": ["situation", "proposal", "if_silent"]
            }),
            Kind::Close => json!({
                "type": "object",
                "properties": {
                    "ticket": { "type": "string" },
                    "verdict": { "type": "string", "enum": ["done", "dropped"] },
                    "reason": { "type": "string" }
                },
                "required": ["ticket", "verdict", "reason"]
            }),
            Kind::Dispatch => json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "task": { "type": "string" },
                    "cwd": { "type": "string" },
                    "acceptance": { "type": "array", "items": { "type": "string" } },
                    "parent": { "type": "string", "description": "父工单编号；授权从它继承" }
                },
                "required": ["title", "task", "cwd", "acceptance", "parent"]
            }),
            Kind::Remember => json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "ticket": { "type": "string", "description": "依据哪张工单" }
                },
                "required": ["text"]
            }),
            Kind::StandDown => json!({ "type": "object", "properties": {} }),
        }
    }

    async fn execute(&self, args: Value) -> ToolResult {
        match self.kind {
            Kind::Inspect => self.inspect(args),
            Kind::Verify => self.verify(args).await,
            Kind::Judge => self.judge(args),
            Kind::Escalate => self.escalate(args),
            Kind::Close => self.close(args),
            Kind::Dispatch => self.dispatch(args).await,
            Kind::Remember => self.remember(args),
            Kind::StandDown => ToolResult::ok("本轮结束。"),
        }
    }
}

impl SupervisorTool {
    fn inspect(&self, args: Value) -> ToolResult {
        let Some(key) = field(&args, "ticket") else {
            return ToolResult::err("supervise_inspect: 缺少 ticket");
        };
        let Some(t) = resolve(&self.shell.assistant, &key) else {
            return ToolResult::err(format!(
                "supervise_inspect: {}",
                miss(&self.shell.assistant, &key)
            ));
        };
        let what = strings(&args, "what");
        let all = what.is_empty();
        let mut out = String::new();
        if all || what.iter().any(|w| w == "status") {
            out.push_str(&format!(
                "## {}\n状态：{}\n任务：{}\n",
                t.label(),
                t.status.label(),
                t.task.trim()
            ));
            if !t.acceptance.is_empty() {
                out.push_str(&format!("验收：{}\n", t.acceptance.join("；")));
            }
            let ch = self
                .shell
                .assistant
                .read(|s| s.charter_for(&t).map(|c| (c.name.clone(), c.may.clone())));
            if let Some((name, may)) = ch {
                out.push_str(&format!(
                    "授权：{name}（可自决：{}）\n",
                    may.iter().map(|a| a.label()).collect::<Vec<_>>().join("、")
                ));
            }
            out.push_str(&format!(
                "预算：{} 轮 / {} 分钟 / 判断 {}/{}\n",
                t.used.turns, t.used.minutes, t.judgement.rounds, t.budget.max_judgements
            ));
            if let Some(ev) = &t.evidence {
                out.push_str(&format!(
                    "证据：{}（verified={}）\n",
                    ev.summary(),
                    ev.verified
                ));
            }
            if let Some(q) = &t.question {
                out.push_str(&format!("它的问题：{q}\n"));
            }
            if let Some(sm) = &t.summary {
                out.push_str(&format!("最近汇报：{}\n", clip(sm, 800)));
            }
            if let Some(d) = &t.open_directive {
                out.push_str(&format!("未回执的指令：{d}\n"));
            }
        }
        if all || what.iter().any(|w| w == "diff") {
            let (changes, hash) = futures::executor::block_on(verify::git_state(&t.cwd));
            out.push_str(&format!(
                "\n## git 改动（{}）\n",
                hash.unwrap_or_else(|| "无".into())
            ));
            if changes.is_empty() {
                out.push_str("（干净或不是 git 仓库）\n");
            }
            for c in changes {
                out.push_str(&format!("- {c}\n"));
            }
        }
        if all || what.iter().any(|w| w == "ledger") {
            out.push_str("\n## 最近台账\n");
            let entries = self.shell.assistant.ledger.for_ticket(&t.id, 20);
            if entries.is_empty() {
                out.push_str("（没有）\n");
            }
            for e in entries {
                out.push_str(&format!(
                    "- {} {:?} {:?} {}\n",
                    e.at,
                    e.kind,
                    e.actor,
                    clip(&e.data.to_string(), 300)
                ));
            }
        }
        ToolResult::ok(out)
    }

    async fn verify(&self, args: Value) -> ToolResult {
        let Some(key) = field(&args, "ticket") else {
            return ToolResult::err("supervise_verify: 缺少 ticket");
        };
        let Some(t) = resolve(&self.shell.assistant, &key) else {
            return ToolResult::err(format!(
                "supervise_verify: {}",
                miss(&self.shell.assistant, &key)
            ));
        };
        let checks = self.shell.assistant.read(|s| {
            s.charter_for(&t)
                .map(|c| c.checks.clone())
                .unwrap_or_default()
        });
        let mut runs = t.evidence.clone().unwrap_or_default().checks;
        if !checks.is_empty() {
            runs.extend(verify::rerun(&t.cwd, &checks).await);
        }
        let note = t
            .evidence
            .as_ref()
            .map(|e| e.note.clone())
            .unwrap_or_default();
        let ev = verify::collect(&t.cwd, runs, note, verify::Level::Recorded).await;
        let ok = ev.sufficient() && ev.checks.iter().all(|c| c.ok);
        let _ = self.shell.assistant.with(|s| {
            if let Some(x) = s.ticket_mut(&t.id) {
                x.evidence = Some(ev.clone());
                x.updated_at = now_ms();
            }
        });
        self.shell.assistant.ledger.append(
            &LedgerEntry::new(LedgerKind::Verify, Actor::Supervisor)
                .ticket(t.id.clone())
                .data(json!({ "ok": ok, "evidence": ev })),
        );
        ToolResult::ok(format!(
            "{}证据：{}\n{}",
            if ok { "通过。" } else { "不足。" },
            ev.summary(),
            ev.changes
                .iter()
                .take(10)
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    fn judge(&self, args: Value) -> ToolResult {
        let Some(key) = field(&args, "ticket") else {
            return ToolResult::err("supervise_judge: 缺少 ticket");
        };
        let Some(directive) = field(&args, "directive") else {
            return ToolResult::err("supervise_judge: 缺少 directive");
        };
        let basis = field(&args, "basis").unwrap_or_else(|| "（没写依据）".into());
        let names = strings(&args, "actions");
        if names.is_empty() {
            return ToolResult::err("supervise_judge: actions 至少给一个动作类别");
        }
        let mut actions = Vec::new();
        for n in &names {
            match ActionClass::parse(n) {
                Some(a) => actions.push(a),
                None => {
                    return ToolResult::err(format!(
                        "supervise_judge: 不认识的动作类别 `{n}`。可用：{}",
                        ActionClass::ALL
                            .iter()
                            .map(|a| a.id())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    ));
                }
            }
        }
        let Some(t) = resolve(&self.shell.assistant, &key) else {
            return ToolResult::err(format!(
                "supervise_judge: {}",
                miss(&self.shell.assistant, &key)
            ));
        };
        if t.status.ended() {
            return ToolResult::err(format!(
                "{} 已经结束（{}），不用再下指令",
                t.label(),
                t.status.label()
            ));
        }
        let (mode, charter) = self
            .shell
            .assistant
            .read(|s| (s.settings.supervisor.mode, s.charter_for(&t).cloned()));
        if mode == SuperviseMode::Off {
            return ToolResult::err("supervise_judge: 值守已关闭，不能下指令");
        }
        if t.judgement.rounds >= t.budget.max_judgements.max(1) {
            return ToolResult::err(format!(
                "supervise_judge: 这张工单已经判断 {} 次，达到上限。用 supervise_escalate 交给人。",
                t.judgement.rounds
            ));
        }
        let Some(ch) = charter else {
            return ToolResult::err("supervise_judge: 找不到可用的授权书");
        };
        let now = now_ms();
        for a in &actions {
            if matches!(a.risk(), charter::Risk::Irreversible) {
                return ToolResult::err(format!(
                    "supervise_judge: 「{}」不可逆，值守永远不能自己批 —— 用 supervise_escalate 请示人。",
                    a.label()
                ));
            }
            if !ch.allows(*a, now) {
                return ToolResult::err(format!(
                    "supervise_judge: 「{}」不在授权书「{}」的可自决范围内。用 supervise_escalate 请示人。",
                    a.label(),
                    ch.name
                ));
            }
        }
        let directive_id = format!("dir_{now}_{}", rand_id());
        let text = format!(
            "【值守指令 · {directive_id}】\n{directive}\n\n\
             （依据：{basis}；授权：{}。做完用 assistant_report 汇报，in_reply_to 填 {directive_id}。）",
            ch.name
        );
        if mode == SuperviseMode::Observe {
            let _ = self.shell.assistant.with(|s| {
                if let Some(x) = s.ticket_mut(&t.id) {
                    x.judgement.rounds += 1;
                    x.judgement.last_at = now;
                }
            });
            self.shell.assistant.ledger.append(
                &LedgerEntry::new(LedgerKind::Directive, Actor::Supervisor)
                    .ticket(t.id.clone())
                    .auth(AuthRef::Charter(ch.id.clone()))
                    .data(json!({
                        "proposed": true,
                        "directive": directive,
                        "actions": names,
                        "basis": basis,
                        "charter": ch.name,
                    })),
            );
            return ToolResult::ok(format!(
                "观察模式：已记下建议（未发送）。\n建议：{directive}\n依据：{basis}"
            ));
        }
        if !deliver(&self.shell, &t.id, text, Wake::Supervisor) {
            return ToolResult::err(format!("{} 已经叫停，指令没有送达", t.label()));
        }
        let _ = self.shell.assistant.with(|s| {
            if let Some(x) = s.ticket_mut(&t.id) {
                x.open_directive = Some(directive_id.clone());
                x.judgement.rounds += 1;
                x.judgement.last_at = now;
                x.status = TicketStatus::Queued;
                x.updated_at = now;
            }
        });
        self.shell.assistant.ledger.append(
            &LedgerEntry::new(LedgerKind::Directive, Actor::Supervisor)
                .ticket(t.id.clone())
                .auth(AuthRef::Charter(ch.id.clone()))
                .data(json!({
                    "directive_id": directive_id,
                    "directive": directive,
                    "actions": names,
                    "basis": basis,
                    "charter": ch.name,
                })),
        );
        crate::protocol::now_ms();
        ToolResult::ok(format!(
            "已把指令送进 {}（{directive_id}）。它接下来会按这个做，汇报时带 in_reply_to。",
            t.label()
        ))
    }

    fn escalate(&self, args: Value) -> ToolResult {
        let Some(situation) = field(&args, "situation") else {
            return ToolResult::err("supervise_escalate: 缺少 situation");
        };
        let Some(proposal) = field(&args, "proposal") else {
            return ToolResult::err("supervise_escalate: 缺少 proposal");
        };
        let Some(if_silent) = field(&args, "if_silent") else {
            return ToolResult::err("supervise_escalate: 缺少 if_silent");
        };
        let ticket = field(&args, "ticket").and_then(|k| resolve(&self.shell.assistant, &k));
        let mut options: Vec<Choice> = Vec::new();
        if let Some(list) = args.get("options").and_then(Value::as_array) {
            for (i, o) in list.iter().enumerate() {
                let default_id = ((b'a' + i as u8) as char).to_string();
                let id = o
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or(default_id);
                let label = o
                    .get("label")
                    .and_then(Value::as_str)
                    .or_else(|| o.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let Some(label) = label else {
                    return ToolResult::err("supervise_escalate: 每个选项都要有 label");
                };
                options.push(Choice {
                    id,
                    label,
                    consequence: o
                        .get("consequence")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    risky: o.get("risky").and_then(Value::as_bool).unwrap_or(false),
                    actions: o
                        .get("actions")
                        .and_then(Value::as_array)
                        .map(|v| {
                            v.iter()
                                .filter_map(Value::as_str)
                                .filter_map(ActionClass::parse)
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
        }
        let brief = escalation::Brief {
            situation,
            proposal: proposal.clone(),
            options,
            if_silent,
            refs: ticket.iter().map(|t| t.label()).collect(),
        };
        let topic = field(&args, "topic").unwrap_or_else(|| clip(&proposal, 40));
        let minutes = args
            .get("expires_minutes")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(
                escalation::MIN_TTL_MS / 60_000,
                escalation::MAX_TTL_MS / 60_000,
            );
        let mut e = escalation::Escalation::open(
            ticket
                .as_ref()
                .map(Ticket::label)
                .unwrap_or_else(|| "（无关联工单）".into()),
            ticket.as_ref().map(|t| t.id.clone()),
            brief,
            minutes * 60_000,
        );
        e.topic = topic;
        if e.brief.options.iter().any(|o| o.risky) {
            e.on_expiry = escalation::Expiry::Hold;
        }
        match push_escalation(&self.shell, e.clone()) {
            Ok(()) => ToolResult::ok(format!("已请示用户（{}）。等答复，别自己往下做。", e.id)),
            Err(err) => ToolResult::err(format!("supervise_escalate: {err}")),
        }
    }

    fn close(&self, args: Value) -> ToolResult {
        let Some(key) = field(&args, "ticket") else {
            return ToolResult::err("supervise_close: 缺少 ticket");
        };
        let Some(t) = resolve(&self.shell.assistant, &key) else {
            return ToolResult::err(format!(
                "supervise_close: {}",
                miss(&self.shell.assistant, &key)
            ));
        };
        let Some(verdict) = field(&args, "verdict") else {
            return ToolResult::err("supervise_close: 缺少 verdict");
        };
        let reason = field(&args, "reason").unwrap_or_default();
        let status = match verdict.as_str() {
            "done" => TicketStatus::Done,
            "dropped" => TicketStatus::Dropped,
            other => {
                return ToolResult::err(format!(
                    "supervise_close: verdict 只能是 done / dropped，收到 `{other}`"
                ));
            }
        };
        let now = now_ms();
        let _ = self.shell.assistant.with(|s| {
            if let Some(x) = s.ticket_mut(&t.id) {
                x.status = status;
                x.question = None;
                x.options.clear();
                x.wait = None;
                x.pending.clear();
                x.open_directive = None;
                x.updated_at = now;
            }
        });
        self.shell.assistant.ledger.append(
            &LedgerEntry::new(LedgerKind::Outcome, Actor::Supervisor)
                .ticket(t.id.clone())
                .data(json!({ "verdict": verdict, "reason": reason })),
        );
        ToolResult::ok(format!("{} 已标为{}。", t.label(), status.label()))
    }

    async fn dispatch(&self, args: Value) -> ToolResult {
        let (Some(title), Some(task), Some(cwd), Some(parent_key)) = (
            field(&args, "title"),
            field(&args, "task"),
            field(&args, "cwd"),
            field(&args, "parent"),
        ) else {
            return ToolResult::err("supervise_dispatch: 缺少 title / task / cwd / parent");
        };
        if !std::path::Path::new(&cwd).is_absolute() || !std::path::Path::new(&cwd).is_dir() {
            return ToolResult::err(format!(
                "supervise_dispatch: 目录 {cwd} 不存在（要绝对路径）"
            ));
        }
        let acceptance = strings(&args, "acceptance");
        if acceptance.is_empty() {
            return ToolResult::err("supervise_dispatch: 必须给 acceptance（验收标准）");
        }
        let Some(parent) = resolve(&self.shell.assistant, &parent_key) else {
            return ToolResult::err(format!(
                "supervise_dispatch: {}",
                miss(&self.shell.assistant, &parent_key)
            ));
        };
        let (parent_charter, requested) = self.shell.assistant.read(|s| {
            let c = s.charter_for(&parent).cloned();
            let requested = vec![
                ActionClass::Inspect,
                ActionClass::RunChecks,
                ActionClass::EditCode,
                ActionClass::Retry,
                ActionClass::AdjustPlan,
            ];
            (c, requested)
        });
        let Some(pc) = parent_charter else {
            return ToolResult::err("supervise_dispatch: 父工单没有可用的授权书");
        };
        let may = pc.intersect(&requested, now_ms());
        if may.is_empty() {
            return ToolResult::err("supervise_dispatch: 父授权收窄后什么都不剩，不能派");
        }
        let mut child = charter::Charter::default_for(Scope::Project(cwd.clone()));
        child.name = format!("子工单授权 · {}", parent.label());
        child.may = may;
        child.checks = pc.checks.clone();
        child.granted = Grant::Default;
        let charter_id = child.id.clone();
        let opened = self.shell.assistant.with(|s| {
            s.charters.push(child);
            s.assign_ticket_with(&cwd, &title, &task, acceptance.clone(), Some(charter_id))
        });
        let id = match opened {
            Ok(id) => id,
            Err(e) => return ToolResult::err(format!("supervise_dispatch: 写盘失败：{e}")),
        };
        let label = self
            .shell
            .assistant
            .read(|s| s.ticket(&id).map(Ticket::label))
            .unwrap_or(id.clone());
        self.shell.assistant.ledger.append(
            &LedgerEntry::new(LedgerKind::Directive, Actor::Supervisor)
                .ticket(parent.id.clone())
                .data(json!({ "dispatch": id, "title": title, "acceptance": acceptance })),
        );
        launch_queued(&self.shell).await;
        ToolResult::ok(format!("已派出子工单 {label}。"))
    }

    fn remember(&self, args: Value) -> ToolResult {
        let Some(text) = field(&args, "text") else {
            return ToolResult::err("supervise_remember: 缺少 text");
        };
        let ticket = field(&args, "ticket").and_then(|k| resolve(&self.shell.assistant, &k));
        let now = now_ms();
        let project = ticket.as_ref().map(|t| t.cwd.clone());
        let evidence = ticket.iter().map(|t| t.label()).collect::<Vec<_>>();
        let _ = self.shell.assistant.with(|s| {
            s.memories.push(crate::assistant::memory::Memory {
                id: format!("mem_{now}_s"),
                text: clip(&text, 200),
                source: format!("{} · 值守推断", crate::assistant::memory::day_of(now)),
                audience: crate::assistant::memory::Audience::Worker,
                project,
                // 推断不是授权：置信度封顶 60。
                confidence: 60,
                evidence,
                created_at: now,
                updated_at: now,
            });
        });
        ToolResult::ok(format!("已记下（推断，置信度 60）：{text}"))
    }
}

fn rand_id() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    now_ms().hash(&mut h);
    std::process::id().hash(&mut h);
    format!("{:04x}", h.finish() & 0xffff)
}

/// 默认预算（测试与前台派单共用）。
pub fn default_budget() -> Budget {
    Budget::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::tests::source;

    fn shell(tag: &str) -> (Arc<AppShell>, Arc<AssistantStore>) {
        let dir = std::env::temp_dir().join(format!(
            "carter-decide-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = AssistantStore::open(dir.join("assistant.json"));
        // 暂停：只验证状态变化，不真的开执行会话。
        store.with(|s| s.settings.paused = true).unwrap();
        let shell = Arc::new(
            Arc::into_inner(AppShell::new(carter::config::Config::default()))
                .unwrap()
                .with_assistant(store.clone()),
        );
        (shell, store)
    }

    fn ticket(store: &AssistantStore, status: TicketStatus) -> String {
        store
            .with(|s| {
                let id = s.open_ticket(&source(), "r", "修登录", "改登录逻辑", vec![]);
                s.ticket_mut(&id).unwrap().status = status;
                id
            })
            .unwrap()
    }

    #[tokio::test]
    async fn the_supervisor_cannot_push_without_a_user_grant() {
        let (shell, store) = shell("push");
        let id = ticket(&store, TicketStatus::NeedsDecision);
        store
            .with(|s| s.settings.supervisor.mode = SuperviseMode::Assist)
            .unwrap();
        let tool = SupervisorTool::new(shell, Kind::Judge);
        let r = tool
            .execute(json!({
                "ticket": id,
                "directive": "推一下",
                "actions": ["push"],
                "basis": "用户说过"
            }))
            .await;
        assert!(!r.ok, "{}", r.content);
        assert!(r.content.contains("不可逆"), "{}", r.content);
    }

    #[tokio::test]
    async fn observe_mode_records_but_does_not_send() {
        let (shell, store) = shell("observe");
        let id = ticket(&store, TicketStatus::NeedsDecision);
        let tool = SupervisorTool::new(shell.clone(), Kind::Judge);
        let r = tool
            .execute(json!({
                "ticket": id,
                "directive": "按 A 改",
                "actions": ["edit_code"],
                "basis": "授权内"
            }))
            .await;
        assert!(r.ok, "{}", r.content);
        assert!(r.content.contains("观察模式"));
        let t = store.read(|s| s.ticket(&id).cloned().unwrap());
        assert_eq!(t.status, TicketStatus::NeedsDecision, "没发送就不该改状态");
        assert!(
            shell
                .assistant
                .ledger
                .for_ticket(&id, 10)
                .iter()
                .any(|e| e.kind == LedgerKind::Directive && e.data["proposed"] == true)
        );
    }

    #[tokio::test]
    async fn assist_mode_sends_a_directive_and_counts_a_round() {
        let (shell, store) = shell("assist");
        let id = ticket(&store, TicketStatus::NeedsDecision);
        store
            .with(|s| s.settings.supervisor.mode = SuperviseMode::Assist)
            .unwrap();
        let tool = SupervisorTool::new(shell.clone(), Kind::Judge);
        let r = tool
            .execute(json!({
                "ticket": id,
                "directive": "按 A 改",
                "actions": ["edit_code"],
                "basis": "授权内"
            }))
            .await;
        assert!(r.ok, "{}", r.content);
        let t = store.read(|s| s.ticket(&id).cloned().unwrap());
        assert_eq!(t.status, TicketStatus::Queued);
        assert_eq!(t.judgement.rounds, 1);
        assert!(t.open_directive.is_some());
        assert!(t.pending.iter().any(|p| p.contains("值守指令")));
    }

    #[tokio::test]
    async fn an_escalation_needs_all_four_sections() {
        let (shell, _store) = shell("esc");
        let tool = SupervisorTool::new(shell, Kind::Escalate);
        let r = tool
            .execute(json!({"situation": "x", "proposal": "", "if_silent": "y"}))
            .await;
        assert!(!r.ok);
        let r = tool
            .execute(json!({
                "situation": "改数据前需要决定",
                "proposal": "用 A",
                "if_silent": "搁置",
                "options": [{"id":"a","label":"A"},{"id":"b","label":"B"}]
            }))
            .await;
        assert!(r.ok, "{}", r.content);
    }

    #[tokio::test]
    async fn a_child_charter_is_strictly_narrower() {
        let (shell, store) = shell("child");
        let parent = ticket(&store, TicketStatus::Working);
        let tool = SupervisorTool::new(shell.clone(), Kind::Dispatch);
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let r = tool
            .execute(json!({
                "title": "子任务",
                "task": "做点事",
                "cwd": dir,
                "acceptance": ["能跑"],
                "parent": parent
            }))
            .await;
        assert!(r.ok, "{}", r.content);
        let child = store.read(|s| {
            s.tickets
                .iter()
                .find(|t| t.title == "子任务")
                .cloned()
                .unwrap()
        });
        let ch = store.read(|s| {
            s.charters
                .iter()
                .find(|c| Some(&c.id) == child.charter.as_ref())
                .cloned()
        });
        let ch = ch.unwrap();
        assert!(ch.may.iter().all(|a| a.default_grantable()));
        assert!(!ch.may.contains(&ActionClass::Push));
    }
}
