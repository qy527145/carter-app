use super::edit::render;
use super::*;

#[cfg(test)]
mod scan_and_edit {
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
        /// 项目级目录：`<dir>/proj/.carter/skills`。
        fn project(&self) -> PathBuf {
            self.dir.join("proj").join(".carter").join("skills")
        }
        fn project_disabled(&self) -> PathBuf {
            self.project().with_file_name("skills.disabled")
        }
        fn scan(&self, externals: &[&Path], ignored: &[&str]) -> Scan {
            self.scan_with_project(None, externals, ignored)
        }
        fn scan_with_project(
            &self,
            project: Option<&Path>,
            externals: &[&Path],
            ignored: &[&str],
        ) -> Scan {
            std::fs::create_dir_all(self.local()).unwrap();
            if let Some(p) = project {
                std::fs::create_dir_all(p).unwrap();
            }
            Scan {
                project_disabled: project.map(|p| p.with_file_name("skills.disabled")),
                project_enabled: project.map(|p| p.to_path_buf()),
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
        assert_eq!(
            read(&scan, "pdf", None).unwrap().trim(),
            "停用的本地 的正文"
        );
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
                source: None,
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
                source: None,
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
        let err = delete(&scan, "excel", None).expect_err("外部技能不该被删");
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

        let ext_s = ext.to_string_lossy().to_string();
        assert!(
            set_enabled(&mut app, &scan, "excel", false, Some(&ext_s)).unwrap(),
            "改了忽略名单 → 调用方要落盘"
        );
        assert!(app.skill_ignored("excel"));
        assert!(ext.join("excel").is_dir(), "外部目录必须原样留着");
        // 忽略名单住在 app 里，所以并集要重新构一份 Scan 才算得对 ——
        // 这正是 `set_enabled` 的文档里那句提醒。
        assert!(union_metas(&rescan(&f, &[&ext], &app)).is_empty());

        assert!(set_enabled(&mut app, &scan, "excel", true, Some(&ext_s)).unwrap());
        assert!(!app.skill_ignored("excel"));
        assert_eq!(
            names(&union_metas(&rescan(&f, &[&ext], &app))),
            vec!["excel"]
        );
        f.cleanup();
    }

    /// 本地技能走搬目录那条路，设置不动。
    #[test]
    fn toggling_a_local_skill_moves_the_directory_and_not_the_settings() {
        let f = Fixture::new("toggle-local");
        write_skill(&f.local(), "pdf", "本地的");

        let scan = f.scan(&[], &[]);
        let mut app = AppSettings::default();
        let local_s = f.local().to_string_lossy().to_string();

        assert!(!set_enabled(&mut app, &scan, "pdf", false, Some(&local_s)).unwrap());
        assert!(f.disabled().join("pdf").is_dir());
        assert!(app.ignored_skills.is_empty(), "本地技能不走忽略名单");

        assert!(!set_enabled(&mut app, &scan, "pdf", true, Some(&local_s)).unwrap());
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
        assert!(set_enabled(&mut app, &scan, "pdf", false, Some(&ext.to_string_lossy())).unwrap());
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
        let local_s = f.local().to_string_lossy().to_string();
        assert!(!set_enabled(&mut app, &scan, "pdf", true, Some(&local_s)).unwrap());
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
        assert_eq!(rows[0].source, RootKind::Local);
        assert!(rows[0].exists && rows[0].found == 1 && rows[0].effective == 1);
        assert_eq!(rows[0].subdirs, 1);

        let ext_row = &rows[1];
        assert_eq!(ext_row.source, RootKind::External);
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
            vec![codex.join(".system").to_string_lossy().to_string()],
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
            vec![
                home.join(".claude")
                    .join("skills")
                    .to_string_lossy()
                    .to_string()
            ],
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
        assert!(
            set_enabled(
                &mut app,
                &scan,
                "real-dir",
                false,
                Some(&ext.to_string_lossy())
            )
            .unwrap()
        );
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

    /// 项目级：并集里排在用户级**前面**（同一个项目里，项目钉死的版本更具体）。
    #[test]
    fn the_project_root_wins_over_the_user_root() {
        let f = Fixture::new("project-wins");
        let proj = f.project();
        write_skill(&f.local(), "pdf", "用户的");
        write_skill(&proj, "pdf", "项目的");

        let scan = f.scan_with_project(Some(&proj), &[], &[]);
        let metas = union_metas(&scan);
        assert_eq!(names(&metas), vec!["pdf"]);
        assert_eq!(metas[0].description, "项目的", "同名的取项目那份");
        assert_eq!(
            find_body(&scan, "pdf").unwrap().trim(),
            "项目的 的正文",
            "正文必须与系统提示里的那一份同源"
        );
        f.cleanup();
    }

    /// 项目级与用户级都是本应用的目录：可写、可搬、可删。列表里要能分清是哪一类。
    #[test]
    fn project_rows_are_writable_and_tagged_as_project() {
        let f = Fixture::new("project-rows");
        let proj = f.project();
        write_skill(&proj, "pdf", "项目的");
        write_skill(&f.local(), "excel", "用户的");

        let scan = f.scan_with_project(Some(&proj), &[], &[]);
        let rows = list(&scan);
        let project = rows.iter().find(|r| r.name == "pdf").unwrap();
        assert_eq!(project.scope, RootKind::Project);
        assert!(!project.readonly && project.enabled && project.effective);
        let local = rows.iter().find(|r| r.name == "excel").unwrap();
        assert_eq!(local.scope, RootKind::Local);

        // 根一览也是项目在前
        let roots = roots(&scan);
        assert_eq!(roots[0].source, RootKind::Project);
        assert_eq!(roots[1].source, RootKind::Local);
        f.cleanup();
    }

    /// 停用项目技能 = 在项目自己的两个目录之间搬，别动用户目录。
    #[test]
    fn toggling_a_project_skill_moves_it_within_the_project() {
        let f = Fixture::new("toggle-project");
        let proj = f.project();
        write_skill(&proj, "pdf", "项目的");

        let scan = f.scan_with_project(Some(&proj), &[], &[]);
        let mut app = AppSettings::default();
        let src = proj.to_string_lossy().to_string();

        assert!(!set_enabled(&mut app, &scan, "pdf", false, Some(&src)).unwrap());
        assert!(f.project_disabled().join("pdf").is_dir());
        assert!(!proj.join("pdf").is_dir());
        assert!(union_metas(&scan).is_empty(), "停用后不参与并集");
        assert!(!f.local().join("pdf").is_dir(), "用户目录不该多出东西");

        assert!(!set_enabled(&mut app, &scan, "pdf", true, Some(&src)).unwrap());
        assert!(proj.join("pdf").is_dir());
        assert!(app.ignored_skills.is_empty(), "项目技能不走忽略名单");
        f.cleanup();
    }

    /// 忽略名单只作用于外部：项目里同名的那份照样进并集。
    #[test]
    fn the_ignore_list_never_touches_project_skills() {
        let f = Fixture::new("ignore-project");
        let proj = f.project();
        write_skill(&proj, "pdf", "项目的");

        let scan = f.scan_with_project(Some(&proj), &[], &["pdf"]);
        assert_eq!(names(&union_metas(&scan)), vec!["pdf"]);
        f.cleanup();
    }

    /// 往用户目录新建一个项目里已有的名字 → 拒绝。项目优先，刚写的这份永远不生效。
    #[test]
    fn save_refuses_a_user_skill_the_project_already_shadows() {
        let f = Fixture::new("project-shadow");
        let proj = f.project();
        write_skill(&proj, "pdf", "项目的");

        let scan = f.scan_with_project(Some(&proj), &[], &[]);
        let mut d = draft("pdf");
        d.source = Some(f.local().to_string_lossy().to_string());
        let err = save(&scan, &d).expect_err("项目里已有同名，用户目录新建要拒绝");
        assert!(err.contains("项目") && err.contains("pdf"), "{err}");
        assert!(!f.local().join("pdf").exists(), "拒绝之后不该留下目录");
        f.cleanup();
    }

    /// 往项目里新建：落在项目的 `.carter/skills`，用户目录一个字不动。
    #[test]
    fn save_into_the_project_writes_only_the_project_root() {
        let f = Fixture::new("save-project");
        let proj = f.project();
        write_skill(&f.local(), "pdf", "用户的");

        let scan = f.scan_with_project(Some(&proj), &[], &[]);
        let mut d = draft("excel");
        d.source = Some(proj.to_string_lossy().to_string());
        let wrote = save(&scan, &d).expect("项目里新建应当成功");

        assert_eq!(wrote, proj);
        assert!(proj.join("excel").join("SKILL.md").is_file());
        assert!(!f.local().join("excel").exists(), "用户目录不该多出东西");
        // 项目优先：并集里 excel 来自项目
        assert_eq!(
            find_body(&scan, "excel").unwrap().trim(),
            "# 用法\n\n先 read_file，再 bash。"
        );
        f.cleanup();
    }

    /// 同名技能在项目与用户两处都有时，按来源删 —— 别删错那一份。
    #[test]
    fn deleting_by_source_removes_exactly_that_copy() {
        let f = Fixture::new("delete-project");
        let proj = f.project();
        write_skill(&proj, "pdf", "项目的");
        write_skill(&f.local(), "pdf", "用户的");

        let scan = f.scan_with_project(Some(&proj), &[], &[]);
        delete(&scan, "pdf", Some(&proj.to_string_lossy())).unwrap();
        assert!(!proj.join("pdf").exists());
        assert!(f.local().join("pdf").is_dir(), "用户那份必须还在");
        f.cleanup();
    }

    /// 外部路径不能当写目标 —— 提示要指向可行动的做法。
    #[test]
    fn saving_into_an_external_root_is_refused() {
        let f = Fixture::new("save-external");
        let ext = f.external("ext");
        write_skill(&ext, "pdf", "外部的");

        let scan = f.scan(&[&ext], &[]);
        let mut d = draft("pdf");
        d.source = Some(ext.to_string_lossy().to_string());
        let err = save(&scan, &d).expect_err("外部路径只读");
        assert!(err.contains("外部"), "{err}");
        f.cleanup();
    }

    /// 会话开在子目录里也要找得到项目根；最近的那个赢。
    #[test]
    fn project_roots_walk_up_and_take_the_nearest_one() {
        let f = Fixture::new("walk-up");
        let outer = f.dir.join("repo").join(".carter").join("skills");
        let inner = outer
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("sub")
            .join(".carter")
            .join("skills");
        std::fs::create_dir_all(&outer).unwrap();
        std::fs::create_dir_all(&inner).unwrap();
        let deep = inner.parent().unwrap().parent().unwrap().join("src");

        let got = project_roots(&deep, &f.local(), &f.disabled()).expect("应当找到项目根");
        assert_eq!(got.0, inner, "最近的 .carter/skills 赢");
        f.cleanup();
    }

    /// 用户自己的 `~/.carter/skills` 不能被当成项目根 —— 它在并集里已有位置。
    #[test]
    fn project_roots_skip_the_users_own_skills_dir() {
        let f = Fixture::new("skip-local");
        // 把「用户目录」放在一个假的项目形状里：<home>/.carter/skills
        let home = f.dir.join("home");
        let local = home.join(".carter").join("skills");
        let disabled = home.join(".carter").join("skills.disabled");
        std::fs::create_dir_all(&local).unwrap();
        let deep = home.join("work").join("proj");
        std::fs::create_dir_all(&deep).unwrap();

        let got = project_roots(&deep, &local, &disabled).expect("回落也不该是 None");
        assert_ne!(
            got.0, local,
            "ancestors 里那个 ~/.carter/skills 是用户目录，不能当项目根"
        );
        assert_eq!(
            got.0,
            deep.join(".carter").join("skills"),
            "一个 .carter 都没有时，回落到 cwd 自己的 .carter/skills"
        );
        f.cleanup();
    }

    /// 端到端：`from_settings_at` 把 cwd 的项目根接进并集，并且**项目赢**。
    ///
    /// 只有这条测试碰进程级环境（`CARTER_HOME`），所以它拿 [`crate::paths::ENV_LOCK`]
    /// —— 与 `config_edit` / `changes` 里那些同样改进程环境的测试串行。
    #[test]
    fn from_settings_at_attaches_the_project_root_and_it_wins() {
        let _g = crate::paths::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = tmp_dir("from-settings");
        // SAFETY: 与其它改进程级环境变量的测试共用同一把锁。
        unsafe { std::env::set_var("CARTER_HOME", &home) };

        let proj = home.join("proj");
        let project_skills = proj.join(".carter").join("skills");
        std::fs::create_dir_all(&project_skills).unwrap();
        let user_skills = home.join("skills");
        write_skill(&project_skills, "pdf", "项目的");
        write_skill(&user_skills, "pdf", "用户的");

        // 用户把项目目录也手填进了扫描路径 —— 不该出现两次。
        let settings = AppSettings {
            skill_roots: vec![
                project_skills.to_string_lossy().to_string(),
                home.join("ext").to_string_lossy().to_string(),
            ],
            ..AppSettings::default()
        };

        let scan = Scan::from_settings_at(&settings, Some(&proj));
        assert_eq!(
            scan.project_enabled.as_deref(),
            Some(project_skills.as_path())
        );
        assert_eq!(scan.local_enabled, user_skills);
        assert_eq!(scan.externals.len(), 1, "项目根重复出现在设置里时只留一份");

        let metas = union_metas(&scan);
        assert_eq!(names(&metas), vec!["pdf"]);
        assert_eq!(metas[0].description, "项目的", "项目优先于用户目录");
        assert_eq!(find_body(&scan, "pdf").unwrap().trim(), "项目的 的正文");

        // 没有 cwd 时退回旧行为：只有用户级与外部根。
        let scan = Scan::from_settings_at(&settings, None);
        assert!(scan.project_enabled.is_none());
        assert_eq!(union_metas(&scan)[0].description, "用户的");

        unsafe { std::env::remove_var("CARTER_HOME") };
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 符号链接 / 目录联接里的技能也要扫得到 —— 用户常把技能目录软链进扫描根。
    ///
    /// Windows 上建目录链接需要开发者模式或管理员权限，建不了就跳过（不是功能坏了）。
    #[test]
    fn symlinked_skill_directories_are_discovered() {
        let f = Fixture::new("symlink");
        let target = f.dir.join("elsewhere").join("linked-skill");
        write_skill(target.parent().unwrap(), "linked-skill", "软链进来的");
        if !scan_create_link(&target, &f.local().join("linked-skill")) {
            // Windows 上建目录链接要开发者模式 / 管理员权限，建不了就跳过。
            f.cleanup();
            return;
        }

        let scan = f.scan(&[], &[]);
        let metas = union_metas(&scan);
        assert_eq!(names(&metas), vec!["linked-skill"]);
        assert_eq!(metas[0].description, "软链进来的");
        assert_eq!(
            find_body(&scan, "linked-skill").unwrap().trim(),
            "软链进来的 的正文"
        );
        // 子目录计数也要跟随链接，否则界面上「子目录 0 / 技能 1」自相矛盾。
        assert!(roots(&scan).iter().any(|r| r.subdirs >= 1));
        f.cleanup();
    }

    /// 软链进来的技能用 `delete` 删掉时只能摘链接，目标目录必须原样还在 ——
    /// 那是用户别处在用的目录。
    #[test]
    fn deleting_a_symlinked_skill_keeps_its_target() {
        let f = Fixture::new("symlink-del");
        let target = f.dir.join("elsewhere").join("linked-skill");
        write_skill(target.parent().unwrap(), "linked-skill", "别处");
        if !scan_create_link(&target, &f.local().join("linked-skill")) {
            f.cleanup();
            return;
        }
        let scan = f.scan(&[], &[]);
        delete(&scan, "linked-skill", None).unwrap();
        assert!(!f.local().join("linked-skill").exists(), "链接该被摘掉");
        assert!(target.join("SKILL.md").is_file(), "目标目录不能动");
        f.cleanup();
    }

    /// 建目录链接。返回 `false` 表示这台机器没权限（Windows 非开发者模式）。
    fn scan_create_link(target: &Path, link: &Path) -> bool {
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            // `symlink_dir` 需要开发者模式 / 管理员；失败就当作「跳过」。
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        }
    }

    fn draft(name: &str) -> SkillDraft {
        SkillDraft {
            name: name.into(),
            description: "把 PDF 拆成文本".into(),
            allowed_tools: vec!["read_file".into(), "bash".into()],
            body: "# 用法\n\n先 read_file，再 bash。".into(),
            source: None,
        }
    }
}
