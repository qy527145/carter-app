// 工具页：内置工具的开关。
//
// ## 这个开关只影响本应用
//
// carter 0.1.4 的 `ToolRegistry` 是「全部常开」，`Config` 里也没有开关字段，
// 所以名单存在 `~/.carter-app/settings.json`（应用私有数据，见方案 D8）。
// carter CLI 读不到它、也不会受它影响 —— 页面上必须把这句话说出来，
// 否则用户会以为在命令行里也关掉了。

import { useState } from "react";
import { ChevronRight } from "lucide-react";

import { listTools, setAppSettings, type AppSettings, type ToolRow } from "../../lib/api";
import { toastError } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { useStore } from "../../store";
import { Switch } from "../../components/ui/Input";
import { Banner, Card, Collapse } from "../../components/ui/Panel";
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
      <h1 className="mb-4 text-base font-semibold">工具</h1>

      <PageIntro>
        这里列的是
        <strong className="font-medium text-ink">本地会执行</strong>的工具。
        厂商在服务端执行的（web_search 等）属于渠道能力，见「渠道」页；MCP 提供的工具
        要连上 server 才知道名字，见「MCP」页。
      </PageIntro>

      <div className="mb-4">
        <Banner kind="warn">
          工具开关存在 <span className="mono">~/.carter-app/settings.json</span>，
          <strong className="font-medium">只对本应用生效</strong> ——
          carter 命令行读不到它，在那边这些工具依然是开着的。
        </Banner>
      </div>

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
