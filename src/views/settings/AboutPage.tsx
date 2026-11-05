// 关于：版本、路径、以及几个「为什么」。
//
// 设置项里有几件事只能在文件里做（比如 hooks、env、system.md）。与其假装它们
// 不存在，不如把路径列在这里 —— 用户至少知道该去翻哪个文件。

import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink, FolderOpen } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card, Section } from "../../components/ui/Panel";
import { checkUpdate, installUpdate, networkRoute, type UpdateStatus } from "../../lib/api";
import { toastError } from "../../lib/toast";
import { useStore } from "../../store";

/** 时间戳 → 「今天 10:32」这种能一眼看出新旧的样子。 */
function fmtWhen(ms: number | null | undefined): string {
  if (!ms) return "还没检查过";
  const d = new Date(ms);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return sameDay ? `今天 ${time}` : `${d.toLocaleDateString()} ${time}`;
}

function fmtMB(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 更新状态一句话（关于页用；侧栏另有一个更短的版本）。 */
function updateLine(s: UpdateStatus | null): string {
  if (!s) return "…";
  switch (s.phase) {
    case "idle":
      return s.error
        ? `已是最新（${fmtWhen(s.checked_at)}）· 备注：${s.error}`
        : `已是最新（${fmtWhen(s.checked_at)}）`;
    case "checking":
      return "正在检查…";
    case "downloading":
      return `正在下载 v${s.version}${
        s.total
          ? ` · ${Math.min(100, Math.round((s.downloaded / s.total) * 100))}%`
          : ` · ${fmtMB(s.downloaded)}`
      }`;
    case "ready":
      return `v${s.version} 已下载并验签通过（${fmtMB(s.downloaded)}），重启即生效`;
    case "installing":
      return `正在安装 v${s.version}，马上重启…`;
    case "failed":
      return `这一次没成功：${s.error}`;
  }
}

function UpdateSection() {
  const status = useStore((s) => s.updateStatus);
  const version = useStore((s) => s.appInfo?.version);
  const [busy, setBusy] = useState(false);
  const [route, setRoute] = useState<string>("");

  useEffect(() => {
    // 探测一下「现在走的是哪条线」，让人在排查「下不动」时有据可查。
    networkRoute().then(setRoute, () => setRoute(""));
  }, []);

  const check = async () => {
    setBusy(true);
    try {
      await checkUpdate();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const install = async () => {
    setBusy(true);
    try {
      await installUpdate();
    } catch (e) {
      toastError(e);
      setBusy(false);
    }
  };

  const ready = status?.phase === "ready";
  return (
    <Section title="更新">
      <Card>
        <div className="flex items-center gap-3 py-1.5">
          <span className="w-24 shrink-0 text-xs text-dim">当前版本</span>
          <span className="mono text-xs text-ink">{version ?? "…"}</span>
        </div>
        <div className="flex items-baseline gap-3 py-1.5">
          <span className="w-24 shrink-0 text-xs text-dim">状态</span>
          <span className="min-w-0 flex-1 break-all text-[11px] text-ink">
            {updateLine(status)}
          </span>
        </div>
        <div className="flex items-center gap-3 py-1.5">
          <span className="w-24 shrink-0 text-xs text-dim">网络线路</span>
          <span className="text-[11px] text-ink">{route || "…"}</span>
        </div>
        <div className="mt-2 flex items-center gap-2 border-t border-edge pt-2.5">
          <Button size="sm" onClick={check} disabled={busy || status?.phase === "downloading"}>
            检查更新
          </Button>
          {ready && (
            <Button size="sm" variant="primary" onClick={install} disabled={busy}>
              重启更新
            </Button>
          )}
          <span className="text-[11px] text-dim">
            新版本在后台下载，验签通过后才出现在这里。装的是自签名的包，加速站也替换不了内容。
          </span>
        </div>
      </Card>
    </Section>
  );
}

function PathRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline gap-3 py-1.5">
      <span className="w-24 shrink-0 text-xs text-dim">{label}</span>
      <span className="mono min-w-0 flex-1 break-all text-[11px] text-ink">{value}</span>
    </div>
  );
}

export function AboutPage() {
  const appInfo = useStore((s) => s.appInfo);

  const home = appInfo?.default_cwd ?? "";
  const carterHome = home ? `${home}\\.carter` : "~/.carter";
  const appHome = home ? `${home}\\.carter-app` : "~/.carter-app";

  return (
    <>
      <h1 className="mb-1 text-[16px] font-semibold">关于</h1>
      <div className="mb-5 text-xs text-dim">
        {appInfo ? `${appInfo.name} v${appInfo.version}` : "…"}
      </div>

      <UpdateSection />

      <Section title="路径">
        <Card>
          <PathRow label="配置文件" value={appInfo?.config_path ?? "…"} />
          <PathRow label="共享数据" value={carterHome} />
          <PathRow label="应用私有" value={appHome} />
          <PathRow label="默认工作目录" value={appInfo?.default_cwd ?? "…"} />
          <div className="mt-2 border-t border-edge pt-2 text-[11px] leading-relaxed text-dim">
            共享数据（config / sessions / skills / models.json）与 carter 命令行是同一份。
            应用私有目录只放本应用独有的东西：原始报文抓包、界面设置。
          </div>
        </Card>
      </Section>

      <Section title="当前状态">
        <Card>
          <div className="space-y-1.5 text-xs">
            <div className="flex items-center gap-2">
              <span className="text-dim">配置文件</span>
              <span className={appInfo?.config_exists ? "text-ink" : "text-tone-orange"}>
                {appInfo?.config_exists ? "已存在" : "还没建出来 —— 去「渠道」页加一个渠道"}
              </span>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-dim">models.dev 缓存</span>
              <span className={appInfo?.models_cache_exists ? "text-ink" : "text-tone-orange"}>
                {appInfo?.models_cache_exists ? "已存在" : "还没有 —— 去「模型」页点「刷新元数据」"}
              </span>
            </div>
            <div className="flex items-baseline gap-2">
              <span className="text-dim">目录来源</span>
              {/* `source === "builtin"`：上次刷新没联网成功，用的是包里带的快照。 */}
              <span className="min-w-0 flex-1 break-all text-[11px] text-ink">
                {!appInfo?.models_meta
                  ? `内置快照（${appInfo?.models_snapshot_date ?? "…"}）`
                  : appInfo.models_meta.source === "builtin"
                    ? `内置快照（${appInfo.models_snapshot_date}）· ${fmtWhen(appInfo.models_meta.fetched_at)} 尝试联网刷新未成功`
                    : `${appInfo.models_meta.source} · ${fmtWhen(appInfo.models_meta.fetched_at)} · ${appInfo.models_meta.count} 条`}
              </span>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-dim">默认模型</span>
              <span className="mono text-ink">{appInfo?.default_model || "（未设置）"}</span>
            </div>
          </div>
        </Card>
      </Section>

      <Section title="这一版还没做的事">
        <Card>
          <ul className="space-y-1.5 text-[11px] leading-relaxed text-dim">
            <li>
              <span className="text-ink">hooks / env / system.md</span> —— 只在配置文件里能改。
            </li>
            <li>
              <span className="text-ink">工具审批与沙箱</span> —— 首版不做，
              但拦截点已经在底座预留。
            </li>
            <li>
              <span className="text-ink">API Key 进系统凭据库</span> —— 目前和命令行一样存在
              config.toml 里（或写成 <span className="mono">{"${环境变量}"}</span> 由环境提供）。
            </li>
            <li>
              <span className="text-ink">单个 MCP 工具的开关</span> —— MCP 工具名要连上 server
              才知道，所以只能在 server 一级启停。
            </li>
          </ul>
        </Card>
      </Section>

      <Section>
        <button
          onClick={() =>
            openUrl("https://github.com/qy527145/carter").catch(() => {
              /* 打不开浏览器不是错误 */
            })
          }
          className="inline-flex items-center gap-1.5 rounded-md bg-ink/5 px-3 py-1.5 text-sm text-dim transition-colors hover:bg-ink/10 hover:text-ink"
        >
          <ExternalLink size={13} />
          carter 仓库
        </button>
        <span className="ml-2 inline-flex items-center gap-1.5 text-[11px] text-dim">
          <FolderOpen size={11} />
          底层是一个 Rust agent harness，本应用只做界面与编排。
        </span>
      </Section>
    </>
  );
}
