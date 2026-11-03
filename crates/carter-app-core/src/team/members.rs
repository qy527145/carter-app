//! 成员：`.carter/members/<member-id>.json`，一人一个文件。
//!
//! 格式与加入流程见 `docs/team-protocol.md` 第 3 节。
//!
//! ## 为什么不再「身份就是 git 提交里那一串」
//!
//! 上一版不建成员表，任务的 `assignee` 直接存 `名字 <邮箱>`。这在三种情况下会出错：
//!
//! * **一个人两台机器**：公司机器上是工作邮箱、家里是个人邮箱，同一个人变成两个人，
//!   「我的任务」在其中一台上是空的。
//! * **改名**：昵称一改，历史上所有指派都对不上了。
//! * **还没提交过的新同事**：git 历史里没有他，没法在拆任务时就把活派给他。
//!
//! 所以成员是一等实体，有稳定的 `m-` id；git 邮箱只用来认人（「本机是谁」），
//! 一个成员可以挂多个邮箱。引用一律存 id，昵称只是显示用。
//!
//! ## 兼容旧数据
//!
//! 旧任务里的 `assignee` 是 `名字 <邮箱>`。[`Roster::resolve`] 两种都认：`m-` 开头
//! 按 id 查，否则按邮箱匹配。写的时候只写 id（见 [`Roster::canonical`]）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::Identity;

const SCHEMA_VERSION: u32 = 1;
/// 成员目录相对团队根的位置。
pub const MEMBERS_DIR: &str = "members";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    #[serde(default = "kind_member")]
    pub kind: String,
    pub id: String,
    /// 在这个团队里的称呼。
    pub name: String,
    /// 认人用的 git 邮箱。比较时一律小写。
    #[serde(default)]
    pub emails: Vec<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub joined_at: u64,
}

fn schema_version() -> u32 {
    SCHEMA_VERSION
}
fn kind_member() -> String {
    "member".into()
}

/// 成员 id 的前缀与格式。
pub fn valid_member_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("m-") else {
        return false;
    };
    !rest.is_empty()
        && rest.len() <= 64
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn new_member_id() -> String {
    let t = super::model::new_id(super::model::Kind::Task);
    format!("m-{}", t.trim_start_matches("t-"))
}

fn norm(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

impl Member {
    pub fn new(name: &str, email: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind: "member".into(),
            id: new_member_id(),
            name: name.trim().to_string(),
            emails: if email.trim().is_empty() {
                Vec::new()
            } else {
                vec![norm(email)]
            },
            role: String::new(),
            joined_at: crate::protocol::now_ms(),
        }
    }

    pub fn has_email(&self, email: &str) -> bool {
        let e = norm(email);
        !e.is_empty() && self.emails.iter().any(|x| norm(x) == e)
    }
}

/// 一次读出来的全部成员。
#[derive(Debug, Clone, Default)]
pub struct Roster {
    pub members: Vec<Member>,
    /// 读不出来的文件：`(相对路径, 原因)`。
    pub issues: Vec<(String, String)>,
}

impl Roster {
    pub fn by_id(&self, id: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.id == id)
    }

    pub fn by_email(&self, email: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.has_email(email))
    }

    /// 本机（某个 git 身份）在这个团队里是谁。
    pub fn me(&self, identity: &Identity) -> Option<&Member> {
        self.by_email(&identity.email)
    }

    /// 把一个引用解析成成员。认三种写法：成员 id、`名字 <邮箱>`、裸邮箱，
    /// 最后再按昵称精确匹配（大小写不敏感）—— 模型常常只写名字。
    pub fn resolve(&self, raw: &str) -> Option<&Member> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if raw.starts_with("m-") {
            return self.by_id(raw);
        }
        if let Some(id) = Identity::parse(raw) {
            if !id.email.is_empty() {
                if let Some(m) = self.by_email(&id.email) {
                    return Some(m);
                }
            }
        }
        if raw.contains('@') && !raw.contains('<') {
            if let Some(m) = self.by_email(raw) {
                return Some(m);
            }
        }
        let lower = raw.trim_start_matches('@').to_lowercase();
        let mut hits = self
            .members
            .iter()
            .filter(|m| m.name.to_lowercase() == lower);
        let first = hits.next()?;
        // 重名时不猜：指派给错的人比报错更糟。
        if hits.next().is_some() {
            return None;
        }
        Some(first)
    }

    /// 写回时用的规范形式：能认出是谁就写 id，认不出就原样保留（不丢别人的数据）。
    pub fn canonical(&self, raw: &str) -> String {
        self.resolve(raw)
            .map(|m| m.id.clone())
            .unwrap_or_else(|| raw.trim().to_string())
    }

    /// 给人看的名字。认不出时从 `名字 <邮箱>` 里取名字，再不行原样返回。
    pub fn display(&self, raw: &str) -> String {
        if let Some(m) = self.resolve(raw) {
            return m.name.clone();
        }
        Identity::parse(raw)
            .map(|i| i.short().to_string())
            .unwrap_or_else(|| raw.to_string())
    }

    /// 两个引用是不是同一个人（`assignee` 与「本机是谁」比较时用）。
    pub fn same(&self, a: &str, b: &str) -> bool {
        match (self.resolve(a), self.resolve(b)) {
            (Some(x), Some(y)) => x.id == y.id,
            _ => {
                let (x, y) = (Identity::parse(a), Identity::parse(b));
                match (x, y) {
                    (Some(x), Some(y)) if !x.email.is_empty() => norm(&x.email) == norm(&y.email),
                    _ => a.trim() == b.trim(),
                }
            }
        }
    }
}

fn dir(team_root: &Path) -> PathBuf {
    team_root.join(MEMBERS_DIR)
}

fn file(team_root: &Path, id: &str) -> PathBuf {
    dir(team_root).join(format!("{id}.json"))
}

/// 读全部成员。目录不存在就是还没人加入，不是错误。
pub fn load(team_root: &Path) -> Roster {
    let mut out = Roster::default();
    let Ok(entries) = std::fs::read_dir(dir(team_root)) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let rel = format!(
            "{MEMBERS_DIR}/{}",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("")
        );
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|raw| super::store::parse_json::<Member>(&raw));
        match parsed {
            Ok(m) => {
                // 文件名就是 id：对不上说明被手改过，别信它，否则按 id 找人会找错文件。
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                if m.id != stem {
                    out.issues
                        .push((rel, format!("文件里的 id `{}` 与文件名对不上", m.id)));
                } else {
                    out.members.push(m);
                }
            }
            Err(e) => out.issues.push((rel, e)),
        }
    }
    out.members
        .sort_by(|a, b| a.joined_at.cmp(&b.joined_at).then_with(|| a.id.cmp(&b.id)));
    out
}

/// 读全部成员：工作区里的，**加上**其他分支（本地与远端）上已经提交了的。
///
/// 成员文件是状态、跟着分支走。只读工作区的话，张三在 `main` 上加入，李四在
/// `feature/x` 上就不知道有张三 —— @ 不到他，也没法把任务派给他，直到合并。
/// 名单本来就只增不减、一人一个文件，把各分支上的并起来不会有歧义：同一个 id
/// 以工作区里的为准（那是本机最新写下的），其余分支上的按提交时间取最新的一份。
pub fn load_all(store: &super::store::Store) -> Roster {
    let mut out = load(store.root());
    let root = store.project_root();
    let dir = format!("{}/{MEMBERS_DIR}", team_rel(store));
    // 所有分支上碰过成员目录的提交里出现过的成员文件。`--name-only` 列文件名，
    // 每个文件取最新那次提交里的内容。
    let Ok(raw) = super::git::git(
        root,
        &[
            "log",
            "--branches",
            "--remotes",
            "--format=%x1e%H",
            "--name-only",
            "--diff-filter=AM",
            "--",
            &dir,
        ],
    ) else {
        return out;
    };
    let mut seen: std::collections::HashSet<String> =
        out.members.iter().map(|m| m.id.clone()).collect();
    for chunk in raw.split('\u{1e}') {
        let mut lines = chunk.lines().map(str::trim).filter(|l| !l.is_empty());
        let Some(sha) = lines.next() else { continue };
        for path in lines {
            let Some(id) = path
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".json"))
            else {
                continue;
            };
            // `log` 新的在前：第一次见到的就是最新的那份。
            if !valid_member_id(id) || !seen.insert(id.to_string()) {
                continue;
            }
            let Ok(text) = super::git::git(root, &["show", &format!("{sha}:{path}")]) else {
                continue;
            };
            if let Ok(m) = super::store::parse_json::<Member>(&text) {
                if m.id == id {
                    out.members.push(m);
                }
            }
        }
    }
    out.members
        .sort_by(|a, b| a.joined_at.cmp(&b.joined_at).then_with(|| a.id.cmp(&b.id)));
    out
}

/// 原子写一个成员文件。
pub fn save(team_root: &Path, m: &Member) -> Result<(), String> {
    if !valid_member_id(&m.id) {
        return Err(format!("成员 id `{}` 不合法", m.id));
    }
    if m.name.trim().is_empty() {
        return Err("成员的称呼不能为空".into());
    }
    let path = file(team_root, &m.id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
    }
    let mut json = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("改名到 {} 失败：{e}", path.display()))
}

/// 加入方式。
#[derive(Debug, Clone)]
pub enum Join {
    /// 新成员，用这个称呼。
    New { name: String },
    /// 我是已有的某个成员，这是我的另一台机器：把本机邮箱挂到他名下。
    Existing { member_id: String },
}

/// 让本机的 git 身份加入团队。返回加入后的成员，以及「是不是新建的」。
///
/// 已经在名单里（邮箱匹配上）时直接返回那个人，不重复建 —— 界面上多点一次
/// 「加入」不该多出一个同名的成员。
pub fn join(team_root: &Path, identity: &Identity, how: Join) -> Result<(Member, bool), String> {
    if identity.email.trim().is_empty() {
        return Err("git 还没有配置 user.email，没法认出你是谁".into());
    }
    let roster = load(team_root);
    if let Some(m) = roster.me(identity) {
        return Ok((m.clone(), false));
    }
    match how {
        Join::New { name } => {
            let name = name.trim();
            if name.is_empty() {
                return Err("给自己取个称呼".into());
            }
            let m = Member::new(name, &identity.email);
            save(team_root, &m)?;
            Ok((m, true))
        }
        Join::Existing { member_id } => {
            let mut m = roster
                .by_id(member_id.trim())
                .cloned()
                .ok_or(format!("没有成员 {member_id}"))?;
            m.emails.push(norm(&identity.email));
            save(team_root, &m)?;
            Ok((m, false))
        }
    }
}

/// 加入并让别人知道：写成员文件 → **只提交成员目录** → 广播一条 `join`。
///
/// 只提交成员目录、不连带别的：用户工作区里可能有一堆还没打算提交的团队改动
/// （模型刚建了一半的任务），加入这个动作不该替他把那些也提交了。
///
/// 已经在名单里时什么都不做，返回那个成员。
pub fn join_and_announce(
    store: &super::store::Store,
    how: Join,
    via: Option<&str>,
) -> Result<(Member, bool), String> {
    let root = store.project_root();
    let me = super::git::local_identity(root)?;
    let (m, created) = join(store.root(), &me, how)?;
    // 「我是 X 的另一台机器」也改了文件（追加了邮箱），一样要提交。
    let changed = created || load(store.root()).by_id(&m.id).is_some_and(|x| x.has_email(&me.email));
    if changed && super::git::in_repo(root) {
        let rel = team_rel(store);
        let dir = format!("{rel}/{MEMBERS_DIR}");
        super::git::stage_team(root, &dir)?;
        let subject = if created {
            format!("chore(carter): {} 加入团队", m.name)
        } else {
            format!("chore(carter): {} 添加一台机器", m.name)
        };
        super::git::commit_team(root, &dir, &subject)?;
        if created {
            super::events::append(
                root,
                &super::events::Draft {
                    kind: super::events::EventKind::JOIN.into(),
                    from: Some(m.id.clone()),
                    to: vec![super::events::EVERYONE.into()],
                    via: via.map(str::to_string),
                    subject: format!("carter/join {}", m.name),
                    ..Default::default()
                },
            )?;
            let _ = super::events::push(root);
        }
    }
    Ok((m, created))
}

fn team_rel(store: &super::store::Store) -> String {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (repo, team) = (canon(store.project_root()), canon(store.root()));
    team.strip_prefix(&repo)
        .ok()
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| super::store::DIR.to_string())
}

/// 改自己的称呼。
pub fn rename(team_root: &Path, member_id: &str, name: &str) -> Result<Member, String> {
    let roster = load(team_root);
    let mut m = roster
        .by_id(member_id)
        .cloned()
        .ok_or(format!("没有成员 {member_id}"))?;
    m.name = name.trim().to_string();
    save(team_root, &m)?;
    Ok(m)
}

/// 从 git 历史里找出还没进名单的提交者，给「邀请 / 指派给还没加入的人」做候选。
///
/// 只看最近一段（`-n500`）：老到几年前的贡献者多半已经不在团队里了。
pub fn git_candidates(project_root: &Path, roster: &Roster) -> Vec<Identity> {
    let Ok(raw) = super::git::git(project_root, &["log", "-n500", "--format=%an%x1f%ae"]) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in raw.lines() {
        let Some((name, email)) = line.split_once('\u{1f}') else {
            continue;
        };
        let e = norm(email);
        if e.is_empty() || !seen.insert(e.clone()) || roster.by_email(&e).is_some() {
            continue;
        }
        out.push(Identity {
            name: name.trim().to_string(),
            email: e,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn tmp() -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("carter-members-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn id(raw: &str) -> Identity {
        Identity::parse(raw).unwrap()
    }

    #[test]
    fn joining_twice_does_not_create_a_second_member() {
        let root = tmp();
        let me = id("xq <Xu.Qiao@Kotei.com.cn>");
        let (a, new) = join(&root, &me, Join::New { name: "徐侨".into() }).unwrap();
        assert!(new);
        let (b, new) = join(&root, &me, Join::New { name: "别的名字".into() }).unwrap();
        assert!(!new);
        assert_eq!(a.id, b.id);
        assert_eq!(load(&root).members.len(), 1);
    }

    #[test]
    fn a_second_machine_attaches_to_the_existing_member() {
        let root = tmp();
        let (m, _) = join(&root, &id("xq <work@corp>"), Join::New { name: "徐侨".into() }).unwrap();
        join(
            &root,
            &id("wmymz <home@me>"),
            Join::Existing { member_id: m.id.clone() },
        )
        .unwrap();
        let r = load(&root);
        assert_eq!(r.members.len(), 1);
        assert_eq!(r.me(&id("x <HOME@me>")).unwrap().id, m.id, "邮箱大小写不敏感");
        assert_eq!(r.me(&id("x <work@corp>")).unwrap().id, m.id);
    }

    #[test]
    fn references_resolve_in_every_historical_form() {
        let root = tmp();
        let (m, _) = join(&root, &id("张三 <z@x>"), Join::New { name: "小张".into() }).unwrap();
        let r = load(&root);
        for raw in [m.id.as_str(), "张三 <z@x>", "Whoever <Z@X>", "z@x", "小张", "@小张"] {
            assert_eq!(r.resolve(raw).map(|x| x.id.as_str()), Some(m.id.as_str()), "{raw}");
        }
        assert_eq!(r.canonical("张三 <z@x>"), m.id, "写回时一律用 id");
        assert_eq!(r.canonical("外人 <o@x>"), "外人 <o@x>", "认不出的原样保留");
        assert_eq!(r.display("外人 <o@x>"), "外人");
        assert!(r.same(&m.id, "张三 <Z@x>"));
    }

    #[test]
    fn duplicate_names_are_not_guessed() {
        let root = tmp();
        join(&root, &id("a <a@x>"), Join::New { name: "小王".into() }).unwrap();
        join(&root, &id("b <b@x>"), Join::New { name: "小王".into() }).unwrap();
        assert!(load(&root).resolve("小王").is_none(), "重名时不能随便挑一个");
    }

    #[test]
    fn a_file_whose_id_does_not_match_its_name_is_reported() {
        let root = tmp();
        let mut m = Member::new("x", "x@x");
        save(&root, &m).unwrap();
        let path = file(&root, &m.id);
        m.id = "m-other".into();
        std::fs::write(&path, serde_json::to_string(&m).unwrap()).unwrap();
        let r = load(&root);
        assert!(r.members.is_empty());
        assert_eq!(r.issues.len(), 1);
    }
}
