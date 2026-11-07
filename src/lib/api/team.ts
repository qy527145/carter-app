import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// 团队目录（目标 / 任务 / 看板）
// ---------------------------------------------------------------------------

/**
 * 这个工作目录（或它上面某一层）的团队目录状态。
 *
 * `null` = 还没初始化。界面上「初始化」与「展示」是两种完全不同的样子，
 * 所以这一条要能明确回答「有没有」，而不是返回一个全空的假对象。
 */
export type TeamStatus = {
  root: string;
  project_root: string;
  name: string;
  remote: string | null;
  initialized: boolean;
  goals: number;
  issues: number;
};

export type TeamInitOutcome = {
  root: string;
  project_root: string;
  steps: string[];
  commit: string | null;
  identity: { name: string; email: string };
  remote: string | null;
  sync_source: boolean;
  warnings: string[];
};

export type TeamCounts = {
  total: number;
  todo: number;
  doing: number;
  blocked: number;
  done: number;
  dropped: number;
};

export type TeamTask = {
  id: string;
  title: string;
  /** `todo` | `doing` | `blocked` | `done` | `dropped` */
  status: string;
  status_label: string;
  priority: string;
  assignee: string | null;
  parent_id: string | null;
  depends_on: string[];
  children: string[];
  /** 还没完成的前置任务。 */
  waiting_on: string[];
  /** 0~100。有子任务时按子任务算。 */
  progress: number;
  /** 前置全完成、状态是待办 —— 可以直接开工。 */
  ready: boolean;
  blocked_reason: string;
  estimate_hours: number | null;
  due_at: number | null;
};

export type TeamGoal = {
  id: string;
  title: string;
  description: string;
  /** `active` | `paused` | `done` | `archived` */
  status: string;
  status_label: string;
  priority: string;
  owner: string | null;
  due_at: number | null;
  percent: number;
  counts: TeamCounts;
  /** 根任务 id，按顺序。 */
  roots: string[];
  tasks: TeamTask[];
};

/** 一块看板的元信息。正文是一整页 HTML，用 [`teamBoardHtml`] 按需取。 */
export type TeamBoard = {
  /** `boards/` 下的文件名，不带 `.html`。 */
  id: string;
  title: string;
  goal_id: string | null;
};

/**
 * 一条团队事件（协议见 `docs/team-protocol.md`）。
 *
 * `kind` 是开放的字符串：新版客户端写的、本版不认识的种类也要能显示出来，
 * 不能吞掉 —— 发的人以为对方看到了。
 */
export type TeamEvent = {
  /** 提交的短 SHA。只用来显示；引用请用 `eid`。 */
  sha: string;
  /** 协议版本。0 = 旧版打在代码分支上的事件。 */
  v: number;
  /** `comment` | `mention` | `broadcast` | `request` | `assign` | `handoff` | `claim` | `release` | `join` | `note` | `sync` | … */
  kind: string;
  /** 稳定的事件 id（`e-…`）。旧事件用 SHA 顶上。 */
  eid: string;
  target: string | null;
  goal_id: string | null;
  /** 发出者的成员 id。旧事件没有，按作者邮箱去名单里认。 */
  from: string | null;
  /** 收件人成员 id，`*` = 全员。 */
  to: string[];
  /** `"carter"` = 由 agent 代发。 */
  via: string | null;
  reply_to: string | null;
  due: number | null;
  body: string;
  subject: string;
  author: { name: string; email: string } | null;
  at: number;
  legacy: boolean;
};

/** 收件箱里的一条：事件 + 为什么在这里。 */
export type TeamInboxItem = TeamEvent & {
  reason: "mention" | "broadcast" | "assigned" | "my_task";
};

export type TeamMember = {
  id: string;
  name: string;
  emails: string[];
  role: string;
  joined_at: number;
};

export type TeamPeople = {
  members: TeamMember[];
  /** 本机的成员 id。`null` = 还没加入这个团队。 */
  me: string | null;
  /** 本机的 git 身份（`名字 <邮箱>`）。没配 git 时为空，那时没法加入。 */
  identity: string | null;
  /** 提交过代码、还没加入的人（`名字 <邮箱>`）。 */
  pending: string[];
};

export type TeamOverview = {
  root: string;
  name: string;
  remote: string | null;
  identity: string | null;
  goals: TeamGoal[];
  boards: TeamBoard[];
  events: TeamEvent[];
  /** 读不出来的文件。给人看的诊断信息。 */
  warnings: string[];
};

export type TeamTodo = {
  id: string;
  title: string;
  status: string;
  status_label: string;
  priority: string;
  goal_id: string;
  goal_title: string;
  blocked_reason: string;
  /** 还差几个前置。 */
  waiting_on: number;
  ready: boolean;
};

/** 这个工作目录的团队目录状态。`null` = 没初始化过。 */
export function teamStatus(cwd: string): Promise<TeamStatus | null> {
  return invoke<TeamStatus | null>("team_status", { cwd });
}

/** 初始化团队目录。会 `git init`（必要时）并提交一次。 */
export function teamInitialize(
  projectRoot: string,
  name?: string | null,
): Promise<TeamInitOutcome> {
  return invoke<TeamInitOutcome>("team_initialize", { projectRoot, name: name ?? null });
}

/** 全景：目标 + 任务树 + 看板 + 最近事件。界面一次拉完。 */
export function teamOverview(cwd: string): Promise<TeamOverview> {
  return invoke<TeamOverview>("team_overview", { cwd });
}

/** 一块看板的整页 HTML。 */
export function teamBoardHtml(cwd: string, boardId: string): Promise<string> {
  return invoke<string>("team_board_html", { cwd, boardId });
}

/** 我名下未完成的任务。消息框下方的待办条用它。 */
export function teamMyTasks(cwd: string): Promise<TeamTodo[]> {
  return invoke<TeamTodo[]>("team_my_tasks", { cwd });
}

/**
 * 把团队数据的改动提交成一条 git 提交。返回提交短 SHA；没有改动时是 `null`。
 *
 * 由界面在**轮到结束**时调用 —— 一轮里模型可能改了好多处，那是同一件事。
 */
export function teamCommit(cwd: string, message: string): Promise<string | null> {
  return invoke<string | null>("team_commit", { cwd, message });
}

/** 立刻同步一次（拉远端、只做快进）。观察源也会定时跑。 */
export function teamSync(cwd: string): Promise<string> {
  return invoke<string>("team_sync", { cwd });
}

/**
 * 配（或改）远端，并顺手把当前分支推上去。
 *
 * 远端不必叫 `origin` —— 后端会自动找已有的那个。界面上给一个输入框就够了，
 * 不该让人为了同步去开终端。
 */
export function teamSetRemote(cwd: string, url: string): Promise<string> {
  return invoke<string>("team_set_remote", { cwd, url });
}

/** 团队里有谁、本机是谁。 */
export function teamPeople(cwd: string): Promise<TeamPeople> {
  return invoke<TeamPeople>("team_people", { cwd });
}

/**
 * 加入团队。给 `memberId` = 「我是这个人，这是我的另一台机器」（把本机邮箱挂到他名下）；
 * 否则用 `name` 新建一个成员。已经在名单里时什么都不做。
 */
export function teamJoin(
  cwd: string,
  join: { name: string } | { memberId: string },
): Promise<{ member: TeamMember; inbox_source: string | null }> {
  return invoke<{ member: TeamMember; inbox_source: string | null }>("team_join", {
    cwd,
    name: "name" in join ? join.name : null,
    memberId: "memberId" in join ? join.memberId : null,
  });
}

export function teamRenameMe(cwd: string, name: string): Promise<TeamMember> {
  return invoke<TeamMember>("team_rename_me", { cwd, name });
}

/**
 * 团队操作：指派、认领、评论、@、广播……参数与 `team` 工具一致（`action` + 各自的字段），
 * 后端走同一份实现，只是不带「经卡特」的标记。返回给人看的一句结果。
 */
export type TeamAction =
  | { action: "assign"; task_id: string; assignee: string; text?: string }
  | { action: "task_claim" | "task_release"; task_id: string }
  | { action: "task_update"; task_id: string; assignee: string }
  /**
   * 推进任务状态。看板上拖一下就走这条 —— 原来只有模型能改状态，人要在界面上
   * 把一件事标成完成，只能开一条对话让卡特去做。
   *
   * `blocked` 必须带 `blocked_reason`（后端会拒），其余状态带了也会写进去。
   */
  | {
      action: "task_status";
      task_id: string;
      status: "todo" | "doing" | "blocked" | "done" | "dropped";
      blocked_reason?: string;
    }
  | { action: "comment"; task_id?: string; goal_id?: string; text: string; to?: string[] }
  | { action: "mention"; to: string[]; text: string; task_id?: string; goal_id?: string }
  | { action: "request"; to: string[]; text: string; task_id?: string; due_at?: number }
  | { action: "broadcast"; text: string; goal_id?: string };

export function teamAct(cwd: string, args: TeamAction): Promise<string> {
  return invoke<string>("team_act", { cwd, args });
}

/** 跟本机有关的消息，新的在前。`fetch` = 先拉一次远端。 */
export function teamInbox(cwd: string, fetch = false): Promise<TeamInboxItem[]> {
  return invoke<TeamInboxItem[]>("team_inbox", { cwd, fetch });
}
