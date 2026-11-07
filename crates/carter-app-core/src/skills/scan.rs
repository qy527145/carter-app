use super::*;

/// 一次扫描的全部输入。
///
/// 这是把「环境」与「纯逻辑」切开的切面：[`Scan::from_settings_at`] 是**唯一**读环境变量
/// 的地方，其余函数都只吃这个结构体。于是测试可以喂手搓的临时目录，不必改进程级的
/// `CARTER_HOME` —— cargo 并行跑测试，多一个 env 依赖就多一类随机失败。
#[derive(Debug, Clone)]
pub struct Scan {
    /// `<cwd>/.carter/skills`：当前项目自己的技能目录，可写。`None` = 会话不在项目里。
    pub project_enabled: Option<PathBuf>,
    /// `<cwd>/.carter/skills.disabled`：停用的项目技能搬到这里。
    pub project_disabled: Option<PathBuf>,
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
    /// 不带项目上下文的一份扫描（设置页在还没有会话时也能用）。
    pub fn from_settings(s: &AppSettings) -> Self {
        Self::from_settings_at(s, None)
    }

    /// 唯一读环境的地方。`cwd` 用来定位项目级技能目录。
    pub fn from_settings_at(s: &AppSettings, cwd: Option<&Path>) -> Self {
        let local = carter::config::paths::skills_dir();
        let local_disabled = local.with_file_name("skills.disabled");
        let project = cwd.and_then(|c| project_roots(c, &local, &local_disabled));

        let mut externals = s.skill_root_paths();
        if let Some((enabled, disabled)) = &project {
            // 用户把项目目录也手填进「扫描路径」时只留一份 —— 否则它会在列表里
            // 出现两次，一遍算项目、一遍算外部（而且外部那份是只读的，自相矛盾）。
            externals.retain(|r| !same_dir(r, enabled) && !same_dir(r, disabled));
        }

        Self {
            project_disabled: project.as_ref().map(|(_, d)| d.clone()),
            project_enabled: project.map(|(e, _)| e),
            local_disabled,
            local_enabled: local,
            externals,
            ignored: s.ignored_skills.clone(),
        }
    }

    /// 参与并集的根，按优先级。停用侧不参与。
    pub(crate) fn union_roots(&self) -> Vec<(PathBuf, RootKind)> {
        let mut v: Vec<(PathBuf, RootKind)> = Vec::new();
        if let Some(p) = &self.project_enabled {
            v.push((p.clone(), RootKind::Project));
        }
        v.push((self.local_enabled.clone(), RootKind::Local));
        v.extend(
            self.externals
                .iter()
                .map(|p| (p.clone(), RootKind::External)),
        );
        v
    }

    /// 本应用可写的根（项目 + 用户），启用的在前。
    ///
    /// 顺序与 [`Scan::union_roots`] 一致，只多出两个停用侧 —— 停用的技能仍要能
    /// 编辑、删除、重新启用，所以它们也在这里。
    pub(crate) fn writable_roots(&self) -> Vec<(PathBuf, RootKind)> {
        let mut v: Vec<(PathBuf, RootKind)> = Vec::new();
        for (dir, kind) in [
            (self.project_enabled.as_ref(), RootKind::Project),
            (self.project_disabled.as_ref(), RootKind::Project),
            (Some(&self.local_enabled), RootKind::Local),
            (Some(&self.local_disabled), RootKind::Local),
        ] {
            if let Some(d) = dir {
                v.push((d.clone(), kind));
            }
        }
        v
    }

    /// 全部能读的根，按并集优先级，停用侧排在各自的启用侧之后。
    pub(crate) fn readable_roots(&self) -> Vec<(PathBuf, RootKind)> {
        let mut v = self.writable_roots();
        v.extend(
            self.externals
                .iter()
                .map(|p| (p.clone(), RootKind::External)),
        );
        v
    }

    /// 一个路径字符串对应哪个根（含停用侧与外部根）。
    pub(crate) fn root_of(&self, raw: &str) -> Option<(PathBuf, RootKind)> {
        let p = Path::new(raw);
        self.readable_roots()
            .into_iter()
            .find(|(root, _)| same_dir(root, p))
    }

    /// 这个名字在这里是不是「被忽略的外部技能」。
    ///
    /// `kind` 这个参数是这条规则的全部：忽略名单**只作用于外部**，见模块头。
    pub(crate) fn ignored_here(&self, name: &str, kind: RootKind) -> bool {
        kind == RootKind::External && self.ignored.iter().any(|n| n == name)
    }
}

/// 两个路径是不是同一个目录（隔一层 `Path` 而已）。
pub(crate) fn same_dir(a: &Path, b: &Path) -> bool {
    paths::same_path(&a.to_string_lossy(), &b.to_string_lossy())
}

/// 从 `cwd` 往上找最近的 `.carter/skills`（或只有停用侧存在）。
///
/// 往上找而不是只看 cwd：会话常常开在子目录里，而项目根在仓库顶层。找到最近的一个
/// 就停 —— 嵌套项目里「该项目生效」比「所有祖先并集」好解释，也不会有两份同名技能
/// 悄悄竞争。
///
/// **一个都找不到时回落到 `<cwd>/.carter/skills`**：否则「新建项目技能」永远没有落点，
/// 第一份项目技能只能从文件管理器里手搭。创建目录发生在保存那一刻（[`save`]），
/// 现在只是一条「将会用到哪里」的路径。
///
/// 用户自己的 `~/.carter/skills` 被排除掉：它在并集里已经有一席之地，再当项目根
/// 算一遍就会既是「项目」又是「用户」，优先级也变得看 cwd 深浅。家目录本身也不再
/// 往上走 —— 项目不是你的 home。
pub fn project_roots(
    cwd: &Path,
    local_enabled: &Path,
    local_disabled: &Path,
) -> Option<(PathBuf, PathBuf)> {
    let home = paths::home_dir();
    for anc in cwd.ancestors() {
        if home.as_deref() == Some(anc) {
            break;
        }
        let base = anc.join(".carter");
        let enabled = base.join("skills");
        let disabled = base.join("skills.disabled");
        if same_dir(&enabled, local_enabled) || same_dir(&enabled, local_disabled) {
            continue;
        }
        if enabled.is_dir() || disabled.is_dir() {
            return Some((enabled, disabled));
        }
    }

    let base = cwd.join(".carter");
    let enabled = base.join("skills");
    if same_dir(&enabled, local_enabled) || same_dir(&enabled, local_disabled) {
        return None;
    }
    Some((enabled, base.join("skills.disabled")))
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
pub(crate) fn discover_at(root: &Path) -> Vec<SkillMeta> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        // `file_type()` 报的是**链接本身**（符号链接 / Windows 目录联接是
        // 「不是目录」），于是把技能目录软链进来的用户永远看不到它。
        // `is_dir()` 会跟随链接，失效链接自然为 false。
        if !entry.path().is_dir() {
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
pub(crate) fn winners(scan: &Scan) -> Vec<(SkillMeta, PathBuf, RootKind)> {
    let mut out: Vec<(SkillMeta, PathBuf, RootKind)> = Vec::new();
    for (root, kind) in scan.union_roots() {
        for m in discover_at(&root) {
            // 忽略判定必须在「已经见过」判定**之前**：否则一个被忽略的名字会在下一个
            // 根里又冒出来，与「忽略这个名字」的语义对不上。
            if scan.ignored_here(&m.name, kind) {
                continue;
            }
            if out.iter().any(|(seen, _, _)| seen.name == m.name) {
                continue;
            }
            out.push((m, root.clone(), kind));
        }
    }
    out
}

/// 并集里的技能元数据 —— 进系统提示的那一份。
pub fn union_metas(scan: &Scan) -> Vec<SkillMeta> {
    winners(scan).into_iter().map(|(m, _, _)| m).collect()
}

/// 并集里这个技能的正文 —— 也就是模型用 `skill` 工具实际会拿到的那一份。
pub fn find_body(scan: &Scan, name: &str) -> Option<String> {
    let root = winners(scan)
        .into_iter()
        .find(|(m, _, _)| m.name == name)
        .map(|(_, root, _)| root)?;
    load(&root, name).map(|s| s.body)
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillRow {
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    /// 界面上那个开关的状态：本应用的技能看目录在哪一侧，外部技能看忽略名单。
    pub enabled: bool,
    /// [`SkillRow::name`] 那一份 `SKILL.md` 的绝对路径。
    pub path: String,
    /// 这一行来自哪个扫描根（绝对路径）。界面按它分组；写操作也照它定位。
    pub source: String,
    /// 这一行属于哪一类根。界面按它分组与标注。
    pub scope: RootKind,
    /// 外部路径 → `true`。只读：不能编辑、不能删除，开关也只能改忽略名单。
    pub readonly: bool,
    /// 有没有进**并集** —— 也就是模型到底用不用得到它。
    pub effective: bool,
    /// 被同名技能挡住时，指向挡住它的那一份 `SKILL.md`。
    pub shadowed_by: Option<String>,
}

/// 一行技能。`kind` 决定可写性（只有外部根只读）。
pub(crate) fn row(
    winners: &[(SkillMeta, PathBuf, RootKind)],
    dir: &Path,
    m: &SkillMeta,
    enabled: bool,
    kind: RootKind,
) -> SkillRow {
    let effective = winners
        .iter()
        .any(|(w, root, _)| w.name == m.name && root.as_path() == dir);
    SkillRow {
        name: m.name.clone(),
        description: m.description.clone(),
        allowed_tools: m.allowed_tools.clone(),
        enabled,
        path: skill_md(dir, &m.name),
        source: dir.to_string_lossy().to_string(),
        scope: kind,
        readonly: kind == RootKind::External,
        effective,
        // 开关是开的、却没进并集 → 一定是被前面的根里同名的那一个挡住了。
        // 开关本来就关着的（停用 / 外部忽略）不叫遮蔽，那是用户自己关的。
        shadowed_by: if enabled && !effective {
            winners
                .iter()
                .find(|(w, _, _)| w.name == m.name)
                .map(|(_, root, _)| skill_md(root, &m.name))
        } else {
            None
        },
    }
}

pub(crate) fn skill_md(root: &Path, name: &str) -> String {
    root.join(name)
        .join("SKILL.md")
        .to_string_lossy()
        .to_string()
}

/// 列举全部技能：项目 / 本地（各自启用 + 停用）/ 各外部根，**不去重**（见模块头）。
pub fn list(scan: &Scan) -> Vec<SkillRow> {
    let winners = winners(scan);
    let mut rows = Vec::new();
    // 每一类的两个目录都是启用在前，于是「停用」紧跟在同类后面，读起来是成组的。
    // 顺序与并集优先级一致：项目 → 本地 → 外部。
    let mut writable: Vec<(PathBuf, RootKind, bool)> = Vec::new();
    if let Some(p) = &scan.project_enabled {
        writable.push((p.clone(), RootKind::Project, true));
    }
    if let Some(p) = &scan.project_disabled {
        writable.push((p.clone(), RootKind::Project, false));
    }
    writable.push((scan.local_enabled.clone(), RootKind::Local, true));
    writable.push((scan.local_disabled.clone(), RootKind::Local, false));

    for (dir, kind, enabled) in writable {
        for m in discover_at(&dir) {
            rows.push(row(&winners, &dir, &m, enabled, kind));
        }
    }
    for root in &scan.externals {
        for m in discover_at(root) {
            // 外部技能的开关状态就是「有没有被忽略」。
            let enabled = !scan.ignored_here(&m.name, RootKind::External);
            rows.push(row(&winners, root, &m, enabled, RootKind::External));
        }
    }
    rows
}

/// 技能正文（不含 frontmatter）。用于预览与编辑。
///
/// `source` 给了就**只读那一个根**：列表里同名的行可能有两三行（项目 / 用户 / 外部），
/// 编辑器打开的是其中具体一行，按名字猜会把别处的正文显示在这一行里。
///
/// 不给 `source` 时按并集优先级找，但多走一段**停用侧**：停用的技能仍然可读，
/// 编辑弹窗要预览它，而它不在并集里。外部那份与并集同源 —— 被忽略的名字读不到
/// （模型本来也加载不到）。
pub fn read(scan: &Scan, name: &str, source: Option<&str>) -> Result<String, String> {
    if let Some(raw) = source {
        let Some((dir, kind)) = scan.root_of(raw) else {
            return Err(format!("扫描列表里没有 `{raw}`"));
        };
        if scan.ignored_here(name, kind) {
            return Err(format!("技能 `{name}` 已被忽略，先恢复它再编辑"));
        }
        return load(&dir, name)
            .map(|s| s.body)
            .ok_or_else(|| format!("`{}` 里找不到技能 `{name}`", dir.display()));
    }
    for (dir, kind) in scan.readable_roots() {
        if scan.ignored_here(name, kind) {
            continue;
        }
        if let Some(s) = load(&dir, name) {
            return Ok(s.body);
        }
    }
    Err(format!("找不到技能 `{name}`"))
}
