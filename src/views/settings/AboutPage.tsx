// 关于：版本、路径、以及几个「为什么」。
//
// 设置项里有几件事只能在文件里做（比如 hooks、env、system.md）。与其假装它们
// 不存在，不如把路径列在这里 —— 用户至少知道该去翻哪个文件。

import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink, FolderOpen } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card, Section } from "../../components/ui/Panel";
import {
  cancelUpdateDownload,
  checkUpdate,
  downloadUpdate,
  installUpdate,
  type ReleaseInfo,
  restartApp,
  UPDATE_PROGRESS,
  type UpdateCheck,
} from "../../lib/api";
import { useStore } from "../../store";

function PathRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline gap-3 py-1.5">
      <span className="w-24 shrink-0 text-xs text-dim">{label}</span>
      <span className="mono min-w-0 flex-1 break-all text-[11px] text-ink">{value}</span>
    </div>
  );
}

/** 下载到哪一步了。`idle` 之外的状态都会锁住「检查更新」按钮。 */
type Download = { kind: "idle" } | { kind: "downloading"; done: number; total: number | null };

function mb(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/**
 * 更新卡片。
 *
 * 流程刻意是「检查 → 下载 → 装」三步各自一个按钮，而不是一个「一键更新」：
 * 下载一百多 MB 之前，用户得能看到**要下哪个文件、多大** —— Release 里没有
 * 签名清单，文件名与大小就是他能核对的全部。装完了也不自动重启（macOS 的
 * 重启会丢掉正跑着的会话），由用户自己选时机。
 */
function UpdateCard() {
  const appInfo = useStore((s) => s.appInfo);
  // 启动后自动检查查到的新版（如果有）直接作为初始结果，免得进这一页再查一次。
  const autoFound = useStore((s) => s.updateAvailable);
  const [result, setResult] = useState<UpdateCheck | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [download, setDownload] = useState<Download>({ kind: "idle" });
  /** 下载好的安装包路径。 */
  const [file, setFile] = useState<string | null>(null);
  const [installed, setInstalled] = useState<string | null>(null);

  // 进度事件。订阅放在这里而不是全局：只有这一页开着的时候才需要进度。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen<{ downloaded: number; total: number | null }>(UPDATE_PROGRESS, ({ payload }) => {
      setDownload({ kind: "downloading", done: payload.downloaded, total: payload.total });
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const current = appInfo?.version ?? "";
  const release: ReleaseInfo | null = result?.release ?? autoFound;

  const run = async (f: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title="版本">
      <Card>
        <div className="flex flex-wrap items-center gap-2">
          <span className="mono text-xs text-ink">{current ? `v${current}` : "…"}</span>
          <Button
            size="sm"
            disabled={busy || download.kind === "downloading"}
            onClick={() =>
              run(async () => {
                setFile(null);
                setInstalled(null);
                const r = await checkUpdate();
                setResult(r);
                // 手动查过就以这次为准：启动时那个标记可能已经过期。
                useStore.getState().setUpdateAvailable(r.release);
              })
            }
          >
            {busy ? "检查中…" : "检查更新"}
          </Button>
          {result && !result.has_update && (
            <span className="text-[11px] text-dim">已经是最新版本</span>
          )}
          {release && (
            <span className="text-[11px] text-tone-orange">
              有新版 {release.version}
              {release.asset_name ? `（${release.asset_name}，${mb(release.asset_size)}）` : ""}
            </span>
          )}
        </div>

        {error && <div className="mt-2 text-[11px] text-tone-red">{error}</div>}

        {release?.asset_name && !installed && (
          <div className="mt-3 flex flex-wrap items-center gap-2 border-t border-edge pt-3">
            {download.kind === "downloading" ? (
              <>
                <div className="h-1 min-w-40 flex-1 overflow-hidden rounded-full bg-bubble">
                  <div
                    className="h-full bg-accent transition-[width]"
                    style={{
                      width: download.total
                        ? `${Math.min(100, (download.done / download.total) * 100)}%`
                        : "100%",
                    }}
                  />
                </div>
                <span className="text-[11px] tabular-nums text-dim">
                  {download.total
                    ? `${mb(download.done)} / ${mb(download.total)}`
                    : `已下载 ${mb(download.done)}`}
                </span>
                <Button size="sm" variant="ghost" onClick={() => void cancelUpdateDownload()}>
                  取消
                </Button>
              </>
            ) : file ? (
              <>
                <Button
                  size="sm"
                  onClick={() => run(async () => setInstalled(await installUpdate(file)))}
                >
                  安装
                </Button>
                <span className="text-[11px] text-dim">
                  安装包已下好：{file.split(/[\\/]/).pop()}
                </span>
              </>
            ) : (
              <Button
                size="sm"
                disabled={busy}
                onClick={() =>
                  run(async () => {
                    setDownload({ kind: "downloading", done: 0, total: release.asset_size });
                    try {
                      setFile(await downloadUpdate());
                    } finally {
                      setDownload({ kind: "idle" });
                    }
                  })
                }
              >
                下载
              </Button>
            )}
            <Button
              size="sm"
              variant="ghost"
              onClick={() => void openUrl(release.html_url).catch(() => {})}
            >
              更新说明
            </Button>
          </div>
        )}

        {installed && (
          <div className="mt-3 border-t border-edge pt-3">
            <div className="text-[11px] leading-relaxed text-ink">{installed}</div>
            <Button size="sm" className="mt-2" onClick={() => void restartApp()}>
              重启 Carter
            </Button>
          </div>
        )}

        <div className="mt-2 text-[11px] leading-relaxed text-dim">
          Release 里直接放各平台的安装包：Windows 的 NSIS、macOS（Apple Silicon）的 DMG、 Linux 的
          deb。
        </div>
      </Card>
    </Section>
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

      <UpdateCard />

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
                    ? `内置快照（${appInfo.models_snapshot_date}）· ${new Date(appInfo.models_meta.fetched_at).toLocaleString()} 尝试联网刷新未成功`
                    : `${appInfo.models_meta.source} · ${new Date(appInfo.models_meta.fetched_at).toLocaleString()} · ${appInfo.models_meta.count} 条`}
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
