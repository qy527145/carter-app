// 设置 / 管理（M7，只读）。
//
// 全是**只读**视图：改配置要写 `config.toml` 再热重载，那是另一件事（见后端注释）。
// 这里先把「看得到现状」做出来 —— 在此之前，用户想知道自己配了哪些渠道、有哪些技能，
// 只能去翻文件。凡是用户可能以为能编辑的地方，都明确写清「为什么现在不能改」。
//
// 会话页是唯一有写操作的地方（删除），因为那是数据而非配置。

import { useEffect, useState, type ReactNode } from "react";
import {
  ChevronRight,
  CircleCheck,
  CircleX,
  Info,
  KeyRound,
  LoaderCircle,
  MessagesSquare,
  Plug,
  RefreshCw,
  Server,
  Settings as SettingsIcon,
  Sparkles,
  Trash,
  TriangleAlert,
  Wrench,
} from "lucide-react";

import {
  closeSession,
  deleteSession,
  listChannels,
  listMcpServers,
  listSessions,
  listSkills,
  listTools,
  type ChannelRow,
  type McpServerRow,
  type SkillRow,
  type ToolRow,
} from "../lib/api";
import { useStore } from "../store";
import { Button } from "./ui/Button";

// ---------------------------------------------------------------------------
// 通用小件
// ---------------------------------------------------------------------------

type AsyncState<T> = {
  data: T | null;
  error: string | null;
  loading: boolean;
  reload: () => void;
};

/**
 * 一次性取数。`load` 每次渲染都是新函数，所以不放依赖，用 `deps + nonce` 控制重取。
 * 每个页签只在被激活时挂载，于是数据是「点开才拉」，不会一进设置就并发四条命令。
 */
function useAsync<T>(load: () => Promise<T>, deps: unknown[]): AsyncState<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    load()
      .then((d) => {
        if (!cancelled) {
          setData(d);
          setLoading(false);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setError(String(e));
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [...deps, nonce]);

  return { data, error, loading, reload: () => setNonce((n) => n + 1) };
}

function AsyncBody<T>({
  state,
  empty,
  children,
}: {
  state: AsyncState<T>;
  empty: ReactNode;
  children: (data: T) => ReactNode;
}) {
  if (state.loading) {
    return (
      <div className="flex items-center gap-2 py-8 text-sm text-slate-500">
        <LoaderCircle size={14} className="animate-spin" />
        加载中…
      </div>
    );
  }
  if (state.error) {
    return (
      <div className="flex items-start gap-2 rounded-lg border border-amber-900/50 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
        <TriangleAlert size={15} className="mt-0.5 shrink-0" />
        <span className="flex-1 break-words">{state.error}</span>
        <button
          onClick={state.reload}
          className="inline-flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-xs hover:bg-amber-900/40"
        >
          <RefreshCw size={12} />
          重试
        </button>
      </div>
    );
  }
  const data = state.data;
  if (data === null || (Array.isArray(data) && data.length === 0)) {
    return (
      <div className="rounded-lg border border-dashed border-slate-800 px-4 py-8 text-center text-sm leading-relaxed text-slate-500">
        {empty}
      </div>
    );
  }
  return <>{children(data)}</>;
}

/** 各页签底部的一句「为什么不能改」说明。 */
function ReadOnlyNote({ children }: { children: ReactNode }) {
  return (
    <div className="mt-4 flex items-start gap-1.5 text-[11px] leading-relaxed text-slate-600">
      <Info size={12} className="mt-0.5 shrink-0" />
      <span>{children}</span>
    </div>
  );
}

function Badge({ children, tone = "slate" }: { children: ReactNode; tone?: "slate" | "sky" }) {
  const cls =
    tone === "sky" ? "bg-sky-900/60 text-sky-300" : "bg-slate-700/70 text-slate-300";
  return <span className={`rounded px-1.5 py-0.5 text-[10px] ${cls}`}>{children}</span>;
}

// ---------------------------------------------------------------------------
// 渠道
// ---------------------------------------------------------------------------

function ChannelsTab() {
  const state = useAsync(listChannels, []);
  return (
    <>
      <AsyncBody
        state={state}
        empty={<>没有配置任何渠道。渠道写在配置文件的 [providers.*] 下。</>}
      >
        {(channels: ChannelRow[]) => (
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr className="border-b border-slate-800 text-left text-xs text-slate-500">
                <th className="py-2 pr-3 font-medium">名称</th>
                <th className="py-2 pr-3 font-medium">类型</th>
                <th className="py-2 pr-3 font-medium">Base URL</th>
                <th className="py-2 pr-3 font-medium">API Key</th>
                <th className="py-2 pr-3 font-medium">模型</th>
              </tr>
            </thead>
            {channels.map((c) => (
              <tbody key={c.name} className="border-b border-slate-800/70">
                <tr className="align-top hover:bg-slate-800/30">
                  <td className="py-2 pr-3">
                    <div className="flex items-center gap-1.5">
                      <span className="font-medium text-slate-200">{c.name}</span>
                      {c.is_default && <Badge tone="sky">默认</Badge>}
                    </div>
                  </td>
                  <td className="py-2 pr-3 font-mono text-xs text-slate-400">
                    {/* kind 与 protocol 不同 = 走了别名/兼容映射，两个都要显示，
                        否则用户会以为配置写错了。 */}
                    {c.kind === c.protocol ? (
                      c.kind
                    ) : (
                      <span className="whitespace-nowrap">
                        {c.kind} <span className="text-slate-600">→</span>{" "}
                        <span className="text-slate-300">{c.protocol}</span>
                      </span>
                    )}
                  </td>
                  <td className="py-2 pr-3 font-mono text-xs break-all text-slate-400">
                    {c.base_url || "—"}
                  </td>
                  <td className="py-2 pr-3">
                    {c.has_api_key ? (
                      <span className="inline-flex items-center gap-1 text-emerald-400">
                        <CircleCheck size={13} />
                        <span className="text-xs">已配置</span>
                      </span>
                    ) : (
                      <span className="inline-flex items-center gap-1 text-slate-500">
                        <CircleX size={13} />
                        <span className="text-xs">未配置</span>
                      </span>
                    )}
                  </td>
                  <td
                    className="py-2 pr-3 text-xs text-slate-400"
                    title={c.models.join("\n")}
                  >
                    {c.models.length} 个
                  </td>
                </tr>
                {c.unsupported_server_tools.length > 0 && (
                  <tr>
                    <td colSpan={5} className="pb-2">
                      <div className="flex items-start gap-2 rounded border border-amber-900/50 bg-amber-950/30 px-2.5 py-2 text-xs text-amber-200">
                        <TriangleAlert size={14} className="mt-0.5 shrink-0" />
                        <span>
                          该渠道声明了服务端工具{" "}
                          <span className="font-mono text-amber-100">
                            {c.unsupported_server_tools.join("、")}
                          </span>
                          ，但 <span className="font-mono">{c.protocol}</span> 协议没有对应物，
                          <strong className="font-medium">请求时会被忽略</strong> —— 模型不会真的去调用它们。
                        </span>
                      </div>
                    </td>
                  </tr>
                )}
              </tbody>
            ))}
          </table>
        )}
      </AsyncBody>
      <ReadOnlyNote>
        渠道的增删改要编辑配置文件的 [providers.*]，本页只展示现状。API key 只显示「有没有」，
        不显示内容本身。
      </ReadOnlyNote>
    </>
  );
}

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

function ToolsTab() {
  const state = useAsync(listTools, []);
  return (
    <>
      <AsyncBody state={state} empty={<>没有可用的内置工具。</>}>
        {(tools: ToolRow[]) => (
          <div className="space-y-2">
            {tools.map((t) => (
              <div key={t.name} className="rounded-lg border border-slate-800 bg-slate-900/40">
                <div className="px-3 py-2">
                  <div className="font-mono text-sm text-slate-200">{t.name}</div>
                  <div className="mt-0.5 text-xs leading-relaxed text-slate-400">
                    {t.description || "（无描述）"}
                  </div>
                </div>
                <details className="group border-t border-slate-800">
                  <summary className="flex cursor-pointer list-none items-center gap-1.5 px-3 py-1.5 text-xs text-slate-500 hover:text-slate-300">
                    <ChevronRight size={12} className="transition-transform group-open:rotate-90" />
                    参数（JSON Schema）
                  </summary>
                  <pre className="max-h-80 overflow-auto border-t border-slate-800 p-2 font-mono text-[11px] leading-relaxed text-slate-300">
                    {JSON.stringify(t.parameters, null, 2)}
                  </pre>
                </details>
              </div>
            ))}
          </div>
        )}
      </AsyncBody>
      <ReadOnlyNote>
        这些是<strong className="font-medium">本地会执行</strong>的工具；厂商在服务端执行的
        （web_search 等）属于渠道能力，见「渠道」页。启用 / 禁用要改配置，尚未接入。
      </ReadOnlyNote>
    </>
  );
}

// ---------------------------------------------------------------------------
// 技能
// ---------------------------------------------------------------------------

function SkillsTab() {
  const state = useAsync(listSkills, []);
  return (
    <>
      <AsyncBody
        state={state}
        empty={
          <>
            没有发现技能。
            <br />
            技能从 <code className="font-mono text-slate-400">~/.carter/skills/&lt;名称&gt;/SKILL.md</code>{" "}
            发现；若配置里 <code className="font-mono text-slate-400">skills.enabled = false</code>，
            已安装的技能也会被隐藏。
          </>
        }
      >
        {(skills: SkillRow[]) => (
          <div className="space-y-2">
            {skills.map((s) => (
              <div key={s.name} className="rounded-lg border border-slate-800 bg-slate-900/40 px-3 py-2">
                <div className="flex items-center gap-1.5">
                  <Sparkles size={13} className="text-indigo-300" />
                  <span className="font-medium text-slate-200">{s.name}</span>
                </div>
                <div className="mt-0.5 text-xs leading-relaxed text-slate-400">
                  {s.description || "（无描述）"}
                </div>
                {s.allowed_tools.length > 0 && (
                  <div className="mt-1.5 flex flex-wrap gap-1">
                    {s.allowed_tools.map((tool) => (
                      <span
                        key={tool}
                        className="rounded bg-slate-700/70 px-1.5 py-0.5 font-mono text-[10px] text-slate-300"
                      >
                        {tool}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </AsyncBody>
      <ReadOnlyNote>
        技能的启用 / 停用要改配置文件，本页只展示已发现的技能。
      </ReadOnlyNote>
    </>
  );
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

function McpTab() {
  const state = useAsync(listMcpServers, []);
  return (
    <>
      <AsyncBody
        state={state}
        empty={
          <>
            没有配置 MCP server。
            <br />
            在配置文件的 <code className="font-mono text-slate-400">[mcp.servers.*]</code> 下添加。
          </>
        }
      >
        {(servers: McpServerRow[]) => (
          <div className="space-y-2">
            {servers.map((s) => (
              <div
                key={s.name}
                className="flex items-center gap-3 rounded-lg border border-slate-800 bg-slate-900/40 px-3 py-2"
              >
                <Server size={14} className="shrink-0 text-slate-400" />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="font-medium text-slate-200">{s.name}</span>
                    <Badge>{s.transport}</Badge>
                  </div>
                  <div className="mt-0.5 truncate font-mono text-[11px] text-slate-500" title={s.target}>
                    {s.target || "—"}
                  </div>
                </div>
                <span
                  className={`shrink-0 text-xs ${s.enabled ? "text-emerald-400" : "text-slate-500"}`}
                >
                  {s.enabled ? "已启用" : "已停用"}
                </span>
              </div>
            ))}
          </div>
        )}
      </AsyncBody>
      <ReadOnlyNote>
        server 的增删改要编辑配置文件的 [mcp.servers.*]，本页只展示现状。
      </ReadOnlyNote>
    </>
  );
}

// ---------------------------------------------------------------------------
// 会话（有删除，唯一的写操作）
// ---------------------------------------------------------------------------

function SessionsTab() {
  const state = useAsync(() => listSessions(null), []);
  const sessionId = useStore((s) => s.sessionId);
  const setNotice = useStore((s) => s.setNotice);
  const setSessions = useStore((s) => s.setSessions);
  const resetSession = useStore((s) => s.resetSession);

  const [confirming, setConfirming] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  async function doDelete(id: string) {
    setBusy(id);
    try {
      await deleteSession(id);
      // 删掉的正好是当前打开的会话：UI 里也得关掉它，否则界面还停在一条已经不存在的
      // 对话上，继续发消息会重建出一个同名新会话，看起来像「删了没生效」。
      if (id === sessionId) {
        try {
          await closeSession(id);
        } catch {
          /* 会话可能已经自己结束了 */
        }
        resetSession();
        setNotice("当前会话已删除，请点「新建会话」继续。");
      }
      // 侧栏与会话页都刷新。
      setSessions(await listSessions(null));
      state.reload();
    } catch (e) {
      setNotice(String(e));
    } finally {
      setBusy(null);
      setConfirming(null);
    }
  }

  return (
    <>
      <AsyncBody state={state} empty={<>还没有历史会话。</>}>
        {(sessions) => (
          <div className="space-y-1.5">
            {sessions.map((s) => {
              const active = s.id === sessionId;
              return (
                <div
                  key={s.id}
                  className={`flex items-center gap-3 rounded-lg border px-3 py-2 ${
                    active ? "border-sky-800/70 bg-sky-950/20" : "border-slate-800 bg-slate-900/40"
                  }`}
                >
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-sm text-slate-200">
                      {s.title || "未命名会话"}
                      {active && <span className="ml-2 text-[10px] text-sky-400">当前</span>}
                    </div>
                    <div className="mt-0.5 flex items-center gap-1.5 text-[11px] text-slate-500">
                      <span className="truncate">{s.model}</span>
                      <span>·</span>
                      <span className="shrink-0">
                        {new Date(s.created_at).toLocaleString("zh-CN")}
                      </span>
                    </div>
                  </div>

                  {/* 删除不可恢复 —— 必须两步，不能一键删掉。 */}
                  {confirming === s.id ? (
                    <div className="flex shrink-0 items-center gap-1.5">
                      <span className="text-[11px] text-amber-300">不可恢复，确认删除？</span>
                      <Button
                        variant="danger"
                        disabled={busy === s.id}
                        onClick={() => doDelete(s.id)}
                      >
                        {busy === s.id && <LoaderCircle size={13} className="animate-spin" />}
                        确认删除
                      </Button>
                      <Button variant="ghost" onClick={() => setConfirming(null)}>
                        取消
                      </Button>
                    </div>
                  ) : (
                    <Button
                      variant="ghost"
                      className="shrink-0"
                      onClick={() => setConfirming(s.id)}
                    >
                      <Trash size={13} />
                      删除
                    </Button>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </AsyncBody>
      <ReadOnlyNote>
        删除会直接删掉磁盘上的会话文件（<code className="font-mono">~/.carter/projects/…/*.jsonl</code>），
        不可恢复。重命名 / 全文检索等要等后续版本。
      </ReadOnlyNote>
    </>
  );
}

// ---------------------------------------------------------------------------
// 外壳
// ---------------------------------------------------------------------------

const TABS = [
  { id: "channels", label: "渠道", icon: Plug },
  { id: "tools", label: "工具", icon: Wrench },
  { id: "skills", label: "技能", icon: Sparkles },
  { id: "mcp", label: "MCP", icon: Server },
  { id: "sessions", label: "会话", icon: MessagesSquare },
] as const;

type TabId = (typeof TABS)[number]["id"];

export function Settings() {
  const [tab, setTab] = useState<TabId>("channels");

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 border-b border-slate-800 px-4 py-2">
        <SettingsIcon size={16} className="text-slate-400" />
        <span className="text-sm font-semibold text-slate-200">设置</span>
        <span className="ml-auto inline-flex items-center gap-1 text-[11px] text-slate-500">
          <KeyRound size={12} />
          只读视图
        </span>
      </div>

      <div className="flex gap-1 border-b border-slate-800 px-3 py-1.5">
        {TABS.map((t) => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            className={`inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs transition-colors ${
              tab === t.id
                ? "bg-slate-700 text-slate-100"
                : "text-slate-400 hover:bg-slate-800/60 hover:text-slate-200"
            }`}
          >
            <t.icon size={14} />
            {t.label}
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        {tab === "channels" && <ChannelsTab />}
        {tab === "tools" && <ToolsTab />}
        {tab === "skills" && <SkillsTab />}
        {tab === "mcp" && <McpTab />}
        {tab === "sessions" && <SessionsTab />}
      </div>
    </div>
  );
}
