// 会话页：跨所有项目的全量列表 + 全文搜索 + 重命名 / fork / 导出 / 删除。
//
// 侧栏那个列表是「**当前目录**最近在聊的几条」，这里多出来的两件事才是本页存在的
// 理由：属于别的 cwd 的会话（`list_sessions(all=true)`），以及跨全部会话的全文搜索。
// 所以按项目分组而不是按今天/昨天 —— 时间分组侧栏已经有一份，按目录分组只有这里有，
// 而且「点开一条别的目录的会话」正是本页最需要说清楚的操作。
//
// 两个列表共用 `SessionList` 的行，行内动作只有一份实现。

import { useEffect, useMemo, useState } from "react";
import { Folder, Search, X } from "lucide-react";

import { listSessions, searchSessions, type SearchHit, type SessionRow } from "../../lib/api";
import { fmtBytes, fullTime, relTime, shortenPath } from "../../lib/format";
import { displayName, modelTitle, useModelCatalog } from "../../lib/modelLabel";
import { toastError } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { SessionList } from "../../components/SessionList";
import { openSession } from "../../lib/sessionOps";
import { useStore } from "../../store";
import { Button, IconButton } from "../../components/ui/Button";
import { Input } from "../../components/ui/Input";
import { Badge, Card } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

/** 一处目录下的一批会话。 */
type ProjectGroup = { cwd: string; sessions: SessionRow[] };

/**
 * 按所属目录分组，当前目录排第一，其余按最近一条会话的时间排。
 *
 * 后端返回的列表已经按 created_at 倒序，所以组内不用再排，组间取每组的首条比较即可。
 */
function groupByProject(sessions: SessionRow[], currentCwd: string): ProjectGroup[] {
  const groups = new Map<string, SessionRow[]>();
  for (const s of sessions) {
    const list = groups.get(s.cwd);
    if (list) list.push(s);
    else groups.set(s.cwd, [s]);
  }
  const out = [...groups].map(([cwd, sessions]) => ({ cwd, sessions }));
  return out.sort((a, b) => {
    if (a.cwd === currentCwd) return -1;
    if (b.cwd === currentCwd) return 1;
    return (b.sessions[0]?.created_at ?? 0) - (a.sessions[0]?.created_at ?? 0);
  });
}

/** 搜索命中列表。与「列表」是两种视图，所以单独一块。 */
function SearchResults({
  hits,
  onOpen,
  onClear,
}: {
  hits: SearchHit[];
  onOpen: (id: string) => void;
  onClear: () => void;
}) {
  const models = useModelCatalog();
  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2 text-xs text-dim">
        <span>命中 {hits.length} 条会话</span>
        <IconButton title="清空搜索" onClick={onClear}>
          <X size={12} />
        </IconButton>
      </div>
      {hits.map((h) => (
        <Card key={h.id} onClick={() => onOpen(h.id)} className="space-y-1">
          <div className="flex items-center gap-2">
            <span className="truncate text-sm text-ink">{h.title || "未命名会话"}</span>
            <Badge tone="blue">{h.matches} 处命中</Badge>
            <span className="ml-auto shrink-0 text-[11px] text-dim" title={fullTime(h.created_at)}>
              {relTime(h.created_at)}
            </span>
          </div>
          <div className="text-xs leading-relaxed text-dim">{h.snippet}</div>
          <div className="mono flex items-center gap-1.5 truncate text-[10px] text-dim/70">
            <span className="truncate" title={modelTitle(h.model, models)}>
              {displayName(h.model, models)}
            </span>
            <span>·</span>
            <span className="truncate" title={h.cwd}>
              {shortenPath(h.cwd)}
            </span>
          </div>
        </Card>
      ))}
    </div>
  );
}

export function SessionsPage() {
  // 侧栏里改名 / fork / 删掉一条时，这一份也得跟着变 —— 两处各显示一份不同的
  // 会话表是最难解释的那种不一致。`sessionsNonce` 就是「别处的列表动过了」。
  const sessionsNonce = useStore((s) => s.sessionsNonce);
  const state = useAsync(() => listSessions(true), [sessionsNonce]);
  // 高亮当前正开着的那条；当前目录那个分组也标出来。
  const activeId = useStore((s) => s.sessionId);
  const currentCwd = useStore((s) => s.cwd);

  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [searching, setSearching] = useState(false);

  // 搜索按 250ms 防抖 —— 每敲一个字就扫一遍全部会话文件，长会话多了会卡。
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setHits(null);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const timer = setTimeout(() => {
      searchSessions(q)
        .then((r) => {
          if (!cancelled) setHits(r);
        })
        .catch((e) => {
          if (!cancelled) {
            setHits([]);
            toastError(e);
          }
        })
        .finally(() => {
          if (!cancelled) setSearching(false);
        });
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [query]);

  const sessions = state.data ?? [];
  const bytes = sessions.reduce((a, s) => a + s.bytes, 0);
  const groups = useMemo(
    () => groupByProject(sessions, currentCwd),
    [sessions, currentCwd],
  );

  return (
    <>
      <h1 className="mb-4 text-base font-semibold">会话</h1>

      <PageIntro>
        这里是<strong className="font-medium text-ink">全部</strong>会话，按工作目录分组，
        包括当前对话之外的目录。会话文件在{" "}
        <span className="mono">~/.carter/projects/&lt;项目&gt;/*.jsonl</span>，
        与 carter 命令行共用 —— 那边也能 `/resume` 到这些对话。
        侧栏只列当前目录的那些；点开这里别的目录的会话时，工具与工作目录都跟着那条会话
        自己记的目录走。
      </PageIntro>

      <div className="relative mb-4">
        <Search
          size={13}
          className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-dim"
        />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索全部会话内容（跨所有目录）…"
          className="pl-8"
        />
        {query && (
          <button
            onClick={() => setQuery("")}
            aria-label="清空"
            className="absolute right-2.5 top-1/2 -translate-y-1/2 text-dim transition-colors hover:text-ink"
          >
            <X size={13} />
          </button>
        )}
      </div>

      {hits !== null ? (
        searching && hits.length === 0 ? (
          <div className="py-8 text-center text-sm text-dim">搜索中…</div>
        ) : hits.length === 0 ? (
          <div className="py-8 text-center text-sm text-dim">
            没有找到包含「{query.trim()}」的会话。
          </div>
        ) : (
          <SearchResults
            hits={hits}
            onOpen={(id) => {
              setQuery("");
              openSession(id);
            }}
            onClear={() => setQuery("")}
          />
        )
      ) : (
        <AsyncBody state={state} empty={<>还没有历史会话。</>}>
          {(rows: SessionRow[]) => (
            <>
              <div className="mb-3 flex items-center gap-2 text-[11px] text-dim">
                <span>{rows.length} 条</span>
                <span>·</span>
                <span>{groups.length} 个目录</span>
                <span>·</span>
                <span>{fmtBytes(bytes)}</span>
                <Button size="sm" variant="ghost" className="ml-auto" onClick={state.reload}>
                  刷新
                </Button>
              </div>

              <div className="space-y-4">
                {groups.map((g) => (
                  <div key={g.cwd}>
                    <div className="mb-1 flex items-center gap-1.5 px-3 text-[11px] text-dim">
                      <Folder size={11} className="shrink-0" />
                      <span className="mono truncate" title={g.cwd}>
                        {shortenPath(g.cwd)}
                      </span>
                      {g.cwd === currentCwd && <Badge tone="green">当前</Badge>}
                      <span className="ml-auto shrink-0">{g.sessions.length} 条</span>
                    </div>
                    {/* 本页已经按目录分组，再套一层今天/昨天就是两层表头。 */}
                    <SessionList
                      sessions={g.sessions}
                      activeId={activeId}
                      dense={false}
                      groupByTime={false}
                    />
                  </div>
                ))}
              </div>
            </>
          )}
        </AsyncBody>
      )}
    </>
  );
}