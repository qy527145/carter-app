// 团队就绪状态：这个项目的团队协作到底有没有在工作，坏了就当场修。
//
// ## 为什么是一个抽屉，不是设置页的一栏
//
// 这一页回答的是「我正看着的这个团队现在能不能用」，属于**项目上下文**。放到设置页
// 就丢了「哪个项目」这件事。抽屉与任务抽屉同一套交互：从右侧滑出、点背板关掉、
// Esc 关掉，人不用学第二种打开方式。
//
// ## 为什么每行只有一句话加一个按钮
//
// 诊断类界面的通病是把它做成「状态矩阵」：七八个项目 × 四五列，看得出信息量很大，
// 但坏了的时候没人知道从哪下手。这里每行只回答两件事 —— 现在是什么样、能不能一键
// 弄好。正常的行连按钮都不给（按钮出现在正常的行上，等于暗示它有问题）。
//
// ## 为什么「找回」要先确认
//
// 整个面板只有这一个动作会往工作区写文件。虽然是只写回**缺失**的那些，但用户按下去
// 之前应该先看见「会恢复哪几个文件」，尤其当那个数字是 40 的时候。

import { Check, CircleAlert, LoaderCircle, TriangleAlert, Wrench, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";

import { ConfirmDialog } from "../../components/ui/Panel";
import { type TeamCheck, type TeamHealth, teamHealth, teamRepair } from "../../lib/api";
import { toastError, toastOk } from "../../lib/toast";
import { checkGlyph, type Glyph, healthSummary, previewPaths, splitChecks } from "./teamHealthUi";

/** 图标与颜色。字形与文字成对出现（文字在 `Glyph.label`），不靠颜色单独传达状态。 */
function GlyphIcon({ glyph, size = 13 }: { glyph: Glyph; size?: number }) {
  const cls = "mt-0.5 shrink-0";
  switch (glyph.tone) {
    case "ok":
      return <Check size={size} className={`${cls} text-tone-green`} />;
    case "warn":
      return <TriangleAlert size={size} className={`${cls} text-tone-orange`} />;
    case "broken":
      return <CircleAlert size={size} className={`${cls} text-[var(--danger)]`} />;
    case "muted":
      return <span className={`${cls} inline-block size-[13px]`} />;
  }
}

export function TeamHealthDrawer({
  cwd,
  onClose,
  onRepaired,
  onConfigureRemote,
}: {
  cwd: string;
  onClose: () => void;
  /** 修完之后让团队页重读 —— 观察源补回来了、目录找回来了，页面上的东西都变了。 */
  onRepaired: () => void;
  /** 「配远端」不在这里重写一遍：团队页头部已经有那个输入框。 */
  onConfigureRemote: () => void;
}) {
  const [health, setHealth] = useState<TeamHealth | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  /** 待确认的恢复：列出会写回的文件。 */
  const [confirming, setConfirming] = useState<TeamCheck | null>(null);

  const load = useCallback(async () => {
    try {
      setHealth(await teamHealth(cwd));
      setError(null);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }, [cwd]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // 确认框在上面一层时，Esc 归它。它的 Modal 自己会处理。
      if (e.key === "Escape" && !confirming) onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, confirming]);

  const run = useCallback(
    async (scope: string, after?: () => void) => {
      setBusy(scope);
      try {
        const out = await teamRepair(cwd, scope);
        const detail = [...out.steps, ...out.warnings].filter(Boolean).join(" · ");
        if (out.warnings.length > 0) toastError(out.warnings.join("；"));
        else toastOk(detail || "已经补好了");
        await load();
        onRepaired();
        after?.();
      } catch (e) {
        toastError(e);
      } finally {
        setBusy(null);
      }
    },
    [cwd, load, onRepaired],
  );

  /** 一个有动作的项被点了。`files` 且要写回文件时先确认。 */
  const act = useCallback(
    (c: TeamCheck) => {
      if (c.action_kind === "remote") {
        onClose();
        onConfigureRemote();
        return;
      }
      if (c.action_kind === "files" && c.paths.length > 0) {
        setConfirming(c);
        return;
      }
      if (c.action_kind) void run(c.action_kind);
    },
    [onClose, onConfigureRemote, run],
  );

  const summary = health ? healthSummary(health.checks) : null;
  const split = health ? splitChecks(health.checks) : null;
  const preview = confirming ? previewPaths(confirming.paths) : null;

  return (
    <div className="fixed inset-0 z-30 flex justify-end" onClick={onClose}>
      <aside
        role="dialog"
        aria-modal="true"
        aria-label="团队就绪状态"
        onClick={(e) => e.stopPropagation()}
        className="carter-drawer flex h-full w-[28rem] max-w-[92vw] flex-col border-l border-edge bg-elevated shadow-2xl"
      >
        <header className="flex items-start gap-2 border-b border-edge px-4 py-3">
          <div className="min-w-0 flex-1">
            <h3 className="text-sm font-semibold">团队就绪状态</h3>
            <p className="mt-0.5 text-[11px] leading-relaxed text-dim">
              git 仓库、团队目录、后台观察源——它们一起决定了目标和任务能不能同步给别人。
            </p>
          </div>
          <button
            onClick={onClose}
            aria-label="关闭"
            className="carter-press -mr-1 shrink-0 rounded-md p-1 text-dim hover:bg-ink/5 hover:text-ink"
          >
            <X size={15} />
          </button>
        </header>

        {/* 一条总结 + 唯一的批量动作。没有待修项时整块不出现。 */}
        {summary && summary.attention > 0 && (
          <div className="flex items-center gap-2 border-b border-edge bg-card px-4 py-2">
            <span className="min-w-0 flex-1 text-[11px] text-ink">{summary.label}</span>
            <button
              onClick={() => {
                const files = split?.attention.find((c) => c.action_kind === "files");
                if (files && files.paths.length > 0) setConfirming(files);
                else void run("all");
              }}
              disabled={busy !== null}
              className="flex shrink-0 items-center gap-1 rounded-md bg-ink px-2.5 py-1 text-[11px] font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-40"
            >
              {busy === "all" ? (
                <LoaderCircle size={11} className="animate-spin" />
              ) : (
                <Wrench size={11} />
              )}
              全部修复
            </button>
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
          {error ? (
            <p className="text-xs leading-relaxed text-[var(--danger)]">{error}</p>
          ) : !health || !split ? (
            <div className="flex items-center gap-2 py-8 text-xs text-dim">
              <LoaderCircle size={13} className="animate-spin" />
              正在检查…
            </div>
          ) : (
            <>
              <div className="space-y-2">
                {split.attention.map((c) => (
                  <CheckRow key={c.id} check={c} busy={busy} onAct={() => act(c)} />
                ))}
              </div>

              {/* 安静的行：有东西要修时折成一行，没有时全展开（让人确认「我看过了」）。 */}
              {split.quiet.length > 0 &&
                (split.attention.length > 0 ? (
                  <div className="mt-4 flex flex-wrap items-center gap-x-2 gap-y-1 px-1 text-[10px] text-dim">
                    <Check size={11} className="text-tone-green" />
                    {split.quiet.map((c) => c.title).join(" · ")} 都正常
                  </div>
                ) : (
                  <div className="space-y-2">
                    {split.quiet.map((c) => (
                      <CheckRow key={c.id} check={c} busy={busy} onAct={() => act(c)} />
                    ))}
                  </div>
                ))}
            </>
          )}

          {health && (
            <div
              className="mono mt-4 truncate px-1 text-[10px] text-dim"
              title={health.project_root}
            >
              {health.project_root}
            </div>
          )}
        </div>
      </aside>

      {confirming && preview && (
        <ConfirmDialog
          title="从 git 历史找回"
          confirmLabel={`找回 ${confirming.paths.length} 个文件`}
          busy={busy === "files"}
          message={
            <>
              <p className="mb-2">
                这些文件在工作区里已经不见了，但上次提交里还留着。会按那份写回来：
              </p>
              <div className="mono max-h-40 overflow-y-auto rounded-md bg-bubble px-2 py-1.5 text-[10px] leading-relaxed">
                {preview.shown.map((p) => (
                  <div key={p} className="truncate" title={p}>
                    {p}
                  </div>
                ))}
                {preview.more > 0 && <div className="text-dim">…等 {preview.more} 个</div>}
              </div>
              <p className="mt-2 text-[11px]">
                还在的文件一个都不会动——你改过但还没提交的内容会原样留着。
              </p>
            </>
          }
          onCancel={() => setConfirming(null)}
          onConfirm={() => {
            setConfirming(null);
            void run("files");
          }}
        />
      )}
    </div>
  );
}

/** 一行检查。标题 + 状态标签 + 一句话；有动作时右下角一个按钮。 */
function CheckRow({
  check,
  busy,
  onAct,
}: {
  check: TeamCheck;
  busy: string | null;
  onAct: () => void;
}) {
  const glyph = checkGlyph(check.state);
  const working = busy !== null && busy === check.action_kind;
  return (
    <div className="rounded-xl bg-card px-4 py-3">
      <div className="flex items-start gap-2">
        <GlyphIcon glyph={glyph} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
            <span className="text-xs font-medium text-ink">{check.title}</span>
            <span
              className={`rounded px-1.5 py-[1px] text-[10px] ${
                glyph.tone === "broken"
                  ? "bg-[var(--error-bg)] text-[var(--danger)]"
                  : glyph.tone === "warn"
                    ? "bg-tone-orange-soft text-tone-orange"
                    : glyph.tone === "ok"
                      ? "bg-tone-green-soft text-tone-green"
                      : "bg-bubble text-dim"
              }`}
            >
              {glyph.label}
            </span>
          </div>
          <p className="mt-1 text-[11px] leading-relaxed text-dim">{check.detail}</p>
        </div>
      </div>
      {check.action && (
        <div className="mt-2 flex justify-end">
          <button
            onClick={onAct}
            disabled={busy !== null}
            className={`flex items-center gap-1 rounded-md px-2.5 py-1 text-[11px] font-medium transition-opacity hover:opacity-90 disabled:opacity-40 ${
              glyph.tone === "broken"
                ? "bg-ink text-[var(--accent-contrast)]"
                : "bg-ink/5 text-dim hover:text-ink"
            }`}
          >
            {working && <LoaderCircle size={11} className="animate-spin" />}
            {check.action}
          </button>
        </div>
      )}
    </div>
  );
}
