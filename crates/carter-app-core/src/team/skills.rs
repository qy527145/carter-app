//! 内置技能：教 agent 怎么用 `team` 工具读写 `.carter/`。
//!
//! ## 为什么技能要随项目落盘，而不是编进系统提示
//!
//! `.carter/skills/` 已经是 carter 认的**项目级技能根**（见 `skills/mod.rs` 模块头：
//! 从会话 cwd 往上找最近的 `.carter/skills`，优先级最高）。把约定写成技能有几个
//! 好处：它随仓库走，clone 下来的人立刻有同一份；用户能直接编辑它，把团队的
//! 特殊约定加进去；它不占系统提示的固定篇幅，只在模型判断相关时才展开。
//!
//! ## 为什么只有工具、没有命令行脚本
//!
//! 早先这里还装一份 `.carter/bin/carter-team`（POSIX sh + jq），技能教 agent 去调它。
//! 那条路有三个洞：Windows 上没有 sh / jq 根本跑不起来；它与 `team` 工具是两套实现，
//! 校验口径、id 格式、提交粒度都对不齐，修一边漏一边（单个依赖被吞成 `[]` 就是只
//! 在脚本里有的 bug）；agent 两条路都能走，同一个仓库里就混着两种行为。
//!
//! 现在只留 `team` 工具一个入口：三平台一致，写前过校验器，一轮收成一条提交。
//! 已初始化仓库里的旧脚本由 [`retire_script`] 清掉（只删我们自己写的版本）。
//!
//! ## 只补缺，不覆盖 —— 例外是「教的写法已经失效」的那几篇
//!
//! 用户改过的 `SKILL.md` 不会被下次初始化冲掉。但旧版技能如果教的是新版已经不认
//! 的写法（看板从 JSON 改成 HTML、从调脚本改成调工具），留着它等于让模型照着旧
//! 说明书干活。这类技能带一个版本标记（见 [`SKILL_VERSIONS`]），只升级我们自己
//! 写的旧版；用户改过的（标记被删、或开头不再是我们写的样子）一律不碰。

use std::path::{Path, PathBuf};

/// 内置技能的清单：目录名 + 正文。
///
/// 每个技能的 `description` 都写成一句**可判定**的话（什么情况下该读它），
/// 因为模型是靠这句话决定要不要展开的。
const BUILTINS: &[(&str, &str)] = &[
    ("carter-team", CARTER_TEAM),
    ("carter-goal", CARTER_GOAL),
    ("carter-task", CARTER_TASK),
    ("carter-board", CARTER_BOARD),
];

/// 把内置技能写进 `skills_dir`，返回**这一次真的写出来**的技能名。
pub fn install_builtin(skills_dir: &Path) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(skills_dir).map_err(|e| format!("建技能目录失败：{e}"))?;
    let mut written = Vec::new();
    for (name, body) in BUILTINS {
        let dir = skills_dir.join(name);
        let file = dir.join("SKILL.md");
        let action = match SKILL_VERSIONS.iter().find(|(n, ..)| n == name) {
            Some((_, version, v1_head)) => skill_action(
                std::fs::read_to_string(&file).ok().as_deref(),
                *version,
                v1_head,
            ),
            None if file.is_file() => ScriptAction::Keep,
            None => ScriptAction::Create,
        };
        if action == ScriptAction::Keep {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("建 {} 失败：{e}", dir.display()))?;
        std::fs::write(&file, body).map_err(|e| format!("写 {} 失败：{e}", file.display()))?;
        written.push(match action {
            ScriptAction::Upgrade(from) => format!("{name}（v{from} → 新版）"),
            _ => (*name).to_string(),
        });
    }
    Ok(written)
}

#[derive(Debug, PartialEq, Eq)]
enum ScriptAction {
    Create,
    Upgrade(u32),
    Keep,
}

/// 旧版命令行脚本的位置：`<项目根>/.carter/bin/carter-team`。
fn legacy_script_path(project_root: &Path) -> PathBuf {
    project_root
        .join(super::store::DIR)
        .join("bin")
        .join("carter-team")
}

/// 这份脚本是不是**我们自己写的**某一版（v1 没有版本行，靠开头认；v2 起第二行是
/// `# carter-team-version: N`）。用户换成了别的内容就不是。
fn is_our_script(text: &str) -> bool {
    text.starts_with("#!/bin/sh\n# carter-team —— 团队目录的薄壳命令。")
        || text
            .lines()
            .take(3)
            .any(|l| l.starts_with("# carter-team-version:"))
}

/// 删掉旧版初始化装进来的 `carter-team` 脚本。
///
/// 只删我们写的版本；用户自己放在那个路径上的东西不碰，只在结果里提一句。
/// `bin/` 删空了就一起删掉，不留一个空目录在仓库里。
/// 返回给界面看的一句话；什么都没做时是 `None`。
///
/// 由 `init.rs` 调，传的是它手里那个确定的项目根 —— 不从技能目录往上推两层去猜：
/// 技能目录不一定在 `<项目根>/.carter/skills`，猜错了就会去动别处的文件。
pub fn retire_script(project_root: &Path) -> Result<Option<String>, String> {
    let script = legacy_script_path(project_root);
    let Ok(text) = std::fs::read_to_string(&script) else {
        return Ok(None);
    };
    if !is_our_script(&text) {
        return Ok(Some(format!(
            "{} 不是内置脚本的任何一版，保留没动（团队数据请走 team 工具）",
            script.display()
        )));
    }
    std::fs::remove_file(&script).map_err(|e| format!("删 {} 失败：{e}", script.display()))?;
    if let Some(bin) = script.parent() {
        // 只在空了的时候删：`remove_dir` 对非空目录会失败，正好是想要的语义。
        let _ = std::fs::remove_dir(bin);
    }
    Ok(Some(
        "移除旧的 bin/carter-team（改由 team 工具负责）".into(),
    ))
}

/// 需要按版本升级的技能：`(名字, 当前版本, v1 的开头)`。
///
/// v1 没有版本标记，只能靠开头认（frontmatter 前两行加 description 开头几个字）。
///
/// carter-team / goal / task 的 v1 教 agent 调 `.carter/bin/carter-team` 脚本；v2 起
/// 只教 `team` 工具，脚本已经不装了，旧说明书必须换掉。
///
/// carter-team / task 的 v3：消息改走独立的事件链、有了成员名单。v2 说「事件落空提交」
/// 「认领把负责人设成你的 git 身份」，照着做出来的东西与 v3 的数据对不上。
const SKILL_VERSIONS: &[(&str, u32, &str)] = &[
    (
        "carter-team",
        3,
        "---\nname: carter-team\ndescription: 项目里有 .carter/ 团队目录时，先读这一篇",
    ),
    (
        "carter-goal",
        2,
        "---\nname: carter-goal\ndescription: 需要新建 / 修改 / 归档一个团队目标",
    ),
    (
        "carter-task",
        3,
        "---\nname: carter-task\ndescription: 需要拆解任务、更新任务状态、认领任务",
    ),
    (
        "carter-board",
        2,
        "---\nname: carter-board\ndescription: 需要做团队回顾、建一块看板、往看板上加卡片",
    ),
];

/// 现有技能内容 → 要不要写。规则同脚本：只升级我们自己写的旧版。
fn skill_action(existing: Option<&str>, current: u32, v1_head: &str) -> ScriptAction {
    let Some(text) = existing else {
        return ScriptAction::Create;
    };
    let version = text
        .lines()
        .take(8)
        .find_map(|l| l.trim().strip_prefix("<!-- carter-skill-version:"))
        .and_then(|v| v.trim_end_matches("-->").trim().parse::<u32>().ok());
    match version {
        Some(v) if v < current => ScriptAction::Upgrade(v),
        Some(_) => ScriptAction::Keep,
        None if text.starts_with(v1_head) => ScriptAction::Upgrade(1),
        None => ScriptAction::Keep,
    }
}

// ---------------------------------------------------------------------------
// 技能正文
// ---------------------------------------------------------------------------

const CARTER_TEAM: &str = r#"---
name: carter-team
description: 项目里有 .carter/ 团队目录时，先读这一篇：目标 / 任务 / 看板怎么组织，所有读写都通过 team 工具完成（不要手改文件），以及成员、指派、认领、@ / 广播、同步、校验的规矩。只要这次任务涉及「现在有什么目标 / 我手上有什么任务 / 把这件活记下来 / 更新进展」，就读它。
---
<!-- carter-skill-version: 3 -->

# 团队目录：`.carter/`

这个项目用 `.carter/` 记录**目标、任务、看板**。它们是团队的公共账本，进 git、
所有人共享。你的每一次改动别人都能看到，所以按下面的规矩来。

## 一律用 `team` 工具

读、建、改、认领、评论、同步、校验，**全部**通过 `team` 工具，参数里的 `action`
决定做什么。它在 Windows / macOS / Linux 上行为一致，会替你生成 id、算排序、
在写盘前校验、并在这一轮结束时把改动收成一条 git 提交。

**不要**用 `write_file` / `edit_file` / bash 直接改 `.carter/` 下的文件，也不要自己跑
`git commit` 提交它们：手写的 JSON 很容易「语法对但语义坏」，那种错误不会当场报，
而是几天后在别人那里表现为任务永远开不了工。

| 想做的事 | 调用 |
|---|---|
| 看全景（目标、任务树、进度、谁在等谁） | `{ "action": "overview" }` |
| 我名下还没做完的 | `{ "action": "my_tasks" }` |
| 新建 / 修改目标 | `goal_new` / `goal_update` —— 见 `carter-goal` |
| 新建 / 修改任务、推进状态 | `task_new` / `task_update` / `task_status` —— 见 `carter-task` |
| 团队里有谁 | `{ "action": "members" }` |
| 指派 / 转交任务 | `{ "action": "assign", "task_id": "…", "assignee": "成员 id 或昵称" }` |
| 认领 / 释放任务 | `task_claim` / `task_release` |
| 对任务 / 目标留评论 | `{ "action": "comment", "task_id" 或 "goal_id": "…", "text": "…" }` |
| @ 某人 | `{ "action": "mention", "to": ["昵称或 id"], "text": "…" }`（可带 `task_id`） |
| 请人评审 / 拍板 | `{ "action": "request", "to": […], "text": "…", "due_at": 毫秒 }` |
| 全员广播 | `{ "action": "broadcast", "text": "…" }` |
| 跟我有关的消息 | `{ "action": "inbox" }` |
| 最近的团队动态 | `{ "action": "events", "limit": 30 }` |
| 看板 | `board_list` / `board_new` / `board_update` —— 见 `carter-board` |
| 全库校验 | `{ "action": "check" }` |
| 拉取远端更新 | `{ "action": "sync" }` |

参数名必须与上面完全一致，**拼错的参数会被直接拒绝**（整次调用不执行），
按报错里列出的合法参数名改正后再调一次即可。

## 先看现状再动手

回答「现在做到哪了 / 我该做什么」之前，先调一次 `overview` 或 `my_tasks`。
凭印象说「应该还没做」很容易错 —— 别人可能刚推过来一批更新。
`overview` 里任务前面的 `▶` 表示「现在就能开工」（待办、前置都完成了）。

## 硬规矩

1. **id 由工具生成。** 新建后工具回灌的文本里有单独一行 `id = …`，后续操作用它。
   不要自己编 id —— 编出来的会和别人撞，一撞就是静默覆盖别人的任务。
2. **状态落文件，消息落事件链。** `task_status` / `assign` 改的是任务文件；
   `comment` / `mention` / `broadcast` / `assign` 的通知 / 认领记录写在独立的事件链
   （`refs/carter/events`）上，不在代码分支里，写完立刻推送。不要用改描述的方式「发消息」。
3. **认领要推送成功才算数。** 有远端时 `task_claim` 会立刻推送；被拒说明别人先占了，
   工具会撤回你本地的改动并告诉你 —— 这时**不要重试覆盖**，先 `sync` 再看，
   或者换一个任务、问用户。
4. **依赖不能成环。** 写入前工具会验，验不过就换一种拆法。

## 成员与指派

团队成员在 `.carter/members/` 里，一人一个文件，有稳定的 id（`m-…`）。**引用人一律
用成员 id 或昵称**，工具会替你解析；重名时会让你改写 id。还没加入的人（新同事）
可以先写 `名字 <邮箱>`，他加入之后按邮箱就能对上。

* **拆任务时就考虑派给谁。** 拆完一个目标后先 `members` 看有谁，按「谁最近改过相关
  代码 / 谁负责这一块」给出建议，**列给用户确认后**再用 `assign` 写进去。拿不准的
  留空，放进任务池等人认领 —— 指派错人比不指派更糟。
* 指派与转交会自动给对方发一条通知（`assign` / `handoff`），不用再单独 @。
* 派给别人的任务照样能「开工」：能不能开工只看状态与前置，不看归谁。

## 什么时候发消息

你发的每一条消息都标着「经卡特」，对方能看出不是用户亲手写的。所以：

* 用户让你说的，照说。
* 完成、受阻、改了方案时，在那个任务上留一条 `comment` 说清**为什么** —— 这是以后
  回头看「这件事为什么这么做」的唯一记录。
* 不要为了「汇报我在干活」发消息，不要把测试数据发成评论，不要替用户做承诺。
* 要对方做事、需要回应的用 `request`（可以带期限），只是告知的用 `mention`。

## 写错了怎么办

写操作在落盘前都会过一遍校验器。不合格时**这次改动不会写进文件**，回灌给你的是
一份编号清单，每一条后面括号里就是该怎么改。照着改、再调一次，直到成功为止。

批量改完、或者怀疑仓库里有坏文件（比如别人手改过）时，跑 `{ "action": "check" }`。
它逐个文件检查能否解析、id 与文件名是否一致、枚举值是否合法、受阻有没有写原因、
父子与依赖有没有成环。**不要停在「我改过了，应该行了」**，要跑到它说「校验通过」。

## 枚举值的写法

| 字段 | 合法的值 |
|---|---|
| `priority` | `P0` `P1` `P2` `P3` |
| 任务 `status` | `todo` `doing` `blocked` `done` `dropped` |
| 目标 `status` | `active` `paused` `done` `archived` |

大小写写错工具也认（会归一成上表的写法），但其它说法（`high`、`finished`）会被拒绝。

## 同步需要有远端

`sync` 与认领的推送都要求仓库配了远端（不必叫 `origin`，任何一个都行）。没有远端时
团队数据只在本地，改动照样落盘、照样提交，只是传不出去 —— 这时不要反复重试，
也不要自己编一个远端地址，告诉用户「在『目标与任务』页右上角点『配远端』」即可。

## 布局（只读参考）

```
.carter/
  team.json                         项目元信息
  goals/<goal-id>/goal.json         一个目标
  goals/<goal-id>/tasks/<id>.json   一个任务（一任务一个文件）
  members/<member-id>.json          一个成员（一人一个文件）
  boards/<board-id>.html            一块看板（一整页 HTML）
  skills/                           技能（就是这一篇）
```

消息不在这些文件里，在 git 的 `refs/carter/events` 上（协议见仓库里的说明）。
知道文件在哪有助于理解 `check` 的报错，但改动请一律走工具。
"#;

const CARTER_GOAL: &str = r#"---
name: carter-goal
description: 需要新建 / 修改 / 归档一个团队目标（阶段、模块、一次迭代）时读它。目标是任务树的根，负责划分范围；具体做什么由任务表达。所有操作通过 team 工具的 goal_new / goal_update 完成。
---
<!-- carter-skill-version: 2 -->

# 目标

目标是**范围划分**：一个阶段、一个模块、一次迭代。一个项目可以有多个目标，
任务挂在目标下面。

## 什么时候该开一个新目标

* 用户的意图是「接下来一段时间做这一摊事」，而不是「做这一件具体的事」。
* 现有目标的标题已经概括不了这件事（出现了第二个主题）。
* 想按阶段切（「第一阶段做数据层，第二阶段做界面」）—— 那是两个目标。

反过来：**不要为每件小事开目标。** 目标太多等于没有目标。开之前先 `overview`
看一眼，能挂进已有目标的就挂进去。

## 新建

```json
{
  "action": "goal_new",
  "title": "第一阶段：数据层",
  "description": "为什么做、做到什么算完",
  "priority": "P1"
}
```

`title` 必填，其余可选（`priority` 默认 `P2`）。回灌里有单独一行 `id = g-…`，
接下来用它往这个目标下拆任务（见 `carter-task`）。

## 修改

```json
{ "action": "goal_update", "goal_id": "g-…", "status": "done" }
```

可以改的字段：`title`、`description`、`priority`、`status`、`assignee`（目标负责人，
写成 `名字 <邮箱>`，传空串表示清掉）。没给的字段保持原样。

## 状态

* `active` —— 正在推进，下面的任务会出现在大家的「我的任务」里。
* `paused` —— 暂时搁置（等人、等上游）。任务还在，但不算当前工作。
* `done` —— 达成。**不要删目标**：历史与回顾要靠它。
* `archived` —— 归档，界面默认折叠。

工具没有「删除目标」这个动作，这是有意的：删目标会连带删掉它下面的所有任务，
在 git 之外无法恢复。不想要了就改成 `archived`。
"#;

const CARTER_TASK: &str = r#"---
name: carter-task
description: 需要拆解任务、更新任务状态、认领任务、或者回答「我现在该做什么」时读它。任务是 .carter 里的基本执行单位，带父子关系与依赖关系；所有操作通过 team 工具的 task_new / task_update / task_status / task_claim / task_release 完成。
---
<!-- carter-skill-version: 3 -->

# 任务

一个任务 = 一件可以独立说「做完了」的事，挂在某个目标下面。

## 父子 vs 依赖：两种不同的关系

* **父子**（`parent_id`）—— 「这件事由哪几件事组成」，拆解用。父任务的进度由子任务
  算出来，它自己的状态不必手动维护得很细。
* **依赖**（`depends_on`）—— 「这件事要等谁先完成」，先后用。

两者**分开写，不要混**：把依赖写成 `parent_id` 会让进度算错，画图时布局也会乱。
父任务与依赖都必须在**同一个目标**里。

## 新建

```json
{
  "action": "task_new",
  "goal_id": "g-…",
  "title": "写序列化",
  "description": "做法、注意事项",
  "parent_id": "t-…",
  "depends_on": ["t-…", "t-…"],
  "priority": "P1",
  "assignee": "小张",
  "estimate_hours": 4,
  "deliverable": "一句话说清交什么",
  "acceptance": ["cargo test 通过", "golden fixture 往返一致"]
}
```

只有 `goal_id` 与 `title` 必填。`depends_on` 是**数组**，一个依赖也写成
`["t-…"]`。回灌里有单独一行 `id = t-…`，拆下一层时用它当 `parent_id`，
排先后时把它放进别的任务的 `depends_on`。

拆一棵树的顺序：先建父任务拿到 id，再建子任务；先建被依赖的，再建依赖它的。

## 推进状态

```json
{ "action": "task_status", "task_id": "t-…", "status": "doing" }
{ "action": "task_status", "task_id": "t-…", "status": "blocked", "blocked_reason": "等上游确认接口" }
{ "action": "task_status", "task_id": "t-…", "status": "done" }
```

* `blocked` 必须带 `blocked_reason`，否则会被拒绝。
* 改成 `done` 时，回灌会告诉你**哪些任务因此可以开工了** —— 那就是下一步。
* **每真的推进一步就更新一次**，不要攒到最后。状态是别人判断「能不能开工」的唯一
  依据，落后于实际的状态比没有状态更坏。

## 修改其它字段

```json
{ "action": "task_update", "task_id": "t-…", "title": "…", "depends_on": ["t-…"] }
```

可改：`title`、`description`、`priority`、`assignee`、`parent_id`、`depends_on`、
`estimate_hours`、`deliverable`、`acceptance`。没给的字段保持原样；
`depends_on` / `acceptance` 给了就是**整列替换**；`parent_id` 传空串变回根任务。

## 指派与认领

```json
{ "action": "assign", "task_id": "t-…", "assignee": "小张", "text": "接口文档在 docs/api.md" }
{ "action": "task_claim", "task_id": "t-…" }
{ "action": "task_release", "task_id": "t-…" }
```

* **指派**（`assign`）：把任务交给某个人，对方会收到通知。原来有人负责、换成别人的
  就是「转交」。`task_new` / `task_update` 里直接写 `assignee` 也会通知。
* **认领**（`task_claim`）：用户自己说「这件我来」。负责人设成用户在团队里的成员 id，
  在事件链上记一条，有远端时立刻推送；**推送失败、或者链上有人先认领了，都算认领
  没成**，工具会撤回本地改动并说是谁 —— 这时换个任务，不要覆盖。
* 已经归别人的任务，`task_claim` 会直接拒绝；真要换人，问过用户之后用 `assign`。

## 拆到多细

一个任务应该是**一个人在一两天内能说「做完了」**的粒度。再大就拆，再小就别建 ——
一堆「改一个字段」的任务只会让人不再看任务图。

`acceptance` 写可验证的标准（「cargo test 通过」），不要写「质量好」。
"#;

const CARTER_BOARD: &str = r#"---
name: carter-board
description: 需要做团队回顾、阶段小结、风险盘点，建一块看板或改一块现有看板，或者读看板来总结「大家在关心什么」时读它。看板是一整页由你自由排版的 HTML。
---
<!-- carter-skill-version: 2 -->

# 看板

看板是一页 **HTML**：`.carter/boards/<id>.html`。回顾、阶段小结、风险盘点、方案对比
都行，版式完全由你决定 —— 几列卡片、表格、时间线、图表，按内容挑最清楚的那种。

看板**不**替代任务 —— 看板上写的是「观点」与「结论」，任务是「活」。回顾里冒出来要做
的事，另外用 `task_new` 建成任务，再在看板上写上任务 id（`t-…`）指过去。

## 怎么建、怎么改

用 `team` 工具，不要手写文件（id 要由工具发，标题与关联也由它补进页面）：

* 新建：`board_new` { title, html, goal_id? }
* 改：`board_update` { board_id, html?, title?, goal_id? } —— 给了 `html` 就**整页替换**，
  所以先 `board_list` 找到文件、读出原文，在原文上改完再整页交回去，别把别人加的内容丢了。

一轮结束时会自动提交，不需要自己 `git commit`。

## 页面约定

```html
<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <title>十月回顾</title>                          <!-- 列表里显示的名字 -->
  <meta name="carter-goal" content="g-...">        <!-- 可选：关联的目标 -->
  <style>
    /* 用 var(--text) / var(--bg) 等变量，界面会注入当前主题的值 */
  </style>
</head>
<body>…</body>
</html>
```

* **一个文件自给自足。** 样式、脚本、SVG 全部内联。页面在沙箱里显示，读不到仓库里
  的其他文件，相对路径的 `<img src="./a.png">` 不会显示。要用图表库可以走 CDN。
* **跟着应用主题走。** 界面会往页面里注入这些 CSS 变量（浅色 / 深色自动切换）：
  `--bg` `--bg-card` `--bg-elevated` `--text` `--text-dim` `--border`
  `--accent` `--ok` `--danger` `--tone-blue` `--tone-green` `--tone-orange`
  `--tone-violet` `--tone-pink`（及对应的 `-soft` 背板色）。
  用它们而不是写死 `#fff` / `#000`，深色模式下才不会一片刺眼的白。
* 页面里**不能**用 `localStorage`、不能弹窗跳转主界面 —— 它只是一块展示板。
* 人名写成 `名字`，任务与目标写 id（`t-…` / `g-…`），便于别人对照。

## 写回顾的建议

* 一条只说一件事。三件事揉在一条里，讨论就没法聚焦。
* 「要改的」每一条都尽量带一个可尝试的动作，否则下一轮回顾会发现它原封不动还在。
* 复盘时先读全文再总结，别只看醒目的那几条 —— 一笔带过的往往是没人愿意说的那件事。
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_skills_are_written_once() {
        let dir = std::env::temp_dir().join(format!("carter-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = install_builtin(&dir).unwrap();
        assert!(first.iter().any(|s| s == "carter-team"));
        assert!(dir.join("carter-team/SKILL.md").is_file());

        // 用户改过之后，再装一次不能覆盖。
        std::fs::write(dir.join("carter-team/SKILL.md"), "我改的\n").unwrap();
        let second = install_builtin(&dir).unwrap();
        assert!(!second.iter().any(|s| s == "carter-team"), "已有的不重写");
        assert_eq!(
            std::fs::read_to_string(dir.join("carter-team/SKILL.md")).unwrap(),
            "我改的\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_skill_has_frontmatter_the_parser_will_take() {
        for (name, body) in BUILTINS {
            assert!(body.starts_with("---\n"), "{name} 缺 frontmatter");
            let rest = &body[4..];
            let end = rest.find("\n---").expect("frontmatter 要闭合");
            let head = &rest[..end];
            assert!(
                head.contains(&format!("name: {name}")),
                "{name} 的 name 对不上"
            );
            assert!(
                head.lines()
                    .any(|l| l.starts_with("description: ") && l.len() > 40),
                "{name} 的 description 要写清什么时候读它"
            );
        }
    }

    #[test]
    fn the_old_json_board_skill_is_upgraded_but_user_edits_are_kept() {
        let (_, version, v1_head) = *SKILL_VERSIONS
            .iter()
            .find(|(n, ..)| *n == "carter-board")
            .unwrap();
        assert!(CARTER_BOARD.contains(&format!("<!-- carter-skill-version: {version} -->")));
        assert_eq!(
            skill_action(Some(CARTER_BOARD), version, v1_head),
            ScriptAction::Keep
        );
        // 旧版（教模型写 board.json 的那篇）原样留着：升级。
        let v1 = format!("{v1_head}，或者读现有看板来总结。\n---\n# 看板\n");
        assert_eq!(
            skill_action(Some(&v1), version, v1_head),
            ScriptAction::Upgrade(1)
        );
        // 用户自己重写过的：不碰。
        assert_eq!(
            skill_action(
                Some("---\nname: carter-board\ndescription: 我们组自己的\n---\n"),
                version,
                v1_head
            ),
            ScriptAction::Keep
        );

        let dir = std::env::temp_dir().join(format!("carter-skills-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("carter-board")).unwrap();
        std::fs::write(dir.join("carter-board/SKILL.md"), &v1).unwrap();
        let written = install_builtin(&dir).unwrap();
        assert!(
            written.iter().any(|s| s.starts_with("carter-board（v1")),
            "{written:?}"
        );
        let body = std::fs::read_to_string(dir.join("carter-board/SKILL.md")).unwrap();
        assert!(body.contains("board_new"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 三篇改成「只教 team 工具」的技能：旧版（教 agent 调脚本的那版）要升级，
    /// 当前版与用户改过的不碰。
    #[test]
    fn script_era_skills_are_upgraded_to_the_tool_only_versions() {
        let bodies = [
            ("carter-team", CARTER_TEAM),
            ("carter-goal", CARTER_GOAL),
            ("carter-task", CARTER_TASK),
        ];
        for (name, body) in bodies {
            let (_, version, v1_head) = *SKILL_VERSIONS
                .iter()
                .find(|(n, ..)| *n == name)
                .unwrap_or_else(|| panic!("{name} 不在版本表里"));
            assert!(
                body.starts_with(v1_head.split("\ndescription:").next().unwrap()),
                "{name} 的 frontmatter 开头变了"
            );
            assert!(
                body.contains(&format!("<!-- carter-skill-version: {version} -->")),
                "{name} 要带版本标记"
            );
            assert_eq!(
                skill_action(Some(body), version, v1_head),
                ScriptAction::Keep
            );
            // v1：开头一致、没有版本标记。
            let v1 = format!("{v1_head}……（旧版正文，教 agent 调 carter-team 脚本）\n---\n");
            assert_eq!(
                skill_action(Some(&v1), version, v1_head),
                ScriptAction::Upgrade(1)
            );
        }
    }

    /// 新版技能里不能再出现「去调脚本」的指引。
    #[test]
    fn no_skill_tells_the_agent_to_run_the_script() {
        for (name, body) in BUILTINS {
            assert!(
                !body.contains("bin/carter-team") && !body.contains("carter-team <"),
                "{name} 还在教 agent 调命令行脚本"
            );
        }
    }

    /// 每篇技能里出现的 `"action": "xxx"` 都必须是工具真认的动作 ——
    /// 文档与工具对不上，模型照着写就是一次被拒的调用。
    #[test]
    fn every_action_named_in_the_skills_exists_on_the_tool() {
        use carter::tools::Tool;
        let tool = super::super::tool::TeamTool::new(std::env::temp_dir());
        let schema = tool.parameters();
        let legal: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        for (name, body) in BUILTINS {
            for piece in body.split("\"action\": \"").skip(1) {
                let action = piece.split('"').next().unwrap();
                assert!(
                    legal.contains(&action),
                    "{name} 提到的 action `{action}` 工具不认"
                );
            }
        }
    }

    #[test]
    fn our_old_script_is_retired_but_a_users_own_file_is_kept() {
        let root = std::env::temp_dir().join(format!("carter-retire-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join(super::super::store::DIR).join("bin");
        std::fs::create_dir_all(&bin).unwrap();

        // 没有脚本：什么都不做。
        std::fs::remove_dir(&bin).unwrap();
        assert_eq!(retire_script(&root).unwrap(), None);

        // v1（无版本行）与 v2（有版本行）都是我们写的：删掉，bin/ 空了一起删。
        for text in [
            "#!/bin/sh\n# carter-team —— 团队目录的薄壳命令。\nset -eu\n",
            "#!/bin/sh\n# carter-team-version: 2\n# carter-team —— …\n",
        ] {
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("carter-team"), text).unwrap();
            assert!(retire_script(&root).unwrap().is_some());
            assert!(!bin.join("carter-team").exists());
            assert!(!bin.exists(), "bin/ 空了就不该留着");
        }

        // bin/ 里还有用户自己的别的东西：删脚本，不删目录。
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("carter-team"),
            "#!/bin/sh\n# carter-team-version: 2\n",
        )
        .unwrap();
        std::fs::write(bin.join("mine.sh"), "echo hi\n").unwrap();
        retire_script(&root).unwrap();
        assert!(bin.join("mine.sh").is_file());

        // 用户自己放在同一路径上的东西：不碰，只提一句。
        std::fs::write(bin.join("carter-team"), "#!/bin/bash\necho ours\n").unwrap();
        let note = retire_script(&root).unwrap().unwrap();
        assert!(note.contains("保留"), "{note}");
        assert!(bin.join("carter-team").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fresh_install_writes_no_script() {
        let root = std::env::temp_dir().join(format!("carter-noscript-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let skills = root.join(super::super::store::DIR).join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        install_builtin(&skills).unwrap();
        assert!(!root.join(super::super::store::DIR).join("bin").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
