// 侧栏底部的更新提示。
//
// 只在「有东西可做」的时候出现：下载中（看进度）、已就绪（点一下重启换新版本）。
// 检查中 / 失败不占这里的地方 —— 一次网络抖动不该在侧栏常驻一条红字，那些去
// 「设置 · 关于」看。状态本身由后端推（`update_changed` 事件），这里只负责显示。

import { invoke } from "@tauri-apps/api/core";
import { Loader2, RotateCw } from "lucide-react";
import { useState } from "react";
import { installUpdate, type UpdateStatus } from "../lib/api";
import { toastError } from "../lib/toast";
import { useStore } from "../store";

function fmtMB(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 状态 → 侧栏那一行；没什么可显示时是 `null`。 */
function describe(s: UpdateStatus | null): { text: string; pct: number | null } | null {
  if (!s) return null;
  switch (s.phase) {
    case "downloading": {
      const pct = s.total ? Math.min(100, Math.round((s.downloaded / s.total) * 100)) : null;
      return {
        text: `正在下载 v${s.version} · ${pct !== null ? `${pct}%` : fmtMB(s.downloaded)}`,
        pct,
      };
    }
    case "ready":
      return { text: `v${s.version} 已就绪`, pct: null };
    case "installing":
      return { text: `正在安装 v${s.version}…`, pct: null };
    default:
      return null;
  }
}

export function UpdateNotice() {
  const status = useStore((s) => s.updateStatus);
  const [busy, setBusy] = useState(false);
  const info = describe(status);
  if (!info) return null;

  const ready = status?.phase === "ready";
  const installing = status?.phase === "installing";

  const install = async () => {
    setBusy(true);
    try {
      // 真正开始装之前把「关窗 = 收进托盘」摘掉：安装要替换正在运行的 `.app`
      // （macOS）或拉起安装器（Windows），这时有任何东西拦住退出，
      // macOS 上 `.app` 就是「文件正在使用」装不进去。
      await invoke("set_update_installing", { installing: true }).catch(() => {});
      await installUpdate();
      // 成功的话进程会被重启，这里通常回不来；回得来就是真出错了。
    } catch (e) {
      toastError(e);
      setBusy(false);
    }
  };

  return (
    <div className="rounded-lg border border-edge bg-ink/[0.03] px-2.5 py-2">
      <div className="text-[11px] leading-snug text-ink">{info.text}</div>
      {info.pct !== null && (
        <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-ink/10">
          {/* 进度条是装饰：进度数字已经写在上面那句里，读屏用户不需要它。 */}
          <div
            className="h-full rounded-full bg-ink/50 transition-[width] duration-300"
            style={{ width: `${info.pct}%` }}
          />
        </div>
      )}
      {ready && (
        <button
          onClick={install}
          disabled={busy}
          className="carter-press mt-1.5 flex w-full items-center justify-center gap-1.5 rounded-md bg-accent px-2 py-1 text-[11px] font-medium text-[var(--accent-contrast)] outline-none disabled:opacity-60"
        >
          <RotateCw size={11} />
          {busy ? "正在重启…" : "重启更新"}
        </button>
      )}
      {installing && (
        <div className="mt-1.5 flex items-center gap-1.5 text-[11px] text-dim">
          <Loader2 size={11} className="animate-spin" />
          马上重启
        </div>
      )}
    </div>
  );
}
