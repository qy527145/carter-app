// 会话页：跨所有项目的全量列表 + 全文搜索 + 重命名 / fork / 导出 / 删除。
//
// 侧栏同样按目录分组，但只展开当前目录、行也更紧凑；这里多出来的是全部展开的
// 宽松视图、文件大小，以及跨全部会话的全文搜索。
//
// 两个列表共用 `SessionList` 的行，行内动作只有一份实现。

import { useEffect, useMemo, useState } from "react";
import { Folder, Search, X } from "lucide-react";

import { listSessions, searchSessions, type SearchHit, type SessionRow } from "../../lib/api";
import { fmtBytes, fullTime, relTime, shortenPath } from "../../lib/format";
import { displayName, modelTitle, useModelCatalog } from "../../lib/modelLabel";
import { toastError } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { groupByProject, SessionList } from "../../components/SessionList";
import { openSession } from "../../lib/sessionOps";
import { useStore } from "../../store";
import { Button, IconButton } from "../../components/ui/Button";
import { Input } from "../../components/ui/Input";
import { Badge, Card, Spinner } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

/** 搜索命中列表。与「列表」是两种视图，所以单独一块。 */
/** 片段里把命中词标出来（不区分大小写，标全部出现处）。 */
function Marked({ text, query }: { text: string; query: string }) {
  const q = query.trim();
  if (!q) return <>{text}</>;
  const parts = text.split(new RegExp(`(${q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, "gi"));
  return (
    <>
      {parts.map((p, i) =>
        i % 2 === 1 ? (
          <mark key={i} className="rounded-[3px] bg-tone-orange-soft px-px text-ink">
            {p}
          </mark>
        ) : (
          p
        ),
      )}
    </>
  );
}

function SearchResults({
  hits,
  query,
  onOpen,
  onClear,
}: {
  hits: SearchHit[];
  query: string;
  onOpen: (id: string) => void;
  onClear: () => void;
}) {
  const models = useModelCatalog();
  return (
    <div
      className="space-y-2"
      // 结果之间 ↑↓ 走动；到顶再 ↑ 回到搜索框。
      onKeyDown={(e) => {
        if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
        const all = Array.from(e.currentTarget.querySelectorAll<HTMLElement>("[data-hit]"));
        const i = all.indexOf(document.activeElement as HTMLElement);
        if (i < 0) return;
        e.preventDefault();
        const next = i + (e.key === "ArrowDown" ? 1 : -1);
        if (next < 0) document.querySelector<HTMLInputElement>("[data-session-search]")?.focus();
        else all[Math.min(next, all.length - 1)].focus();
      }}
    >
      <div className="flex items-center gap-2 text-xs text-dim">
        <span>命中 {hits.length} 条会话</span>
        <IconButton title="清空搜索" onClick={onClear}>
          <X size={12} />
        </IconButton>
      </div>
      {hits.map((h) => (
        <Card key={h.id} onClick={() => onOpen(h.id)} className="carter-press space-y-1" data-hit>
          <div className="flex items-center gap-2">
            <span className="truncate text-sm text-ink">
              <Marked text={h.title || "未命名会话"} query={query} />
            </span>
            <Badge tone="blue">{h.matches} 处命中</Badge>
            <span className="ml-auto shrink-0 text-[11px] text-dim" title={fullTime(h.created_at)}>
              {relTime(h.created_at)}
            </span>
          </div>
          <div className="text-xs leading-relaxed text-dim">
            <Marked text={h.snippet} query={query} />
          </div>
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
  const groups = useMemo(() => groupByProject(sessions), [sessions]);

  return (
    <>
      <h1 className="mb-4 text-base font-semibold">会话</h1>

      <PageIntro>
        这里是<strong className="font-medium text-ink">全部</strong>会话，按工作目录分组，
        包括当前对话之外的目录。会话文件在{" "}
        <span className="mono">~/.carter/projects/&lt;项目&gt;/*.jsonl</span>，
        与 carter 命令行共用 —— 那边也能 `/resume` 到这些对话。
        点开别的目录的会话时，工具与工作目录都跟着那条会话自己记的目录走。
      </PageIntro>

      <div className="relative mb-4">
        <Search
          size={13}
          className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-dim"
        />
        <Input
          autoFocus
          data-session-search
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && query) {
              e.preventDefault();
              setQuery("");
            } else if (e.key === "ArrowDown") {
              const first = document.querySelector<HTMLElement>("[data-hit], [data-session-row]");
              if (first) {
                e.preventDefault();
                first.focus();
              }
            }
          }}
          placeholder="搜索全部会话内容（跨所有目录）…"
          className="pl-8 pr-8"
        />
        {searching ? (
          <span className="absolute right-2.5 top-1/2 -translate-y-1/2">
            <Spinner size={13} />
          </span>
        ) : (
          query && (
            <button
              onClick={() => setQuery("")}
              aria-label="清空"
              className="carter-press absolute right-2 top-1/2 -translate-y-1/2 rounded p-0.5 text-dim hover:text-ink"
            >
              <X size={13} />
            </button>
          )
        )}
      </div>

      {hits !== null ? (
        searching && hits.length === 0 ? (
          <div className="flex items-center justify-center gap-2 py-8 text-sm text-dim">
            <Spinner />
            搜索中…
          </div>
        ) : hits.length === 0 ? (
          <div className="py-8 text-center text-sm text-dim">
            没有找到包含「{query.trim()}」的会话。
          </div>
        ) : (
          <SearchResults
            hits={hits}
            query={query}
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

              <div data-session-scope className="space-y-5">
                {groups.map((g) => (
                  <div key={g.cwd}>
                    <div className="sticky top-0 z-[1] mb-1 flex items-center gap-1.5 bg-base px-3 py-1 text-[11px] text-dim">
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