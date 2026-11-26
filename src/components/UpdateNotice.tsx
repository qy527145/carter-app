// 首页的更新提示条：有新版时贴在对话顶栏下面。
//
// 刻意做成一条安静的行而不是弹窗：用户可能正读着一段回答，盖上来一个模态框
// 只会被当成广告。它只回答两件事 —— 「有新版本」和「点一下就装好」。
//
// 一旦开始更新，这条就变成进度 / 「正在重启」，因为装完进程会自己退出重启，
// 用户得知道刚才那一下不是卡死了。

import { openUrl } from "@tauri-apps/plugin-opener";
import { ArrowUpCircle, ChevronDown, ExternalLink, X } from "lucide-react";
import { useState } from "react";
import { cancelUpdate, percent, startUpdate, updating, useUpdateState } from "../lib/updater";
import { useStore } from "../store";
import { ReleaseNotes } from "./ReleaseNotes";
import { Button, IconButton } from "./ui/Button";

/** 用户点过「稍后」的那一版。换版本（或重启）之后会重新提示。 */
const DISMISS_KEY = "carter.update.dismissed";

function dismissed(version: string): boolean {
  try {
    return localStorage.getItem(DISMISS_KEY) === version;
  } catch {
    return false;
  }
}

function mb(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function UpdateNotice() {
  const release = useStore((s) => s.updateAvailable);
  const state = useUpdateState();
  const [skipped, setSkipped] = useState(() => (release ? dismissed(release.version) : false));
  // 更新内容默认收着：这一条的本分是「安静地提示」，想看的人点一下再展开。
  const [expanded, setExpanded] = useState(false);

  if (!release) return null;
  const busy = updating(state);
  if (!busy && skipped) return null;

  const skip = () => {
    setSkipped(true);
    try {
      localStorage.setItem(DISMISS_KEY, release.version);
    } catch {
      /* 存不下就只是这次运行里不再提示 */
    }
  };

  const pct = percent(state);
  const hasNotes = release.notes.trim().length > 0;
  const showNotes = hasNotes && expanded && !busy;

  return (
    <div className="shrink-0 px-4 pt-2">
      <div className="mx-auto max-w-3xl rounded-lg border border-edge bg-card text-xs">
        <div className="flex items-center gap-2.5 px-3 py-2">
          <ArrowUpCircle
            size={14}
            className={`icon-optical shrink-0 ${busy ? "text-tone-blue" : "text-tone-orange"}`}
          />
          {busy ? (
            <>
              <span className="shrink-0 text-ink">
                {state.kind === "downloading"
                  ? `正在下载 ${release.version}`
                  : state.kind === "installing"
                    ? "正在安装"
                    : "即将重启"}
              </span>
              <span className="h-1 min-w-24 flex-1 overflow-hidden rounded-full bg-bubble">
                <span
                  className={`block h-full bg-accent ${pct === null ? "carter-indeterminate w-1/3" : "transition-[width] duration-200"}`}
                  style={pct === null ? undefined : { width: `${pct}%` }}
                />
              </span>
              <span className="shrink-0 tabular-nums text-dim">
                {state.kind === "downloading"
                  ? state.total
                    ? `${pct}% · ${mb(state.done)} / ${mb(state.total)}`
                    : mb(state.done)
                  : state.kind === "installing"
                    ? "马上就好"
                    : "Carter 会自己重新打开"}
              </span>
              {state.kind === "downloading" && (
                <Button size="sm" variant="ghost" onClick={() => void cancelUpdate()}>
                  取消
                </Button>
              )}
            </>
          ) : (
            <>
              <span className="min-w-0 truncate text-ink">
                有新版 <span className="mono">{release.version}</span>
                {release.asset_size > 0 && (
                  <span className="text-dim">（{mb(release.asset_size)}）</span>
                )}
              </span>
              <span className="ml-auto flex shrink-0 items-center gap-1">
                {hasNotes ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    aria-expanded={expanded}
                    onClick={() => setExpanded((v) => !v)}
                  >
                    更新内容
                    <ChevronDown
                      size={12}
                      className={`ml-1 transition-transform duration-150 ${expanded ? "rotate-180" : ""}`}
                    />
                  </Button>
                ) : (
                  // 没有说明的版本（这套约定之前发的）只能去 Release 页面看。
                  <IconButton
                    title="在浏览器里打开这一版的发布页"
                    aria-label="发布页"
                    onClick={() => void openUrl(release.html_url).catch(() => {})}
                  >
                    <ExternalLink size={12} />
                  </IconButton>
                )}
                <Button size="sm" onClick={() => void startUpdate()}>
                  立即更新
                </Button>
                <IconButton title="稍后再说" aria-label="稍后再说" onClick={skip}>
                  <X size={12} />
                </IconButton>
              </span>
            </>
          )}
        </div>
        {showNotes && (
          <div className="max-h-60 overflow-y-auto border-t border-edge px-3 py-2.5">
            <ReleaseNotes notes={release.notes} />
          </div>
        )}
      </div>
    </div>
  );
}
