# Code Map

给维护者的地图：**改什么去哪找**。产品说明看 README，工程约定看 AGENTS.md。

## 分层

```
前端 (src/)                 React 19，只通过 src/lib/api.ts 调后端
  ▼ invoke / 事件
src-tauri/src/lib.rs        #[tauri::command] + 事件转发 + 托盘；薄壳，不放业务
  ▼
crates/carter-app-core/     全部业务逻辑；不依赖 Tauri，能脱离 webview 测
  ▼
carter / llm_adapt_core     底层的 agent 循环、工具、会话、协议编解码
```

一个请求的典型路径：`组件 → api.ts 的封装 → tauri 命令 → core 的函数 → 返回/事件 → events.ts 分流进 store`。

## 前端（`src/`）

| 路径 | 职责 |
| ---- | ---- |
| `lib/api.ts` | **唯一**调用 `invoke` 的地方：命令签名、事件名、跨语言类型 |
| `lib/events.ts` | 事件总线接进 store；按 `session_id` 分流主对话 / 侧边聊天；增量按帧合并 |
| `store.ts` | zustand 状态根：会话、消息项、界面开关 |
| `lib/updater.ts` | 一键更新的前端状态（首页提示条与关于页共用一份） |
| `lib/sessionOps.ts` | 会话级操作的组合（新建、切换、刷新） |
| `lib/teamUi.ts` / `ticketUi.ts` | 团队看板 / 工单的纯展示逻辑（有 bun 测试） |
| `views/settings/` | 设置页各分区；`ConnectorsPage` / `SkillsPage` / `AboutPage` 是连接器、技能、更新 |
| `views/team/` | 团队看板、依赖图、任务抽屉 |
| `components/` | 对话主体、输入框、侧栏、更新提示条、工具卡片渲染 |
| `components/payload/` | 报文面板：JSON 树、SSE 时间线、可视化 |

## 应用核心（`crates/carter-app-core/src/`）

### 会话与消息

| 文件 | 职责 |
| ---- | ---- |
| `session.rs` | 会话运行器：一个会话一个 tokio 任务，独占 `Thread` |
| `sessions.rs` | 对话管理：列举、重命名、fork、删除、搜索、导出 |
| `shell.rs` | 应用状态根 `AppShell`：配置、会话表、事件广播 |
| `view.rs` | 会话历史 → 前端展示形态 |
| `history.rs` | 历史自愈：补上「有 tool_use 没有 tool_result」的断口 |
| `title.rs` / `timeline.rs` / `prompts.rs` / `images.rs` | 标题、每轮起止时刻、用户原文、图片：都只服务界面 |
| `side.rs` | 侧边聊天（`/btw`） |
| `peers.rs` + `tools/peer.rs` | 跨会话消息 |
| `protocol.rs` | App ↔ 前端 的 IPC 契约（事件与视图类型） |

### 工具

| 文件 | 职责 |
| ---- | ---- |
| `tools/mod.rs` | 工具池装配：换实现、套用户开关、外包记录层 |
| `tools/bash.rs` / `grep.rs` | 跨平台 shell、内置 ripgrep |
| `tools/multi_skill.rs` | 多根 `skill` 工具 |
| `tools/agent.rs` | 并发子 agent |
| `tools/ask_user.rs` | 一次问一组问题 |
| `tools/background.rs` | 后台长命令 |
| `tools/cwd.rs` / `control.rs` / `record.rs` | 相对路径解析、按调用控制、调用记录 |
| `tools/goal_update.rs` / `schedule.rs` | 目标汇报、定时任务 |

### 配置与模型

| 文件 | 职责 |
| ---- | ---- |
| `app_settings.rs` | 界面设置（主题、工具开关、加速/代理、自动更新） |
| `config_edit.rs` | 保真编辑 `~/.carter/config.toml`（保留注释与 `${ENV}`） |
| `catalog.rs` / `models_sync.rs` / `upstream_models.rs` | 模型目录、按需补元数据、从上游拉清单 |
| `provider_link.rs` / `templates.rs` | 用元数据装配 provider、渠道模板 |
| `paths.rs` / `platform.rs` | 应用私有路径、OS / shell 探测、PATH 补全 |

### 网络与更新

| 文件 | 职责 |
| ---- | ---- |
| `net.rs` | **所有外网下载的统一入口**：GitHub 加速站、并发抢源、子进程代理变量。改镜像策略只改这里 |
| `update.rs` | 自更新：查 Release → 挑安装包 → 下载 → 交给系统装 |

### 技能与连接器

| 文件 | 职责 |
| ---- | ---- |
| `skills.rs` | 技能扫描（项目 / 用户 / 外部根）、启停、编辑、删除。**符号链接按目录处理** |
| `connectors/mod.rs` | IM 连接器：厂商表、CLI / skill / 登录态三层探测、安装与登录 |
| `connectors/bun.rs` | 包管理器选路：优先 bun，没有就下载一份，再没有退回 npm；国内源兜底 |

### 团队与助理

| 文件 | 职责 |
| ---- | ---- |
| `team/` | 目标 / 任务 / 看板，存项目里的 `.carter/`，靠 git 同步 |
| `team/store.rs` / `model.rs` / `validate.rs` | 一实体一文件、数据模型、校验器 |
| `team/git.rs` / `events.rs` / `inbox.rs` | git 通道、事件流、收件箱 |
| `team/skills.rs` | 写进项目的内置技能（carter-team / goal / task / board） |
| `assistant/` | 常驻助理：观察源 → 评估 → 开工 → 汇报 |
| `assistant/runtime.rs` / `eval.rs` / `memory.rs` | 轮询主循环、事件评估、长期记忆 |
| `tools/assistant_*.rs` | 助理对话里的工单、配置、汇报工具 |

### 其它

| 文件 | 职责 |
| ---- | ---- |
| `changes.rs` | 观察文件系统得出「这一轮改了哪些文件」 |
| `capture.rs` / `inspect.rs` / `preview.rs` | 原始报文捕获、语义视图、变更预览 |
| `goal.rs` / `schedules.rs` / `memory_suggest.rs` | 长任务状态机、定时任务、记忆建议 |

## Tauri 层（`src-tauri/`）

`src/lib.rs` 一个文件：命令定义 → `generate_handler!` 注册 → 事件名常量。
新增命令三处同改：这里、`src/lib/api.ts`、必要时 `capabilities/default.json`。

## 跨语言同步点

改这些地方必须两边一起改，否则界面静默错位：

- `protocol.rs` 的结构体 ↔ `src/lib/api.ts` 的 `interface`
- 事件名常量：`src-tauri/src/lib.rs` ↔ `api.ts`（`carter://event`、`UPDATE_PROGRESS` 等）
- `net::GITHUB_ACCELS` ↔ `scripts/models-snapshot.ts` 的同名表
- `update::REPO` ↔ `src-tauri/Cargo.toml` 的 `repository`

## 脚本（`scripts/`）

| 脚本 | 职责 |
| ---- | ---- |
| `init.ts` / `sandbox.ts` / `clean.ts` | 初始化、隔离数据目录跑 dev、清产物 |
| `build.ts` / `upgrade.ts` | 按平台打包、三处版本号一起改并打 tag |
| `models-snapshot.ts` | 生成打进安装包的 models.dev 快照 |
| `logs.ts` | 跟踪 `~/.carter-app/app.log` |

## 常见改动落点

- **换镜像 / 加源**：`net.rs` 的 `GITHUB_ACCELS` / `MODEL_SOURCES`，再跑 `bun run models:snapshot`。
- **加一个连接器**：`connectors/mod.rs` 的 `VENDORS` 加一项（安装命令、登录方式、技能来源）。
- **改技能扫描规则**：`skills.rs` 的 `Scan::from_settings_at` 与 `discover_at`。
- **加一个前端可见状态**：`protocol.rs` → `api.ts` → `events.ts` → store。
