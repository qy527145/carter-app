use super::*;

#[cfg(test)]
mod actions {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn project(tag: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("carter-ttool-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.name", "测试"],
            vec!["config", "user.email", "t@example.com"],
        ] {
            std::process::Command::new("git")
                .current_dir(&dir)
                .args(&args)
                .output()
                .unwrap();
        }
        crate::team::init::initialize(&dir, Some("测试团队")).unwrap();
        dir
    }

    fn call(tool: &TeamTool, args: Value) -> String {
        match tool.run(&args) {
            Ok(s) => s,
            Err(e) => panic!("调用失败：{e}"),
        }
    }

    /// 从回灌文本里取 `id = ...` 那一行。
    fn id_of(out: &str) -> String {
        out.lines()
            .find_map(|l| l.trim().strip_prefix("id = "))
            .unwrap_or_else(|| panic!("回灌里没有 id：{out}"))
            .trim()
            .to_string()
    }

    #[test]
    fn create_goal_then_tasks_and_report_overview() {
        let dir = project("flow");
        let t = TeamTool::new(dir.clone());

        let out = call(
            &t,
            json!({"action": "goal_new", "title": "第一阶段", "description": "把数据层做完"}),
        );
        let gid = id_of(&out);

        let out = call(
            &t,
            json!({"action": "task_new", "goal_id": gid, "title": "写模型"}),
        );
        let tid = id_of(&out);
        call(
            &t,
            json!({"action": "task_new", "goal_id": gid, "title": "写存储", "depends_on": [tid]}),
        );

        let overview = call(&t, json!({"action": "overview"}));
        assert!(overview.contains("第一阶段"));
        assert!(overview.contains("写模型"));
        assert!(overview.contains("等："), "依赖要显示出来：{overview}");

        // 完成前置之后，后面的任务应该变成「可以开工」。
        call(
            &t,
            json!({"action": "task_status", "task_id": tid, "status": "done"}),
        );
        let overview = call(&t, json!({"action": "overview"}));
        assert!(
            overview.contains("▶ "),
            "前置完成后要标出可开工：{overview}"
        );
    }

    #[test]
    fn blocked_requires_a_reason() {
        let dir = project("blocked");
        let t = TeamTool::new(dir);
        let out = call(&t, json!({"action": "goal_new", "title": "g"}));
        let gid = id_of(&out);
        let out = call(
            &t,
            json!({"action": "task_new", "goal_id": gid, "title": "t"}),
        );
        let tid = id_of(&out);

        let err = t
            .run(&json!({"action": "task_status", "task_id": tid, "status": "blocked"}))
            .unwrap_err();
        assert!(err.contains("blocked_reason"), "{err}");
    }

    #[test]
    fn claiming_writes_an_event_and_sets_the_assignee() {
        let dir = project("claim");
        let t = TeamTool::new(dir.clone());
        let out = call(&t, json!({"action": "goal_new", "title": "g"}));
        let gid = id_of(&out);
        let out = call(
            &t,
            json!({"action": "task_new", "goal_id": gid, "title": "t"}),
        );
        let tid = id_of(&out);

        call(&t, json!({"action": "task_claim", "task_id": tid}));
        let store = Store::discover(&dir).unwrap();
        let task = store.task(&gid, &tid).unwrap();
        assert_eq!(task.assignee.as_deref(), Some("测试 <t@example.com>"));

        // 事件流里应该有一条 claim。
        let evs = events::read(&dir, 10);
        assert!(
            evs.iter()
                .any(|e| e.kind.is(EventKind::CLAIM) && e.target.as_deref() == Some(&tid)),
            "{evs:?}"
        );

        // 名下的任务看得到。
        let mine = call(&t, json!({"action": "my_tasks"}));
        assert!(mine.contains("t"), "{mine}");
    }

    #[test]
    fn a_second_person_cannot_quietly_steal_a_claim() {
        let dir = project("steal");
        let t = TeamTool::new(dir.clone());
        let out = call(&t, json!({"action": "goal_new", "title": "g"}));
        let gid = id_of(&out);
        let out = call(
            &t,
            json!({"action": "task_new", "goal_id": gid, "title": "t"}),
        );
        let tid = id_of(&out);

        // 手工造一个「别人已经认领」的状态。
        let store = Store::open(dir.join(".carter"));
        let mut task = store.task(&gid, &tid).unwrap();
        task.assignee = Some("别人 <other@example.com>".into());
        store.save_task(&task).unwrap();

        let err = t
            .run(&json!({"action": "task_claim", "task_id": tid}))
            .unwrap_err();
        assert!(err.contains("已经被"), "{err}");
    }

    #[test]
    fn comments_become_events_with_the_author_and_body() {
        let dir = project("comment");
        let t = TeamTool::new(dir.clone());
        let out = call(&t, json!({"action": "goal_new", "title": "g"}));
        let gid = id_of(&out);

        call(
            &t,
            json!({"action": "comment", "goal_id": gid, "text": "第一阶段先不做界面"}),
        );
        let evs = events::read(&dir, 5);
        let c = evs
            .iter()
            .find(|e| e.kind.is(EventKind::COMMENT))
            .expect("要有一条评论");
        assert_eq!(c.body, "第一阶段先不做界面");
        assert_eq!(c.author.as_ref().unwrap().email, "t@example.com");
    }

    #[test]
    fn a_board_is_one_html_page_that_can_be_rewritten() {
        let dir = project("board");
        let t = TeamTool::new(dir.clone());
        let gid = id_of(&call(&t, json!({"action": "goal_new", "title": "g"})));
        let out = call(
            &t,
            json!({
                "action": "board_new", "title": "十月回顾", "goal_id": gid,
                "html": "<!doctype html><html><head></head><body><h1>做得好的</h1></body></html>"
            }),
        );
        let bid = id_of(&out);

        let store = Store::open(dir.join(".carter"));
        let board = store.board(&bid).unwrap();
        assert_eq!(board.title, "十月回顾", "标题由工具补进 <title>");
        assert_eq!(board.goal_id.as_deref(), Some(gid.as_str()));
        assert!(board.html.contains("<h1>做得好的</h1>"));

        // 整页替换时漏写了 <title>：沿用旧标题与关联，不能退化成 id。
        call(
            &t,
            json!({"action": "board_update", "board_id": bid, "html": "<p>改过了</p>"}),
        );
        let board = store.board(&bid).unwrap();
        assert_eq!(board.title, "十月回顾");
        assert_eq!(board.goal_id.as_deref(), Some(gid.as_str()));
        assert!(board.html.contains("改过了"));

        // 只改名不动正文。
        call(
            &t,
            json!({"action": "board_update", "board_id": bid, "title": "十月复盘"}),
        );
        let board = store.board(&bid).unwrap();
        assert_eq!(board.title, "十月复盘");
        assert!(board.html.contains("改过了"));

        assert!(call(&t, json!({"action": "board_list"})).contains("十月复盘"));
        assert!(
            t.run(&json!({"action": "board_new", "title": "空"}))
                .is_err(),
            "没有 html 要拒绝"
        );
        assert!(
            t.run(&json!({"action": "board_update", "board_id": "../x", "html": "x"}))
                .is_err()
        );
    }

    /// 拼错参数名时整次调用不执行，并列出合法参数 —— 否则「改优先级」会静默成功、
    /// 实际什么都没改。
    #[test]
    fn a_misspelled_parameter_is_rejected_not_ignored() {
        let dir = project("typo");
        let t = TeamTool::new(dir.clone());
        let gid = id_of(&call(&t, json!({"action": "goal_new", "title": "g"})));
        let err = t
            .run(&json!({"action": "goal_update", "goal_id": gid, "prioirty": "P0"}))
            .unwrap_err();
        assert!(err.contains("prioirty"), "{err}");
        assert!(err.contains("priority"), "要列出合法的参数名：{err}");
        let store = Store::discover(&dir).unwrap();
        assert_eq!(
            store.goal(&gid).unwrap().priority,
            Priority::P2,
            "被拒的调用不能有副作用"
        );

        // 单个依赖照样能写（脚本时代的那个 bug 不该在工具里出现）。
        let a = id_of(&call(
            &t,
            json!({"action": "task_new", "goal_id": gid, "title": "a"}),
        ));
        let b = id_of(&call(
            &t,
            json!({"action": "task_new", "goal_id": gid, "title": "b", "depends_on": [a]}),
        ));
        assert_eq!(store.task(&gid, &b).unwrap().depends_on, vec![a]);
    }

    #[test]
    fn goal_update_can_set_and_clear_the_owner() {
        let dir = project("owner");
        let t = TeamTool::new(dir.clone());
        let gid = id_of(&call(&t, json!({"action": "goal_new", "title": "g"})));
        call(
            &t,
            json!({"action": "goal_update", "goal_id": gid, "assignee": "张三 <z@example.com>"}),
        );
        let store = Store::discover(&dir).unwrap();
        assert_eq!(
            store.goal(&gid).unwrap().owner.as_deref(),
            Some("张三 <z@example.com>")
        );
        call(
            &t,
            json!({"action": "goal_update", "goal_id": gid, "assignee": ""}),
        );
        assert_eq!(store.goal(&gid).unwrap().owner, None);
    }

    #[test]
    fn missing_team_dir_says_how_to_fix_it() {
        let dir = std::env::temp_dir().join(format!("carter-ttool-nodir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let t = TeamTool::new(dir);
        let err = t.run(&json!({"action": "overview"})).unwrap_err();
        assert!(err.contains("初始化"), "{err}");
    }
}

#[cfg(test)]
mod e2e {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 造一个「裸远端 + 两个成员」的真实场景。
    ///
    /// 这不是模拟：两个独立的工作副本各自 `git config` 成不同的人，认领走的就是
    /// 真实的 `commit` + `push`。空提交当作裁决者这件事，只有在真 push 被拒的时候
    /// 才看得出来对不对。
    struct World {
        base: PathBuf,
        alice: PathBuf,
        bob: PathBuf,
    }

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} 起不来：{e}"));
        assert!(
            out.status.success(),
            "git {args:?} 失败：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn world(tag: &str) -> World {
        let n = std::process::id();
        let base = std::env::temp_dir().join(format!("carter-e2e-{tag}-{n}"));
        let _ = std::fs::remove_dir_all(&base);
        let remote = base.join("remote.git");
        std::fs::create_dir_all(&remote).unwrap();
        git_in(&remote, &["init", "--bare", "-q", "-b", "main"]);

        let seed = base.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        git_in(&seed, &["init", "-q", "-b", "main"]);
        git_in(&seed, &["config", "user.name", "seed"]);
        git_in(&seed, &["config", "user.email", "seed@example.com"]);
        std::fs::write(seed.join("README.md"), "demo\n").unwrap();
        git_in(&seed, &["add", "-A"]);
        git_in(&seed, &["commit", "-qm", "init"]);
        git_in(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git_in(&seed, &["push", "-q", "-u", "origin", "main"]);

        crate::team::init::initialize(&seed, Some("联调团队")).unwrap();
        git_in(&seed, &["push", "-q"]);

        // 闭包在这里借 `base` 会与最后的 move 冲突，写成带参数的局部函数。
        fn clone_into(base: &Path, remote: &Path, who: &str, name: &str, email: &str) -> PathBuf {
            let dir = base.join(who);
            git_in(
                base,
                &[
                    "clone",
                    "-q",
                    remote.to_str().unwrap(),
                    dir.to_str().unwrap(),
                ],
            );
            git_in(&dir, &["config", "user.name", name]);
            git_in(&dir, &["config", "user.email", email]);
            dir
        }
        let alice = clone_into(&base, &remote, "alice", "张三", "zhang@example.com");
        let bob = clone_into(&base, &remote, "bob", "李四", "li@example.com");
        World { base, alice, bob }
    }

    fn id_of(out: &str) -> String {
        out.lines()
            .find_map(|l| l.trim().strip_prefix("id = "))
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn two_members_share_goals_through_the_remote() {
        let w = world("share");
        let alice = TeamTool::new(w.alice.clone());
        let gid = id_of(
            &alice
                .run(&json!({"action": "goal_new", "title": "第一阶段"}))
                .unwrap(),
        );
        let tid = id_of(
            &alice
                .run(&json!({"action": "task_new", "goal_id": gid, "title": "写数据层"}))
                .unwrap(),
        );

        // 李四还没拉到任何东西。
        let bob = TeamTool::new(w.bob.clone());
        bob.run(&json!({"action": "task_new", "goal_id": gid, "title": "x"}))
            .unwrap_err();

        // 张三再留一条评论 —— 那是走空提交的事件，与状态文件是两条不同的通道。
        alice
            .run(&json!({"action": "comment", "goal_id": gid, "text": "数据层先落地"}))
            .unwrap();

        // 张三把改动推到远端之后，李四就能看见了。
        let store = Store::discover(&w.alice).unwrap();
        let rel = ".carter";
        git::stage_team(store.project_root(), rel).unwrap();
        git::commit_team(store.project_root(), rel, "feat(carter): 第一阶段").unwrap();
        git::push(store.project_root()).unwrap();

        assert!(git::sync(&w.bob).is_ok());
        let overview = bob.run(&json!({"action": "overview"})).unwrap();
        assert!(overview.contains("第一阶段"), "{overview}");
        assert!(overview.contains("写数据层"), "{overview}");
        assert!(overview.contains(&tid));
        // 事件流也同步过来了，而且带着作者与正文。
        let evs = events::read(&w.bob, 20);
        let c = evs
            .iter()
            .find(|e| e.kind.is(EventKind::COMMENT))
            .unwrap_or_else(|| panic!("李四没看到张三的评论：{evs:?}"));
        assert_eq!(c.body, "数据层先落地");
        assert_eq!(c.author.as_ref().unwrap().name, "张三");
        let _ = std::fs::remove_dir_all(&w.base);
    }

    /// 有人先认领了：后来的那个在写之前拉一次事件就能看到，直接拒绝，本地什么都不改。
    #[test]
    fn a_claim_already_on_the_remote_is_refused_up_front() {
        let w = world("race");
        let (gid, tid) = shared_task(&w, "抢我");

        // 张三先认领 —— 认领事件立刻推到远端。
        TeamTool::new(w.alice.clone())
            .run(&json!({"action": "task_claim", "task_id": tid}))
            .unwrap();

        let bob = TeamTool::new(w.bob.clone());
        let err = bob
            .run(&json!({"action": "task_claim", "task_id": tid}))
            .unwrap_err();
        assert!(err.contains("张三"), "要说出是谁先认领的：{err}");

        let store_b = Store::discover(&w.bob).unwrap();
        assert!(
            store_b.task(&gid, &tid).unwrap().assignee.is_none(),
            "被拒之后本地不能留下「我认领了」"
        );
        assert!(
            git::status_porcelain(store_b.project_root())
                .trim()
                .is_empty(),
            "工作区应该干净：{}",
            git::status_porcelain(store_b.project_root())
        );
        let _ = std::fs::remove_dir_all(&w.base);
    }

    /// 真正的并发：两边都在对方推送之前写下了自己的认领。事件链会把两条接成一条，
    /// 排在前面的赢；输的那一方本地退回原样。
    #[test]
    fn simultaneous_claims_have_exactly_one_winner() {
        let w = world("tie");
        let (gid, tid) = shared_task(&w, "抢我");
        let store_a = Store::discover(&w.alice).unwrap();
        let store_b = Store::discover(&w.bob).unwrap();

        // 张三的认领先上了远端链。
        let alice = TeamTool::new(w.alice.clone());
        alice
            .run(&json!({"action": "task_claim", "task_id": tid}))
            .unwrap();

        // 李四在拉到之前就写下了自己的认领（模拟两边同时点）。
        events::append(
            &w.bob,
            &Draft {
                kind: EventKind::CLAIM.into(),
                target: Some(tid.clone()),
                goal_id: Some(gid.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        // 推送时被拒、自动接到张三后面 —— 链上张三在前。
        events::push(&w.bob).unwrap();
        let me_b = git::local_identity(&w.bob).unwrap().as_key();
        let roster = members::load_all(&store_b);
        assert_eq!(
            claimed_by_other(&w.bob, &tid, &me_b, &roster).as_deref(),
            Some("张三"),
            "链上排在前面的那条认领赢"
        );
        let me_a = git::local_identity(&w.alice).unwrap().as_key();
        events::fetch(&w.alice).unwrap();
        assert!(
            claimed_by_other(&w.alice, &tid, &me_a, &members::load_all(&store_a)).is_none(),
            "赢的那一方不该被判输"
        );
        let _ = std::fs::remove_dir_all(&w.base);
    }

    /// @ 与指派跨机器送达：张三发，李四拉一次就能在收件箱里看到。
    #[test]
    fn mentions_and_assignments_reach_the_other_member() {
        let w = world("inbox");
        let (_, tid) = shared_task(&w, "接口联调");
        // 两个人都加入团队，名单随状态提交同步。
        let zs = members::join(
            &w.alice.join(".carter"),
            &git::local_identity(&w.alice).unwrap(),
            members::Join::New {
                name: "张三".into(),
            },
        )
        .unwrap()
        .0;
        let ls = members::join(
            &w.alice.join(".carter"),
            &Identity::parse("李四 <li@example.com>").unwrap(),
            members::Join::New {
                name: "李四".into(),
            },
        )
        .unwrap()
        .0;
        let root_a = Store::discover(&w.alice).unwrap();
        git::stage_team(root_a.project_root(), ".carter").unwrap();
        git::commit_team(root_a.project_root(), ".carter", "chore: 成员").unwrap();
        git::push(root_a.project_root()).unwrap();
        git::sync(&w.bob).unwrap();

        let alice = TeamTool::new(w.alice.clone());
        alice
            .run(
                &json!({"action": "mention", "to": ["李四"], "task_id": tid, "text": "接口定好了"}),
            )
            .unwrap();
        let out = alice
            .run(&json!({"action": "assign", "task_id": tid, "assignee": "李四"}))
            .unwrap();
        assert!(out.contains("指派给 李四"), "{out}");
        // 任务文件里存的是成员 id，不是名字。
        let t = root_a.task(&shared_goal(&w), &tid).unwrap();
        assert_eq!(t.assignee.as_deref(), Some(ls.id.as_str()));

        // 李四这边：只拉事件、不合并分支，就能收到。
        let bob = TeamTool::new(w.bob.clone());
        let inbox = bob.run(&json!({"action": "inbox"})).unwrap();
        assert!(inbox.contains("[@我] 张三（经卡特） 提到"), "{inbox}");
        assert!(inbox.contains("[派给我]"), "{inbox}");
        assert!(inbox.contains("接口联调"), "消息里要带任务标题：{inbox}");
        // 张三自己发的不进自己的收件箱。
        let own = alice.run(&json!({"action": "inbox"})).unwrap();
        assert!(!own.contains("接口定好了"), "{own}");
        let _ = zs;
        let _ = std::fs::remove_dir_all(&w.base);
    }

    /// 张三建一个目标 + 任务，提交并推上去，李四同步下来。返回 `(goal_id, task_id)`。
    fn shared_task(w: &World, title: &str) -> (String, String) {
        let alice = TeamTool::new(w.alice.clone());
        let gid = id_of(
            &alice
                .run(&json!({"action": "goal_new", "title": "g"}))
                .unwrap(),
        );
        let tid = id_of(
            &alice
                .run(&json!({"action": "task_new", "goal_id": gid, "title": title}))
                .unwrap(),
        );
        let store_a = Store::discover(&w.alice).unwrap();
        git::stage_team(store_a.project_root(), ".carter").unwrap();
        git::commit_team(store_a.project_root(), ".carter", "feat: 建任务").unwrap();
        git::push(store_a.project_root()).unwrap();
        git::sync(&w.bob).unwrap();
        (gid, tid)
    }

    fn shared_goal(w: &World) -> String {
        Store::discover(&w.alice).unwrap().goals().items[0]
            .id
            .clone()
    }
}
