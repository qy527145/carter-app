use crate::prelude::*;

// ---------------------------------------------------------------------------
// 技能
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct SkillListing {
    skills: Vec<carter_app_core::SkillRow>,
    /// 全局开关（`[skills].enabled`）。关掉时整个技能目录都不加载。
    globally_enabled: bool,
    /// 扫描根一览：项目 → 用户 → 附加路径（顺序 = 优先级）。
    roots: Vec<skills::SkillRootRow>,
    /// Claude Code / Codex 这类默认位置的快捷选项。
    presets: Vec<skills::SkillPreset>,
    /// 被按名字忽略的外部技能。
    ///
    /// 单独回传是因为：一个被忽略的名字可能**再也没有对应的行**（它被同名技能遮蔽，
    /// 或者它所在的那条扫描路径被移除了）。那时候这是用户撤销它的唯一入口。
    ignored_skills: Vec<String>,
}

/// 技能命令共用的扫描：会话 cwd 决定项目级根。
///
/// 技能页拿的是**当前会话**的 cwd（见 `SkillsPage`）。没有会话时传 `None`，
/// 那就只有用户级与外部根 —— 与从前一致。
pub(crate) fn skill_scan(settings: &AppSettings, cwd: Option<&str>) -> Scan {
    let cwd = cwd.filter(|s| !s.is_empty()).map(std::path::PathBuf::from);
    Scan::from_settings_at(settings, cwd.as_deref())
}

#[tauri::command]
pub(crate) fn list_skills(shell: State<'_, Arc<AppShell>>, cwd: Option<String>) -> SkillListing {
    let settings = AppSettings::load();
    let scan = skill_scan(&settings, cwd.as_deref());
    SkillListing {
        skills: skills::list(&scan),
        globally_enabled: shell.config.load().skills.enabled,
        roots: skills::roots(&scan),
        presets: skills::presets(&scan),
        ignored_skills: settings.ignored_skills.clone(),
    }
}

#[tauri::command]
pub(crate) fn read_skill(
    name: String,
    cwd: Option<String>,
    source: Option<String>,
) -> Result<String, String> {
    let scan = skill_scan(&AppSettings::load(), cwd.as_deref());
    skills::read(&scan, &name, source.as_deref())
}

/// 保存技能，返回实际写入的根目录（界面用它说明「存到了项目 / 用户目录」）。
#[tauri::command]
pub(crate) fn save_skill(
    shell: State<'_, Arc<AppShell>>,
    draft: SkillDraft,
    cwd: Option<String>,
) -> Result<String, String> {
    let scan = skill_scan(&AppSettings::load(), cwd.as_deref());
    let dir = skills::save(&scan, &draft)?;
    // 技能目录会进系统提示，所以要让跑着的会话下一轮重算 —— 发个事件叫前端刷新，
    // 会话侧本来每轮都会重读技能目录。
    shell.emit(AppEvent::ConfigChanged);
    Ok(dir.to_string_lossy().to_string())
}

/// 启停单个技能。本地的搬目录、外部的改忽略名单（分派在 `skills::set_enabled` 里）。
///
/// 回传新的设置：外部技能的开关写的是 `ignored_skills`，而前端 store 里那份设置是
/// 旧的。不回传的话，下一次改主题（`set_app_settings` 是**整体覆盖写入**）会把刚关掉
/// 的外部技能又打开。
/// `source` 是**点的是哪一行**（界面上那个 `SkillRow::source`）：同一个名字可能同时在
/// 项目、用户、外部三个根里出现，能区分它们的只有来源 —— 按名字猜会让「点外部那一行」
/// 变成点了没反应（本地目录此刻正好不在启用侧，搬目录那条路会直接返回 Ok）。
/// `None` 只用于从「已忽略」名单里恢复一个名字。
#[tauri::command]
pub(crate) fn toggle_skill(
    shell: State<'_, Arc<AppShell>>,
    name: String,
    enabled: bool,
    source: Option<String>,
    cwd: Option<String>,
) -> Result<AppSettings, String> {
    let mut settings = AppSettings::load();
    let scan = skill_scan(&settings, cwd.as_deref());
    if skills::set_enabled(&mut settings, &scan, &name, enabled, source.as_deref())? {
        settings.save()?;
    }
    shell.emit(AppEvent::ConfigChanged);
    Ok(settings)
}

#[tauri::command]
pub(crate) fn delete_skill(
    shell: State<'_, Arc<AppShell>>,
    name: String,
    source: Option<String>,
    cwd: Option<String>,
) -> Result<(), String> {
    let scan = skill_scan(&AppSettings::load(), cwd.as_deref());
    skills::delete(&scan, &name, source.as_deref())?;
    shell.emit(AppEvent::ConfigChanged);
    Ok(())
}

#[tauri::command]
pub(crate) async fn set_skills_enabled(
    shell: State<'_, Arc<AppShell>>,
    enabled: bool,
) -> Result<(), String> {
    config_edit::set_skills_enabled_reload(shell.inner(), enabled)
}

/// 加一条技能扫描路径（Claude Code / Codex 的默认位置，或任意自定义父目录）。
///
/// 存进 `~/.carter-app/settings.json` 而不是 `~/.carter/config.toml`：carter 0.1.4 的
/// `[skills]` 只有一个 `enabled`，写个 CLI 不认的 `paths` 进去等于让配置文件说谎。
///
/// 回传新的设置，理由同 [`toggle_skill`]。
#[tauri::command]
pub(crate) fn add_skill_root(
    shell: State<'_, Arc<AppShell>>,
    path: String,
) -> Result<AppSettings, String> {
    let mut settings = AppSettings::load();
    skills::add_root(&mut settings, &path)?;
    settings.save()?;
    shell.emit(AppEvent::ConfigChanged);
    Ok(settings)
}

#[tauri::command]
pub(crate) fn remove_skill_root(
    shell: State<'_, Arc<AppShell>>,
    path: String,
) -> Result<AppSettings, String> {
    let mut settings = AppSettings::load();
    skills::remove_root(&mut settings, &path)?;
    settings.save()?;
    shell.emit(AppEvent::ConfigChanged);
    Ok(settings)
}
