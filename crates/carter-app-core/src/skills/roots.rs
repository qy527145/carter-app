use super::*;

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
    /// `project` = 跟着会话走（自动发现，不可移除）；`local` = 本应用自己的用户目录
    /// （不可移除）；`external` = 用户加进来的。
    pub source: RootKind,
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

/// 扫描根一览：项目 → 用户 → 外部根（顺序 = 优先级）。
pub fn roots(scan: &Scan) -> Vec<SkillRootRow> {
    let winners = winners(scan);
    let mut out = Vec::new();
    if let Some(project) = &scan.project_enabled {
        out.push(root_row(scan, &winners, project, RootKind::Project));
    }
    out.push(root_row(
        scan,
        &winners,
        &scan.local_enabled,
        RootKind::Local,
    ));
    out.extend(
        scan.externals
            .iter()
            .map(|root| root_row(scan, &winners, root, RootKind::External)),
    );
    out
}

pub(crate) fn root_row(
    scan: &Scan,
    winners: &[(SkillMeta, PathBuf, RootKind)],
    root: &Path,
    kind: RootKind,
) -> SkillRootRow {
    let metas = discover_at(root);
    let found = metas.len();
    let effective = metas
        .iter()
        .filter(|m| {
            winners
                .iter()
                .any(|(w, r, _)| w.name == m.name && r.as_path() == root)
        })
        .count();
    let ignored = metas
        .iter()
        .filter(|m| scan.ignored_here(&m.name, kind))
        .count();
    SkillRootRow {
        path: root.to_string_lossy().to_string(),
        source: kind,
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
pub(crate) fn count_subdirs(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        // 与 `discover_at` 同一口径：跟随符号链接 / 目录联接，
        // 否则「子目录 N」会比技能数少，看着像扫描出了错。
        .filter(|e| e.path().is_dir())
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

pub(crate) fn preset(
    scan: &Scan,
    id: &str,
    label: &str,
    paths_in: Vec<PathBuf>,
    hint: &str,
) -> SkillPreset {
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
