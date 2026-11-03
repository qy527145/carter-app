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

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
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
  UserRound,
  MessageSquare,
  Plus,
  RefreshCw,
  Send,
  Sparkles,
  Target,
} from "lucide-react";

import {
  teamBoardHtml,
  teamInitialize,
  teamOverview,
  teamSetRemote,
  teamStatus,
  teamSync,
  type TeamBoard,
  type TeamGoal,
  type TeamOverview,
  type TeamStatus,
  type TeamTask,
} from "../../lib/api";
import { baseName } from "../../lib/format";
import { ASSISTANT_CHAT_ID, openProjectChat } from "../../lib/sessionOps";
import { toastError, toastOk } from "../../lib/toast";
import { useStore } from "../../store";
import { BoardFrame } from "./BoardFrame";
import { LogTab, MineTab, markSeen, readSeen, useInbox } from "./MineAndLog";
import { PeopleProvider, usePeople } from "./people";
import { IdentityChip, JoinDialog, PersonAvatar } from "./peopleUi";
import { TaskDrawer } from "./TaskDrawer";
import { layerize, personName, priorityTone, statusTone } from "../../lib/teamUi";

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

  // 只认最后一次请求的结果：快速换项目时，慢的那个回来不能把新项目的数据盖掉。
  const seq = useRef(0);
  const load = useCallback(async () => {
    if (!cwd) return;
    const mine = ++seq.current;
    setLoading(true);
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

  /** 去这个项目的对话里，把一段话摆进输入框。 */
  const askCarter = useCallback(
    (draft?: string) => {
      if (cwd) void openProjectChat(cwd, draft);
    },
    [cwd],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex shrink-0 items-center gap-3 border-b border-edge px-5 py-2.5">
        <ProjectSwitcher current={cwd} teamName={data?.name} onPick={setTeamCwd} />

        {data && (
          <div className="flex items-center gap-1 rounded-lg bg-card p-0.5">
            <TabButton active={tab === "mine"} onClick={() => setTab("mine")} icon={<UserRound size={12} />}>
              我的
              {unread > 0 && <Dot n={unread} />}
            </TabButton>
            <TabButton active={tab === "graph"} onClick={() => setTab("graph")} icon={<GitBranch size={12} />}>
              任务图
            </TabButton>
            <TabButton active={tab === "board"} onClick={() => setTab("board")} icon={<Kanban size={12} />}>
              看板
              {data.boards.length > 0 && <Count n={data.boards.length} />}
            </TabButton>
            <TabButton active={tab === "log"} onClick={() => setTab("log")} icon={<ListChecks size={12} />}>
              动态
            </TabButton>
          </div>
        )}

        <div className="ml-auto flex items-center gap-1.5">
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
                title={data.remote ? `同步远端 ${data.remote}（只做快进）` : "同步远端（还没配远端）"}
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
        <InitPanel onInit={doInit} busy={busy} cwd={cwd} />
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
                所有人指向同一个 git 仓库，目标与任务才能互相同步。
                远端不必叫 <span className="mono">origin</span>，已有的任何一个都会被用上；
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
                <CircleAlert size={12} />
                有 {data.warnings.length} 个文件读不出来
              </div>
              {data.warnings.map((w) => (
                <div key={w} className="mono truncate text-dim">
                  {w}
                </div>
              ))}
            </div>
          )}

          {tab === "mine" && (
            <MineTab data={data} inbox={inbox} seen={seen} onOpenTask={openTask} onJoin={() => setJoining(true)} />
          )}
          {tab === "graph" && (
            <GraphTab data={data} goal={activeGoal} onPick={setPicked} onAsk={askCarter} onOpenTask={openTask} />
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
              <div className={`truncate text-xs ${active ? "font-medium text-ink" : "text-ink/90"}`}>
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
          目标、任务和看板跟着项目走，存在项目的 <span className="mono">.carter/</span> 目录里，
          随 git 与团队同步。
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
}: {
  onInit: () => void;
  busy: boolean;
  cwd: string;
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
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 任务图
// ---------------------------------------------------------------------------

function GraphTab({
  data,
  goal,
  onPick,
  onAsk,
  onOpenTask,
}: {
  data: TeamOverview;
  goal: TeamGoal | null;
  onPick: (id: string) => void;
  onAsk: (draft?: string) => void;
  onOpenTask: (goal: TeamGoal, task: TeamTask) => void;
}) {
  if (data.goals.length === 0) {
    return (
      <Empty
        icon={<Target size={20} />}
        title="还没有目标"
        hint="目标由卡特在对话里帮你建：说清要做成什么，它会拆成任务、理好先后。"
        action="让卡特建一个目标"
        onAction={() => onAsk("帮我在团队目录里建一个目标：")}
      />
    );
  }

  return (
    <div className="flex min-h-0 gap-0">
      {/* 目标列 */}
      <div className="w-[15rem] shrink-0 border-r border-edge p-3">
        <div className="mb-2 px-1 text-[10px] font-medium uppercase tracking-wider text-dim">
          目标 · {data.goals.length}
        </div>
        <div className="space-y-1.5">
          {data.goals.map((g) => (
            <GoalCard
              key={g.id}
              goal={g}
              active={goal?.id === g.id}
              onClick={() => onPick(g.id)}
            />
          ))}
        </div>
      </div>

      {/* 图 */}
      <div className="min-w-0 flex-1 p-5">
        {goal ? <GoalGraph goal={goal} onAsk={onAsk} onOpenTask={onOpenTask} /> : null}
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
      className={`w-full rounded-xl p-2.5 text-left transition-colors ${
        active ? "bg-card shadow-soft" : "hover:bg-ink/[0.04]"
      }`}
    >
      <div className="mb-1 flex items-center gap-1.5">
        <span
          className="size-1.5 shrink-0 rounded-full"
          style={{ background: tone.fg }}
        />
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
  goal,
  onAsk,
  onOpenTask,
}: {
  goal: TeamGoal;
  onAsk: (draft?: string) => void;
  onOpenTask: (goal: TeamGoal, task: TeamTask) => void;
}) {
  const { people } = usePeople();
  // 按人筛：只看某个人的任务（其余的淡出而不是藏起来 —— 依赖关系还要看得见）。
  // `null` = 全部，`""` = 没人负责的。
  const [who, setWho] = useState<string | null>(null);
  const assignees = useMemo(() => {
    const seen = new Map<string, number>();
    for (const t of goal.tasks) if (t.assignee) seen.set(t.assignee, (seen.get(t.assignee) ?? 0) + 1);
    return [...seen];
  }, [goal.tasks]);
  const layers = useMemo(() => layerize(goal.tasks), [goal.tasks]);

  // 泳道 = 依赖深度。同一层内按 `order`（后端已经排好）—— 保持稳定，不抖动。
  const lanes = useMemo(() => {
    const max = Math.max(0, ...[...layers.values()]);
    const out: TeamTask[][] = Array.from({ length: max + 1 }, () => []);
    for (const t of goal.tasks) {
      out[layers.get(t.id) ?? 0].push(t);
    }
    return out;
  }, [goal.tasks, layers]);

  const byId = useMemo(() => new Map(goal.tasks.map((t) => [t.id, t])), [goal.tasks]);

  return (
    <div className="min-w-0">
      <div className="mb-3 flex flex-wrap items-end justify-between gap-2">
        <div className="min-w-0">
          <h2 className="truncate text-base font-semibold tracking-tight">{goal.title}</h2>
          {goal.description && (
            <p className="mt-0.5 max-w-2xl text-xs leading-relaxed text-dim">
              {goal.description}
            </p>
          )}
        </div>
        <div className="flex items-center gap-2 text-[11px] text-dim">
          <Ring value={goal.percent} />
          <span className="tabular-nums">
            {goal.counts.done}/{goal.counts.total - goal.counts.dropped} 完成
          </span>
          {goal.counts.blocked > 0 && (
            <span style={{ color: "var(--tone-orange)" }}>
              {goal.counts.blocked} 受阻
            </span>
          )}
          {goal.counts.doing > 0 && (
            <span style={{ color: "var(--tone-blue)" }}>{goal.counts.doing} 进行中</span>
          )}
          <button
            onClick={() =>
              onAsk(
                goal.tasks.length === 0
                  ? `把目标「${goal.title}」（${goal.id}）拆成任务，理好依赖。`
                  : `看一下目标「${goal.title}」（${goal.id}）的进展，接下来该推进哪件？`,
              )
            }
            className="ml-1 flex items-center gap-1 rounded-md bg-ink/5 px-2 py-1 text-[11px] text-dim transition-colors hover:bg-ink/10 hover:text-ink"
          >
            <Sparkles size={11} />
            {goal.tasks.length === 0 ? "让卡特拆解" : "问问卡特"}
          </button>
        </div>
      </div>

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
          这个目标还没有任务。点右上角「让卡特拆解」，它会在这个项目的对话里拆好。
        </div>
      ) : (
        <div className="relative flex gap-4 overflow-x-auto pb-2">
          {lanes.map((lane, i) => (
            <div key={i} className="flex w-[16.5rem] shrink-0 flex-col gap-2">
              <div className="mb-0.5 px-1 text-[10px] uppercase tracking-wider text-dim">
                {i === 0 ? "起点" : `第 ${i} 层`}
              </div>
              {lane.map((t) => (
                <TaskNode
                  key={t.id}
                  task={t}
                  byId={byId}
                  goal={goal}
                  onAsk={onAsk}
                  onOpen={() => onOpenTask(goal, t)}
                  dim={who !== null && (who === "" ? !!t.assignee : t.assignee !== who)}
                />
              ))}
              {lane.length === 0 && (
                <div className="rounded-lg border border-dashed border-edge px-2 py-6 text-center text-[10px] text-dim">
                  空
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* 依赖关系单独列一块：图上画箭头在横向溢出时会被裁掉，而「谁在等谁」
          是这块最重要的信息，不能只在悬浮里给。 */}
      {goal.tasks.some((t) => t.depends_on.length > 0) && (
        <div className="mt-4 rounded-xl bg-card p-3">
          <div className="mb-1.5 text-[10px] uppercase tracking-wider text-dim">依赖</div>
          <div className="space-y-1">
            {goal.tasks
              .filter((t) => t.depends_on.length > 0)
              .map((t) => (
                <div key={t.id} className="flex flex-wrap items-center gap-1 text-[11px]">
                  {t.depends_on.map((d) => (
                    <span key={d} className="flex items-center gap-1">
                      <span className="rounded bg-bubble px-1.5 py-0.5 text-dim">
                        {byId.get(d)?.title ?? d}
                      </span>
                      <ArrowRight size={10} className="text-dim" />
                    </span>
                  ))}
                  <span className={t.waiting_on.length > 0 ? "text-ink" : "text-dim"}>
                    {t.title}
                  </span>
                  {t.waiting_on.length > 0 && (
                    <span className="text-[10px] text-dim">（还差 {t.waiting_on.length} 个）</span>
                  )}
                </div>
              ))}
          </div>
        </div>
      )}
    </div>
  );
}

function TaskNode({
  task,
  byId,
  goal,
  onAsk,
  onOpen,
  dim,
}: {
  task: TeamTask;
  byId: Map<string, TeamTask>;
  goal: TeamGoal;
  onAsk: (draft?: string) => void;
  onOpen: () => void;
  dim: boolean;
}) {
  const { people } = usePeople();
  const closed = task.status === "done" || task.status === "dropped";
  const tone = statusTone(task.status);
  const parent = task.parent_id ? byId.get(task.parent_id) : null;

  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === "Enter") onOpen();
      }}
      title="打开任务：看要求、指派、讨论"
      className={`group relative cursor-pointer rounded-xl bg-card p-2.5 shadow-soft transition-[box-shadow,opacity] hover:shadow-lift ${
        dim ? "opacity-35" : ""
      }`}
      style={{
        // 能开工的左边挂一条彩色竖线 —— 一眼能扫出「现在能做哪几件」。
        boxShadow: task.ready
          ? "inset 2px 0 0 0 var(--tone-green), var(--shadow-soft)"
          : undefined,
      }}
    >
      <div className="mb-1 flex items-center gap-1.5">
        <span
          className="rounded px-1.5 py-[1px] text-[10px] font-medium"
          style={{ background: tone.bg, color: tone.fg }}
        >
          {task.status_label}
        </span>
        <span className="text-[10px]" style={{ color: priorityTone(task.priority).fg }}>
          {task.priority}
        </span>
        {task.ready && (
          <span className="text-[10px] text-tone-green">可开工</span>
        )}
        <span className="ml-auto flex items-center gap-1">
          {task.assignee ? (
            <span className="flex items-center gap-1 text-[10px] text-dim">
              {personName(people, task.assignee)}
              <PersonAvatar person={task.assignee} size={16} />
            </span>
          ) : (
            !closed && <span className="text-[10px] text-dim opacity-0 group-hover:opacity-100">待指派</span>
          )}
        </span>
      </div>

      <div className="text-xs font-medium leading-snug">{task.title}</div>

      {parent && (
        <div className="mt-0.5 truncate text-[10px] text-dim">
          ↳ 在「{parent.title}」下
        </div>
      )}

      {task.blocked_reason && (
        <div className="mt-1 rounded bg-tone-orange-soft px-1.5 py-1 text-[10px] text-tone-orange">
          {task.blocked_reason}
        </div>
      )}

      {task.children.length > 0 && (
        <div className="mt-1.5">
          <div className="mb-1 flex items-center justify-between text-[10px] text-dim">
            <span>子任务 {task.children.length}</span>
            <span className="tabular-nums">{task.progress}%</span>
          </div>
          <Progress value={task.progress} tone={tone.fg} />
        </div>
      )}

      <div className="mt-1.5 flex items-center gap-1.5 text-[10px] text-dim">
        <span className="mono truncate">{task.id}</span>
        {task.estimate_hours ? <span>· {task.estimate_hours}h</span> : null}
        {/* 悬停才露出来：每张卡常驻一个按钮，整屏就是一片按钮。 */}
        {!closed && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              onAsk(
                `接着做这个任务：\n\n` +
                  `- 任务：${task.title}\n` +
                  `- 任务 id：${task.id}\n` +
                  `- 所属目标：${goal.title}（${goal.id}）\n` +
                  `- 当前状态：${task.status_label}\n` +
                  (task.blocked_reason ? `- 受阻原因：${task.blocked_reason}\n` : "") +
                  `\n先看一眼任务的具体要求（acceptance 里写了验收标准），再动手。`,
              );
            }}
            title="去这个项目的对话里，把任务摆进输入框"
            className="ml-auto hidden shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[10px] text-dim transition-colors hover:bg-ink/5 hover:text-ink group-hover:flex"
          >
            <Send size={10} />
            交给卡特
          </button>
        )}
      </div>
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

  useEffect(() => {
    if (!active) return;
    let live = true;
    teamBoardHtml(cwd, active.id)
      .then((html) => {
        if (!live) return;
        // 内容没变就不换对象：srcdoc 一变 iframe 就重载，滚动位置与页面状态全丢。
        setPage((prev) => (prev?.id === active.id && prev.html === html ? prev : { id: active.id, html }));
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
        onAction={() => onAsk("帮我建一块回顾看板：")}
      />
    );
  }
  return (
    <div className="flex min-h-full gap-4 p-5">
      <div className="w-[12rem] shrink-0 space-y-0.5">
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
          onClick={() => onAsk(`帮我改一下看板「${active.title}」（${active.id}）：`)}
          className="mt-2 flex w-full items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-[11px] text-dim transition-colors hover:bg-ink/[0.04] hover:text-ink"
        >
          <Sparkles size={12} />
          让卡特改这块看板
        </button>
        <button
          onClick={() => onAsk("帮我建一块看板：")}
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
