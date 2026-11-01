// 会话页：全量列表 + 搜索 + 重命名 / fork / 导出 / 删除。
//
// 侧栏那个列表是「最近在聊的几条」，这里是「全部」—— 还包括属于别的 cwd 的会话
// （`list_sessions(all=true)`）。两个列表共用 `SessionList` 的行，所以行内动作
// 只有一份实现。

import { useEffect, useState } from "react";
import { Search, X } from "lucide-react";

import { listSessions, searchSessions, type SearchHit, type SessionRow } from "../../lib/api";
import { fmtBytes, fullTime, relTime } from "../../lib/format";
import { toastError } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { SessionList } from "../../components/SessionList";
import { openSession } from "../../lib/sessionOps";
import { useStore } from "../../store";
import { Button, IconButton } from "../../components/ui/Button";
import { Input } from "../../components/ui/Input";
import { Badge, Card } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

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
          <div className="mono truncate text-[10px] text-dim/70">{h.model}</div>
        </Card>
      ))}
    </div>
  );
}

export function SessionsPage() {
  const state = useAsync(() => listSessions(true), []);
  // 高亮当前正开着的那条 —— 也要能从这里点它回到对话（见 `openSession`）。
  const activeId = useStore((s) => s.sessionId);

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

  const total = state.data?.length ?? 0;
  const bytes = state.data?.reduce((a, s) => a + s.bytes, 0) ?? 0;

  return (
    <>
      <h1 className="mb-4 text-base font-semibold">会话</h1>

      <PageIntro>
        这里是<strong className="font-medium text-ink">全部</strong>会话，包括属于其它目录的。会话文件在{" "}
        <span className="mono">~/.carter/projects/&lt;项目&gt;/*.jsonl</span>，
        与 carter 命令行共用 —— 那边也能 `/resume` 到这些对话。
      </PageIntro>

      <div className="relative mb-4">
        <Search
          size={13}
          className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-dim"
        />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索全部会话内容…"
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
          {(sessions: SessionRow[]) => (
            <>
              <div className="mb-3 flex items-center gap-2 text-[11px] text-dim">
                <span>{total} 条</span>
                <span>·</span>
                <span>{fmtBytes(bytes)}</span>
                <Button
                  size="sm"
                  variant="ghost"
                  className="ml-auto"
                  onClick={state.reload}
                >
                  刷新
                </Button>
              </div>
              <SessionList sessions={sessions} activeId={activeId} dense={false} />
            </>
          )}
        </AsyncBody>
      )}
    </>
  );
}
