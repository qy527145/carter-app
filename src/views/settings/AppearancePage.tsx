// 外观：主题与对话密度。

import { useState } from "react";
import { ModePill } from "../../components/ModePill";
import { Input, Segmented, Switch } from "../../components/ui/Input";
import { Card, Section } from "../../components/ui/Panel";
import { setAppSettings, type ThemePref } from "../../lib/api";
import { toastError } from "../../lib/toast";
import { useStore } from "../../store";
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
            「跟随系统」会跟着系统的浅色/深色设置走，切换即时生效。 代码块固定用深色配色，不随主题变
            —— 正文是给人读的，代码是给眼睛扫的。
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
              onPointerUp={() =>
                setAppSettings({ ...settings, sidebar_width: width }).catch(toastError)
              }
              onKeyUp={() =>
                setAppSettings({ ...settings, sidebar_width: width }).catch(toastError)
              }
              className="h-1 flex-1 cursor-pointer accent-[var(--accent)]"
            />
            <span className="w-12 text-right text-xs tabular-nums text-dim">{width}px</span>
          </div>
          <div className="mt-2 text-[11px] leading-relaxed text-dim">
            也可以直接拖侧栏右边那条线，双击复位到 260px；往左拖到底会把侧栏收起。
          </div>
        </Card>
      </Section>

      <Section title="网络">
        <Card>
          <div className="flex items-start gap-3">
            <div className="flex-1">
              <div className="text-sm text-ink">GitHub 加速站</div>
              <div className="mt-0.5 text-[11px] leading-relaxed text-dim">
                连不上 GitHub 的网络里，更新检查、连接器安装、团队同步会被重定向到国内的
                加速站（gh-proxy.com、ghproxy.net、ghfast.top）。多个源同时抢，谁先回响应 就用谁 ——
                官方源慢的时候不会再干等它超时；能直连的机器优先走官方。
              </div>
            </div>
            <Switch
              label="GitHub 加速站"
              checked={settings.github_accel}
              onChange={(v) => {
                const next = { ...settings, github_accel: v };
                setSettings(next);
                setAppSettings(next).catch(toastError);
              }}
            />
          </div>
          <label className="mt-3 block border-t border-edge pt-3">
            <span className="text-sm text-ink">代理</span>
            <span className="ml-2 text-[11px] text-dim">留空 = 直连</span>
            <Input
              mono
              defaultValue={settings.http_proxy}
              placeholder="http://127.0.0.1:7897"
              className="mt-1.5 text-xs"
              onBlur={(e) => {
                const proxy = e.target.value.trim();
                if (proxy === settings.http_proxy) return;
                const next = { ...settings, http_proxy: proxy };
                setSettings(next);
                setAppSettings(next).catch(toastError);
              }}
            />
            <span className="mt-1 block text-[11px] leading-relaxed text-dim">
              填了之后下载（更新、连接器、团队同步）都走它，支持{" "}
              <span className="mono">http://</span> 与 <span className="mono">socks5://</span>
              。空着就直连 —— 不读系统代理设置：加速站 在国内，绕去境外再回来只会更慢。
            </span>
          </label>
          <div className="mt-3 flex items-start gap-3 border-t border-edge pt-3">
            <div className="flex-1">
              <div className="text-sm text-ink">启动时检查更新</div>
              <div className="mt-0.5 text-[11px] leading-relaxed text-dim">
                启动几秒后去 GitHub Release 查一次新版本。有的话对话页顶部会出现一条
                「立即更新」，点一下就会下载、静默装好并自动重启。断网时静默失败，
                不会弹报错。关掉之后只能从「关于」页手动检查。
              </div>
            </div>
            <Switch
              label="启动时检查更新"
              checked={settings.auto_check_update}
              onChange={(v) => {
                const next = { ...settings, auto_check_update: v };
                setSettings(next);
                setAppSettings(next).catch(toastError);
              }}
            />
          </div>
          <label className="mt-3 block border-t border-edge pt-3">
            <span className="text-sm text-ink">额外的模型目录源</span>
            <span className="ml-2 text-[11px] text-dim">一行一个 URL，可选</span>
            <textarea
              defaultValue={settings.model_sources}
              rows={2}
              spellCheck={false}
              placeholder="https://我的镜像/models.json"
              className="mono mt-1.5 w-full resize-y rounded-md border border-edge bg-base px-2 py-1.5 text-[11px] text-ink outline-none focus:border-edge-strong"
              onBlur={(e) => {
                const model_sources = e.target.value.trim();
                if (model_sources === settings.model_sources) return;
                const next = { ...settings, model_sources };
                setSettings(next);
                setAppSettings(next).catch(toastError);
              }}
            />
            <span className="mt-1 block text-[11px] leading-relaxed text-dim">
              「模型」页刷新元数据时，models.dev 官方接口拿不到才试这里的。
              全部失败时用打包在应用里的快照，离线也能配模型。
            </span>
          </label>
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
        这些设置存在 <span className="mono">~/.carter-app/settings.json</span>， 与 carter
        命令行共用的 <span className="mono">~/.carter/</span> 分开 —— 那边不认这些选项。
      </PageIntro>
    </>
  );
}
