import {
  ChevronRight,
  CircleCheck,
  FileText,
  GitBranch,
  ListChecks,
  LoaderCircle,
  MoreHorizontal,
  Pencil,
  ScanSearch,
  Settings2,
  Share,
  TriangleAlert,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { isImeComposing } from "../../lib/ime";
import { ASSISTANT_CHAT_ID, export_, fork, rename } from "../../lib/sessionOps";
import { openSettings } from "../../lib/settingsTab";
import { ticketMarkTitle } from "../../lib/ticketUi";
import { useStore } from "../../store";
import { AskForm } from "../AskForm";
import { BackgroundTasksBar } from "../BackgroundTasksBar";
import { Composer } from "../Composer";
import { GoalBar } from "../GoalBar";
import { MemorySuggestBar } from "../MemorySuggestBar";
import { ModePill } from "../ModePill";
import { TeamTodoDock } from "../TeamTodoDock";
import { UpdateNotice } from "../UpdateNotice";
import { Button, IconButton } from "../ui/Button";
import { type MenuEntry, useContextMenu } from "../ui/ContextMenu";
import { Banner, Collapse } from "../ui/Panel";
import { ChangesPanel } from "./changes";
import { AssistantHeader, MessageList } from "./list";

// ---------------------------------------------------------------------------
// 变更 / 待办 / 提示条 / 提问面板 / 输入框
// ---------------------------------------------------------------------------

/**
 * 底栏的待办进度。默认折叠成一行（进度条 + 当前项），点击展开整张清单。
 *
 * **全部完成就自动收起** —— 之前它会一直挂在那里（`todos.length > 0` 就渲染），
 * 一轮做完之后还占着版面，用户分不清那是「还要做」还是「已经做完了」。
 * 清单本身在对话流里有一张 todo_write 卡片（可折叠、可回看），底栏这条只是
 * 「此刻进行到哪」的提示，做完就该让位。
 */
function TodoPanel() {
  const todos = useStore((s) => s.todos);
  const [dismissed, setDismissed] = useState(false);
  // 默认折叠成一行：整张清单常驻底栏太占地方，对话流里那张 todo_write 卡片
  // 才是看全貌的地方，这里点开即可临时展开。
  const [expanded, setExpanded] = useState(false);

  const allDone = todos.length > 0 && todos.every((t) => t.status === "completed");

  // 全部完成之后**自己收起来**。
  //
  // 留 6 秒而不是立刻消失：最后一项打勾得让人看见，否则「做完了没有」这件事
  // 会变成一个悬案。留太久又等于没做 —— 一轮做完之后它还挂在那里，用户分不清
  // 那是「还要做」还是「已经做完了」。
  useEffect(() => {
    if (!allDone) {
      // 新一轮开始（出现未完成项）时重置，否则后续任务不再显示进度。
      setDismissed(false);
      return;
    }
    const t = setTimeout(() => setDismissed(true), 6000);
    return () => clearTimeout(t);
  }, [allDone]);

  if (todos.length === 0) return null;
  if (allDone && dismissed) return null;

  const done = todos.filter((t) => t.status === "completed").length;
  // 折叠时标题行只露「此刻在做的那一项」；没有进行中的就露下一项待做的。
  const current =
    todos.find((t) => t.status === "in_progress") ?? todos.find((t) => t.status === "pending");

  return (
    <div className="shrink-0 px-4 pt-2">
      <div className="mx-auto max-w-3xl rounded-xl bg-card px-3 py-1.5">
        <div className="flex items-center gap-2">
          <button
            onClick={() => setExpanded((o) => !o)}
            title={expanded ? "折叠待办" : "展开待办"}
            className="flex min-w-0 flex-1 items-center gap-2 py-0.5 text-left text-xs text-dim transition-colors hover:text-ink"
          >
            <ChevronRight
              size={12}
              className={`shrink-0 transition-transform ${expanded ? "rotate-90" : ""}`}
            />
            <span className="shrink-0 tabular-nums">
              待办 {done}/{todos.length}
            </span>
            <span className="h-1 w-16 shrink-0 overflow-hidden rounded-full bg-ink/[0.08]">
              <span
                className="block h-full rounded-full bg-[var(--ok)] transition-[width] duration-300"
                style={{ width: `${(done / todos.length) * 100}%` }}
              />
            </span>
            {allDone ? (
              <span className="text-[var(--ok)]">已全部完成</span>
            ) : (
              !expanded &&
              current && (
                <span className="flex min-w-0 items-center gap-1.5 text-ink">
                  {current.status === "in_progress" && (
                    <LoaderCircle size={12} className="shrink-0 animate-spin text-tone-blue" />
                  )}
                  <span className="truncate">
                    {current.status === "in_progress" ? current.active_form : current.content}
                  </span>
                </span>
              )
            )}
          </button>
          <IconButton title="关闭待办" onClick={() => setDismissed(true)}>
            <X size={12} />
          </IconButton>
        </div>
        <Collapse open={expanded}>
          {/* 清单很长时限高滚动，别把对话区挤没了。 */}
          <div className="max-h-48 space-y-1 overflow-y-auto pb-1 pl-5 pt-1">
            {todos.map((t, i) => (
              // biome-ignore lint/suspicious/noArrayIndexKey: 清单内容可能重复，位置才是身份
              <div key={i} className="flex items-center gap-2 text-xs">
                {t.status === "completed" ? (
                  <CircleCheck size={13} className="carter-pop shrink-0 text-[var(--ok)]" />
                ) : t.status === "in_progress" ? (
                  <LoaderCircle size={13} className="shrink-0 animate-spin text-tone-blue" />
                ) : (
                  <span className="inline-block size-3 shrink-0 rounded-full border border-edge" />
                )}
                <span className={t.status === "completed" ? "text-dim line-through" : "text-ink"}>
                  {t.status === "in_progress" ? t.active_form : t.content}
                </span>
              </div>
            ))}
          </div>
        </Collapse>
      </div>
    </div>
  );
}

/**
 * 默认模型还不能用（首次启动只有空的起步渠道、没填密钥之类）时的提示条。
 *
 * 不可关闭：缺的东西不补上，发出去的每一句都会失败。补上之后后端广播
 * `config_changed`，应用信息重拉、`setup_hint` 变空，这条自己就收起了。
 */
function SetupBanner() {
  const hint = useStore((s) => s.appInfo?.setup_hint);
  if (!hint) return null;
  return (
    <div className="px-4 pt-3">
      <div className="mx-auto max-w-3xl">
        <Banner
          kind="info"
          action={
            <Button size="sm" onClick={() => openSettings("channels")}>
              去设置
            </Button>
          }
        >
          <div className="flex items-start gap-2 pt-0.5">
            <Settings2 size={14} className="icon-optical shrink-0 text-tone-blue" />
            <span className="break-words">{hint}，请到「设置 → 渠道」里配置模型后再开始对话。</span>
          </div>
        </Banner>
      </div>
    </div>
  );
}

function NoticeBanner() {
  const notice = useStore((s) => s.notice);
  const setNotice = useStore((s) => s.setNotice);
  if (!notice) return null;
  return (
    <div className="px-4 pt-3">
      <div className="mx-auto max-w-3xl">
        <Banner kind="warn" onClose={() => setNotice(null)}>
          <div className="flex items-start gap-2">
            <TriangleAlert size={14} className="icon-optical shrink-0" />
            <span className="whitespace-pre-wrap break-words">{notice}</span>
          </div>
        </Banner>
      </div>
    </div>
  );
}

/**
 * 顶部条：当前会话标题 + 呈现密度 + 打开报文面板的入口。
 *
 * 标题**点一下就能改**：会话名是对话管理里最常改的东西，不该要人回到侧栏、
 * 找到那一行再右键。还没落盘的新对话（列表里没有这一条）不给改 —— 后端没有文件可写。
 */
function ChatHeader() {
  const sessionId = useStore((s) => s.sessionId);
  const sessions = useStore((s) => s.sessions);
  const openInspector = useStore((s) => s.openInspector);
  const menu = useContextMenu();
  const row = sessions.find((s) => s.id === sessionId);
  const title = row?.title;
  // 这条会话是助理派出去的工单：标题照常显示（就是派单时那个名字，不带编号），
  // 只在旁边加一枚标记 —— 工单对话能用的工具与普通对话不一样，得让人看得出。
  const ticket = useStore((s) =>
    sessionId ? s.assistant?.tickets.find((t) => t.session_id === sessionId) : undefined,
  );
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const settled = useRef(false);

  if (sessionId === ASSISTANT_CHAT_ID) return <AssistantHeader />;

  const startEdit = () => {
    if (!row || !sessionId) return;
    settled.current = false;
    setDraft(title || "");
    setEditing(true);
  };
  const finish = (commit: boolean) => {
    if (settled.current) return;
    settled.current = true;
    setEditing(false);
    const t = draft.trim();
    if (commit && sessionId && t && t !== title) void rename(sessionId, t);
  };

  const sessionMenu = (): MenuEntry[] =>
    sessionId && row
      ? [
          { label: "重命名", icon: <Pencil size={12} />, onSelect: startEdit },
          {
            label: "复制一份",
            icon: <GitBranch size={12} />,
            onSelect: () => void fork(sessionId),
          },
          "separator",
          {
            label: "导出 Markdown",
            icon: <Share size={12} />,
            onSelect: () => void export_(sessionId, "markdown"),
          },
          {
            label: "导出 JSON",
            icon: <FileText size={12} />,
            onSelect: () => void export_(sessionId, "json"),
          },
          "separator",
          {
            label: "查看 HTTP 报文",
            icon: <ScanSearch size={12} />,
            onSelect: () => openInspector(null),
          },
        ]
      : [];

  return (
    <div className="flex h-11 shrink-0 items-center gap-2 border-b border-edge px-4">
      {editing ? (
        <input
          autoFocus
          value={draft}
          aria-label="会话名称"
          onFocus={(e) => e.currentTarget.select()}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => finish(true)}
          onKeyDown={(e) => {
            if (isImeComposing(e)) return;
            if (e.key === "Enter") finish(true);
            if (e.key === "Escape") finish(false);
          }}
          className="carter-field -ml-1.5 w-80 max-w-[60%] rounded-md border border-edge-strong bg-elevated px-1.5 py-0.5 text-sm font-medium text-ink outline-none"
        />
      ) : (
        <button
          onClick={startEdit}
          disabled={!row}
          title={row ? "点击重命名" : "发出第一句话后可以重命名"}
          className="group/title -ml-1.5 flex min-w-0 items-center gap-1.5 rounded-md px-1.5 py-0.5 text-left outline-none enabled:hover:bg-ink/[0.045]"
        >
          <span className={`truncate text-sm ${title ? "font-medium text-ink" : "text-dim"}`}>
            {title || "新对话"}
          </span>
          {row && (
            <Pencil
              size={11}
              className="shrink-0 text-dim opacity-0 transition-opacity group-hover/title:opacity-100"
            />
          )}
        </button>
      )}
      {ticket && (
        <span
          title={ticketMarkTitle(ticket.status)}
          className="flex shrink-0 items-center gap-1 rounded bg-tone-violet-soft px-1.5 py-0.5 text-[10px] text-tone-violet"
        >
          <ListChecks size={10} />
          工单
        </span>
      )}
      <div className="ml-auto flex items-center gap-1">
        <ModePill className="mr-1" />
        <IconButton
          disabled={!sessionId}
          onClick={() => openInspector(null)}
          title="查看本次会话抓到的 HTTP 报文"
        >
          <ScanSearch size={15} />
        </IconButton>
        <IconButton
          disabled={!row}
          title="会话操作"
          aria-label="会话操作"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            menu.open({ x: r.right, y: r.bottom + 4, align: "end" }, sessionMenu());
          }}
        >
          <MoreHorizontal size={15} />
        </IconButton>
      </div>
    </div>
  );
}

function MainChangesPanel() {
  const sessionId = useStore((s) => s.sessionId);
  const turns = useStore((s) => s.turns);
  const changed = useStore((s) => s.changed);
  const since = useStore((s) => s.changesSince);
  const skip = useStore((s) => s.changesSkip);
  const cwd = useStore((s) => s.cwd);
  return (
    <ChangesPanel
      sessionId={sessionId}
      turns={turns}
      changedByTurn={changed}
      since={since}
      skip={skip}
      cwd={cwd}
    />
  );
}

export function ChatView() {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <ChatHeader />
      {/* 有新版时贴在顶栏下面：一条安静的行，不挡对话，点一下就装。
          放在 `NoticeBanner` 之前 —— 通知条讲的是「这一轮发生了什么」，
          它讲的是「这个应用本身」，后者靠上更像系统级的东西。 */}
      <UpdateNotice />
      <SetupBanner />
      <NoticeBanner />
      <MessageList />
      <MainChangesPanel />
      <TodoPanel />
      {/* 团队待办：贴着输入框，点一条就把它交给这条对话。
          放在 `TodoPanel` 之后 —— 那一块是「这一轮模型自己列的清单」，
          这一块是「团队账本里归我的活」，前者更临时，后者更持久。 */}
      <TeamTodoDock />
      <AskForm />
      {/* 目标条贴着输入框：它和「我要它干什么」是一件事，读起来是一句话。 */}
      <GoalBar />
      {/* 后台任务还在跑：前台这一轮可能已经结束了，这里要一直提醒到它跑完。 */}
      <BackgroundTasksBar />
      <MemorySuggestBar />
      <Composer />
    </div>
  );
}
