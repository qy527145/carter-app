use super::*;

/// 这个名字在哪个可写目录里（项目或用户，启用侧优先）。
pub(crate) fn writable_dir_of(scan: &Scan, name: &str) -> Option<(PathBuf, RootKind)> {
    scan.writable_roots()
        .into_iter()
        .find(|(dir, _)| dir.join(name).is_dir())
}

/// 这个名字在哪个外部根里（按优先级取第一个）。忽略名单不参与 —— 删不掉的时候
/// 报错要能指出它在哪，哪怕它已经被忽略了。
pub(crate) fn external_root_of(scan: &Scan, name: &str) -> Option<PathBuf> {
    scan.externals
        .iter()
        .find(|root| discover_at(root).iter().any(|m| m.name == name))
        .cloned()
}

/// 搬目录启停。项目与用户各有一对目录，形状完全一样。
pub(crate) fn move_skill(
    scan: &Scan,
    name: &str,
    to_enabled: bool,
    kind: RootKind,
) -> Result<(), String> {
    let (enabled, disabled) = match kind {
        RootKind::Project => (
            scan.project_enabled
                .as_ref()
                .ok_or_else(|| "当前会话不在项目里".to_string())?,
            scan.project_disabled
                .as_ref()
                .ok_or_else(|| "当前会话不在项目里".to_string())?,
        ),
        RootKind::Local => (&scan.local_enabled, &scan.local_disabled),
        RootKind::External => {
            return Err("外部路径里的技能不能搬目录，只能按名字忽略".into());
        }
    };
    let (from, to) = if to_enabled {
        (disabled, enabled)
    } else {
        (enabled, disabled)
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
/// * 项目级 / 用户级那一行 → 搬目录（界面与文件管理器都看得见）；
/// * 外部那一行 → 记进忽略名单，**文件系统一字不动**。
///
/// 为什么要把「点的是哪一行」传进来，而不是只看名字在不在可写目录里：同一个名字可能
/// 同时在项目、用户、外部三个根里出现，那时列表里就有三行开关。按名字猜的话，点外部
/// 那一行会走到「搬本地目录」那条路，而本地目录此刻正好不在启用侧 —— `move_skill`
/// 认为「已经在目标那一侧了」直接返回，于是**点了没反应**。能区分它们的只有来源。
///
/// `source = None` 只有一种用法：从「已忽略」那一行恢复一个名字 —— 那个技能可能已经
/// 不在任何扫描根里了（目录被删了），界面上没有来源路径可给。
///
/// 合并成一个入口而不是给前端两个命令：界面上就是一个开关。
///
/// 注意改的是 `app`，不是 `scan`：忽略名单住在设置里，所以改完还想重算并集的话，
/// 得用 `Scan::from_settings_at(app, cwd)` 重新构造一份（`scan` 里那份是旧的快照）。
pub fn set_enabled(
    app: &mut AppSettings,
    scan: &Scan,
    name: &str,
    enabled: bool,
    source: Option<&str>,
) -> Result<bool, String> {
    let Some(raw) = source else {
        return Ok(app.set_skill_ignored(name, !enabled));
    };
    let Some((dir, kind)) = scan.root_of(raw) else {
        return Err(format!("扫描列表里没有 `{raw}`"));
    };
    match kind {
        // 外部那一行：即便项目 / 用户里有同名技能，这次要关的也是外部那份。
        // 按名字忽略只作用于外部路径，所以不会误伤别处的同名技能。
        RootKind::External => {
            if !dir.join(name).is_dir() {
                return Err(format!("找不到技能 `{name}`"));
            }
            Ok(app.set_skill_ignored(name, !enabled))
        }
        _ => {
            move_skill(scan, name, enabled, kind)?;
            Ok(false)
        }
    }
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
    /// 写到哪个根（列表里那一行的 `source`）。
    ///
    /// `None` = 按名字找它现在所在的目录；新技能回落用户目录。编辑停用列表里的技能时
    /// 前端会把它的来源原样带回来，于是「改一个字」不会顺手把它变成启用。
    #[serde(default)]
    pub source: Option<String>,
}

/// 把草稿渲染成 `SKILL.md`。
///
/// 用 carter 手写 frontmatter 解析器认的那三个键，写法照它的解析规则来：
/// 键值一行一个，`allowed-tools` 用逗号分隔。
pub(crate) fn render(d: &SkillDraft) -> String {
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

/// 新建或覆盖一个技能。**只写项目级 / 用户级**，外部路径一个字节都不碰。
///
/// 落点由 [`SkillDraft::source`] 决定；它缺省时按名字找技能现在所在的目录，新技能
/// 落用户目录。返回实际写入的根，供调用方在界面上回显（「已保存到项目」）。
pub fn save(scan: &Scan, d: &SkillDraft) -> Result<PathBuf, String> {
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

    let (dir, kind) = match &d.source {
        Some(raw) => {
            let Some((dir, kind)) = scan.root_of(raw) else {
                return Err(format!("扫描列表里没有 `{raw}`"));
            };
            if kind == RootKind::External {
                return Err(format!(
                    "`{}` 是外部扫描路径，本应用不往里写。想放自己的版本，写到用户目录或项目目录里。",
                    dir.display()
                ));
            }
            (dir, kind)
        }
        // 已存在的写在它当前所在的目录，别把它从「停用」意外变成「启用」。
        None => writable_dir_of(scan, name)
            .unwrap_or_else(|| (scan.local_enabled.clone(), RootKind::Local)),
    };

    if !dir.join(name).is_dir() {
        // 同名的在另一侧（停用的那份）→ 拒绝。写下去就会出现两份同名目录，
        // 而列表只会把「启用」那份算进并集 —— 用户改的那份可能永远不生效。
        if let Some((sibling, _)) = sibling_dir(scan, &dir, kind)
            && sibling.join(name).is_dir()
        {
            return Err(format!(
                "`{name}` 在 {} 里已经有一份（启用 / 停用两侧各存一份会互相打架）：\
                     先在列表里处理那一份再保存。",
                sibling.display()
            ));
        }

        // 名字撞上外部路径里已有的技能 → 拒绝。写下去会把那一份遮住，而那边
        // 可能正被别的工具用着；磁盘上那边一个字没动，界面状态却变了 ——
        // 这是最难查的一类问题。报错是可行动的：改个名字，或者先忽略那一份。
        //
        // 例外：那个外部技能**已经被忽略**时不拒绝。它本来就不参与并集，同名不构成
        // 竞争，而且用户已经表达过「不要它」。
        if !scan.ignored_here(name, RootKind::External)
            && let Some(root) = external_root_of(scan, name)
        {
            return Err(format!(
                "外部扫描路径 {} 里已经有一个叫 `{name}` 的技能：写下去会把它遮住。\
                     换个名字，或者先把它忽略掉。",
                root.display()
            ));
        }

        // 项目优先于用户：往用户目录写一个项目里已有的名字，模型用到的会是项目那份，
        // 刚写的这一份永远不生效。
        if kind == RootKind::Local
            && let Some(project) = &scan.project_enabled
            && project.join(name).is_dir()
        {
            return Err(format!(
                "当前项目（{}）里已经有一个叫 `{name}` 的技能：项目优先于用户目录，\
                         模型用到的会是那一份。要改它就编辑项目里那一行，或者换个名字。",
                project.display()
            ));
        }
    }

    let text = render(d);
    // 先验证渲染结果能被 carter 解析回来 —— 否则会写下一个「存在但永不加载」
    // 的技能，用户完全看不出问题在哪。
    if parse_skill_md(&text).is_none() {
        return Err("生成的 SKILL.md 无法被解析，请检查名称与描述".into());
    }

    let target = dir.join(name);
    std::fs::create_dir_all(&target).map_err(|e| format!("建目录失败：{e}"))?;
    std::fs::write(target.join("SKILL.md"), text).map_err(|e| format!("写 SKILL.md 失败：{e}"))?;
    Ok(dir)
}

/// 与 `dir` 同类、但属于另一侧（启用 ↔ 停用）的目录。
pub(crate) fn sibling_dir(scan: &Scan, dir: &Path, kind: RootKind) -> Option<(PathBuf, RootKind)> {
    let pair = match kind {
        RootKind::Project => (
            scan.project_enabled.as_ref(),
            scan.project_disabled.as_ref(),
        ),
        RootKind::Local => (Some(&scan.local_enabled), Some(&scan.local_disabled)),
        RootKind::External => return None,
    };
    let (enabled, disabled) = pair;
    let other = if same_dir(dir, enabled?) {
        disabled?
    } else {
        enabled?
    };
    Some((other.clone(), kind))
}

/// 删一个技能目录：是链接就只摘链接本身，别顺着它把目标目录删了。
///
/// `remove_dir_all` 在符号链接上的行为按平台而不同（Unix 上直接失败、Windows
/// 上某些情况会跟进去），而用户软链进来的技能很可能指向别处正在用的目录。
pub(crate) fn remove_skill_dir(p: &Path) -> Result<(), String> {
    let link = std::fs::symlink_metadata(p).map_err(|e| format!("删除失败：{e}"))?;
    if link.file_type().is_symlink() {
        return std::fs::remove_file(p).map_err(|e| format!("删除链接失败：{e}"));
    }
    std::fs::remove_dir_all(p).map_err(|e| format!("删除失败：{e}"))
}

/// 删除技能（目录整个删掉，不可恢复）。**只删项目级 / 用户级的**。
///
/// `source` 指到哪个根就删哪个根 —— 同名的技能可能在项目与用户两处都有，按名字猜
/// 会删错一份。
pub fn delete(scan: &Scan, name: &str, source: Option<&str>) -> Result<(), String> {
    if let Some(raw) = source {
        let Some((dir, kind)) = scan.root_of(raw) else {
            return Err(format!("扫描列表里没有 `{raw}`"));
        };
        if kind == RootKind::External {
            return Err(format!(
                "`{name}` 来自外部扫描路径 {} —— 那是别的工具自己的目录，本应用不会删除它。\
                 不想要的话，关掉它的开关（记进忽略名单），或者把这条扫描路径移除。",
                dir.display()
            ));
        }
        let p = dir.join(name);
        if p.is_dir() {
            return remove_skill_dir(&p);
        }
        return Err(format!("`{}` 里找不到技能 `{name}`", dir.display()));
    }
    for (dir, _) in scan.writable_roots() {
        let p = dir.join(name);
        if p.is_dir() {
            return remove_skill_dir(&p);
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
