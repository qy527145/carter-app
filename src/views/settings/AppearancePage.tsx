// 外观：主题。

import { setAppSettings, type ThemePref } from "../../lib/api";
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

export function AppearancePage() {
  const settings = useStore((s) => s.settings);
  const setSettings = useStore((s) => s.setSettings);

  if (!settings) return null;

  const pick = (theme: ThemePref) => {
    const next = { ...settings, theme };
    setSettings(next); // 先落界面，切换立刻可见
    setAppSettings(next).catch(toastError);
  };

  return (
    <>
      <h1 className="mb-4 text-base font-semibold">外观</h1>

      <Section title="主题">
        <Card>
          <Segmented value={settings.theme} options={THEMES} onChange={pick} />
          <div className="mt-3 text-[11px] leading-relaxed text-dim">
            「跟随系统」会跟着 Windows 的浅色/深色设置走，切换即时生效。
            代码块固定用深色配色，不随主题变 —— 正文是给人读的，代码是给眼睛扫的。
          </div>
        </Card>
      </Section>

      <Section title="侧栏">
        <Card>
          <div className="text-sm text-ink">宽度</div>
          <div className="mt-1 text-[11px] leading-relaxed text-dim">
            当前 {settings.sidebar_width}px。拖左侧栏与内容之间的竖线可以调，
            双击那条线复位到 260px。
          </div>
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
