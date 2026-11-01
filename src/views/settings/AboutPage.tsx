// 关于：版本、路径、以及几个「为什么」。
//
// 设置项里有几件事只能在文件里做（比如 hooks、env、system.md）。与其假装它们
// 不存在，不如把路径列在这里 —— 用户至少知道该去翻哪个文件。

import { openUrl } from "@tauri-apps/plugin-opener";
import { ExternalLink, FolderOpen } from "lucide-react";

import { useStore } from "../../store";
import { Card, Section } from "../../components/ui/Panel";

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
      <h1 className="mb-1 text-base font-semibold">关于</h1>
      <div className="mb-5 text-xs text-dim">
        {appInfo ? `${appInfo.name} v${appInfo.version}` : "…"}
      </div>

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
                {appInfo?.models_cache_exists
                  ? "已存在"
                  : "还没有 —— 去「模型」页点「刷新元数据」"}
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
              <span className="text-ink">API Key 进系统凭据库</span> ——
              目前和命令行一样存在 config.toml 里（或写成{" "}
              <span className="mono">{"${环境变量}"}</span> 由环境提供）。
            </li>
            <li>
              <span className="text-ink">单个 MCP 工具的开关</span> ——
              MCP 工具名要连上 server 才知道，所以只能在 server 一级启停。
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
