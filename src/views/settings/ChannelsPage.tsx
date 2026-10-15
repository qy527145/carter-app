// 渠道页：渠道列表 + 新建/编辑 + 协议对照。

import { useState } from "react";
import { AlertTriangle, CircleCheck, CircleX, Plus, Star } from "lucide-react";

import { listChannels, type ChannelRow } from "../../lib/api";
import { presetFor } from "../../lib/protocols";
import { useAsync } from "../../lib/useAsync";
import { useStore } from "../../store";
import { Button } from "../../components/ui/Button";
import { Badge, Banner, Card } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";
import { ProviderEditor } from "./ProviderEditor";

/** 一个渠道卡片。整块可点，点开编辑。 */
function ChannelCard({ c, onEdit }: { c: ChannelRow; onEdit: () => void }) {
  const preset = presetFor(c.kind);
  return (
    <Card onClick={onEdit} className="space-y-2">
      <div className="flex items-center gap-2">
        <span className="text-sm font-medium text-ink">{c.name}</span>
        {c.is_default && (
          <span className="inline-flex items-center gap-1 text-[10px] text-tone-blue">
            <Star size={10} />
            默认
          </span>
        )}
        <span className="ml-auto flex items-center gap-1.5">
          {/* kind 与 protocol 不同 = 走了别名/兼容映射，两个都要显示，
              否则用户会以为配置写错了。 */}
          {c.kind === c.protocol ? (
            <Badge>{c.protocol}</Badge>
          ) : (
            <span className="flex items-center gap-1">
              <Badge>{c.kind}</Badge>
              <span className="text-[11px] text-dim">→</span>
              <Badge tone="blue">{c.protocol}</Badge>
            </span>
          )}
        </span>
      </div>

      <div className="mono truncate text-[11px] text-dim" title={c.base_url || "(协议默认)"}>
        {c.base_url || preset?.defaultBaseUrl || "(协议默认)"}
      </div>

      <div className="flex flex-wrap items-center gap-3 text-[11px]">
        {c.has_api_key ? (
          <span className="inline-flex items-center gap-1 text-tone-green">
            <CircleCheck size={11} />
            密钥已配置
          </span>
        ) : (
          <span className="inline-flex items-center gap-1 text-dim">
            <CircleX size={11} />
            未配置密钥
          </span>
        )}
        <span className="text-dim" title={c.models.join("\n")}>
          {c.models.length} 个模型
        </span>
        {c.supported_server_tools.length > 0 && (
          <span className="text-dim">
            服务端工具 {c.supported_server_tools.length} 个
          </span>
        )}
      </div>

      {/* 声明了、但协议不认的服务端工具 —— 必须显式报出来。
          静默丢弃会让用户以为联网搜索开着，而模型一个结果都拿不到。 */}
      {c.unsupported_server_tools.length > 0 && (
        <div className="flex items-start gap-1.5 rounded-md border border-[var(--warn-border)] bg-[var(--warn-bg)] px-2 py-1.5 text-[11px] leading-relaxed">
          <AlertTriangle size={12} className="mt-0.5 shrink-0 text-tone-orange" />
          <span>
            声明了 <span className="mono">{c.unsupported_server_tools.join("、")}</span>，但{" "}
            <span className="mono">{c.protocol}</span> 没有对应物，
            <strong className="font-medium">请求时会被丢掉</strong> —— 模型不会真的调用它们。
          </span>
        </div>
      )}
    </Card>
  );
}

/** 三协议对照。解释了「为什么这个渠道的 web_search 不生效」。 */
function ProtocolNotes() {
  return (
    <div className="rounded-xl border border-edge bg-card p-4">
      <div className="mb-2 text-sm font-medium">三种线协议</div>
      <div className="space-y-2 text-[11px] leading-relaxed text-dim">
        <div>
          <span className="mono text-ink">anthropic</span> —— Anthropic Messages。
          服务端工具支持 web_search / web_fetch / code_execution / computer_use。
        </div>
        <div>
          <span className="mono text-ink">openai_responses</span> —— OpenAI Responses。
          服务端工具支持 web_search / web_search_preview / code_interpreter / file_search。
        </div>
        <div>
          <span className="mono text-ink">openai_chat</span> —— OpenAI Chat Completions。
          <strong className="font-medium text-ink">没有任何服务端工具</strong>
          ，渠道里声明的会被明确报出来并忽略。
        </div>
        <div className="pt-1">
          渠道用的是哪一种由 <span className="mono">kind</span> 决定；
          kind 可以是别名（<span className="mono">deepseek</span>、
          <span className="mono">ollama</span>…），carter 会把它映射到上面三种之一，
          并补上默认地址。
        </div>
      </div>
    </div>
  );
}

export function ChannelsPage() {
  const state = useAsync(listChannels, []);
  const [editing, setEditing] = useState<string | null | undefined>(undefined);
  const appInfo = useStore((s) => s.appInfo);

  return (
    <>
      <div className="mb-4 flex items-center gap-2">
        <h1 className="text-base font-semibold">渠道</h1>
        <Button
          variant="primary"
          size="sm"
          className="ml-auto"
          onClick={() => setEditing(null)}
        >
          <Plus size={13} />
          新建渠道
        </Button>
      </div>

      <PageIntro>
        渠道写在 <span className="mono">{appInfo?.config_path ?? "~/.carter/config.toml"}</span> 的{" "}
        <span className="mono">[providers.*]</span> 下。这里改完
        <strong className="font-medium text-ink">会保留文件里的注释与 </strong>
        <span className="mono">{"${环境变量}"}</span>
        <strong className="font-medium text-ink"> 引用</strong>，只动你改过的那几个键。
      </PageIntro>

      <div className="space-y-2">
        <AsyncBody
          state={state}
          empty={
            <>
              还没有配置任何渠道。
              <br />
              点右上角「新建渠道」，或者在配置文件里加一段{" "}
              <span className="mono">[providers.名字]</span>。
            </>
          }
        >
          {(channels: ChannelRow[]) => (
            <>
              {channels.map((c) => (
                <ChannelCard key={c.name} c={c} onEdit={() => setEditing(c.name)} />
              ))}
            </>
          )}
        </AsyncBody>
      </div>

      {!state.loading && !state.error && (
        <div className="mt-6">
          <ProtocolNotes />
        </div>
      )}

      {state.loading === false && state.data?.length === 0 && (
        <div className="mt-4">
          <Banner kind="info">
            没有渠道时无法新建会话 —— 会话要挑一个模型，而模型挂在渠道下。
          </Banner>
        </div>
      )}

      {editing !== undefined && (
        <ProviderEditor
          editing={editing}
          onClose={(changed) => {
            setEditing(undefined);
            if (changed) state.reload();
          }}
        />
      )}
    </>
  );
}
