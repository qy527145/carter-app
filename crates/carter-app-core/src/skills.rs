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
//! ## 扫描路径：并集，本地优先
//!
//! 技能不只从 `~/.carter/skills` 来。界面上能加**额外的扫描父目录**（Claude Code 的
//! `~/.claude/skills`、Codex 的 `~/.codex/skills`，或任意自定义路径），加进来之后按
//! **并集**用：系统提示里的技能目录与 `skill` 工具都认这些根。
//!
//! 优先级：本地 `~/.carter/skills` → 外部根（按加入顺序），按名字去重、**先出现者胜**。
//!
//! ## 外部技能为什么只读
//!
//! `~/.claude/skills` 是 Claude Code 自己的目录。本应用对那里的技能做任何写操作
//! （搬目录、删除、覆盖 SKILL.md）都是在别的工具的地盘上动土，而那边可能正指着同一个
//! 文件。所以外部技能只能**看**和**按名字忽略**，界面上的编辑 / 删除按钮直接不出现。
//!
//! 忽略名单（[`AppSettings::ignored_skills`]）**只作用于外部**：本地技能已经有自己的
//! 开关机制（搬目录，文件管理器里看得见），两套混在一起就会出现「文件明明在
//! `~/.carter/skills/pdf/`、技能却不加载」这种隐形状态 —— 正是本模块一直在躲的那类错。
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

/// 一次扫描的全部输入。
///
/// 这是把「环境」与「纯逻辑」切开的切面：[`Scan::from_settings`] 是**唯一**读环境变量
/// 的地方，其余函数都只吃这个结构体。于是测试可以喂手搓的临时目录，不必改进程级的
/// `CARTER_HOME` —— cargo 并行跑测试，多一个 env 依赖就多一类随机失败。
#[derive(Debug, Clone)]
pub struct Scan {
    /// `~/.carter/skills`：本应用自己的目录，接受写操作。
    pub local_enabled: PathBuf,
    /// `~/.carter/skills.disabled`：停用的本地技能搬到这里。
    pub local_disabled: PathBuf,
    /// 额外的扫描根，顺序 = 优先级（靠前的赢）。
    pub externals: Vec<PathBuf>,
    /// 按名字忽略的外部技能。
    pub ignored: Vec<String>,
}

impl Scan {
    /// 唯一读环境的地方。
    pub fn from_settings(s: &AppSettings) -> Self {
        let local = carter::config::paths::skills_dir();
        Self {
            local_disabled: local.with_file_name("skills.disabled"),
            local_enabled: local,
            externals: s.skill_root_paths(),
            ignored: s.ignored_skills.clone(),
        }
    }

    /// 参与并集的根，按优先级。`true` = 外部根。
    fn union_roots(&self) -> Vec<(PathBuf, bool)> {
        let mut v: Vec<(PathBuf, bool)> = vec![(self.local_enabled.clone(), false)];
        v.extend(self.externals.iter().map(|p| (p.clone(), true)));
        v
    }

    /// 这个名字在这里是不是「被忽略的外部技能」。
    ///
    /// `external` 这个参数是这条规则的全部：忽略名单**不作用于本地**，见模块头。
    fn ignored_here(&self, name: &str, external: bool) -> bool {
        external && self.ignored.iter().any(|n| n == name)
    }
}

/// 扫一个根，并把 frontmatter 里那点「格式噪音」抹平。
///
/// 两处修正，都是为了**让模型真的用得上**列出来的技能：
///
/// * **身份取目录名**，不取 frontmatter 里的 `name`。carter 的 `load` 本来就是按目录找的
///   （`<root>/<name>/SKILL.md`），两者不一致时会变成「目录里列着这个名字、`skill` 工具
///   却永远加载不到」—— 而模型会老老实实照着你列出来的名字去调。
/// * **褪掉成对的引号**。Codex 的技能写成 `name: "imagegen"` / `description: "…"`，
///   而 carter 的逐行解析器不去引号，于是名字成了带引号的字面量、描述整段被引号包住，
///   在系统提示里既难看又要模型照抄引号。YAML 的引号是语法，不是内容。
///
/// 其余（`description` 折行、`metadata:` 之类的子块）保持 carter 的行为不变 ——
/// 那些是它的解析规则的一部分，本应用不去替它兜底。
fn discover_at(root: &Path) -> Vec<SkillMeta> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(entry.path().join("SKILL.md")) else {
            continue;
        };
        // 解析不出来就跳过 —— 目录存在但没有（或没有合法）SKILL.md 不是错误，
        // 界面上的「子目录 N / 技能 M」会把这种情况显示出来。
        let Some((mut meta, _)) = parse_skill_md(&raw) else {
            continue;
        };
        meta.name = entry.file_name().to_string_lossy().to_string();
        meta.description = paths::strip_wrapping_quotes(&meta.description).to_string();
        meta.allowed_tools = meta
            .allowed_tools
            .iter()
            .map(|t| paths::strip_wrapping_quotes(t).to_string())
            .collect();
        out.push(meta);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 并集：谁在、各自来自哪个根（按优先级）。
///
/// [`union_metas`] / [`find_body`] / [`list`] / [`roots`] 都走这一个函数 —— 顺序必须
/// **处处一致**，否则会出现「系统提示里是本地那个，点开预览却是外部那个」。
fn winners(scan: &Scan) -> Vec<(SkillMeta, PathBuf)> {
    let mut out: Vec<(SkillMeta, PathBuf)> = Vec::new();
    for (root, external) in scan.union_roots() {
        for m in discover_at(&root) {
            // 忽略判定必须在「已经见过」判定**之前**：否则一个被忽略的名字会在下一个
            // 根里又冒出来，与「忽略这个名字」的语义对不上。
            if scan.ignored_here(&m.name, external) {
                continue;
            }
            if out.iter().any(|(seen, _)| seen.name == m.name) {
                continue;
            }
            out.push((m, root.clone()));
        }
    }
    out
}

/// 并集里的技能元数据 —— 进系统提示的那一份。
pub fn union_metas(scan: &Scan) -> Vec<SkillMeta> {
    winners(scan).into_iter().map(|(m, _)| m).collect()
}

/// 并集里这个技能的正文 —— 也就是模型用 `skill` 工具实际会拿到的那一份。
pub fn find_body(scan: &Scan, name: &str) -> Option<String> {
    let root = winners(scan)
        .into_iter()
        .find(|(m, _)| m.name == name)
        .map(|(_, root)| root)?;
    load(&root, name).map(|s| s.body)
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    /// 界面上那个开关的状态：本地技能看目录在哪一侧，外部技能看忽略名单。
    pub enabled: bool,
    /// [`SkillRow::name`] 那一份 `SKILL.md` 的绝对路径。
    pub path: String,
    /// 这一行来自哪个扫描根（绝对路径）。界面按它分组。
    pub source: String,
    /// 外部路径 → `true`。只读：不能编辑、不能删除，开关也只能改忽略名单。
    pub readonly: bool,
    /// 有没有进**并集** —— 也就是模型到底用不用得到它。
    pub effective: bool,
    /// 被同名技能挡住时，指向挡住它的那一份 `SKILL.md`。
    pub shadowed_by: Option<String>,
}

/// 一行技能。`readonly` 由调用方给（本地两个目录都是可写的）。
fn row(
    winners: &[(SkillMeta, PathBuf)],
    dir: &Path,
    m: &SkillMeta,
    enabled: bool,
    readonly: bool,
) -> SkillRow {
    let effective = winners
        .iter()
        .any(|(w, root)| w.name == m.name && root.as_path() == dir);
    SkillRow {
        name: m.name.clone(),
        description: m.description.clone(),
        allowed_tools: m.allowed_tools.clone(),
        enabled,
        path: skill_md(dir, &m.name),
        source: dir.to_string_lossy().to_string(),
        readonly,
        effective,
        // 开关是开的、却没进并集 → 一定是被前面的根里同名的那一个挡住了。
        // 开关本来就关着的（本地停用 / 外部忽略）不叫遮蔽，那是用户自己关的。
        shadowed_by: if enabled && !effective {
            winners
                .iter()
                .find(|(w, _)| w.name == m.name)
                .map(|(_, root)| skill_md(root, &m.name))
        } else {
            None
        },
    }
}

fn skill_md(root: &Path, name: &str) -> String {
    root.join(name)
        .join("SKILL.md")
        .to_string_lossy()
        .to_string()
}

/// 列举全部技能：本地启用 / 本地停用 / 各外部根，**不去重**（见模块头）。
pub fn list(scan: &Scan) -> Vec<SkillRow> {
    let winners = winners(scan);
    let mut rows = Vec::new();
    // 本地两个目录：启用在前，于是「停用」的紧跟在它的同类后面，读起来是成组的。
    for (dir, enabled) in [(&scan.local_enabled, true), (&scan.local_disabled, false)] {
        for m in discover_at(dir) {
            rows.push(row(&winners, dir, &m, enabled, false));
        }
    }
    for root in &scan.externals {
        for m in discover_at(root) {
            // 外部技能的开关状态就是「有没有被忽略」。
            let enabled = !scan.ignored_here(&m.name, true);
            rows.push(row(&winners, root, &m, enabled, true));
        }
    }
    rows
}

/// 技能正文（不含 frontmatter）。用于预览与编辑。
///
/// 比并集多一层**本地停用目录**：停用的本地技能仍然可读，编辑弹窗要预览它，而它
/// 不在并集里。外部那份与并集同源 —— 被忽略的名字读不到（模型本来也加载不到）。
pub fn read(scan: &Scan, name: &str) -> Result<String, String> {
    for dir in [&scan.local_enabled, &scan.local_disabled] {
        if let Some(s) = load(dir, name) {
            return Ok(s.body);
        }
    }
    for root in &scan.externals {
        if scan.ignored_here(name, true) {
            continue;
        }
        if let Some(s) = load(root, name) {
            return Ok(s.body);
        }
    }
    Err(format!("找不到技能 `{name}`"))
}

/// 这个名字的本地目录在哪一侧（`None` = 本地没有）。
fn local_dir_of(scan: &Scan, name: &str) -> Option<PathBuf> {
    for dir in [&scan.local_enabled, &scan.local_disabled] {
        if dir.join(name).is_dir() {
            return Some(dir.clone());
        }
    }
    None
}

/// 这个名字在哪个外部根里（按优先级取第一个）。忽略名单不参与 —— 删不掉的时候
/// 报错要能指出它在哪，哪怕它已经被忽略了。
fn external_root_of(scan: &Scan, name: &str) -> Option<PathBuf> {
    scan.externals
        .iter()
        .find(|root| discover_at(root).iter().any(|m| m.name == name))
        .cloned()
}

fn move_skill(scan: &Scan, name: &str, to_enabled: bool) -> Result<(), String> {
    let (from, to) = if to_enabled {
        (&scan.local_disabled, &scan.local_enabled)
    } else {
        (&scan.local_enabled, &scan.local_disabled)
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
    std::fs::create_dir_all(to).map_err(|e| format!("建目录失败：{e}"))?;
    std::fs::rename(&src, to.join(name)).map_err(|e| format!("移动技能目录失败：{e}"))
}

/// 启停一个技能。返回 `true` 表示**应用设置被改动了，调用方要落盘**。
///
/// 两套机制只在这一处交汇：
///
/// * 本地那一行（`external = false`）→ 搬目录（现状不变，界面与文件管理器都看得见）；
/// * 外部那一行（`external = true`）→ 记进忽略名单，**文件系统一字不动**。
///
/// 为什么要把「点的是哪一行」传进来，而不是只看名字在不在本地目录里：本地技能**停用**时
/// 外部的同名技能会顶上来，那时列表里两行都有开关。按名字猜的话，点外部那一行会走到
/// 「搬本地目录」那条路，而本地目录此刻正好不在启用侧 —— `move_skill` 认为「已经在目标
/// 那一侧了」直接返回，于是**点了没反应**。名字在这两行里是同一个，能区分它们的只有来源。
///
/// 合并成一个入口而不是给前端两个命令：界面上就是一个开关。
///
/// 注意改的是 `app`，不是 `scan`：忽略名单住在设置里，所以改完还想重算并集的话，
/// 得用 `Scan::from_settings(app)` 重新构造一份（`scan` 里那份是旧的快照）。
pub fn set_enabled(
    app: &mut AppSettings,
    scan: &Scan,
    name: &str,
    enabled: bool,
    external: bool,
) -> Result<bool, String> {
    if !external {
        move_skill(scan, name, enabled)?;
        return Ok(false);
    }
    // 外部那一行：即便本地有同名技能，这次要关的也是外部那份。
    // 按名字忽略只作用于外部路径，所以不会误伤本地的同名技能。
    if external_root_of(scan, name).is_none() {
        return Err(format!("找不到技能 `{name}`"));
    }
    Ok(app.set_skill_ignored(name, !enabled))
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

/// 新建或覆盖一个技能。新技能默认落在启用目录。**只写本地目录** —— 外部路径
/// 一个字节都不碰。
pub fn save(scan: &Scan, d: &SkillDraft) -> Result<(), String> {
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

    // 名字撞上外部路径里已有的技能 → 拒绝。重名时本地优先，于是「新建的技能把
    // Claude Code 的技能顶掉了」这句话在磁盘上并不成立（那边一个字没动），
    // 界面状态与磁盘状态不一致是最难查的一类问题 —— 而报错是可行动的，
    // 编辑器里的名字本来就能改。
    //
    // 例外：那个外部技能**已经被忽略**时不拒绝。它本来就不参与并集，同名不构成竞争，
    // 而且用户已经表达过「不要它」。
    let blocked_by = if local_dir_of(scan, name).is_none() && !scan.ignored_here(name, true) {
        external_root_of(scan, name)
    } else {
        None
    };
    if let Some(root) = blocked_by {
        return Err(format!(
            "外部扫描路径 {} 里已经有一个叫 `{name}` 的技能：重名时本地优先，\
             那样模型用到的就不是你刚写的这一份。换个名字，或者先把它忽略掉。",
            root.display()
        ));
    }

    let text = render(d);
    // 先验证渲染结果能被 carter 解析回来 —— 否则会写下一个「存在但永不加载」
    // 的技能，用户完全看不出问题在哪。
    if parse_skill_md(&text).is_none() {
        return Err("生成的 SKILL.md 无法被解析，请检查名称与描述".into());
    }

    // 已存在的话写在它当前所在的目录，别把它从「停用」意外变成「启用」。
    let dir = local_dir_of(scan, name).unwrap_or_else(|| scan.local_enabled.clone());
    let target = dir.join(name);
    std::fs::create_dir_all(&target).map_err(|e| format!("建目录失败：{e}"))?;
    std::fs::write(target.join("SKILL.md"), text).map_err(|e| format!("写 SKILL.md 失败：{e}"))
}

/// 删除技能（目录整个删掉，不可恢复）。**只删本地的**。
pub fn delete(scan: &Scan, name: &str) -> Result<(), String> {
    for dir in [&scan.local_enabled, &scan.local_disabled] {
        let p = dir.join(name);
        if p.is_dir() {
            return std::fs::remove_dir_all(&p).map_err(|e| format!("删除失败：{e}"));
        }
    }
    // 只存在于外部路径。说清它在哪、以及该怎么办，比一句「找不到技能」有用得多。
    if let Some(root) = external_root_of(scan, name) {
        return Err(format!(
            "`{name}` 来自外部扫描路径 {} —— 那是别的工具自己的目录，本应用不会删除它。\
             不想要的话，关掉它的开关（记进忽略名单），或者把这条扫描路径移除。",
            root.display()
        ));
    }
    Err(format!("找不到技能 `{name}`"))
}

// ---------------------------------------------------------------------------
// 扫描路径本身的管理
// ---------------------------------------------------------------------------

/// 加一条扫描路径。返回规范化后的路径；设置是否落盘由调用方决定。
///
/// 「别把本应用自己的技能目录当外部根加进来」这条判断放在这里而不是
/// [`crate::paths::normalize_root`]：只有这一层知道本地目录是哪些。
pub fn add_root(app: &mut AppSettings, raw: &str) -> Result<PathBuf, String> {
    let p = paths::normalize_root(raw)?;
    let s = p.to_string_lossy().to_string();

    let local = carter::config::paths::skills_dir();
    for special in [&local, &local.with_file_name("skills.disabled")] {
        if paths::same_path(&s, &special.to_string_lossy()) {
            return Err(format!(
                "`{}` 是本应用自己的技能目录，一直都在扫描，不用加。",
                special.display()
            ));
        }
    }
    if app.skill_roots.iter().any(|r| paths::same_path(r, &s)) {
        return Err(format!("`{s}` 已经在扫描列表里了"));
    }

    app.skill_roots.push(s);
    Ok(p)
}

/// 移除一条扫描路径。设置是否落盘由调用方决定。
pub fn remove_root(app: &mut AppSettings, raw: &str) -> Result<(), String> {
    // 移除时不强求规范化成功：列表里的路径本来就是规范化过的，但万一规范化规则
    // 后来改过（或者用户手改了 settings.json），也得能把旧的那条删掉。
    let s = paths::normalize_root(raw)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| raw.trim().to_string());

    let before = app.skill_roots.len();
    app.skill_roots.retain(|r| !paths::same_path(r, &s));
    if app.skill_roots.len() == before {
        return Err(format!("扫描列表里没有 `{s}`"));
    }
    Ok(())
}

/// 一条扫描路径在界面上的样子。
///
/// 这几个计数是「为什么扫到 20 个目录、只列出 8 个技能」的答案，每个数对应一种被排除
/// 的原因，缺一个就解释不清。`subdirs` 专门用来暴露「目录层级不对」这种情形：
/// Codex 的 `~/.codex/skills` 底下只有一个 `.system/`，真正的技能在它的下一层 ——
/// 只加第一条会显示「子目录 1 / 技能 0」，一眼看出是层级问题而不是路径填错。
#[derive(Debug, Clone, Serialize)]
pub struct SkillRootRow {
    pub path: String,
    /// `local` = 本应用自己的目录（不可移除）；`external` = 用户加进来的。
    pub source: String,
    pub exists: bool,
    /// 直接子目录数。
    pub subdirs: usize,
    /// 解析出 `SKILL.md` 的技能数。
    pub found: usize,
    /// 进了并集的数量（模型实际用得到的）。
    pub effective: usize,
    /// 因为同名被前面的根挡住的数量。
    pub shadowed: usize,
    /// 因为忽略名单被排掉的数量。
    pub ignored: usize,
}

/// 扫描根一览：本地目录排第一，然后是外部根（顺序 = 优先级）。
pub fn roots(scan: &Scan) -> Vec<SkillRootRow> {
    let winners = winners(scan);
    let mut out = vec![root_row(scan, &winners, &scan.local_enabled, false)];
    out.extend(
        scan.externals
            .iter()
            .map(|root| root_row(scan, &winners, root, true)),
    );
    out
}

fn root_row(
    scan: &Scan,
    winners: &[(SkillMeta, PathBuf)],
    root: &Path,
    external: bool,
) -> SkillRootRow {
    let metas = discover_at(root);
    let found = metas.len();
    let effective = metas
        .iter()
        .filter(|m| {
            winners
                .iter()
                .any(|(w, r)| w.name == m.name && r.as_path() == root)
        })
        .count();
    let ignored = if external {
        metas
            .iter()
            .filter(|m| scan.ignored_here(&m.name, true))
            .count()
    } else {
        0
    };
    SkillRootRow {
        path: root.to_string_lossy().to_string(),
        source: if external { "external".into() } else { "local".into() },
        exists: root.is_dir(),
        subdirs: count_subdirs(root),
        found,
        effective,
        // 没生效的要么被遮蔽、要么被忽略，剩下的就是被遮蔽的。
        shadowed: found.saturating_sub(effective).saturating_sub(ignored),
        ignored,
    }
}

/// 直接子目录数（含以 `.` 开头的 —— `.system` 那种正是要看见的）。
fn count_subdirs(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .count()
}

/// 快捷选项：别的工具放技能的默认位置。
#[derive(Debug, Clone, Serialize)]
pub struct SkillPreset {
    pub id: String,
    pub label: String,
    /// 这个预设包含的全部路径。
    pub paths: Vec<String>,
    /// 其中**还没加**的那些。界面只加这些，于是重复点也不会撞上「已经在列表里」。
    pub missing: Vec<String>,
    /// 至少有一条路径存在（不存在就只是装的工具不在，不是错误）。
    pub exists: bool,
    pub hint: String,
}

/// 预设列表。没有家目录时返回空 —— 给一个指向当前目录的假路径比什么都不给更坏。
pub fn presets(scan: &Scan) -> Vec<SkillPreset> {
    let Some(home) = paths::home_dir() else {
        return Vec::new();
    };
    let codex = home.join(".codex").join("skills");
    vec![
        preset(
            scan,
            "claude-code",
            "Claude Code",
            vec![home.join(".claude").join("skills")],
            "Claude Code 放个人技能的位置。它自己的目录，本应用只读不写。",
        ),
        preset(
            scan,
            "codex",
            "Codex",
            // 两条一起给：用户技能在 skills/，它自带的在下一层 .system/，而发现只扫一级。
            vec![codex.clone(), codex.join(".system")],
            "Codex 的技能目录。用户技能在 skills/，它自带的在下一层 .system/ —— \
             发现只扫一级，所以两条一起加。",
        ),
    ]
}

fn preset(scan: &Scan, id: &str, label: &str, paths_in: Vec<PathBuf>, hint: &str) -> SkillPreset {
    let paths: Vec<String> = paths_in
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let missing = paths
        .iter()
        .filter(|p| {
            !scan
                .externals
                .iter()
                .any(|r| paths::same_path(&r.to_string_lossy(), p.as_str()))
        })
        .cloned()
        .collect();
    SkillPreset {
        id: id.into(),
        label: label.into(),
        exists: paths_in.iter().any(|p| p.is_dir()),
        paths,
        missing,
        hint: hint.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试一个自己的临时目录。**不碰进程级环境变量** —— 扫描根全是参数传进来的，
    /// 所以不需要 `ENV_LOCK` 那套串行化。
    fn tmp_dir(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "carter-app-skills-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 造一个技能目录：`<root>/<name>/SKILL.md`。
    fn write_skill(root: &Path, name: &str, description: &str) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{description} 的正文\n"),
        )
        .unwrap();
    }

    /// 用临时目录拼一个 `Scan`：`local` 底下自动分出 `skills/` 与 `skills.disabled/`。
    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            Self { dir: tmp_dir(tag) }
        }
        fn local(&self) -> PathBuf {
            self.dir.join("skills")
        }
        fn disabled(&self) -> PathBuf {
            self.dir.join("skills.disabled")
        }
        fn external(&self, tag: &str) -> PathBuf {
            let d = self.dir.join(tag);
            std::fs::create_dir_all(&d).unwrap();
            d
        }
        fn scan(&self, externals: &[&Path], ignored: &[&str]) -> Scan {
            std::fs::create_dir_all(self.local()).unwrap();
            Scan {
                local_enabled: self.local(),
                local_disabled: self.disabled(),
                externals: externals.iter().map(|p| p.to_path_buf()).collect(),
                ignored: ignored.iter().map(|s| s.to_string()).collect(),
            }
        }
        fn cleanup(&self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn names(metas: &[SkillMeta]) -> Vec<String> {
        metas.iter().map(|m| m.name.clone()).collect()
    }

    /// 拿一份改过设置之后的 `Scan` —— 等价于 `Scan::from_settings`，只是扫描根由
    /// 测试指定（那个函数读的是进程级环境）。
    fn rescan(f: &Fixture, externals: &[&Path], app: &AppSettings) -> Scan {
        let mut s = f.scan(externals, &[]);
        s.ignored = app.ignored_skills.clone();
        s
    }

    /// 并集要按名去重，且**先出现的根赢**。
    #[test]
    fn the_union_is_deduped_and_the_first_root_wins() {
        let f = Fixture::new("union");
        let ext = f.external("ext");
        write_skill(&f.local(), "pdf", "本地的");
        write_skill(&ext, "pdf", "外部的");
        write_skill(&ext, "excel", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let metas = union_metas(&scan);
        assert_eq!(
            names(&metas),
            vec!["pdf", "excel"],
            "顺序 = 优先级（本地在前、外部按加入顺序），不是按名字排 —— 界面上的\
             「谁生效」正是照这个顺序读的"
        );
        let pdf = metas.iter().find(|m| m.name == "pdf").unwrap();
        assert_eq!(pdf.description, "本地的", "同名的取本地那份");
        assert_eq!(
            find_body(&scan, "pdf").unwrap().trim(),
            "本地的 的正文",
            "正文必须与系统提示里的那一份同源"
        );
        f.cleanup();
    }

    /// 忽略名单只作用于外部：同名在本地时，本地照样进并集。
    #[test]
    fn the_ignore_list_never_touches_local_skills() {
        let f = Fixture::new("ignore-local");
        let ext = f.external("ext");
        write_skill(&f.local(), "pdf", "本地的");
        write_skill(&ext, "pdf", "外部的");

        let scan = f.scan(&[&ext], &["pdf"]);
        assert_eq!(
            names(&union_metas(&scan)),
            vec!["pdf"],
            "本地那份必须留着 —— 忽略名单管不到本地技能"
        );
        f.cleanup();
    }

    /// 被忽略的外部技能不进并集（也不该在下一个根里又冒出来）。
    #[test]
    fn ignored_external_skills_never_enter_the_union() {
        let f = Fixture::new("ignore-ext");
        let a = f.external("a");
        let b = f.external("b");
        write_skill(&a, "pdf", "a 的");
        write_skill(&b, "pdf", "b 的");

        let scan = f.scan(&[&a, &b], &["pdf"]);
        assert!(union_metas(&scan).is_empty(), "两个根都该被忽略掉");
        assert!(find_body(&scan, "pdf").is_none());
        f.cleanup();
    }

    /// 停用的本地技能不参与并集 —— 它是「用户自己关的」，不是被遮蔽。
    #[test]
    fn a_stopped_local_skill_is_out_of_the_union_and_of_no_competition() {
        let f = Fixture::new("stopped");
        let ext = f.external("ext");
        std::fs::create_dir_all(f.disabled()).unwrap();
        write_skill(&f.disabled(), "pdf", "停用的本地");
        write_skill(&ext, "pdf", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let metas = union_metas(&scan);
        assert_eq!(names(&metas), vec!["pdf"]);
        assert_eq!(
            metas[0].description, "外部的",
            "本地那份停用了，外部的就顶上来了 —— 并集的必然结果"
        );

        let rows = list(&scan);
        let stopped = rows
            .iter()
            .find(|r| !r.readonly && r.name == "pdf")
            .expect("停用的本地技能也要列出来");
        assert!(!stopped.enabled && !stopped.effective);
        assert!(stopped.shadowed_by.is_none(), "不是被遮蔽，是自己关的");
        let winning = rows.iter().find(|r| r.readonly && r.name == "pdf").unwrap();
        assert!(winning.effective, "外部的这一份生效");
        f.cleanup();
    }

    /// 列表**不去重**：被遮蔽的那一行必须还在，并且指出是谁遮蔽的。
    #[test]
    fn the_list_keeps_shadowed_rows_and_says_who_shadowed_them() {
        let f = Fixture::new("shadow");
        let ext = f.external("ext");
        write_skill(&f.local(), "pdf", "本地的");
        write_skill(&ext, "pdf", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let rows = list(&scan);
        assert_eq!(rows.len(), 2, "同名两行都要在，界面上才解释得清");
        let local = rows.iter().find(|r| !r.readonly).unwrap();
        assert!(local.effective && !local.readonly, "本地那份才生效");
        let shadowed = rows.iter().find(|r| r.readonly).unwrap();
        assert!(!shadowed.effective);
        assert!(
            shadowed.shadowed_by.as_deref().unwrap().contains("pdf"),
            "要指出挡住它的是哪一份：{:?}",
            shadowed.shadowed_by
        );
        f.cleanup();
    }

    /// 外部技能一律只读；本地的可写。
    #[test]
    fn external_rows_are_readonly_and_local_rows_are_not() {
        let f = Fixture::new("readonly");
        let ext = f.external("ext");
        write_skill(&f.local(), "pdf", "本地的");
        write_skill(&ext, "excel", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let rows = list(&scan);
        let local = rows.iter().find(|r| r.name == "pdf").unwrap();
        assert!(!local.readonly && local.enabled && local.effective);
        let external = rows.iter().find(|r| r.name == "excel").unwrap();
        assert!(external.readonly && external.enabled && external.effective);
        f.cleanup();
    }

    /// 停用的本地技能仍要能读 —— 编辑弹窗要预览它。
    #[test]
    fn a_stopped_local_skill_can_still_be_read() {
        let f = Fixture::new("read-stopped");
        std::fs::create_dir_all(f.disabled()).unwrap();
        write_skill(&f.disabled(), "pdf", "停用的本地");

        let scan = f.scan(&[], &[]);
        assert!(union_metas(&scan).is_empty(), "不进并集");
        assert_eq!(read(&scan, "pdf").unwrap().trim(), "停用的本地 的正文");
        f.cleanup();
    }

    /// 名字撞上外部已有的技能 → 拒绝，且**外部目录里不能多出任何东西**。
    #[test]
    fn save_refuses_a_name_that_collides_with_an_external_skill() {
        let f = Fixture::new("collide");
        let ext = f.external("ext");
        write_skill(&ext, "pdf", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let before = std::fs::read_dir(&ext).unwrap().flatten().count();
        let err = save(
            &scan,
            &SkillDraft {
                name: "pdf".into(),
                description: "新的".into(),
                allowed_tools: vec![],
                body: "正文".into(),
            },
        )
        .expect_err("重名应当被拒绝");
        assert!(err.contains("pdf") && err.contains("忽略"), "{err}");
        assert!(
            !f.local().join("pdf").exists(),
            "拒绝之后本地也不该留下目录"
        );
        assert_eq!(
            std::fs::read_dir(&ext).unwrap().flatten().count(),
            before,
            "外部目录一个字都不能变"
        );
        f.cleanup();
    }

    /// 外面那个同名技能已经被忽略时，本地新建是允许的 —— 它不构成竞争。
    #[test]
    fn save_allows_a_name_whose_external_twin_is_ignored() {
        let f = Fixture::new("collide-ignored");
        let ext = f.external("ext");
        write_skill(&ext, "pdf", "外部的");

        let scan = f.scan(&[&ext], &["pdf"]);
        save(
            &scan,
            &SkillDraft {
                name: "pdf".into(),
                description: "新的".into(),
                allowed_tools: vec![],
                body: "正文".into(),
            },
        )
        .expect("被忽略的外部同名不该挡住本地新建");
        assert_eq!(names(&union_metas(&scan)), vec!["pdf"]);
        assert_eq!(find_body(&scan, "pdf").unwrap().trim(), "正文");
        f.cleanup();
    }

    /// 只存在于外部的技能删不掉，而且要说清它在哪。
    #[test]
    fn delete_refuses_for_external_only_skills() {
        let f = Fixture::new("delete");
        let ext = f.external("ext");
        write_skill(&ext, "excel", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let err = delete(&scan, "excel").expect_err("外部技能不该被删");
        assert!(err.contains("excel") && err.contains("忽略"), "{err}");
        assert!(ext.join("excel").is_dir(), "外部目录必须原样留着");
        f.cleanup();
    }

    /// 外部技能的启停只改忽略名单，**文件系统一字不动**。
    #[test]
    fn toggling_an_external_skill_only_writes_the_ignore_list() {
        let f = Fixture::new("toggle-ext");
        let ext = f.external("ext");
        write_skill(&ext, "excel", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let mut app = AppSettings::default();

        assert!(
            set_enabled(&mut app, &scan, "excel", false, true).unwrap(),
            "改了忽略名单 → 调用方要落盘"
        );
        assert!(app.skill_ignored("excel"));
        assert!(ext.join("excel").is_dir(), "外部目录必须原样留着");
        // 忽略名单住在 app 里，所以并集要重新构一份 Scan 才算得对 ——
        // 这正是 `set_enabled` 的文档里那句提醒。
        assert!(union_metas(&rescan(&f, &[&ext], &app)).is_empty());

        assert!(set_enabled(&mut app, &scan, "excel", true, true).unwrap());
        assert!(!app.skill_ignored("excel"));
        assert_eq!(names(&union_metas(&rescan(&f, &[&ext], &app))), vec!["excel"]);
        f.cleanup();
    }

    /// 本地技能走搬目录那条路，设置不动。
    #[test]
    fn toggling_a_local_skill_moves_the_directory_and_not_the_settings() {
        let f = Fixture::new("toggle-local");
        write_skill(&f.local(), "pdf", "本地的");

        let scan = f.scan(&[], &[]);
        let mut app = AppSettings::default();

        assert!(!set_enabled(&mut app, &scan, "pdf", false, false).unwrap());
        assert!(f.disabled().join("pdf").is_dir());
        assert!(app.ignored_skills.is_empty(), "本地技能不走忽略名单");

        assert!(!set_enabled(&mut app, &scan, "pdf", true, false).unwrap());
        assert!(f.local().join("pdf").is_dir());
        f.cleanup();
    }

    /// 本地技能**停用**时，外部的同名技能会顶上来，这时列表里两行都有开关。
    /// 点了外部那一行的开关必须动忽略名单 —— 按名字猜「先看本地在不在」的话，会走到
    /// 搬本地目录那条路，而本地目录此刻正好不在启用侧，于是**点了没反应**。
    #[test]
    fn toggling_the_external_row_of_a_duplicated_name_ignores_it_and_leaves_the_local_one_alone() {
        let f = Fixture::new("dup-name");
        let ext = f.external("ext");
        std::fs::create_dir_all(f.disabled()).unwrap();
        write_skill(&f.disabled(), "pdf", "停用的本地");
        write_skill(&ext, "pdf", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let mut app = AppSettings::default();

        // 外部那一行（readonly=true）→ 忽略名单。
        assert!(set_enabled(&mut app, &scan, "pdf", false, true).unwrap());
        assert!(app.skill_ignored("pdf"));
        assert!(
            f.disabled().join("pdf").is_dir(),
            "本地那份是停用的，不该被这次点击搬到任何地方"
        );
        assert!(!f.local().join("pdf").is_dir(), "尤其不该被启用起来");
        assert!(
            union_metas(&rescan(&f, &[&ext], &app)).is_empty(),
            "两边都不该生效了"
        );

        // 本地那一行 → 搬目录，与忽略名单无关。
        assert!(!set_enabled(&mut app, &scan, "pdf", true, false).unwrap());
        assert!(f.local().join("pdf").is_dir());
        assert!(app.skill_ignored("pdf"), "点本地那一行不该动忽略名单");
        f.cleanup();
    }

    /// 计数要能解释「扫到 N 个只列出 M 个」。
    #[test]
    fn root_rows_explain_the_exclusions() {
        let f = Fixture::new("roots");
        let ext = f.external("ext");
        write_skill(&f.local(), "pdf", "本地的");
        write_skill(&ext, "pdf", "外部的"); // 被本地遮蔽
        write_skill(&ext, "excel", "外部的");
        write_skill(&ext, "slides", "外部的");
        // 一个「扫得到目录、但解析不出技能」的目录 —— Codex 的 .system 那种层级问题。
        std::fs::create_dir_all(ext.join(".system")).unwrap();

        let scan = f.scan(&[&ext], &["slides"]);
        let rows = roots(&scan);
        assert_eq!(rows.len(), 2, "本地目录排第一，然后是外部根");
        assert_eq!(rows[0].source, "local");
        assert!(rows[0].exists && rows[0].found == 1 && rows[0].effective == 1);
        assert_eq!(rows[0].subdirs, 1);

        let ext_row = &rows[1];
        assert_eq!(ext_row.source, "external");
        assert_eq!(ext_row.found, 3);
        assert_eq!(ext_row.effective, 1, "只有 excel 进了并集");
        assert_eq!(ext_row.shadowed, 1, "pdf 被本地那个挡住");
        assert_eq!(ext_row.ignored, 1, "slides 在忽略名单里");
        assert_eq!(ext_row.subdirs, 4, ".system 也要算进子目录数");
        f.cleanup();
    }

    /// 不存在的扫描根不是错误，只是扫不到东西。
    #[test]
    fn a_missing_external_root_is_harmless() {
        let f = Fixture::new("missing");
        let scan = f.scan(&[Path::new("C:\\definitely\\not\\here")], &[]);
        assert!(union_metas(&scan).is_empty());
        assert_eq!(list(&scan).len(), 0);
        let rows = roots(&scan);
        assert_eq!(rows.len(), 2);
        assert!(!rows[1].exists && rows[1].subdirs == 0 && rows[1].found == 0);
        f.cleanup();
    }

    /// 加扫描路径：`~` 展开、去重、拒绝相对路径、拒绝把本地目录当外部根。
    #[test]
    fn adding_a_root_normalizes_dedupes_and_refuses_the_local_dir() {
        let mut app = AppSettings::default();
        let dir = std::env::temp_dir();
        let raw = dir.to_string_lossy().to_string();

        let added = add_root(&mut app, &format!("{raw}/")).unwrap();
        assert_eq!(added, dir, "尾部分隔符要去掉");
        assert_eq!(app.skill_roots.len(), 1);

        // 同一个目录换个写法不该能加第二次 —— 多余的那条同时具备「重复」和
        // 「优先顺序」两种含义，混起来就没法解释了。
        assert!(
            add_root(&mut app, &raw).is_err(),
            "重复加要被拒（尾部分隔符的差别不算新路径）"
        );
        assert_eq!(app.skill_roots.len(), 1);
        #[cfg(windows)]
        assert!(
            add_root(&mut app, &raw.to_uppercase()).is_err(),
            "Windows 上大小写不同也是同一个目录"
        );

        assert!(
            add_root(&mut app, ".claude/skills").is_err(),
            "相对路径要被拒"
        );

        // 本应用自己的技能目录恒在扫描范围里，不该作为外部根再加一遍。
        let local = carter::config::paths::skills_dir();
        let err = add_root(&mut app, &local.to_string_lossy()).expect_err("本地目录要被拒");
        assert!(err.contains("本应用自己的技能目录"), "{err}");
    }

    /// 移除扫描路径：认得出同一个目录的不同写法，不在列表里就报错。
    #[test]
    fn removing_a_root_matches_the_same_path_written_differently() {
        let mut app = AppSettings::default();
        let dir = std::env::temp_dir();
        let raw = dir.to_string_lossy().to_string();
        add_root(&mut app, &raw).unwrap();

        remove_root(&mut app, &format!("{raw}/")).unwrap();
        assert!(app.skill_roots.is_empty());
        assert!(remove_root(&mut app, &raw).is_err(), "已经没了就该报错");
    }

    /// 预设给的是两条 Codex 路径，`missing` 只报没加过的那条。
    #[test]
    fn presets_report_only_the_paths_that_are_missing() {
        let f = Fixture::new("presets");
        let Some(home) = paths::home_dir() else {
            f.cleanup();
            return;
        };
        let codex = home.join(".codex").join("skills");
        let scan = f.scan(&[&codex], &[]);

        let all = presets(&scan);
        let codex_preset = all.iter().find(|p| p.id == "codex").unwrap();
        assert_eq!(codex_preset.paths.len(), 2, "用户技能 + 内置技能两条");
        assert_eq!(
            codex_preset.missing,
            vec![codex
                .join(".system")
                .to_string_lossy()
                .to_string()],
            "已经加过的那条不该再出现在 missing 里"
        );
        assert!(codex_preset.hint.contains(".system"), "要说清为什么是两条");

        let claude = all.iter().find(|p| p.id == "claude-code").unwrap();
        assert_eq!(claude.missing.len(), 1);
        f.cleanup();
    }

    /// 加的路径必须是绝对路径 —— 这里是给「加扫描路径」这条链路留的回归钉子：
    /// 一旦有人把 `normalize_root` 换成「按当前目录拼一下」，这条会先红。
    #[test]
    fn a_tilde_root_round_trips_through_the_settings_list() {
        let mut app = AppSettings::default();
        let Some(home) = paths::home_dir() else {
            return;
        };
        add_root(&mut app, "~/.claude/skills").unwrap();
        assert_eq!(
            app.skill_roots,
            vec![home
                .join(".claude")
                .join("skills")
                .to_string_lossy()
                .to_string()],
            "存的是展开后的绝对路径，不是 `~` 原文 —— 这样读的每一处都不必再记得展开一次"
        );
    }

    /// Codex 那边的 frontmatter 写成 `name: "imagegen"` / `description: "…"`，
    /// 而 carter 的逐行解析器不去引号。不处理的话：名字成了带引号的字面量，
    /// 而 `skill` 工具是按**目录名**找文件的 —— 那些技能会「列得出来、永远加载不到」。
    #[test]
    fn quoted_frontmatter_is_cleaned_up_so_the_skills_are_actually_loadable() {
        let f = Fixture::new("quotes");
        let ext = f.external("ext");
        let d = ext.join("imagegen");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            "---\nname: \"imagegen\"\ndescription: \"画图用的\"\nallowed-tools: \"read_file\", \"bash\"\n---\n正文\n",
        )
        .unwrap();

        let scan = f.scan(&[&ext], &[]);
        let metas = union_metas(&scan);
        assert_eq!(names(&metas), vec!["imagegen"], "身份取目录名，不带引号");
        assert_eq!(metas[0].description, "画图用的", "描述上的引号是 YAML 语法");
        assert_eq!(metas[0].allowed_tools, vec!["read_file", "bash"]);
        assert!(
            find_body(&scan, "imagegen").is_some(),
            "按目录名必须加载得到 —— 这正是模型会照抄的那个名字"
        );
        f.cleanup();
    }

    /// frontmatter 的 `name` 与目录名不一致时，**以目录名为准**：carter 的 `load`
    /// 本来就是按目录找的，取 frontmatter 那个名字只会得到「列出来但加载不到」。
    #[test]
    fn the_directory_name_wins_over_a_mismatched_frontmatter_name() {
        let f = Fixture::new("mismatch");
        let ext = f.external("ext");
        let d = ext.join("real-dir");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            "---\nname: something-else\ndescription: x\n---\n正文\n",
        )
        .unwrap();

        let scan = f.scan(&[&ext], &[]);
        assert_eq!(names(&union_metas(&scan)), vec!["real-dir"]);
        assert!(find_body(&scan, "real-dir").is_some());
        assert!(find_body(&scan, "something-else").is_none());
        // 启停也照这个名字走，否则开关会「点了没反应」。
        let mut app = AppSettings::default();
        assert!(set_enabled(&mut app, &scan, "real-dir", false, true).unwrap());
        assert!(app.skill_ignored("real-dir"));
        f.cleanup();
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
        let f = Fixture::new("bad-names");
        let scan = f.scan(&[], &[]);
        for bad in ["../evil", "a/b", "a b", ""] {
            let mut d = draft(bad);
            d.name = bad.into();
            assert!(save(&scan, &d).is_err(), "`{bad}` 应当被拒绝");
        }
        f.cleanup();
    }

    fn draft(name: &str) -> SkillDraft {
        SkillDraft {
            name: name.into(),
            description: "把 PDF 拆成文本".into(),
            allowed_tools: vec!["read_file".into(), "bash".into()],
            body: "# 用法\n\n先 read_file，再 bash。".into(),
        }
    }
}
