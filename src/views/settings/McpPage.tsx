// MCP 页：server 的增删改。
//
// carter 的 `McpServerConfig` **没有** `Default`，`transport` 是必填 —— 表单得挡住空值。
// stdio 与 http 两种 transport 的字段是互斥的：后端只写当前 transport 用得上的键，
// 另一个的键会被删掉（留着一个用不上的 url 会让人以为它生效）。

import { useState } from "react";
import { Pencil, Plus, Server, Trash, X } from "lucide-react";

import {
  deleteMcpServer,
  listMcpServers,
  saveMcpServer,
  type McpDraft,
  type McpServerRow,
} from "../../lib/api";
import { toastError, toastOk } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { Button, IconButton } from "../../components/ui/Button";
import { Field, Input, Segmented } from "../../components/ui/Input";
import { Badge, Banner, Card, ConfirmDialog, Modal } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

type Pair = [string, string];

/** 键值对编辑器（env / headers 共用）。 */
function PairEditor({
  pairs,
  onChange,
  keyPlaceholder,
}: {
  pairs: Pair[];
  onChange: (p: Pair[]) => void;
  keyPlaceholder: string;
}) {
  return (
    <div className="space-y-1.5">
      {pairs.map((p, i) => (
        <div key={i} className="flex gap-2">
          <Input
            mono
            value={p[0]}
            onChange={(e) => onChange(pairs.map((x, j) => (j === i ? [e.target.value, x[1]] : x)))}
            placeholder={keyPlaceholder}
            className="flex-1 py-1.5 text-xs"
          />
          <Input
            mono
            value={p[1]}
            onChange={(e) => onChange(pairs.map((x, j) => (j === i ? [x[0], e.target.value] : x)))}
            placeholder="值"
            className="flex-1 py-1.5 text-xs"
          />
          <IconButton
            title="移除这一行"
            onClick={() => onChange(pairs.filter((_, j) => j !== i))}
          >
            <X size={12} />
          </IconButton>
        </div>
      ))}
      <Button size="sm" onClick={() => onChange([...pairs, ["", ""]])}>
        <Plus size={12} />
        添加一项
      </Button>
    </div>
  );
}

function McpEditor({
  editing,
  onClose,
}: {
  editing: McpServerRow | null;
  onClose: (changed: boolean) => void;
}) {
  const [name, setName] = useState(editing?.name ?? "");
  const [enabled, setEnabled] = useState(editing?.enabled ?? true);
  const [transport, setTransport] = useState(editing?.transport ?? "stdio");
  const [command, setCommand] = useState(editing?.command ?? "");
  const [args, setArgs] = useState((editing?.args ?? []).join("\n"));
  const [url, setUrl] = useState(editing?.url ?? "");
  const [env, setEnv] = useState<Pair[]>(editing?.env ?? []);
  const [headers, setHeaders] = useState<Pair[]>(editing?.headers ?? []);
  const [saving, setSaving] = useState(false);

  const isStdio = transport === "stdio";
  const valid =
    name.trim() !== "" && (isStdio ? command.trim() !== "" : url.trim() !== "");

  async function submit() {
    setSaving(true);
    try {
      const draft: McpDraft = {
        name: name.trim(),
        enabled,
        transport,
        command: isStdio ? command.trim() : null,
        // 一行一个参数。空行去掉 —— 它们多半是编辑时的残留。
        args: isStdio ? args.split("\n").map((a) => a.trim()).filter(Boolean) : [],
        env: isStdio ? env.filter(([k]) => k.trim()) : [],
        url: isStdio ? null : url.trim(),
        headers: isStdio ? [] : headers.filter(([k]) => k.trim()),
      };
      await saveMcpServer(draft);
      onClose(true);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal
      title={editing ? `编辑 MCP server · ${editing.name}` : "添加 MCP server"}
      onClose={() => onClose(false)}
      width={560}
      footer={
        <>
          <Button onClick={() => onClose(false)} disabled={saving}>
            取消
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || saving}>
            {saving ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      <Field label="名称" hint="只能用字母、数字、-、_">
        <Input
          mono
          value={name}
          onChange={(e) => setName(e.target.value)}
          disabled={editing !== null}
          placeholder="例如 fs、github"
        />
      </Field>

      <Field label="传输方式">
        <Segmented
          value={transport}
          onChange={setTransport}
          options={[
            { id: "stdio", label: "stdio（本地子进程）" },
            { id: "http", label: "http（远程）" },
          ]}
        />
      </Field>

      {isStdio ? (
        <>
          <Field label="command" hint="必填">
            <Input
              mono
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="npx"
            />
          </Field>
          <Field label="args" hint="一行一个">
            <textarea
              value={args}
              onChange={(e) => setArgs(e.target.value)}
              rows={3}
              placeholder={"-y\n@modelcontextprotocol/server-filesystem"}
              className="mono w-full resize-y rounded-md border border-edge bg-base px-3 py-2 text-sm text-ink outline-none transition-colors placeholder:text-dim focus:border-dim"
            />
          </Field>
          <Field label="环境变量 env" hint="键值对">
            <PairEditor pairs={env} onChange={setEnv} keyPlaceholder="变量名" />
          </Field>
        </>
      ) : (
        <>
          <Field label="url" hint="必填">
            <Input
              mono
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://mcp.example.com/sse"
            />
          </Field>
          <Field label="请求头 headers" hint="键值对">
            <PairEditor pairs={headers} onChange={setHeaders} keyPlaceholder="头名" />
          </Field>
        </>
      )}

      <label className="mb-2 flex items-center gap-2 text-xs text-dim">
        <input
          type="checkbox"
          checked={enabled}
          onChange={(e) => setEnabled(e.target.checked)}
          className="h-3.5 w-3.5 accent-[var(--accent)]"
        />
        启用这个 server
      </label>

      <Banner kind="info">
        MCP server 是在<strong className="font-medium">会话建立时</strong>连的。
        改完要新开一个会话才会生效 —— 正在跑的对话不会中途重连。
      </Banner>
    </Modal>
  );
}

export function McpPage() {
  const state = useAsync(listMcpServers, []);
  const [editing, setEditing] = useState<McpServerRow | null | undefined>(undefined);
  const [confirming, setConfirming] = useState<string | null>(null);

  return (
    <>
      <div className="mb-4 flex items-center gap-2">
        <h1 className="text-base font-semibold">MCP</h1>
        <Button
          variant="primary"
          size="sm"
          className="ml-auto"
          onClick={() => setEditing(null)}
        >
          <Plus size={13} />
          添加 server
        </Button>
      </div>

      <PageIntro>
        写在配置的 <span className="mono">[mcp.servers.*]</span> 下。server 提供的工具会在
        会话建立时并入工具池，与内置工具共用一个命名空间。
      </PageIntro>

      <div className="space-y-2">
        <AsyncBody
          state={state}
          empty={
            <>
              没有配置 MCP server。
              <br />
              点右上角「添加 server」。stdio 跑本地子进程，http 连远程端点。
            </>
          }
        >
          {(servers: McpServerRow[]) => (
            <>
              {servers.map((s) => (
                <Card key={s.name} className="space-y-1.5">
                  <div className="flex items-center gap-2">
                    <Server size={13} className="shrink-0 text-tone-blue" />
                    <span className="text-sm font-medium text-ink">{s.name}</span>
                    <Badge>{s.transport}</Badge>
                    {!s.enabled && <Badge tone="orange">已停用</Badge>}
                    <span className="ml-auto flex shrink-0 items-center gap-1">
                      <IconButton title="编辑" onClick={() => setEditing(s)}>
                        <Pencil size={12} />
                      </IconButton>
                      <IconButton title="删除" onClick={() => setConfirming(s.name)}>
                        <Trash size={12} />
                      </IconButton>
                    </span>
                  </div>
                  <div className="mono truncate text-[11px] text-dim" title={s.target}>
                    {s.target || "—"}
                    {s.args.length > 0 && <span className="ml-2">{s.args.join(" ")}</span>}
                  </div>
                  {(s.env.length > 0 || s.headers.length > 0) && (
                    <div className="flex flex-wrap gap-1 text-[10px] text-dim">
                      {[...s.env, ...s.headers].map(([k]) => (
                        <Badge key={k}>{k}</Badge>
                      ))}
                    </div>
                  )}
                </Card>
              ))}
            </>
          )}
        </AsyncBody>
      </div>

      {editing !== undefined && (
        <McpEditor
          editing={editing}
          onClose={(changed) => {
            setEditing(undefined);
            if (changed) state.reload();
          }}
        />
      )}

      {confirming && (
        <ConfirmDialog
          title="删除 MCP server"
          danger
          confirmLabel="确认删除"
          message={
            <>
              将从配置里移除 <span className="mono text-ink">[mcp.servers.{confirming}]</span>。
            </>
          }
          onCancel={() => setConfirming(null)}
          onConfirm={() => {
            const name = confirming;
            setConfirming(null);
            deleteMcpServer(name)
              .then(() => {
                toastOk(`已删除 ${name}`);
                state.reload();
              })
              .catch(toastError);
          }}
        />
      )}
    </>
  );
}
