//! 内置技能与同步脚本。
//!
//! ## 为什么技能要随项目落盘，而不是编进系统提示
//!
//! `.carter/skills/` 已经是 carter 认的**项目级技能根**（见 `skills.rs` 模块头：
//! 从会话 cwd 往上找最近的 `.carter/skills`，优先级最高）。把约定写成技能有几个
//! 好处：它随仓库走，clone 下来的人立刻有同一份；用户能直接编辑它，把团队的
//! 特殊约定加进去；它不占系统提示的固定篇幅，只在模型判断相关时才展开。
//!
//! ## 为什么同时给一个命令行脚本
//!
//! 模型手拼 JSON 会出错 —— 少一个字段、把 `order` 写成字符串、把依赖写漏，都不会
//! 立刻报错，而是三天后在别人的界面上表现为「任务永远开不了工」。所以这里给一个
//! 薄壳命令 `carter-team`，把校验、排序、原子写、git 提交都收进去，技能里明确
//! 要求模型**用它而不是手改文件**。
//!
//! 脚本只覆盖「创建 / 改状态 / 认领 / 评论 / 同步」这些高频操作；看板这类低频且
//! 结构复杂的东西仍由模型直接写 JSON（技能里有格式说明）。
//!
//! ## 只补缺，不覆盖
//!
//! 用户改过的 `SKILL.md` 与脚本不会被下次初始化冲掉。判断依据是「文件存不存在」，
//! 不比对内容 —— 内容比对会让「恢复出厂设置」变得不可预期。

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
        if file.is_file() {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("建 {} 失败：{e}", dir.display()))?;
        std::fs::write(&file, body).map_err(|e| format!("写 {} 失败：{e}", file.display()))?;
        written.push((*name).to_string());
    }
    let script = sync_script_path(&skills_dir.join("..").join(".."));
    match script_action(std::fs::read_to_string(&script).ok().as_deref()) {
        ScriptAction::Keep => {}
        action => {
            if let Some(parent) = script.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("建 {} 失败：{e}", parent.display()))?;
            }
            std::fs::write(&script, SYNC_SCRIPT)
                .map_err(|e| format!("写 {} 失败：{e}", script.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
            }
            written.push(match action {
                ScriptAction::Upgrade(from) => format!("bin/carter-team（v{from} → v{SCRIPT_VERSION}）"),
                _ => "bin/carter-team".into(),
            });
        }
    }
    Ok(written)
}

/// 脚本的版本。改了 [`SYNC_SCRIPT`] 就加一。
///
/// 技能文档「只补缺不覆盖」是对的：那是给团队改的说明书。脚本不一样 —— 它是
/// 工具，不是文档，旧版里的 bug（单个依赖被吞成 []、非法枚举照写、评论卷入用户
/// 暂存的文件）不升级就一直在每个已初始化的仓库里。所以按版本标记升级：
/// 只覆盖**我们自己写的旧版本**，用户动过的（标记没了或改了）一律不碰。
pub const SCRIPT_VERSION: u32 = 2;

#[derive(Debug, PartialEq, Eq)]
enum ScriptAction {
    Create,
    Upgrade(u32),
    Keep,
}

/// 现有脚本内容 → 要不要写。纯函数，单独测。
fn script_action(existing: Option<&str>) -> ScriptAction {
    let Some(text) = existing else {
        return ScriptAction::Create;
    };
    // v1 没有版本行：认它的唯一办法是对照它的开头。只有原样的 v1 才升级 ——
    // 用户在 v1 上改过（开头没变但后面动了）的话也会被升级，这是有意的取舍：
    // v1 的依赖解析是坏的，留着它比覆盖掉一处本地改动更糟。
    const V1_HEAD: &str = "#!/bin/sh\n# carter-team —— 团队目录的薄壳命令。";
    let version = text
        .lines()
        .take(3)
        .find_map(|l| l.strip_prefix("# carter-team-version:"))
        .and_then(|v| v.trim().parse::<u32>().ok());
    match version {
        Some(v) if v < SCRIPT_VERSION => ScriptAction::Upgrade(v),
        Some(_) => ScriptAction::Keep,
        None if text.starts_with(V1_HEAD) => ScriptAction::Upgrade(1),
        // 既不是我们写的任何一版，就是用户自己的东西。
        None => ScriptAction::Keep,
    }
}

/// 团队目录下的同步脚本路径：`<项目根>/.carter/bin/carter-team`。
pub fn sync_script_path(project_root: &Path) -> PathBuf {
    project_root.join(super::store::DIR).join("bin").join("carter-team")
}

// ---------------------------------------------------------------------------
// 技能正文
// ---------------------------------------------------------------------------

const CARTER_TEAM: &str = r#"---
name: carter-team
description: 项目里有 .carter/ 团队目录时，先读这一篇了解目标是怎样组织的、能做什么操作、以及有哪些硬规矩（不要手改 id、状态推进要走 carter-team 命令、认领要 push 成功才算数）。只要这次任务涉及「看看现在有什么目标 / 我手上有什么任务 / 把这件活记下来 / 更新进展」，就读它。
---

# 团队目录：`.carter/`

这个项目用 `.carter/` 记录**目标、任务、看板**。它们是团队的公共账本，进 git、
所有人共享。你的每一次改动别人都能看到，所以按下面的规矩来。

## 布局

```
.carter/
  team.json                     项目元信息
  goals/categories.json         目标分类（阶段 / 模块）
  goals/<goal-id>/goal.json     一个目标
  goals/<goal-id>/tasks/<id>.json   一个任务（一任务一个文件）
  boards/<board-id>/board.json  一块看板
  boards/<board-id>/cards/<id>.json 一张卡
  bin/carter-team               命令行入口
  skills/                       技能（就是这一篇）
```

## 首选：`team` 工具（三平台都一样）

你有 `team` 工具就**优先用它**，不要走命令行脚本：

| 想做的事 | 动作 |
|---|---|
| 看全景 | `team` { action: "overview" } |
| 我名下有什么 | `team` { action: "my_tasks" } |
| 新建目标 / 任务 | `goal_new` / `task_new` |
| 改状态 | `task_status` |
| 认领 / 释放 | `task_claim` / `task_release` |
| 留一条评论 | `comment` |
| 全库校验 | `check` |
| 拉远端 | `sync` |

工具会算顺序值、验依赖、原子写文件、在落盘前校验，一轮结束时自动提交。
写失败时回灌给你的是**带「该怎么改」的清单**，照着改再调一次即可。

## 备选：`carter-team` 脚本（仅 Linux / macOS）

仓库里有 `.carter/bin/carter-team`，是人用的命令行入口。它是一个 `#!/bin/sh` +
`jq` 的脚本，**只在 Linux 与 macOS 上能跑**。Windows 上没有 `sh`、通常也没有
`jq`，不要去试 —— Windows 上一律走 `team` 工具，功能完全一样。

即使你在 Linux / macOS 上，脚本也只是备选：它能做的 `team` 工具都能做，而工具
还多一层校验与结构化回灌。

**唯一直写 JSON 的场合**是看板（`boards/`），因为它结构复杂、改动频率低。格式见
`carter-board` 技能 —— 无论哪个平台都这样。

## 硬规矩

1. **不要自己编 id。** 新建一律走 `team` 工具，它生成 `t-<时间戳>-<随机>` 形式的
   id。手写的 id 会和别人的撞，一撞就是静默覆盖别人的任务。
2. **状态推进落文件，事件落空提交。** `task_status` 改文件；
   `comment` / `task_claim` 打的是空提交。不要用改文件的方式「发消息」。
3. **认领要 push 成功才算数。** 认领会先打空提交再 push；push 被拒说明别人先占了，
   工具会报出来并**撤回本地改动** —— 这时不要重试覆盖，改做别的任务，或者让用户决定。
4. **依赖不能成环。** 加依赖时工具会验；验不过就换一种拆法。
5. **别动 `id` 字段、别改文件名。** 文件名就是 id。

## 枚举值的写法（踩过坑，照这里写）

**大小写都认，但写的时候一律按下面这几种写法**，别自己发明：

| 字段 | 合法的值 |
|---|---|
| `priority` | `P0` `P1` `P2` `P3`（大写 P） |
| 任务 `status` | `todo` `doing` `blocked` `done` `dropped` |
| 目标 `status` | `active` `paused` `done` `archived` |

`priority` 曾经因为大小写不一致出过事：脚本写 `"P3"`、某版应用只认 `"p3"`，
结果是「目标建出来了、界面读不出来」，报错只说到 `unknown variant`。
现在读取两边都认，但你写的时候请用上表的规范形式 —— 归一化只在写回时发生。

## 改完要校验

写任何东西之后跑一次 `team` { action: "check" }（或在 Linux / macOS 上跑
`.carter/bin/carter-team check`）。

它会逐个文件检查：能不能解析、id 与文件名/目录名是否一致、owner 是不是
`名字 <邮箱>`、受阻任务有没有写原因、父子与依赖有没有成环、卡片挂的列存不存在。

**输出里每条问题后面括号里就是该怎么改**。照着改，然后**再跑一次 check**，
直到它说「校验通过」为止 —— 不要停在「我改过了，应该行了」。

写操作本身就在落盘前验过一遍，所以工具报错时你不用再跑 check，直接按回灌的
清单改就行。**手动改过 JSON 之后一定要跑一次 check。**

## 先看现状再动手

`team` { action: "overview" } —— 目标 + 任务树 + 进度 + 谁在等谁，一次给你。
（人自己看的话，Linux / macOS 上也可以 `carter-team list` / `mine`。）

回答用户之前先看一眼，比凭印象说「应该还没做过」可靠得多。

## 同步需要有远端

`sync` 与认领的 push 都要求这个仓库配了远端。没有远端时团队数据只在本地，
所有改动照样落盘、照样提交，只是传不出去 —— 这时不要反复重试同步，也不要
自己编一个远端地址；告诉用户「在界面上点『配远端』填一个仓库地址」即可。

远端**不必叫 `origin`**，叫 `upstream` 也行 —— 工具会自动找。
"#;

const CARTER_GOAL: &str = r#"---
name: carter-goal
description: 需要新建 / 修改 / 归档一个团队目标（阶段、模块、一次迭代）时读它。目标是任务树的根，负责划分范围；具体做什么由任务表达。
---

# 目标

目标（`goals/<goal-id>/goal.json`）是**范围划分**：一个阶段、一个模块、一次迭代。
一个项目可以有多个目标，任务挂在目标下面。

## 什么时候该开一个新目标

* 用户的意图是「接下来一段时间做这一摊事」，而不是「做这一件具体的事」。
* 现有目标的标题已经概括不了这件事（出现了第二个主题）。
* 想按阶段切（「第一阶段做数据层，第二阶段做界面」）—— 那是两个目标。

反过来说：**不要为每件小事开目标。** 目标太多等于没有目标，界面上会变成一列
分不清主次的卡片。

## 创建

```sh
.carter/bin/carter-team goal new "第一阶段：数据层" --desc "把本地存储换成可合并的结构"
```

## 字段

```jsonc
{
  "schema_version": 1,
  "kind": "goal",
  "id": "g-...",                  // 命令生成，别手改
  "title": "第一阶段：数据层",
  "description": "为什么做、做到什么算完",
  "status": "active",             // active | paused | done | archived
  "priority": "P1",               // P0 | P1 | P2 | P3
  "category_id": null,            // 指向 goals/categories.json 里的一条
  "owner": null,                  // "名字 <邮箱>"，与 git 提交身份一致
  "labels": [],
  "order": 1790000000000.0,       // 排序，浮点插值，命令会算
  "start_at": null,
  "due_at": null,
  "created_at": 1790000000000,
  "created_by": "名字 <邮箱>",
  "updated_at": null,
  "updated_by": null
}
```

## 状态

* `active` —— 正在推进，节点会出现在「我的任务」里。
* `paused` —— 暂时搁置（等人、等上游）。任务还在，但不算当前工作。
* `done` —— 达成。**不要删**目标：历史与回顾要靠它。
* `archived` —— 归档，界面默认折叠。

## 改

```sh
.carter/bin/carter-team goal status g-xxx done
.carter/bin/carter-team goal edit g-xxx --title "新标题" --priority P0
```

**不要为了「整理」而删目标。** 删了任务就跟着没了（`goals/<id>/` 整个目录），
那是不可恢复的（除非从 git 历史里翻）。
"#;

const CARTER_TASK: &str = r#"---
name: carter-task
description: 需要拆解任务、更新任务状态、认领任务、或者回答「我现在该做什么」时读它。任务是 .carter 里的基本执行单位，一个任务一个文件，带父子关系与依赖关系。
---

# 任务

一个任务 = 一件可以独立说「做完了」的事。存在
`goals/<goal-id>/tasks/<task-id>.json`，**一个任务一个文件**。

## 父子 vs 依赖：两种不同的关系

* **父子**（`parent_id`）—— 「这件事由哪几件事组成」。拆解用。父任务自己没有状态意义，
  它的进度由子任务算出来。
* **依赖**（`depends_on`）—— 「这件事要等谁先完成」。先后用。与拆解无关：
  一个任务可以依赖另一个目标分支里的任务。

两者在数据上**分开存**，不要混：把依赖写成 `parent_id` 会让父子进度算错，
而且画图时布局会乱。

## 创建

```sh
# 挂在目标根上
.carter/bin/carter-team task new g-xxx "写数据结构" --desc "..." --estimate 4

# 作为某个任务的子任务
.carter/bin/carter-team task new g-xxx "写序列化" --parent t-yyy

# 带依赖：等 t-a 和 t-b 完成才能开始
.carter/bin/carter-team task new g-xxx "联调" --depends t-a,t-b
```

写完回灌给你的是任务的 id，后面要用它。

## 更新进展

```sh
.carter/bin/carter-team task status t-xxx doing
.carter/bin/carter-team task status t-xxx done
.carter/bin/carter-team task status t-xxx blocked --reason "等上游确认接口"
```

**每次真的推进一步就更新一次**，不要攒到最后一次性改。状态是别人判断「能不能开工」
的唯一依据，落后于实际的状态比没有状态更坏。

## 认领

```sh
.carter/bin/carter-team task claim t-xxx
.carter/bin/carter-team task release t-xxx
```

认领打一条空提交并 push。**push 被拒 = 别人已经占了**，命令会明说。这时老老实实
换个任务，不要去覆盖。

## 字段

```jsonc
{
  "schema_version": 1,
  "kind": "task",
  "id": "t-...",
  "goal_id": "g-...",
  "parent_id": null,              // 拆解用
  "title": "写数据结构",
  "description": "",
  "status": "todo",               // todo | doing | blocked | done | dropped
  "priority": "P2",
  "assignee": null,               // "名字 <邮箱>"
  "depends_on": [],               // 先后用，不能成环
  "order": 1790000000000.0,
  "estimate_hours": 4.0,
  "deliverable": "一句话说明交什么",
  "acceptance": ["可验证的标准一", "标准二"],
  "blocked_reason": "",
  "due_at": null,
  "created_at": 1790000000000,
  "created_by": "...",
  "updated_at": null,
  "updated_by": null
}
```

## 拆到多细

一个任务应该是**一个人在一天到两天的跨度里能说完「做完了」**的粒度。再大就拆，
再小就别建 —— 建一堆「改一个字段」的任务，看板上全是噪音，人反而不看它。

`acceptance` 写可验证的标准（「cargo test 通过」），不要写「质量好」。
"#;

const CARTER_BOARD: &str = r#"---
name: carter-board
description: 需要做团队回顾、建一块看板、往看板上加卡片，或者读现有看板来总结「大家在关心什么」时读它。看板是自由形态的，卡片是话而不是活。
---

# 看板

看板是一块自由形态的板子：几列、可选几个主题分组、上面贴卡片。典型用途是**团队回顾**
（做得好的 / 要改的 / 待跟进）和**风险记录**。

看板**不**替代任务 —— 卡片是「观点」，任务是「活」。想把一条回顾意见变成实际的活，
卡片上留 `task_id` 指过去，或者另外建一个任务。

## 布局

```
.carter/boards/<board-id>/board.json     板的定义：列、分组
.carter/boards/<board-id>/cards/<id>.json 一张卡一个文件
```

看板**没有**命令，直接写 JSON（这是唯一允许手写的地方）。写完记得提交：

```sh
git add .carter/boards && git commit -m "docs(carter): 回顾看板"
```

## board.json

```jsonc
{
  "schema_version": 1,
  "kind": "board",
  "id": "b-...",
  "title": "十月回顾",
  "description": "",
  "goal_id": null,                       // null = 跨目标的团队级看板
  "columns": [
    { "id": "col-good", "name": "做得好的", "order": 1, "wip_limit": null },
    { "id": "col-bad",  "name": "要改的",   "order": 2, "wip_limit": null },
    { "id": "col-next", "name": "待跟进",   "order": 3, "wip_limit": 3 }
  ],
  "groups": [],                          // 可选：横向切主题
  "created_at": 1790000000000,
  "created_by": "名字 <邮箱>",
  "updated_at": null,
  "updated_by": null
}
```

`wip_limit` 是「这一列最多几张」。只有回顾类看板用得着 —— 待改进堆到 20 张，
就说明这次回顾没收敛。

## cards/<id>.json

```jsonc
{
  "schema_version": 1,
  "kind": "card",
  "id": "k-...",
  "board_id": "b-...",
  "column_id": "col-bad",
  "group_id": null,
  "text": "任务图那个单文件格式让两个人改同一目标时总冲突",
  "task_id": null,                       // 可选：指回一个任务
  "votes": 0,                            // 投票数，用来挑最该跟进的
  "order": 1790000000000.0,
  "created_at": 1790000000000,
  "created_by": "名字 <邮箱>",
  "updated_at": null,
  "updated_by": null
}
```

## 写回顾的建议

* 一张卡只说一件事。把三件事揉在一张卡里，投票就投不出结果。
* 「要改的」每一条都尽量带一个可尝试的动作，否则下一轮回顾会发现它原封不动还在。
* 复盘时先读全卡再总结，别只看票数高的那几张 —— 沉默的卡往往是没人愿意说的那件事。
"#;

// ---------------------------------------------------------------------------
// 同步脚本
// ---------------------------------------------------------------------------

/// `.carter/bin/carter-team`：POSIX sh 的薄壳命令。
///
/// 刻意用 sh + `git`/`jq` 而不是一个编出来的二进制：它随仓库走，任何人 clone
/// 下来就能跑，不需要为多个平台各发一份。依赖只有 `git` 与 `jq`（`jq` 在脚本
/// 开头检查，缺了直接说清楚怎么装）。
const SYNC_SCRIPT: &str = r#"#!/bin/sh
# carter-team-version: 2
# carter-team —— 团队目录的命令行入口（Linux / macOS；Windows 上请用应用里的 team 工具）。
#
# 为什么是一份脚本而不是让 agent 直接改 JSON：
# 顺序值要算、枚举要验、依赖要验、写要原子、改完要提交。手改 JSON 写错不会当场
# 报错，而是在几天后表现为「某个任务永远开不了工」。把这些收进一条命令，错在当场。
#
# 写这份脚本时要守住的几条 shell 规矩（每一条都踩过）：
# * 拿逗号分隔的列表时不要用 `printf '%s' | while read`：没有尾换行时最后一项
#   读不到，单个依赖会被静默吞成 []。这里一律用 jq 的 split 在一处解析。
# * 校验失败要能真正中止：管道里的 `exit` 只退出子 shell，外面照样往下跑。
#   所以校验都放在主 shell 里做，不放进管道。
# * 未知参数直接报错：`--prority P0` 被静默忽略，比报错糟得多。
# * 提交只限定 .carter/：不带 pathspec 的 commit 会把用户自己暂存的无关文件一起
#   提交进一条「评论」里。
#
# 依赖：git、jq。

set -eu

die() { printf 'carter-team: %s\n' "$*" >&2; exit 1; }

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || die "不在 git 仓库里"
TEAM="$ROOT/.carter"
REL=".carter"
[ -d "$TEAM" ] || die "$TEAM 不存在，先在应用里初始化团队目录"
command -v jq >/dev/null 2>&1 || die "需要 jq：brew install jq / apt install jq（Windows 上请用应用里的 team 工具）"

NOW_MS=$(($(date +%s) * 1000))
ME_NAME=$(git config user.name || true)
ME_EMAIL=$(git config user.email || true)
[ -n "$ME_NAME" ] && [ -n "$ME_EMAIL" ] || die "git 还没配身份：git config user.name / user.email"
ME="$ME_NAME <$ME_EMAIL>"

# 合法值。与应用里 model.rs 的 allowed() 一致；写进文件的一律是这里的规范形式。
PRIORITIES="P0 P1 P2 P3"
TASK_STATUSES="todo doing blocked done dropped"
GOAL_STATUSES="active paused done archived"

# ---------------------------------------------------------------------------
# 小工具
# ---------------------------------------------------------------------------

# 规范化并校验一个枚举值。$1 = 值，$2 = 合法列表，$3 = 字段名。
# priority 大小写都认、写成大写；状态写成小写。
norm_enum() {
  case "$3" in
    priority) v=$(printf '%s' "$1" | tr '[:lower:]' '[:upper:]') ;;
    *)        v=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]') ;;
  esac
  for ok in $2; do
    [ "$v" = "$ok" ] && { printf '%s' "$v"; return 0; }
  done
  die "$3 不能是「$1」，只能是：$(printf '%s' "$2" | sed 's/ / \/ /g')"
}

is_number() { printf '%s' "$1" | grep -Eq '^[0-9]+([.][0-9]+)?$'; }

need_value() { [ $# -ge 2 ] && [ -n "$2" ] || die "$1 后面要跟一个值"; }

rand() { od -An -N2 -tu2 /dev/urandom 2>/dev/null | tr -d ' ' || echo $$; }
# id 形式与应用一致：<前缀>-<十六进制毫秒>-<随机>。
newid() { printf '%s-%x-%s' "$1" "$NOW_MS" "$(rand)"; }

# 同一批里最大的 order + 1。没有文件时是 1。
next_order() {
  found=0
  for f in "$@"; do [ -f "$f" ] && { found=1; break; }; done
  [ "$found" = 1 ] || { echo 1; return; }
  jq -s 'map(.order // 0) | max + 1' "$@"
}

# 原子写：先写同目录临时文件，验过 JSON 再改名。$1 = 目标文件，内容从 stdin 来。
write_json() {
  tmp="$1.tmp"
  cat > "$tmp"
  jq -e . "$tmp" >/dev/null 2>&1 || { rm -f "$tmp"; die "生成的 JSON 不合法（这是脚本的 bug）"; }
  mv "$tmp" "$1"
}

# 只提交 .carter/，不碰用户自己暂存的其它文件。
commit_team() {
  git add -A -- "$REL"
  git diff --cached --quiet -- "$REL" && return 0
  git commit -q -m "$1" -- "$REL"
}

# 事件：空提交，同样限定在 .carter/ 上。
event() {
  # $1 kind, $2 target, $3 goal, $4 subject, $5 body
  trailer="Carter-Team: kind=$1 id=$2${3:+ goal=$3}"
  git add -A -- "$REL"
  if [ -n "${5:-}" ]; then
    git commit -q --allow-empty -m "$4" -m "$5" -m "$trailer" -- "$REL"
  else
    git commit -q --allow-empty -m "$4" -m "$trailer" -- "$REL"
  fi
}

find_goal() {
  case "$1" in g-*) ;; *) die "目标 id 应该以 g- 开头，收到「$1」" ;; esac
  p="$TEAM/goals/$1/goal.json"
  [ -f "$p" ] || die "找不到目标 $1（carter-team list 看看有哪些）"
  printf '%s' "$p"
}

find_task() {
  case "$1" in t-*) ;; *) die "任务 id 应该以 t- 开头，收到「$1」" ;; esac
  for p in "$TEAM"/goals/*/tasks/"$1".json; do
    [ -f "$p" ] && { printf '%s' "$p"; return 0; }
  done
  die "找不到任务 $1（carter-team list 看看有哪些）"
}

# 逗号分隔的 id 列表 → JSON 数组，并逐个确认存在于同一目标。
# 在主 shell 里循环，校验失败能真正中止。
id_list() {
  # $1 = "a,b,c"，$2 = 必须属于的目标 id
  out="[]"
  old_ifs=$IFS; IFS=','
  for d in $1; do
    IFS=$old_ifs
    d=$(printf '%s' "$d" | tr -d ' ')
    [ -n "$d" ] || continue
    p=$(find_task "$d")
    g=$(jq -r .goal_id "$p")
    [ "$g" = "$2" ] || die "依赖 $d 属于目标 $g，不在 $2 里"
    out=$(printf '%s' "$out" | jq -c --arg d "$d" 'if index($d) then . else . + [$d] end')
    IFS=','
  done
  IFS=$old_ifs
  printf '%s' "$out"
}

# 逗号分隔的文字 → JSON 字符串数组（验收标准用）。
str_list() { jq -cn --arg s "$1" '$s | split(",") | map(gsub("^ +| +$"; "")) | map(select(length > 0))'; }

usage() {
  cat <<'EOF'
用法：carter-team <命令>        （Windows 上请用应用里的 team 工具）

  list                          目标与任务全景
  mine                          我名下的任务
  goal new <标题> [--desc 说明] [--priority P0..P3]
  goal status <目标> <状态>      active | paused | done | archived
  goal edit <目标> [--title 标题] [--priority P] [--desc 说明]
  task new <目标> <标题> [--parent 任务] [--depends a,b] [--desc 说明]
      [--priority P] [--estimate 小时] [--deliverable 交付物] [--accept a1,a2]
  task status <任务> <状态> [--reason 原因]   todo | doing | blocked | done | dropped
  task edit <任务> [--title 标题] [--priority P] [--desc 说明] [--depends a,b]
      [--assignee "名字 <邮箱>"]
  task claim <任务>             认领：提交并推送；被拒 = 别人先占了
  task release <任务>
  comment <任务|目标> <正文>    留一条评论（空提交，永不冲突）
  check                         校验全部文件
  log [数量]                    最近的团队事件
  sync                          拉取远端更新（只做快进）
EOF
}

# ---------------------------------------------------------------------------
# 命令
# ---------------------------------------------------------------------------

cmd="${1:-help}"; [ $# -gt 0 ] && shift

case "$cmd" in
  help|-h|--help) usage ;;

  list)
    found=0
    for g in "$TEAM"/goals/*/goal.json; do
      [ -f "$g" ] || continue; found=1
      dir=$(dirname "$g")
      jq -r '"\(.title)  [\(.status)] \(.priority)  \(.id)"' "$g"
      # 按 order 排，与界面一致；文件名顺序是随机的。
      set -- "$dir"/tasks/*.json
      [ -f "$1" ] && jq -rs 'sort_by(.order)[] | "  - [\(.status)] \(.title)  (\(.id))"
          + (if .assignee then "  @" + (.assignee | split(" <")[0]) else "" end)
          + (if (.depends_on | length) > 0 then "  等：" + (.depends_on | join(",")) else "" end)' "$@"
    done
    [ "$found" = 1 ] || echo "还没有目标" ;;

  mine)
    found=0
    for t in "$TEAM"/goals/*/tasks/*.json; do
      [ -f "$t" ] || continue
      line=$(jq -r --arg me "$ME" 'select(.assignee == $me and (.status | IN("todo","doing","blocked")))
        | "[\(.status)] \(.priority) \(.title)  (\(.id))"' "$t")
      [ -n "$line" ] && { echo "$line"; found=1; }
    done
    [ "$found" = 1 ] || echo "你名下没有未完成的任务" ;;

  goal)
    sub="${1:-}"; [ $# -gt 0 ] && shift
    case "$sub" in
      new)
        title="${1:-}"; [ -n "$title" ] || die "goal new 需要标题"; shift
        desc=""; prio="P2"
        while [ $# -gt 0 ]; do
          case "$1" in
            --desc) need_value "$@"; desc="$2"; shift 2 ;;
            --priority) need_value "$@"; prio=$(norm_enum "$2" "$PRIORITIES" priority); shift 2 ;;
            *) die "goal new 不认识参数「$1」" ;;
          esac
        done
        id=$(newid g); dir="$TEAM/goals/$id"; mkdir -p "$dir/tasks"
        order=$(next_order "$TEAM"/goals/*/goal.json)
        jq -n --arg id "$id" --arg t "$title" --arg d "$desc" --arg p "$prio" \
              --arg me "$ME" --argjson now "$NOW_MS" --argjson order "$order" \
          '{schema_version:1, kind:"goal", id:$id, title:$t, description:$d,
            status:"active", priority:$p, category_id:null, owner:null, labels:[],
            order:$order, start_at:null, due_at:null,
            created_at:$now, created_by:$me, updated_at:null, updated_by:null}' \
          | write_json "$dir/goal.json"
        commit_team "feat(carter): 新建目标 $title"
        echo "$id" ;;

      status)
        [ $# -eq 2 ] || die "用法：goal status <目标> <状态>"
        p=$(find_goal "$1"); st=$(norm_enum "$2" "$GOAL_STATUSES" status)
        jq --arg s "$st" --arg me "$ME" --argjson now "$NOW_MS" \
           '.status=$s | .updated_at=$now | .updated_by=$me' "$p" | write_json "$p"
        commit_team "chore(carter): 目标 $1 -> $st"
        echo "目标 $1 现在是 $st" ;;

      edit)
        id="${1:-}"; [ -n "$id" ] || die "goal edit 需要目标 id"; shift
        p=$(find_goal "$id"); [ $# -gt 0 ] || die "goal edit 至少要改一项（--title / --priority / --desc）"
        patch='{}'
        while [ $# -gt 0 ]; do
          case "$1" in
            --title) need_value "$@"; patch=$(printf '%s' "$patch" | jq -c --arg v "$2" '.title=$v'); shift 2 ;;
            --desc) need_value "$@"; patch=$(printf '%s' "$patch" | jq -c --arg v "$2" '.description=$v'); shift 2 ;;
            --priority) need_value "$@"; v=$(norm_enum "$2" "$PRIORITIES" priority)
                        patch=$(printf '%s' "$patch" | jq -c --arg v "$v" '.priority=$v'); shift 2 ;;
            *) die "goal edit 不认识参数「$1」" ;;
          esac
        done
        jq --argjson patch "$patch" --arg me "$ME" --argjson now "$NOW_MS" \
           '. + $patch | .updated_at=$now | .updated_by=$me' "$p" | write_json "$p"
        commit_team "chore(carter): 更新目标 $id"
        echo "已更新 $id" ;;

      *) usage >&2; exit 1 ;;
    esac ;;

  task)
    sub="${1:-}"; [ $# -gt 0 ] && shift
    case "$sub" in
      new)
        [ $# -ge 2 ] || die "用法：task new <目标> <标题> [选项]"
        goal="$1"; title="$2"; shift 2
        find_goal "$goal" >/dev/null
        parent=""; deps="[]"; desc=""; prio="P2"; est="null"; deliv=""; acc="[]"
        while [ $# -gt 0 ]; do
          case "$1" in
            --parent) need_value "$@"
                      pp=$(find_task "$2")
                      [ "$(jq -r .goal_id "$pp")" = "$goal" ] || die "父任务 $2 不在目标 $goal 里"
                      parent="$2"; shift 2 ;;
            --depends) need_value "$@"; deps=$(id_list "$2" "$goal"); shift 2 ;;
            --desc) need_value "$@"; desc="$2"; shift 2 ;;
            --priority) need_value "$@"; prio=$(norm_enum "$2" "$PRIORITIES" priority); shift 2 ;;
            --estimate) need_value "$@"; is_number "$2" || die "--estimate 要一个非负数，收到「$2」"; est="$2"; shift 2 ;;
            --deliverable) need_value "$@"; deliv="$2"; shift 2 ;;
            --accept) need_value "$@"; acc=$(str_list "$2"); shift 2 ;;
            *) die "task new 不认识参数「$1」" ;;
          esac
        done
        id=$(newid t); dir="$TEAM/goals/$goal/tasks"; mkdir -p "$dir"
        order=$(next_order "$dir"/*.json)
        jq -n --arg id "$id" --arg goal "$goal" --arg t "$title" --arg d "$desc" \
              --arg parent "$parent" --arg p "$prio" --arg me "$ME" --argjson deps "$deps" \
              --argjson est "$est" --arg deliv "$deliv" --argjson acc "$acc" \
              --argjson now "$NOW_MS" --argjson order "$order" \
          '{schema_version:1, kind:"task", id:$id, goal_id:$goal,
            parent_id:(if $parent == "" then null else $parent end),
            title:$t, description:$d, status:"todo", priority:$p, assignee:null,
            depends_on:$deps, order:$order, estimate_hours:$est, deliverable:$deliv,
            acceptance:$acc, blocked_reason:"", due_at:null,
            created_at:$now, created_by:$me, updated_at:null, updated_by:null}' \
          | write_json "$dir/$id.json"
        commit_team "feat(carter): 新建任务 $title"
        echo "$id" ;;

      status)
        [ $# -ge 2 ] || die "用法：task status <任务> <状态> [--reason 原因]"
        id="$1"; st=$(norm_enum "$2" "$TASK_STATUSES" status); shift 2
        reason=""
        while [ $# -gt 0 ]; do
          case "$1" in
            --reason) need_value "$@"; reason="$2"; shift 2 ;;
            *) die "task status 不认识参数「$1」" ;;
          esac
        done
        p=$(find_task "$id")
        if [ "$st" = blocked ] && [ -z "$reason" ] && [ -z "$(jq -r '.blocked_reason // ""' "$p")" ]; then
          die "改成 blocked 要用 --reason 说清卡在哪"
        fi
        jq --arg s "$st" --arg r "$reason" --arg me "$ME" --argjson now "$NOW_MS" \
           '.status=$s | (if $r != "" then .blocked_reason=$r else . end)
            | .updated_at=$now | .updated_by=$me' "$p" | write_json "$p"
        commit_team "chore(carter): 任务 $id -> $st"
        echo "任务 $id 现在是 $st" ;;

      edit)
        id="${1:-}"; [ -n "$id" ] || die "task edit 需要任务 id"; shift
        p=$(find_task "$id"); goal=$(jq -r .goal_id "$p")
        [ $# -gt 0 ] || die "task edit 至少要改一项"
        patch='{}'
        while [ $# -gt 0 ]; do
          case "$1" in
            --title) need_value "$@"; patch=$(printf '%s' "$patch" | jq -c --arg v "$2" '.title=$v'); shift 2 ;;
            --desc) need_value "$@"; patch=$(printf '%s' "$patch" | jq -c --arg v "$2" '.description=$v'); shift 2 ;;
            --priority) need_value "$@"; v=$(norm_enum "$2" "$PRIORITIES" priority)
                        patch=$(printf '%s' "$patch" | jq -c --arg v "$v" '.priority=$v'); shift 2 ;;
            --depends) need_value "$@"; d=$(id_list "$2" "$goal")
                       printf '%s' "$d" | jq -e --arg me "$id" 'index($me) | not' >/dev/null || die "任务不能依赖自己"
                       patch=$(printf '%s' "$patch" | jq -c --argjson v "$d" '.depends_on=$v'); shift 2 ;;
            --assignee)
              [ $# -ge 2 ] || die "--assignee 后面要跟「名字 <邮箱>」，传空串表示取消"
              case "$2" in ""|*"<"*"@"*">") ;; *) die "--assignee 要写成「名字 <邮箱>」，收到「$2」" ;; esac
              patch=$(printf '%s' "$patch" | jq -c --arg v "$2" '.assignee=(if $v == "" then null else $v end)'); shift 2 ;;
            *) die "task edit 不认识参数「$1」" ;;
          esac
        done
        jq --argjson patch "$patch" --arg me "$ME" --argjson now "$NOW_MS" \
           '. + $patch | .updated_at=$now | .updated_by=$me' "$p" | write_json "$p"
        commit_team "chore(carter): 更新任务 $id"
        echo "已更新 $id" ;;

      claim|release)
        [ $# -eq 1 ] || die "用法：task $sub <任务>"
        id="$1"; p=$(find_task "$id"); goal=$(jq -r .goal_id "$p")
        current=$(jq -r '.assignee // ""' "$p")
        if [ "$sub" = claim ] && [ -n "$current" ] && [ "$current" != "$ME" ]; then
          die "$id 已经被 $current 认领了"
        fi
        cp "$p" "$p.bak"
        if [ "$sub" = claim ]; then
          jq --arg me "$ME" --argjson now "$NOW_MS" '.assignee=$me | .updated_at=$now | .updated_by=$me' "$p" | write_json "$p"
        else
          jq --arg me "$ME" --argjson now "$NOW_MS" '.assignee=null | .updated_at=$now | .updated_by=$me' "$p" | write_json "$p"
        fi
        event "$sub" "$id" "$goal" "carter/$sub $id $ME_NAME"
        if ! git remote | grep -q .; then
          rm -f "$p.bak"
          echo "已在本地$([ "$sub" = claim ] && echo 认领 || echo 释放)（还没有远端，别人暂时看不到）"
          exit 0
        fi
        # push 成功才算数：这是 git 唯一能真正串行化的地方。
        if git push -q 2>/dev/null || git push -q -u "$(git remote | head -1)" "$(git rev-parse --abbrev-ref HEAD)" 2>/dev/null; then
          rm -f "$p.bak"
          echo "$([ "$sub" = claim ] && echo 认领 || echo 释放)成功：$id"
        else
          # 被拒就把本地也退回去，否则磁盘上写着自己、远端写着别人。
          git reset -q --soft HEAD~1
          mv "$p.bak" "$p"
          git reset -q -- "$REL"
          die "推送被拒：$id 可能已经被别人占了。先 carter-team sync 再看，或者换一个任务。"
        fi ;;

      *) usage >&2; exit 1 ;;
    esac ;;

  comment)
    [ $# -eq 2 ] || die "用法：comment <任务|目标> <正文>"
    target="$1"; body="$2"; [ -n "$body" ] || die "评论不能为空"
    case "$target" in
      t-*) goal=$(jq -r .goal_id "$(find_task "$target")") ;;
      g-*) find_goal "$target" >/dev/null; goal="$target" ;;
      *) die "评论对象要是任务（t-）或目标（g-）id" ;;
    esac
    event comment "$target" "$goal" "carter/comment $target $ME_NAME" "$body"
    echo "已记录评论" ;;

  check)
    # 脚本侧只做 jq 判得出来的：JSON 合法性、必填字段、枚举、id 与文件名一致、
    # 依赖 / 父任务存在。父子环、卡片列引用等由应用里的校验器负责。
    # 问题攒进临时文件再统一数：管道里 while 的计数器出不了子 shell。
    errfile=$(mktemp); trap 'rm -f "$errfile"' EXIT INT TERM
    err() { printf '%s\n' "$1" >> "$errfile"; }
    for g in "$TEAM"/goals/*/; do
      [ -d "$g" ] || continue
      gid=$(basename "$g"); f="$g/goal.json"
      [ -f "$f" ] || { err "goals/$gid：缺 goal.json"; continue; }
      jq -e . "$f" >/dev/null 2>&1 || { err "goals/$gid/goal.json：不是合法 JSON"; continue; }
      jq -r --arg gid "$gid" '
        (if (.title // "") == "" then "缺标题" else empty end),
        (if .id != $gid then "id 是 \(.id)，与目录名不一致" else empty end),
        (if (.priority | ascii_upcase | IN("P0","P1","P2","P3")) | not then "priority 不能是 \(.priority)（P0..P3）" else empty end),
        (if (.status | ascii_downcase | IN("active","paused","done","archived")) | not then "status 不能是 \(.status)" else empty end)
      ' "$f" | while IFS= read -r l; do err "goals/$gid/goal.json：$l"; done
      for t in "$g"tasks/*.json; do
        [ -f "$t" ] || continue
        tid=$(basename "$t" .json); rel="goals/$gid/tasks/$tid.json"
        jq -e . "$t" >/dev/null 2>&1 || { err "$rel：不是合法 JSON"; continue; }
        jq -r --arg tid "$tid" --arg gid "$gid" '
          (if (.title // "") == "" then "缺标题" else empty end),
          (if .id != $tid then "id 是 \(.id)，与文件名不一致" else empty end),
          (if .goal_id != $gid then "goal_id 是 \(.goal_id)，但文件在 \($gid) 下" else empty end),
          (if (.priority | ascii_upcase | IN("P0","P1","P2","P3")) | not then "priority 不能是 \(.priority)（P0..P3）" else empty end),
          (if (.status | ascii_downcase | IN("todo","doing","blocked","done","dropped")) | not then "status 不能是 \(.status)" else empty end),
          (if .status == "blocked" and ((.blocked_reason // "") == "") then "受阻但没写 blocked_reason" else empty end),
          (if (.depends_on // []) | index($tid) then "依赖了自己" else empty end)
        ' "$t" | while IFS= read -r l; do err "$rel：$l"; done
        for d in $(jq -r '.depends_on[]?' "$t"); do
          [ -f "$g/tasks/$d.json" ] || err "$rel：依赖的 $d 不在这个目标里"
        done
        par=$(jq -r '.parent_id // ""' "$t")
        [ -z "$par" ] || [ -f "$g/tasks/$par.json" ] || err "$rel：父任务 $par 不在这个目标里"
      done
    done
    if [ -s "$errfile" ]; then
      cat "$errfile" >&2
      die "发现 $(wc -l < "$errfile" | tr -d ' ') 个问题，改完再跑一次"
    fi
    echo "校验通过" ;;

  log)
    n="${1:-20}"; is_number "$n" || die "log 的数量要是数字"
    git log --format='%h %an %ar  %s' --grep='Carter-Team: kind=' --fixed-strings "-n$n" ;;

  sync)
    git remote | grep -q . || { echo "还没有远端，团队数据只在本地"; exit 0; }
    git pull -q --ff-only || die "无法快进（本地有未提交改动，或与远端分叉），需要手动处理"
    echo "已同步" ;;

  *) usage >&2; exit 1 ;;
esac
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
            assert!(head.contains(&format!("name: {name}")), "{name} 的 name 对不上");
            assert!(
                head.lines().any(|l| l.starts_with("description: ") && l.len() > 40),
                "{name} 的 description 要写清什么时候读它"
            );
        }
    }

    #[test]
    fn the_script_is_upgraded_only_when_it_is_ours() {
        assert_eq!(script_action(None), ScriptAction::Create);
        // 原样的 v1（没有版本行）要升级：它的依赖解析会把单个依赖吞掉。
        let v1 = "#!/bin/sh\n# carter-team —— 团队目录的薄壳命令。\n...";
        assert_eq!(script_action(Some(v1)), ScriptAction::Upgrade(1));
        // 当前版本不重写。
        assert_eq!(script_action(Some(SYNC_SCRIPT)), ScriptAction::Keep);
        // 用户自己换成了别的东西：不碰。
        assert_eq!(script_action(Some("#!/bin/bash\necho mine\n")), ScriptAction::Keep);
        // 标记是更高版本（别人用新版应用初始化过）：不降级。
        let newer = "#!/bin/sh\n# carter-team-version: 99\n";
        assert_eq!(script_action(Some(newer)), ScriptAction::Keep);
    }

    #[test]
    fn script_carries_its_version_marker() {
        assert!(
            SYNC_SCRIPT
                .lines()
                .nth(1)
                .is_some_and(|l| l == format!("# carter-team-version: {SCRIPT_VERSION}")),
            "第二行必须是版本标记，升级逻辑靠它认出自己写的脚本"
        );
    }

    #[test]
    fn sync_script_is_installed_next_to_the_team_dir() {
        let root = std::env::temp_dir().join(format!("carter-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(super::super::store::DIR).join("skills")).unwrap();
        install_builtin(&root.join(super::super::store::DIR).join("skills")).unwrap();
        assert!(sync_script_path(&root).is_file(), "脚本要落在 .carter/bin/ 下");
        let body = std::fs::read_to_string(sync_script_path(&root)).unwrap();
        assert!(body.starts_with("#!/bin/sh"));
        assert!(body.contains("Carter-Team:"), "事件提交必须带 trailer");
        let _ = std::fs::remove_dir_all(&root);
    }
}
