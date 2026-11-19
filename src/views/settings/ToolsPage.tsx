// 工具页：内置工具的开关。
//
// ## 这个开关只影响本应用
//
// carter 0.1.4 的 `ToolRegistry` 是「全部常开」，`Config` 里也没有开关字段，
// 所以名单存在 `~/.carter-app/settings.json`（应用私有数据，见方案 D8）。
// carter CLI 读不到它、也不会受它影响 —— 页面上必须把这句话说出来，
// 否则用户会以为在命令行里也关掉了。

import { ChevronRight } from "lucide-react";
import { useEffect, useState } from "react";
import { Input, Segmented, Switch } from "../../components/ui/Input";
import { Banner, Card, Collapse } from "../../components/ui/Panel";
import {
  type AppSettings,
  type BashTimeoutSettings,
  getBashTimeout,
  listTools,
  type OnTimeout,
  setAppSettings,
  setBashTimeout,
  type ToolRow,
} from "../../lib/api";
import { toastError } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { useStore } from "../../store";
import { AsyncBody, PageIntro } from "./AsyncBody";

function ToolItem({ t, onToggle }: { t: ToolRow; onToggle: (v: boolean) => void }) {
  const [open, setOpen] = useState(false);
  return (
    <Card className="space-y-2">
      <div className="flex items-center gap-2">
        <span className="mono text-sm text-ink">{t.name}</span>
        {t.note && <span className="text-[10px] text-dim">{t.note}</span>}
        <span className="ml-auto">
          {t.toggleable ? (
            <Switch checked={t.enabled} onChange={onToggle} label={t.name} />
          ) : (
            <span className="text-[10px] text-dim">始终启用</span>
          )}
        </span>
      </div>
      <div className="text-xs leading-relaxed text-dim">{t.description || "（无描述）"}</div>

      {t.parameters != null && (
        <div className="border-t border-edge pt-2">
          <button
            onClick={() => setOpen((o) => !o)}
            className="flex w-full items-center gap-1.5 text-left text-[11px] text-dim transition-colors hover:text-ink"
          >
            <ChevronRight
              size={11}
              className={`shrink-0 transition-transform ${open ? "rotate-90" : ""}`}
            />
            参数（JSON Schema）
          </button>
          <Collapse open={open}>
            <pre className="mono mt-2 max-h-72 overflow-auto rounded-md bg-base p-2 text-[11px] leading-relaxed text-ink">
              {JSON.stringify(t.parameters, null, 2)}
            </pre>
          </Collapse>
        </div>
      )}
    </Card>
  );
}

/**
 * bash 前台命令的超时：多久、超时之后转后台还是结束。
 *
 * 存在 `config.toml`（carter CLI 也认），改完下一条命令生效。两种处理给模型的话不一样
 * —— 它要知道命令是还在后台跑，还是已经没了。
 */
function BashTimeoutRow() {
  const state = useAsync(getBashTimeout, []);
  const cur = state.data;
  // 秒数在输入框里先改草稿，失焦 / 回车才存：边打边存会在「1」「12」「120」上各写一次盘。
  const [draft, setDraft] = useState("");
  useEffect(() => {
    if (cur) setDraft(String(cur.secs));
  }, [cur]);

  function save(next: BashTimeoutSettings) {
    setBashTimeout(next.secs, next.on_timeout)
      .then(() => state.reload())
      .catch((e) => {
        toastError(e);
        if (cur) setDraft(String(cur.secs));
      });
  }

  function commitSecs() {
    if (!cur) return;
    const n = Math.round(Number(draft));
    if (!Number.isFinite(n) || n === cur.secs) {
      setDraft(String(cur.secs));
      return;
    }
    save({ ...cur, secs: n });
  }

  if (!cur) return null;
  return (
    <div className="mb-4 rounded-xl border border-edge bg-card px-4 py-3">
      <div className="flex flex-wrap items-center gap-3">
        <span className="text-sm text-ink">命令超时</span>
        <span className="flex items-center gap-1.5">
          <Input
            type="number"
            min={5}
            max={3600}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onBlur={commitSecs}
            onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
            className="w-20 text-xs"
          />
          <span className="text-xs text-dim">秒</span>
        </span>
        <span className="text-xs text-dim">超时后</span>
        <Segmented<OnTimeout>
          value={cur.on_timeout}
          options={[
            { id: "background", label: "转后台" },
            { id: "kill", label: "终止" },
          ]}
          onChange={(on_timeout) => save({ ...cur, on_timeout })}
        />
      </div>
      <div className="mt-1.5 text-[11px] leading-relaxed text-dim">
        {cur.on_timeout === "background"
          ? "前台命令跑满时长仍未结束，就转到后台继续跑，跑完自动唤醒对话；模型会被告知这是超时转的，不是你操作的。"
          : "前台命令跑满时长仍未结束就结束整棵进程树；模型会被告知这是超时，不是你中断的，并拿到已有的输出。"}
        模型单次调用可以自己指定更长的超时。存在 <span className="mono">config.toml</span>，carter
        命令行也按这个来。
      </div>
    </div>
  );
}

export function ToolsPage() {
  // 会话 cwd 只影响 `skill` 那一行的说明：它要指出项目级目录在不在扫描范围里。
  const cwd = useStore((s) => s.cwd);
  const state = useAsync(() => listTools(cwd || null), [cwd]);
  const settings = useStore((s) => s.settings);
  const setSettings = useStore((s) => s.setSettings);
  const [saving, setSaving] = useState(false);

  async function toggle(name: string, enabled: boolean) {
    if (!settings) return;
    const disabled = enabled
      ? settings.disabled_tools.filter((d) => d !== name)
      : [...new Set([...settings.disabled_tools, name])];
    const next: AppSettings = { ...settings, disabled_tools: disabled };
    setSettings(next); // 先落界面，开关立刻响应
    setSaving(true);
    try {
      await setAppSettings(next);
    } catch (e) {
      // 存不下就把界面回滚 —— 否则用户以为关掉了，重启又回来了。
      setSettings(settings);
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  const off = settings?.disabled_tools ?? [];

  return (
    <>
      <h1 className="mb-4 text-[16px] font-semibold">工具</h1>

      <PageIntro>
        这里列的是
        <strong className="font-medium text-ink">本地会执行</strong>的工具。
        厂商在服务端执行的（web_search 等）属于渠道能力，见「渠道」页；MCP 提供的工具 要连上 server
        才知道名字，见「MCP」页。
      </PageIntro>

      <div className="mb-4">
        <Banner kind="warn">
          工具开关存在 <span className="mono">~/.carter-app/settings.json</span>，
          <strong className="font-medium">只对本应用生效</strong> —— carter
          命令行读不到它，在那边这些工具依然是开着的。
        </Banner>
      </div>

      <BashTimeoutRow />

      {off.length > 0 && (
        <div className="mb-4 text-xs text-dim">
          已关闭 {off.length} 个：<span className="mono">{off.join("、")}</span>
          {saving && <span className="ml-2">保存中…</span>}
        </div>
      )}

      <div className="space-y-2">
        <AsyncBody state={state} empty={<>没有可用的内置工具。</>}>
          {(tools: ToolRow[]) => (
            <>
              {tools.map((t) => (
                <ToolItem key={t.name} t={t} onToggle={(v) => void toggle(t.name, v)} />
              ))}
            </>
          )}
        </AsyncBody>
      </div>
    </>
  );
}
