// 外观：主题与对话密度。

import { useState } from "react";

import { setAppSettings, type ThemePref } from "../../lib/api";
import { ModePill } from "../../components/ModePill";
import { toastError } from "../../lib/toast";
import { useStore } from "../../store";
import { Segmented } from "../../components/ui/Input";
import { Card, Section } from "../../components/ui/Panel";
import { PageIntro } from "./AsyncBody";

const THEMES: { id: ThemePref; label: string }[] = [
  { id: "system", label: "跟随系统" },
  { id: "light", label: "浅色" },
  { id: "dark", label: "深色" },
];

const IS_MAC = navigator.platform.toLowerCase().includes("mac");
const MOD = IS_MAC ? "⌘" : "Ctrl";

const SHORTCUTS: [string[], string][] = [
  [[MOD, "N"], "新对话"],
  [[MOD, "K"], "搜索会话"],
  [[MOD, "B"], "收起 / 展开侧栏"],
  [[MOD, ","], "打开设置"],
  [["Esc"], "从设置回到对话"],
  [["↑", "↓"], "在会话列表里移动"],
  [["F2"], "重命名选中的会话（也可双击标题）"],
  [["⇧", "F10"], "打开选中会话的菜单"],
];

export function AppearancePage() {
  const settings = useStore((s) => s.settings);
  const setSettings = useStore((s) => s.setSettings);
  const [width, setWidth] = useState(settings?.sidebar_width ?? 260);

  if (!settings) return null;

  const pick = (theme: ThemePref) => {
    const next = { ...settings, theme };
    setSettings(next); // 先落界面，切换立刻可见
    setAppSettings(next).catch(toastError);
  };

  return (
    <>
      <h1 className="mb-4 text-[16px] font-semibold">外观</h1>

      <Section title="主题">
        <Card>
          <Segmented value={settings.theme} options={THEMES} onChange={pick} />
          <div className="mt-3 text-[11px] leading-relaxed text-dim">
            「跟随系统」会跟着系统的浅色/深色设置走，切换即时生效。
            代码块固定用深色配色，不随主题变 —— 正文是给人读的，代码是给眼睛扫的。
          </div>
        </Card>
      </Section>

      <Section title="对话密度">
        <Card>
          <ModePill />
          <div className="mt-3 text-[11px] leading-relaxed text-dim">
            「精简」把思考与每一次工具调用折成一行摘要，只留正文 —— 读对话时不必
            一路划过十几张命令卡片。折起来的过程点一下就能展开，失败的调用会自动展开。
            <br />
            两种模式<span className="text-ink">只差界面</span>：工具照常执行、结果照常
            回灌给模型，发给上游的请求完全一样。切换不影响会话内容，对话顶栏也有同一个
            开关，随时可以来回切。
          </div>
        </Card>
      </Section>

      <Section title="侧栏">
        <Card>
          <div className="flex items-center gap-3">
            <div className="text-sm text-ink">宽度</div>
            <input
              type="range"
              min={200}
              max={420}
              step={10}
              value={width}
              aria-label="侧栏宽度"
              // 拖动时只改界面（侧栏实时跟着变宽），松手再落盘。
              onChange={(e) => {
                const w = Number(e.target.value);
                setWidth(w);
                setSettings({ ...settings, sidebar_width: w });
              }}
              onPointerUp={() => setAppSettings({ ...settings, sidebar_width: width }).catch(toastError)}
              onKeyUp={() => setAppSettings({ ...settings, sidebar_width: width }).catch(toastError)}
              className="h-1 flex-1 cursor-pointer accent-[var(--accent)]"
            />
            <span className="w-12 text-right text-xs tabular-nums text-dim">{width}px</span>
          </div>
          <div className="mt-2 text-[11px] leading-relaxed text-dim">
            也可以直接拖侧栏右边那条线，双击复位到 260px；往左拖到底会把侧栏收起。
          </div>
        </Card>
      </Section>

      <Section title="快捷键">
        <Card className="py-1">
          {SHORTCUTS.map(([keys, label], i) => (
            <div
              key={label}
              className={`flex items-center py-2 text-sm ${i > 0 ? "border-t border-edge" : ""}`}
            >
              <span className="flex-1 text-ink">{label}</span>
              <span className="flex gap-1">
                {keys.map((k) => (
                  <kbd
                    key={k}
                    className="min-w-5 rounded border border-edge bg-base px-1.5 py-px text-center font-sans text-[11px] text-dim"
                  >
                    {k}
                  </kbd>
                ))}
              </span>
            </div>
          ))}
        </Card>
      </Section>

      <PageIntro>
        这些设置存在 <span className="mono">~/.carter-app/settings.json</span>，
        与 carter 命令行共用的 <span className="mono">~/.carter/</span> 分开
        —— 那边不认这些选项。
      </PageIntro>
    </>
  );
}
