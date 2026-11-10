// 团队协作页：我的 → 任务图 → 看板 → 动态。
//
// ## 四个视图的选择理由
//
// * **我的**（默认）—— 开发成员打开这一页的第一个问题是「我现在该做什么、谁在找我」，
//   不是「整个项目长什么样」。详见 `MineAndLog.tsx`。
// * **任务图** —— 目标是「一件事怎么拆开、谁等谁」，这本质上是一张 DAG。
//   用横向泳道（层 = 依赖深度）而不是纵树：依赖是「从左往右推进」的，横向读起来
//   与「进度条」是同一个方向。父子的缩进画在节点标题上，不参与泳道计算 ——
//   父子是「拆解」，依赖是「先后」，两套约束不该抢同一个轴。
// * **看板** —— 回顾、小结这类一次性的产物。每块是卡特写的一整页 HTML，版式随内容，
//   不再是固定的「几列卡片」；在沙箱里渲染（见 BoardFrame.tsx）。
// * **动态** —— 空提交那条通道的原始记录：谁认领了、谁说了什么。它是唯一一处
//   能看到「为什么这么改」的地方，所以单独给一屏，而不是塞在侧栏角落里。
//
// ## 为什么不用图形库
//
// 节点位置全是确定的（泳道 + 序号），连线是正交折线，用 SVG 画几十条路径比引一个
// 布局引擎更可控，也不会为了「力导向好看」把顺序抖动掉 —— 团队图每次打开都长得
// 不一样，人就记不住东西在哪。

import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Activity,
  ArrowRight,
  Check,
  ChevronDown,
  CircleAlert,
  Folder,
  FolderOpen,
  GitBranch,
  GitMerge,
  Kanban,
  LayoutGrid,
  Link2,
  ListChecks,
  LoaderCircle,
  MessageSquare,
  Plus,
  RefreshCw,
  Sparkles,
  Target,
  TriangleAlert,
  UserRound,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import {
  type TeamBoard,
  type TeamGoal,
  type TeamHealth,
  type TeamOverview,
  type TeamStatus,
  type TeamTask,
  teamBoardHtml,
  teamHealth,
  teamInitialize,
  teamOverview,
  teamSetRemote,
  teamStatus,
  teamSync,
} from "../../lib/api";
import { launchFlight, leaveMain } from "../../lib/flight";
import { baseName } from "../../lib/format";
import { contextSummary, formatContext } from "../../lib/promptContext";
import { ASSISTANT_CHAT_ID, openProjectChat, startProjectChat } from "../../lib/sessionOps";
import {
  newBoardHandoff,
  newGoalHandoff,
  personName,
  priorityTone,
  statusTone,
  taskHandoff,
} from "../../lib/teamUi";
import { toastError, toastOk } from "../../lib/toast";
import { useStore } from "../../store";
import { BoardFrame } from "./BoardFrame";
import { DependencyGraph } from "./DependencyGraph";
import { BoardView, GRAPH_VIEWS, ListView, TimelineView, useGraphView } from "./GraphViews";
import { LogTab, MineTab, markSeen, readSeen, useInbox } from "./MineAndLog";
import { PeopleProvider, usePeople } from "./people";
import { IdentityChip, JoinDialog, PersonAvatar } from "./peopleUi";
import { TaskDrawer } from "./TaskDrawer";
import { TeamHealthDrawer } from "./TeamHealth";
import { healthSummary } from "./teamHealthUi";

type Tab = "mine" | "graph" | "board" | "log";

/**
 * 能挑的项目：有过对话的目录（按最近用过排）+ 当前正在看的那个。
 *
 * 家目录不算项目 —— 助理对话住在那里，团队目录建在 `~` 下等于把整台机器当成一个
 * 仓库，几乎一定不是本意。真要用，可以走「选择其他目录…」显式挑。
 */
function useProjects(): string[] {
  const sessions = useStore((s) => s.sessions);
  const home = useStore((s) => s.appInfo?.default_cwd ?? "");
  const teamCwd = useStore((s) => s.teamCwd);
  return useMemo(() => {
    const latest = new Map<string, number>();
    for (const s of sessions) {
      if (s.id === ASSISTANT_CHAT_ID || !s.cwd || s.cwd === home || !s.cwd_exists) continue;
      latest.set(s.cwd, Math.max(latest.get(s.cwd) ?? 0, s.updated_at));
    }
    const out = [...latest].sort((a, b) => b[1] - a[1]).map(([cwd]) => cwd);
    if (teamCwd && !latest.has(teamCwd)) out.unshift(teamCwd);
    return out;
  }, [sessions, home, teamCwd]);
}

/** 弹系统目录选择框。取消返回 `null`。 */
async function pickDirectory(): Promise<string | null> {
  try {
    const dir = await openDialog({ directory: true, multiple: false, title: "选择项目目录" });
    return typeof dir === "string" && dir ? dir : null;
  } catch (e) {
    toastError(e);
    return null;
  }
}

export function TeamPage() {
  const cwd = useStore((s) => s.teamCwd);
  // 名单随全景一起刷新：别人的加入是随 git 同步过来的，重读全景时名单也该重读。
  const [nonce, setNonce] = useState(0);
  const bump = useCallback(() => setNonce((n) => n + 1), []);
  return (
    <PeopleProvider cwd={cwd} nonce={nonce}>
      <TeamPageBody onReloaded={bump} />
    </PeopleProvider>
  );
}

function TeamPageBody({ onReloaded }: { onReloaded: () => void }) {
  const cwd = useStore((s) => s.teamCwd);
  const setTeamCwd = useStore((s) => s.setTeamCwd);
  const { people } = usePeople();
  const [data, setData] = useState<TeamOverview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [tab, setTab] = useState<Tab>("mine");
  /** 抽屉里打开的任务（存 id，数据刷新后跟着最新那份走）。 */
  const [opened, setOpened] = useState<{ goal: string; task: string } | null>(null);
  const [joining, setJoining] = useState(false);
  /** 收件箱跟着全景刷新；`fetchInbox` 只在第一次与手动刷新时拉远端，别的时候读本地。 */
  const [inboxNonce, setInboxNonce] = useState(0);
  const [fetchInbox, setFetchInbox] = useState(true);
  const inbox = useInbox(cwd, inboxNonce, fetchInbox);
  const [seen, setSeen] = useState(() => readSeen(cwd));
  const [picked, setPicked] = useState<string | null>(null);
  /** 正在编辑远端地址。`null` = 没打开。 */
  const [remoteDraft, setRemoteDraft] = useState<string | null>(null);
  /** 就绪状态抽屉。 */
  const [healthOpen, setHealthOpen] = useState(false);
  /** header 角标用的那份（只用到计数，不必等全景）。 */
  const [health, setHealth] = useState<TeamHealth | null>(null);

  // 只认最后一次请求的结果：快速换项目时，慢的那个回来不能把新项目的数据盖掉。
  const seq = useRef(0);
  const load = useCallback(async () => {
    if (!cwd) return;
    const mine = ++seq.current;
    setLoading(true);
    // 就绪状态与全景各走各的：`.carter/` 被删掉时全景会失败，而就绪状态恰恰要
    // 在那时告诉人「能找回」。所以它不能跟在 `teamOverview` 的成败后面。
    teamHealth(cwd)
      .then((h) => {
        if (mine === seq.current) setHealth(h);
      })
      .catch(() => {
        if (mine === seq.current) setHealth(null);
      });
    try {
      const next = await teamOverview(cwd);
      if (mine !== seq.current) return;
      setData(next);
      setError(null);
      onReloaded();
      setInboxNonce((n) => n + 1);
    } catch (e) {
      if (mine !== seq.current) return;
      setData(null);
      setError(String(e));
    } finally {
      if (mine === seq.current) setLoading(false);
    }
  }, [cwd, onReloaded]);

  // 换项目：先清掉旧的，别让上一个项目的目标在新标题下闪一帧。
  useEffect(() => {
    setData(null);
    setError(null);
    setPicked(null);
    setRemoteDraft(null);
    setOpened(null);
    setFetchInbox(true);
    setSeen(readSeen(cwd));
    void load();
  }, [load, cwd]);

  // 第一次在这个团队里、git 身份也配好了、还不在名单里：问一次要不要加入。
  // 只问一次（按项目记在本机），点了「以后再说」就不再自动弹 —— 右上角一直有入口。
  useEffect(() => {
    if (!people || people.me || !people.identity || !data) return;
    const key = `carter.team.joinAsked:${cwd}`;
    try {
      if (localStorage.getItem(key)) return;
      localStorage.setItem(key, "1");
    } catch {
      return;
    }
    setJoining(true);
  }, [people, data, cwd]);

  // 看过「我的」或「动态」就算读过：把已读游标推到最新一条。
  useEffect(() => {
    if ((tab !== "mine" && tab !== "log") || inbox.length === 0) return;
    const latest = Math.max(...inbox.map((i) => i.at));
    // 延迟一下再标：刚打开时那几条「新」要让人看见高亮。
    const t = setTimeout(() => {
      markSeen(cwd, latest);
      setSeen(latest);
    }, 4000);
    return () => clearTimeout(t);
  }, [tab, inbox, cwd]);

  const unread = inbox.filter((i) => i.at > seen).length;

  // 团队数据在磁盘上，别处（对话里的模型、git pull）随时会改。回到窗口时对一次。
  useEffect(() => {
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [load]);

  // 「初始化」只在真的没有团队目录时出现 —— 那时后端会拒绝 overview。
  const needsInit =
    error !== null && (error.includes("还没有初始化") || error.includes("初始化过"));

  async function doInit() {
    if (!cwd) return;
    setBusy(true);
    try {
      const out = await teamInitialize(cwd, null);
      const detail = [
        ...out.steps,
        out.commit ? `提交 ${out.commit}` : null,
        out.sync_source ? "已建同步观察源" : null,
        ...out.warnings,
      ]
        .filter(Boolean)
        .join(" · ");
      toastOk(`团队目录已就绪：${detail}`);
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  async function doSync() {
    setBusy(true);
    try {
      toastOk(await teamSync(cwd));
      // 同步本身已经拉过事件，收件箱读本地即可。
      setFetchInbox(false);
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  async function doSetRemote() {
    if (!cwd || remoteDraft === null) return;
    setBusy(true);
    try {
      toastOk(await teamSetRemote(cwd, remoteDraft));
      setRemoteDraft(null);
      await load();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  const activeGoal = useMemo(() => {
    if (!data) return null;
    return data.goals.find((g) => g.id === picked) ?? data.goals[0] ?? null;
  }, [data, picked]);

  const openTask = useCallback((goal: TeamGoal, task: TeamTask) => {
    setOpened({ goal: goal.id, task: task.id });
  }, []);
  const drawer = useMemo(() => {
    if (!opened || !data) return null;
    const goal = data.goals.find((g) => g.id === opened.goal);
    const task = goal?.tasks.find((t) => t.id === opened.task);
    return goal && task ? { goal, task } : null;
  }, [opened, data]);

  // 最后点下的按钮：卡片从它身上飞出去。这样十来个入口都不用各自往外传位置。
  const lastPressed = useRef<Element | null>(null);

  /** 带话 → 在这个项目里开新对话并摆进输入框；不带话 → 回到最近那条。 */
  const askCarter = useCallback(
    (draft?: string) => {
      if (!cwd) return;
      if (draft === undefined) {
        leaveMain(() => void openProjectChat(cwd));
        return;
      }
      launchFlight(lastPressed.current, contextSummary(draft));
      leaveMain(() => void startProjectChat(cwd, draft));
    },
    [cwd],
  );

  return (
    <div
      className="flex min-h-0 flex-1 flex-col"
      onClickCapture={(e) => {
        lastPressed.current = (e.target as Element).closest("button");
      }}
    >
      <header className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-edge px-5 py-2.5">
        <ProjectSwitcher current={cwd} teamName={data?.name} onPick={setTeamCwd} />

        {data && (
          <div className="flex items-center gap-1 rounded-lg bg-card p-0.5">
            <TabButton
              active={tab === "mine"}
              onClick={() => setTab("mine")}
              icon={<UserRound size={12} />}
            >
              我的
              {unread > 0 && <Dot n={unread} />}
            </TabButton>
            <TabButton
              active={tab === "graph"}
              onClick={() => setTab("graph")}
              icon={<GitBranch size={12} />}
            >
              任务图
            </TabButton>
            <TabButton
              active={tab === "board"}
              onClick={() => setTab("board")}
              icon={<Kanban size={12} />}
            >
              看板
              {data.boards.length > 0 && <Count n={data.boards.length} />}
            </TabButton>
            <TabButton
              active={tab === "log"}
              onClick={() => setTab("log")}
              icon={<ListChecks size={12} />}
            >
              动态
            </TabButton>
          </div>
        )}

        <div className="ml-auto flex items-center gap-1.5">
          {cwd && (
            <HealthChip
              health={health}
              onClick={() => setHealthOpen(true)}
              onJoining={health?.team_root === null && needsInit}
            />
          )}
          {data &&
            (data.remote ? (
              <button
                onClick={() => setRemoteDraft(data.remote ?? "")}
                title={`${data.remote}\n点击修改`}
                className="mono hidden max-w-[16rem] truncate text-[10px] text-dim transition-colors hover:text-ink xl:block"
              >
                {data.remote}
              </button>
            ) : (
              <button
                onClick={() => setRemoteDraft("")}
                title="团队数据目前只在本地。填一个仓库地址，别人就能同步到了。"
                className="flex items-center gap-1 rounded-full bg-tone-orange-soft px-2 py-1 text-[10px] text-tone-orange transition-opacity hover:opacity-80"
              >
                <Link2 size={11} />
                配远端
              </button>
            ))}
          {data && <IdentityChip cwd={cwd} onJoin={() => setJoining(true)} />}
          {data && (
            <>
              <IconAction
                onClick={() => {
                  setFetchInbox(true);
                  void load();
                }}
                title="重新读取（顺带拉一次新消息）"
              >
                <RefreshCw size={13} className={loading ? "animate-spin" : ""} />
              </IconAction>
              <IconAction
                onClick={() => void doSync()}
                title={
                  data.remote ? `同步远端 ${data.remote}（只做快进）` : "同步远端（还没配远端）"
                }
              >
                <GitMerge size={13} className={busy ? "animate-pulse" : ""} />
              </IconAction>
            </>
          )}
          {cwd && (
            <button
              onClick={() => askCarter()}
              title={`打开 ${baseName(cwd)} 最近的一条对话`}
              className="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-dim transition-colors hover:bg-ink/5 hover:text-ink"
            >
              <MessageSquare size={12} />
              去对话
            </button>
          )}
        </div>
      </header>

      {!cwd ? (
        <ChooseProject onPick={setTeamCwd} />
      ) : needsInit ? (
        <InitPanel
          onInit={doInit}
          busy={busy}
          cwd={cwd}
          recoverable={(health?.checks.find((c) => c.id === "files")?.paths.length ?? 0) > 0}
          onRecover={() => setHealthOpen(true)}
        />
      ) : error ? (
        <div className="p-6 text-sm text-danger">{error}</div>
      ) : !data ? (
        <div className="flex flex-1 items-center justify-center text-sm text-dim">
          <LoaderCircle size={14} className="mr-2 animate-spin" />
          正在读取 {baseName(cwd)} 的团队目录…
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto">
          {remoteDraft !== null && (
            <div className="mx-5 mt-4 rounded-xl bg-card p-3 shadow-soft">
              <div className="mb-1.5 flex items-center gap-1.5 text-xs font-medium">
                <Link2 size={12} />
                团队同步的远端仓库
              </div>
              <p className="mb-2 text-[11px] leading-relaxed text-dim">
                所有人指向同一个 git 仓库，目标与任务才能互相同步。 远端不必叫{" "}
                <span className="mono">origin</span>，已有的任何一个都会被用上；
                配好之后会立刻把当前分支推上去。
              </p>
              <div className="flex gap-1.5">
                <input
                  autoFocus
                  value={remoteDraft}
                  onChange={(e) => setRemoteDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void doSetRemote();
                    if (e.key === "Escape") setRemoteDraft(null);
                  }}
                  placeholder="git@github.com:acme/team.git"
                  className="mono min-w-0 flex-1 rounded-md bg-bubble px-2 py-1.5 text-[11px] outline-none"
                />
                <button
                  onClick={() => void doSetRemote()}
                  disabled={busy || remoteDraft.trim() === ""}
                  className="shrink-0 rounded-md bg-ink px-3 py-1.5 text-[11px] font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-40"
                >
                  保存并推送
                </button>
                <button
                  onClick={() => setRemoteDraft(null)}
                  className="shrink-0 rounded-md px-2 py-1.5 text-[11px] text-dim transition-colors hover:bg-ink/5 hover:text-ink"
                >
                  取消
                </button>
              </div>
            </div>
          )}

          {data.warnings.length > 0 && (
            <div className="mx-5 mt-4 rounded-lg bg-warn-bg px-3 py-2 text-[11px] text-ink">
              <div className="mb-0.5 flex items-center gap-1.5 font-medium">
                <CircleAlert size={12} />有 {data.warnings.length} 个文件读不出来
              </div>
              {data.warnings.map((w) => (
                <div key={w} className="mono truncate text-dim">
                  {w}
                </div>
              ))}
            </div>
          )}

          {tab === "mine" && (
            <MineTab
              data={data}
              inbox={inbox}
              seen={seen}
              onOpenTask={openTask}
              onHandoff={(g, t) => askCarter(taskHandoff(g, t))}
              onJoin={() => setJoining(true)}
            />
          )}
          {tab === "graph" && (
            <GraphTab
              cwd={cwd}
              data={data}
              goal={activeGoal}
              onPick={setPicked}
              onAsk={askCarter}
              onOpenTask={openTask}
              onChanged={() => void load()}
            />
          )}
          {tab === "board" && (
            <BoardTab cwd={cwd} boards={data.boards} version={data} onAsk={askCarter} />
          )}
          {tab === "log" && (
            <LogTab
              cwd={cwd}
              data={data}
              inbox={inbox}
              seen={seen}
              onChanged={() => void load()}
              onOpenTask={openTask}
              onNeedJoin={() => setJoining(true)}
            />
          )}
        </div>
      )}

      {drawer && data && (
        <TaskDrawer
          cwd={cwd}
          task={drawer.task}
          goal={drawer.goal}
          events={data.events}
          onClose={() => setOpened(null)}
          onChanged={() => void load()}
          onAsk={askCarter}
          onNeedJoin={() => setJoining(true)}
        />
      )}
      {joining && <JoinDialog cwd={cwd} onClose={() => setJoining(false)} />}
      {healthOpen && cwd && (
        <TeamHealthDrawer
          cwd={cwd}
          onClose={() => setHealthOpen(false)}
          onRepaired={() => void load()}
          onConfigureRemote={() => setRemoteDraft(data?.remote ?? "")}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 项目切换
// ---------------------------------------------------------------------------

/**
 * 标题位置就是项目切换器 —— 「我在看哪个项目的团队」是这一页最重要的上下文，
 * 它应当既是标题、又能直接换，而不是要先绕回侧栏开一条那个项目的对话。
 */
function ProjectSwitcher({
  current,
  teamName,
  onPick,
}: {
  current: string;
  teamName?: string;
  onPick: (cwd: string) => void;
}) {
  const projects = useProjects();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const title = teamName || (current ? baseName(current) : "选择项目");

  return (
    <div ref={wrapRef} className="relative min-w-0">
      <button
        onClick={() => setOpen((o) => !o)}
        title={current || "选择要查看的项目"}
        className="flex min-w-0 items-center gap-2 rounded-lg px-1.5 py-1 text-left transition-colors hover:bg-ink/5"
      >
        <div className="flex size-6 shrink-0 items-center justify-center rounded-md bg-ink text-[var(--accent-contrast)]">
          <Target size={13} />
        </div>
        <div className="min-w-0">
          <div className="flex items-center gap-1 text-sm font-semibold tracking-tight">
            <span className="truncate">{title}</span>
            <ChevronDown size={13} className="shrink-0 text-dim" />
          </div>
          {current && (
            <div className="mono max-w-[18rem] truncate text-[10px] text-dim">{current}</div>
          )}
        </div>
      </button>

      {open && (
        <div className="absolute left-0 top-[calc(100%+4px)] z-50 w-[20rem] rounded-xl bg-card py-1 shadow-lift">
          <div className="px-3 pb-1 pt-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">
            项目
          </div>
          <ProjectList
            projects={projects}
            current={current}
            onPick={(cwd) => {
              setOpen(false);
              onPick(cwd);
            }}
            dense
          />
        </div>
      )}
    </div>
  );
}

/**
 * 项目列表，每行带团队目录的状态（几个目标 / 没初始化）。
 *
 * 状态是逐个异步查的：查的是磁盘上的 `.carter/`，项目多时一次性等全部回来会让
 * 菜单晚一拍才出现。先出名字，状态随后补上。
 */
function ProjectList({
  projects,
  current,
  onPick,
  dense = false,
}: {
  projects: string[];
  current: string;
  onPick: (cwd: string) => void;
  dense?: boolean;
}) {
  const [status, setStatus] = useState<Record<string, TeamStatus | null>>({});
  useEffect(() => {
    let alive = true;
    for (const p of projects) {
      teamStatus(p)
        .then((st) => alive && setStatus((m) => ({ ...m, [p]: st })))
        .catch(() => alive && setStatus((m) => ({ ...m, [p]: null })));
    }
    return () => {
      alive = false;
    };
  }, [projects]);

  async function other() {
    const dir = await pickDirectory();
    if (dir) onPick(dir);
  }

  return (
    <div className={dense ? "max-h-80 overflow-y-auto" : ""}>
      {projects.length === 0 && (
        <div className="px-3 py-2 text-xs text-dim">还没有在任何项目里对过话。</div>
      )}
      {projects.map((p) => {
        const st = status[p];
        const active = p === current;
        return (
          <button
            key={p}
            onClick={() => onPick(p)}
            title={p}
            className={`flex w-full items-center gap-2 text-left transition-colors hover:bg-ink/5 ${
              dense ? "px-3 py-1.5" : "rounded-lg px-3 py-2"
            }`}
          >
            <Folder size={13} className={`shrink-0 ${st ? "text-tone-violet" : "text-dim"}`} />
            <div className="min-w-0 flex-1">
              <div
                className={`truncate text-xs ${active ? "font-medium text-ink" : "text-ink/90"}`}
              >
                {st?.name || baseName(p)}
              </div>
              <div className="mono truncate text-[10px] text-dim">{p}</div>
            </div>
            <span className="shrink-0 text-[10px] text-dim">
              {st === undefined ? "" : st ? `${st.goals} 个目标` : "未初始化"}
            </span>
            {active && <Check size={12} className="shrink-0 text-ink" />}
          </button>
        );
      })}
      <div className="my-1 border-t border-edge" />
      <button
        onClick={() => void other()}
        className={`flex w-full items-center gap-2 text-left text-xs text-dim transition-colors hover:bg-ink/5 hover:text-ink ${
          dense ? "px-3 py-1.5" : "rounded-lg px-3 py-2"
        }`}
      >
        <FolderOpen size={13} className="shrink-0" />
        选择其他目录…
      </button>
    </div>
  );
}

/** 还没挑过项目时的落地页：直接把项目列出来，一点就进。 */
function ChooseProject({ onPick }: { onPick: (cwd: string) => void }) {
  const projects = useProjects();
  return (
    <div className="flex flex-1 items-center justify-center p-8">
      <div className="w-full max-w-lg rounded-2xl bg-card p-5 shadow-soft">
        <div className="mb-1 flex items-center gap-2 text-sm font-semibold">
          <Target size={15} className="text-tone-violet" />
          看哪个项目的目标与任务？
        </div>
        <p className="mb-3 text-xs leading-relaxed text-dim">
          目标、任务和看板跟着项目走，存在项目的 <span className="mono">.carter/</span> 目录里， 随
          git 与团队同步。
        </p>
        <div className="-mx-2">
          <ProjectList projects={projects} current="" onPick={onPick} />
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 初始化
// ---------------------------------------------------------------------------

function InitPanel({
  onInit,
  busy,
  cwd,
  recoverable,
  onRecover,
}: {
  onInit: () => void;
  busy: boolean;
  cwd: string;
  /** git 历史里还有团队目录的痕迹（`.carter/team.json` 等）。 */
  recoverable: boolean;
  onRecover: () => void;
}) {
  return (
    <div className="flex flex-1 items-center justify-center p-8">
      <div className="w-full max-w-lg rounded-2xl bg-card p-6 shadow-soft">
        <div className="mb-1 flex items-center gap-2 text-sm font-semibold">
          <Sparkles size={15} className="text-tone-violet" />
          这个项目还没有团队目录
        </div>
        <p className="mb-4 text-xs leading-relaxed text-dim">
          初始化会在项目里建一个 <span className="mono">.carter/</span> 目录，
          用来记团队的目标、任务与回顾看板。它会：
        </p>
        <ul className="mb-5 space-y-1.5 text-xs text-dim">
          {[
            "确认项目在 git 管理下（没有就 git init）",
            "建好目标 / 任务 / 看板的目录结构与内置技能",
            "把这一份结构提交进 git，团队其他人 clone 下来就有",
            "有远端时自动建一个同步观察源，由卡特助理定时拉取",
          ].map((t) => (
            <li key={t} className="flex gap-2">
              <ArrowRight size={12} className="mt-0.5 shrink-0 text-dim" />
              {t}
            </li>
          ))}
        </ul>
        <div className="mono mb-4 truncate rounded-md bg-bubble px-2 py-1.5 text-[10px] text-dim">
          {cwd}
        </div>
        <button
          onClick={onInit}
          disabled={busy}
          className="flex w-full items-center justify-center gap-1.5 rounded-lg bg-ink py-2 text-sm font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-50"
        >
          {busy ? <LoaderCircle size={14} className="animate-spin" /> : <Plus size={14} />}
          初始化团队目录
        </button>
        {/* 团队目录被误删之后，这一页会退化成「还没初始化」—— 而丢掉的目标与任务
            很可能还在 git 历史里。这条是那种时候唯一能找回数据的路。 */}
        {recoverable && (
          <button
            onClick={onRecover}
            className="mt-2 w-full rounded-lg py-1.5 text-xs text-dim transition-colors hover:bg-ink/5 hover:text-ink"
          >
            这个项目以前有团队目录？从 git 历史找回
          </button>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 任务图
// ---------------------------------------------------------------------------

function GraphTab({
  cwd,
  data,
  goal,
  onPick,
  onAsk,
  onOpenTask,
  onChanged,
}: {
  cwd: string;
  data: TeamOverview;
  goal: TeamGoal | null;
  onPick: (id: string) => void;
  onAsk: (draft?: string) => void;
  onOpenTask: (goal: TeamGoal, task: TeamTask) => void;
  onChanged: () => void;
}) {
  if (data.goals.length === 0) {
    return (
      <Empty
        icon={<Target size={20} />}
        title="还没有目标"
        hint="目标由卡特在对话里帮你建：说清要做成什么，它会拆成任务、理好先后。"
        action="让卡特建一个目标"
        onAction={() => onAsk(newGoalHandoff())}
      />
    );
  }

  return (
    <div className="flex min-h-0 flex-col lg:flex-row">
      {/* 目标列 */}
      <div className="w-full shrink-0 border-b border-edge p-3 lg:w-[15rem] lg:border-b-0 lg:border-r">
        <div className="mb-2 flex items-center justify-between px-1">
          <span className="text-[10px] font-medium uppercase tracking-wider text-dim">
            目标 · {data.goals.length}
          </span>
          {/* 有目标之后也得能再加 —— 先开一条对话把「要做成什么」说清楚，让卡特建。 */}
          <button
            onClick={() => onAsk(newGoalHandoff())}
            title="让卡特新建目标"
            aria-label="让卡特新建目标"
            className="flex items-center gap-0.5 rounded-md px-1 py-0.5 text-[10px] text-dim transition-colors hover:bg-ink/5 hover:text-ink"
          >
            <Plus size={11} />
            新建
          </button>
        </div>
        <div className="flex gap-1.5 overflow-x-auto lg:block lg:space-y-1.5 lg:overflow-visible">
          {data.goals.map((g) => (
            <GoalCard key={g.id} goal={g} active={goal?.id === g.id} onClick={() => onPick(g.id)} />
          ))}
        </div>
      </div>

      {/* 图 */}
      <div className="min-w-0 flex-1 p-5">
        {goal ? (
          <GoalGraph
            cwd={cwd}
            goal={goal}
            onAsk={onAsk}
            onOpenTask={onOpenTask}
            onChanged={onChanged}
          />
        ) : null}
      </div>
    </div>
  );
}

function GoalCard({
  goal,
  active,
  onClick,
}: {
  goal: TeamGoal;
  active: boolean;
  onClick: () => void;
}) {
  const tone = statusTone(goal.status);
  return (
    <button
      onClick={onClick}
      className={`w-[14rem] shrink-0 rounded-xl p-2.5 text-left lg:w-full transition-colors ${
        active ? "bg-card shadow-soft" : "hover:bg-ink/[0.04]"
      }`}
    >
      <div className="mb-1 flex items-center gap-1.5">
        <span className="size-1.5 shrink-0 rounded-full" style={{ background: tone.fg }} />
        <span className="truncate text-xs font-medium">{goal.title}</span>
      </div>
      <div className="mb-1.5 flex items-center gap-1.5 text-[10px] text-dim">
        <span style={{ color: priorityTone(goal.priority).fg }}>{goal.priority}</span>
        <span>·</span>
        <span>{tone.label}</span>
        <span>·</span>
        <span className="tabular-nums">
          {goal.counts.done}/{goal.counts.total - goal.counts.dropped}
        </span>
      </div>
      <Progress value={goal.percent} tone={tone.fg} />
    </button>
  );
}

function GoalGraph({
  cwd,
  goal,
  onAsk,
  onOpenTask,
  onChanged,
}: {
  cwd: string;
  goal: TeamGoal;
  onAsk: (draft?: string) => void;
  onOpenTask: (goal: TeamGoal, task: TeamTask) => void;
  /** 在图上直接改了状态之后让团队页重读（看板拖拽、清单勾选）。 */
  onChanged: () => void;
}) {
  const { people } = usePeople();
  // 按人筛：只看某个人的任务（其余的在依赖图上淡出而不是藏起来 —— 依赖关系还要
  // 看得见；另外三种视图里直接藏起来，因为它们本来就是一列一列过一遍用的）。
  // `null` = 全部，`""` = 没人负责的。
  const [who, setWho] = useState<string | null>(null);
  const [view, setView] = useGraphView();
  const assignees = useMemo(() => {
    const seen = new Map<string, number>();
    for (const t of goal.tasks)
      if (t.assignee) seen.set(t.assignee, (seen.get(t.assignee) ?? 0) + 1);
    return [...seen];
  }, [goal.tasks]);

  return (
    <div className="min-w-0">
      <div className="mb-3 flex flex-wrap items-end justify-between gap-2">
        <div className="min-w-0">
          <h2 className="truncate text-[16px] font-semibold tracking-tight">{goal.title}</h2>
          {goal.description && (
            <p className="mt-0.5 max-w-2xl text-xs leading-relaxed text-dim">{goal.description}</p>
          )}
        </div>
        <div className="flex items-center gap-2 text-[11px] text-dim">
          <Ring value={goal.percent} />
          <span className="tabular-nums">
            {goal.counts.done}/{goal.counts.total - goal.counts.dropped} 完成
          </span>
          {goal.counts.blocked > 0 && (
            <span style={{ color: "var(--tone-orange)" }}>{goal.counts.blocked} 受阻</span>
          )}
          {goal.counts.doing > 0 && (
            <span style={{ color: "var(--tone-blue)" }}>{goal.counts.doing} 进行中</span>
          )}
          <button
            onClick={() =>
              onAsk(
                formatContext({
                  kind: "goal",
                  title: goal.title,
                  body:
                    goal.tasks.length === 0
                      ? `把目标「${goal.title}」（${goal.id}）拆成任务，理好依赖。`
                      : `看一下目标「${goal.title}」（${goal.id}）的进展，接下来该推进哪件？`,
                }),
              )
            }
            className="ml-1 flex items-center gap-1 rounded-md bg-ink/5 px-2 py-1 text-[11px] text-dim transition-colors hover:bg-ink/10 hover:text-ink"
          >
            <Sparkles size={11} />
            {goal.tasks.length === 0 ? "让卡特拆解" : "问问卡特"}
          </button>
        </div>
      </div>

      {/* 视图切换。四种排法回答四个不同的问题（见 GraphViews.tsx 开头），
          放在按人筛上面一行：先定「怎么看」，再定「看谁」。 */}
      {goal.tasks.length > 0 && (
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <div
            className="flex items-center gap-0.5 rounded-lg bg-card p-0.5"
            role="tablist"
            aria-label="任务图视图"
          >
            {GRAPH_VIEWS.map((v) => {
              const Icon = v.icon;
              const active = view === v.id;
              return (
                <button
                  key={v.id}
                  role="tab"
                  aria-selected={active}
                  onClick={() => setView(v.id)}
                  className={`carter-press flex items-center gap-1 rounded-md px-2 py-1 text-[11px] transition-colors ${
                    active
                      ? "bg-elevated font-medium text-ink shadow-soft"
                      : "text-dim hover:text-ink"
                  }`}
                >
                  <Icon size={11} />
                  {v.label}
                </button>
              );
            })}
          </div>
        </div>
      )}

      {assignees.length > 0 && (
        <div className="mb-3 flex flex-wrap items-center gap-1 text-[11px]">
          <span className="mr-1 text-[10px] text-dim">按人看</span>
          <FilterChip active={who === null} onClick={() => setWho(null)}>
            全部
          </FilterChip>
          {assignees.map(([a, n]) => (
            <FilterChip key={a} active={who === a} onClick={() => setWho(who === a ? null : a)}>
              <PersonAvatar person={a} size={13} />
              {people?.me === a ? "我" : personName(people, a)}
              <span className="tabular-nums opacity-60">{n}</span>
            </FilterChip>
          ))}
          <FilterChip active={who === ""} onClick={() => setWho(who === "" ? null : "")}>
            没人负责
          </FilterChip>
        </div>
      )}

      {goal.tasks.length === 0 ? (
        <div className="rounded-xl bg-card p-6 text-center text-xs text-dim">
          这个目标还没有任务。点右上角「让卡特拆解」，它会开一条新对话拆好。
        </div>
      ) : view === "lanes" ? (
        <DependencyGraph goal={goal} who={who} onOpen={(t) => onOpenTask(goal, t)} onAsk={onAsk} />
      ) : view === "board" ? (
        <BoardView
          cwd={cwd}
          goal={goal}
          who={who}
          onOpen={(t) => onOpenTask(goal, t)}
          onChanged={onChanged}
          onAsk={onAsk}
        />
      ) : view === "list" ? (
        <ListView
          cwd={cwd}
          goal={goal}
          who={who}
          onOpen={(t) => onOpenTask(goal, t)}
          onAsk={onAsk}
          onChanged={onChanged}
        />
      ) : (
        <TimelineView goal={goal} who={who} onOpen={(t) => onOpenTask(goal, t)} onAsk={onAsk} />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// 看板
// ---------------------------------------------------------------------------

/**
 * 看板：左边一列看板名，右边是选中那一块的整页 HTML（沙箱里渲染，见 BoardFrame）。
 *
 * 正文按需取：全景里只有元信息。`version` 跟着全景刷新变 —— 回到窗口、同步之后
 * 全景会重拉一次，正文也跟着重读，别人刚推上来的改动立刻能看到。
 */
function BoardTab({
  cwd,
  boards,
  version,
  onAsk,
}: {
  cwd: string;
  boards: TeamBoard[];
  version: unknown;
  onAsk: (draft?: string) => void;
}) {
  const [picked, setPicked] = useState<string | null>(null);
  const active = boards.find((b) => b.id === picked) ?? boards[boards.length - 1] ?? null;
  const [page, setPage] = useState<{ id: string; html: string } | null>(null);
  const [error, setError] = useState<string | null>(null);

  // version：团队数据每次重读全景时 +1，看板内容可能刚被别人改过。
  // biome-ignore lint/correctness/useExhaustiveDependencies: version 是重读的触发器，正文只认 active.id
  useEffect(() => {
    if (!active) return;
    let live = true;
    teamBoardHtml(cwd, active.id)
      .then((html) => {
        if (!live) return;
        // 内容没变就不换对象：srcdoc 一变 iframe 就重载，滚动位置与页面状态全丢。
        setPage((prev) =>
          prev?.id === active.id && prev.html === html ? prev : { id: active.id, html },
        );
        setError(null);
      })
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [cwd, active?.id, version]);

  if (!active) {
    return (
      <Empty
        icon={<LayoutGrid size={20} />}
        title="还没有看板"
        hint="看板是一整页由卡特排版的页面：回顾、阶段小结、风险盘点、方案对比都行。"
        action="让卡特建一块看板"
        onAction={() => onAsk(newBoardHandoff(boards))}
      />
    );
  }
  return (
    <div className="flex min-h-full flex-col gap-4 p-5 lg:flex-row">
      <div className="w-full shrink-0 space-y-0.5 lg:w-[12rem]">
        {[...boards].reverse().map((b) => (
          <button
            key={b.id}
            onClick={() => setPicked(b.id)}
            className={`block w-full rounded-lg px-2.5 py-1.5 text-left transition-colors ${
              b.id === active.id ? "bg-card shadow-soft" : "hover:bg-ink/[0.04]"
            }`}
          >
            <div className="truncate text-xs font-medium">{b.title}</div>
            <div className="mono truncate text-[10px] text-dim">
              {b.id}
              {b.goal_id && ` · ${b.goal_id}`}
            </div>
          </button>
        ))}
        <button
          onClick={() =>
            onAsk(
              formatContext({
                kind: "board",
                title: active.title,
                body: `帮我改一下看板「${active.title}」（${active.id}）。`,
              }),
            )
          }
          className="mt-2 flex w-full items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-[11px] text-dim transition-colors hover:bg-ink/[0.04] hover:text-ink"
        >
          <Sparkles size={12} />
          让卡特改这块看板
        </button>
        <button
          onClick={() => onAsk(newBoardHandoff(boards))}
          className="flex w-full items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-[11px] text-dim transition-colors hover:bg-ink/[0.04] hover:text-ink"
        >
          <Plus size={12} />
          新建看板
        </button>
      </div>
      <div className="min-w-0 flex-1">
        {error ? (
          <div className="rounded-xl bg-card p-4 text-xs text-danger">{error}</div>
        ) : page?.id === active.id ? (
          <BoardFrame html={page.html} title={active.title} />
        ) : (
          <div className="flex items-center justify-center py-24 text-xs text-dim">
            <LoaderCircle size={14} className="mr-2 animate-spin" />
            正在读取看板…
          </div>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 动态
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 小件
// ---------------------------------------------------------------------------

/** 空状态：说清这里会放什么，并给一个能直接把事办了的按钮。 */
function Empty({
  icon,
  title,
  hint,
  action,
  onAction,
}: {
  icon: React.ReactNode;
  title: string;
  hint: string;
  action?: string;
  onAction?: () => void;
}) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 py-24 text-center text-sm text-dim">
      {icon}
      <div className="text-ink">{title}</div>
      <div className="max-w-sm text-xs leading-relaxed">{hint}</div>
      {action && onAction && (
        <button
          onClick={onAction}
          className="mt-2 flex items-center gap-1.5 rounded-lg bg-ink px-3 py-1.5 text-xs font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90"
        >
          <Sparkles size={12} />
          {action}
        </button>
      )}
    </div>
  );
}

function Count({ n }: { n: number }) {
  return <span className="text-[10px] tabular-nums text-dim">{n}</span>;
}

function TabButton({
  active,
  onClick,
  icon,
  children,
}: {
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center gap-1.5 rounded-md px-2 py-1 text-[11px] transition-colors ${
        active ? "bg-elevated font-medium text-ink shadow-soft" : "text-dim hover:text-ink"
      }`}
    >
      {icon}
      {children}
    </button>
  );
}

function IconAction({
  onClick,
  title,
  children,
}: {
  onClick: () => void;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className="rounded-md p-1.5 text-dim transition-colors hover:bg-ink/5 hover:text-ink"
    >
      {children}
    </button>
  );
}

/**
 * header 上的就绪状态入口。
 *
 * 一切正常时是一个安静的图标 —— 这个功能 99% 的时间不该占版面。有东西坏了才
 * 变成一条橙色药丸带数字，因为那时候它比「配远端」更该被看见：它说的正是
 * 「你现在看到的团队数据没有在同步」。
 *
 * 不受 `data` 保护：`.carter/` 被删之后团队页退化成初始化面板，那时更需要它。
 */
function HealthChip({
  health,
  onClick,
  onJoining,
}: {
  health: TeamHealth | null;
  onClick: () => void;
  /** 还没检查回来（首次载入）。占位一样安静，避免按钮跳动。 */
  onJoining?: boolean;
}) {
  const summary = health ? healthSummary(health.checks) : null;
  const n = summary?.attention ?? 0;
  const label = summary?.label ?? "";
  const tip = onJoining
    ? "团队就绪状态"
    : n > 0
      ? `${label}：点开看看哪里没在工作`
      : "团队就绪状态：都正常";

  if (n > 0) {
    return (
      <button
        onClick={onClick}
        title={tip}
        aria-label={tip}
        className="flex items-center gap-1 rounded-full bg-tone-orange-soft px-2 py-1 text-[10px] text-tone-orange transition-opacity hover:opacity-80"
      >
        <TriangleAlert size={11} />
        {n}
      </button>
    );
  }
  return (
    <IconAction onClick={onClick} title={tip}>
      <Activity size={13} />
    </IconAction>
  );
}

/** 未读数的小红点。 */
function Dot({ n }: { n: number }) {
  return (
    <span className="min-w-4 rounded-full bg-tone-orange px-1 text-center text-[9px] font-medium leading-4 text-white">
      {n > 99 ? "99+" : n}
    </span>
  );
}

function FilterChip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`flex items-center gap-1 rounded-full px-2 py-0.5 transition-colors ${
        active ? "bg-ink text-[var(--accent-contrast)]" : "bg-card text-dim hover:text-ink"
      }`}
    >
      {children}
    </button>
  );
}

function Progress({ value, tone }: { value: number; tone: string }) {
  return (
    <span className="block h-1 overflow-hidden rounded-full bg-ink/[0.07]">
      <span
        className="block h-full rounded-full transition-[width] duration-500"
        style={{ width: `${Math.max(0, Math.min(100, value))}%`, background: tone }}
      />
    </span>
  );
}

/** 进度环。SVG 圆环比条形更适合放在标题右边一小块地方。 */
function Ring({ value }: { value: number }) {
  const r = 9;
  const c = 2 * Math.PI * r;
  const v = Math.max(0, Math.min(100, value));
  return (
    <svg width="24" height="24" viewBox="0 0 24 24" className="shrink-0 -rotate-90">
      <circle cx="12" cy="12" r={r} fill="none" stroke="var(--border-strong)" strokeWidth="2.5" />
      <circle
        cx="12"
        cy="12"
        r={r}
        fill="none"
        stroke="var(--tone-green)"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeDasharray={`${(v / 100) * c} ${c}`}
        className="transition-[stroke-dasharray] duration-500"
      />
    </svg>
  );
}
