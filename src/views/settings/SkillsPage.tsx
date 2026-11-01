// 技能页：查看 / 启停 / 新建 / 编辑 / 删除，以及**扫描路径**的管理。
//
// ## 单个技能的启停为什么是把目录搬走
//
// carter 的 `parse_skill_md` 只认 frontmatter 的 name / description / allowed-tools，
// 多写一个 `enabled: false` 会被当成未知键**静默忽略**，技能照样加载。
// 所以单技能启停落在文件系统上：目录在 `~/.carter/skills/` 与
// `~/.carter/skills.disabled/` 之间搬（后端 `skills.rs` 做的）。页面上要说清这一点，
// 否则用户去文件管理器里会找不到自己刚停用的技能。
//
// ## 外部扫描路径：取并集，但那里只能看和忽略
//
// 除了 `~/.carter/skills`，还能加别的技能父目录 —— Claude Code 的 `~/.claude/skills`、
// Codex 的 `~/.codex/skills`，或任意自定义路径。加进来之后**取并集**：本地优先，
// 按名字去重、先出现者胜。
//
// 列表**不去重**：被同名技能挡住的那一行也列出来并标明是谁挡的。藏起来的话，用户就
// 看不出「我 Codex 里那个技能为什么不生效」—— 而那正是最需要解释的状态。
//
// 外部路径是别人的目录（Claude Code / Codex 自己的），所以那边的技能**只能看和忽略**：
// 不提供编辑与删除，开关走的是「按名字忽略」，而不是把目录搬走。页面上要把这条写出来，
// 否则用户会以为按钮丢了。
//
// 扫描路径存在 `~/.carter-app/settings.json`，**只对本应用生效** ——
// carter 命令行读不到它，也不会受它影响。

import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { FolderPlus, Pencil, Plus, Sparkles, Trash, X } from "lucide-react";

import {
  addSkillRoot,
  deleteSkill,
  listSkills,
  readSkill,
  removeSkillRoot,
  saveSkill,
  setSkillsEnabled,
  toggleSkill,
  type AppSettings,
  type SkillListing,
  type SkillRoot,
  type SkillRow,
} from "../../lib/api";
import { toastError, toastOk } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { useStore } from "../../store";
import { Button, IconButton } from "../../components/ui/Button";
import { Field, Input, Switch, Textarea } from "../../components/ui/Input";
import { Badge, Banner, Card, ConfirmDialog, Modal } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

/** 技能编辑弹窗：frontmatter 的三个键 + 正文。只服务本地技能。 */
function SkillEditor({
  editing,
  onClose,
}: {
  /** `null` = 新建。 */
  editing: SkillRow | null;
  onClose: (changed: boolean) => void;
}) {
  const [name, setName] = useState(editing?.name ?? "");
  const [description, setDescription] = useState(editing?.description ?? "");
  const [allowed, setAllowed] = useState((editing?.allowed_tools ?? []).join(", "));
  const [body, setBody] = useState("");
  const [saving, setSaving] = useState(false);

  // 正文按需拉：列表接口只带 frontmatter，正文可能很大。
  useEffect(() => {
    if (!editing) return;
    let cancelled = false;
    readSkill(editing.name)
      .then((b) => !cancelled && setBody(b))
      .catch((e) => !cancelled && toastError(e));
    return () => {
      cancelled = true;
    };
  }, [editing]);

  const valid = name.trim() !== "";

  async function submit() {
    setSaving(true);
    try {
      await saveSkill({
        name: name.trim(),
        description: description.trim(),
        allowed_tools: allowed
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean),
        body,
      });
      onClose(true);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <Modal
      title={editing ? `编辑技能 · ${editing.name}` : "新建技能"}
      onClose={() => onClose(false)}
      width={600}
      footer={
        <>
          <Button onClick={() => onClose(false)} disabled={saving}>
            取消
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || saving}>
            {saving ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      <Field label="名称" hint="会成为 ~/.carter/skills/<名称>/SKILL.md，只能用字母、数字、-、_">
        <Input
          mono
          value={name}
          onChange={(e) => setName(e.target.value)}
          disabled={editing !== null}
          placeholder="例如 pdf"
        />
      </Field>

      <Field label="描述" hint="模型据此判断什么时候用这个技能">
        <Input
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          placeholder="一句话说明它做什么"
        />
      </Field>

      <Field label="允许的工具" hint="逗号分隔；留空 = 不限制">
        <Input
          mono
          value={allowed}
          onChange={(e) => setAllowed(e.target.value)}
          placeholder="read_file, bash"
        />
      </Field>

      <Field label="正文" hint="模型加载技能时看到的说明">
        <Textarea
          rows={12}
          value={body}
          onChange={(e) => setBody(e.target.value)}
          placeholder={"# 用法\n\n步骤一……"}
        />
      </Field>
    </Modal>
  );
}

/** 一条扫描路径：路径 + 「扫到多少、为什么没全生效」的计数 + 移除。 */
function RootRow({
  r,
  busy,
  onRemove,
}: {
  r: SkillRoot;
  busy: boolean;
  onRemove: () => void;
}) {
  const local = r.source === "local";
  return (
    <div className="flex items-start gap-2 border-t border-edge pt-2 first:border-t-0 first:pt-0">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <span className="mono truncate text-[11px] text-ink" title={r.path}>
            {r.path}
          </span>
          {local && <Badge title="本应用自己的技能目录，恒在扫描范围里">自有</Badge>}
        </div>
        <div className="mt-0.5 text-[10px] text-dim">
          {!r.exists
            ? "目录不存在 —— 现在扫不到任何技能（别的工具之后建出来就会自动生效）"
            : `子目录 ${r.subdirs} · 技能 ${r.found} · 生效 ${r.effective}` +
              (r.shadowed > 0 ? ` · 被同名遮蔽 ${r.shadowed}` : "") +
              (r.ignored > 0 ? ` · 已忽略 ${r.ignored}` : "")}
        </div>
        {r.exists && r.subdirs > 0 && r.found === 0 && (
          <div className="mt-1 text-[10px] text-dim/80">
            有子目录但没有 <span className="mono">SKILL.md</span> ——
            技能可能在更下一层。Codex 的 <span className="mono">.system/</span> 就是这种情况，
            所以它的预设会一次加两条路径。
          </div>
        )}
      </div>
      {!local && (
        <IconButton title={`移除 ${r.path}`} onClick={onRemove} disabled={busy}>
          <X size={12} />
        </IconButton>
      )}
    </div>
  );
}

/** 扫描路径区块：预设 + 自定义 + 一览。 */
function ScanRootsCard({
  listing,
  busy,
  onAdd,
  onRemove,
  onUnignore,
}: {
  listing: SkillListing;
  busy: boolean;
  onAdd: (path: string) => void;
  /** 移除一条扫描路径。 */
  onRemove: (path: string) => void;
  /** 恢复一个被忽略的技能名。 */
  onUnignore: (name: string) => void;
}) {
  const [custom, setCustom] = useState("");

  async function pick() {
    // 返回值可能是 string | string[] | null（multiple 为 false 时理论上是 string，
    // 但类型上收窄一次更省心），照 Composer 里选工作目录的写法来。
    const dir = await openDialog({ directory: true, multiple: false, title: "选择技能父目录" });
    if (typeof dir !== "string" || !dir) return;
    onAdd(dir);
  }

  function submitCustom() {
    const v = custom.trim();
    if (!v) return;
    onAdd(v);
    setCustom("");
  }

  return (
    <Card className="space-y-3">
      <div className="flex items-center gap-2">
        <FolderPlus size={13} className="shrink-0 text-tone-violet" />
        <span className="text-sm text-ink">扫描路径</span>
        <span className="text-[10px] text-dim">
          自上而下就是优先级：本地优先，然后是下面这些，重名时靠前的赢
        </span>
      </div>

      <div className="space-y-2">
        {listing.roots.map((r) => (
          <RootRow key={r.path} r={r} busy={busy} onRemove={() => onRemove(r.path)} />
        ))}
      </div>

      <div className="space-y-2 border-t border-edge pt-3">
        <div className="text-[10px] text-dim">快捷选项（别的工具放技能的默认位置）</div>
        <div className="flex flex-wrap gap-1.5">
          {listing.presets.map((p) => (
            <Button
              key={p.id}
              size="sm"
              disabled={busy || p.missing.length === 0}
              title={[p.hint, ...p.paths].join("\n")}
              onClick={() => p.missing.forEach(onAdd)}
            >
              <Plus size={12} />
              {p.label}
              {p.missing.length === 0 && " · 已加入"}
            </Button>
          ))}
        </div>
      </div>

      <div className="space-y-2 border-t border-edge pt-3">
        <div className="text-[10px] text-dim">
          自定义：随便一个装着 <span className="mono">&lt;名称&gt;/SKILL.md</span> 的父目录
        </div>
        <div className="flex items-center gap-2">
          <Input
            mono
            value={custom}
            onChange={(e) => setCustom(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submitCustom()}
            placeholder="例如 D:\skills，也可以用 ~ 开头"
            disabled={busy}
          />
          <Button size="sm" disabled={busy || !custom.trim()} onClick={submitCustom}>
            添加
          </Button>
          <Button size="sm" disabled={busy} onClick={() => void pick()}>
            浏览…
          </Button>
        </div>
      </div>

      {listing.ignored_skills.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5 border-t border-edge pt-3 text-[10px] text-dim">
          <span>
            已忽略的技能<strong className="font-medium">名</strong>（不是路径）：
          </span>
          {listing.ignored_skills.map((n) => (
            <button
              key={n}
              type="button"
              title="恢复使用"
              disabled={busy}
              onClick={() => onUnignore(n)}
              className="mono inline-flex items-center gap-1 rounded bg-bubble px-1.5 py-0.5 text-[10px] text-dim transition-colors hover:text-ink disabled:opacity-50"
            >
              {n}
              <X size={9} />
            </button>
          ))}
        </div>
      )}
    </Card>
  );
}

function SkillCard({
  s,
  onEdit,
  onToggle,
  onDelete,
}: {
  s: SkillRow;
  onEdit: () => void;
  onToggle: (v: boolean) => void;
  onDelete: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  // 开关是开的、却没生效 → 被前面的根里同名的那一个挡住了。这时候不给开关：
  // 忽略名单按名字生效，在一个被遮蔽的行上点关闭会连带关掉真正生效的那一个。
  const shadowed = s.enabled && !s.effective;

  return (
    <>
      <Card className="space-y-2">
        <div className="flex items-center gap-2">
          <Sparkles size={13} className="shrink-0 text-tone-violet" />
          <span className="text-sm font-medium text-ink">{s.name}</span>
          {s.readonly && (
            <Badge title="来自外部扫描路径，那是别的工具自己的目录：本应用不改动它，只能忽略">
              外部 · 只读
            </Badge>
          )}
          {s.allowed_tools.length > 0 && (
            <span className="flex flex-wrap gap-1">
              {s.allowed_tools.map((t) => (
                <Badge key={t}>{t}</Badge>
              ))}
            </span>
          )}
          <span className="ml-auto flex shrink-0 items-center gap-1.5">
            {shadowed ? (
              <Badge tone="orange" title={`被同名技能挡住：${s.shadowed_by ?? ""}`}>
                被同名遮蔽
              </Badge>
            ) : (
              <Switch checked={s.enabled} onChange={onToggle} label={`启用 ${s.name}`} />
            )}
            {!s.readonly && (
              <>
                <IconButton title="编辑" onClick={onEdit}>
                  <Pencil size={12} />
                </IconButton>
                <IconButton title="删除" onClick={() => setConfirming(true)}>
                  <Trash size={12} />
                </IconButton>
              </>
            )}
          </span>
        </div>
        <div className="text-xs leading-relaxed text-dim">{s.description || "（无描述）"}</div>
        <div className="mono truncate text-[10px] text-dim/70" title={s.path}>
          {s.path}
        </div>
      </Card>

      {confirming && (
        <ConfirmDialog
          title="删除技能"
          danger
          confirmLabel="确认删除"
          message={
            <>
              将删除 <span className="mono text-ink">{s.path}</span> 所在的整个目录。
              <strong className="font-medium">不可恢复</strong>。
            </>
          }
          onCancel={() => setConfirming(false)}
          onConfirm={() => {
            setConfirming(false);
            onDelete();
          }}
        />
      )}
    </>
  );
}

export function SkillsPage() {
  const state = useAsync(listSkills, []);
  const [editing, setEditing] = useState<SkillRow | null | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const setNotice = useStore((s) => s.setNotice);
  const setSettings = useStore((s) => s.setSettings);

  /** 写操作统一走这里：期间冻结路径区块，成功后就地重拉列表。 */
  async function run(fn: () => Promise<AppSettings | void>) {
    setBusy(true);
    try {
      const next = await fn();
      // 后端回传了新的设置就对齐 store。它是 `set_app_settings`（**整体覆盖写入**）
      // 的来源，不同步的话，下一次在「外观」页改主题会把刚加的扫描路径、
      // 刚忽略的技能一起抹掉。
      if (next) setSettings(next);
      state.reload();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }

  /**
   * 已忽略的技能在「已忽略」那一行上点一下就是恢复 —— 传的是技能名，不是路径。
   * `external = true`：忽略名单本来就只作用于外部路径，撤销它自然也走那一条。
   */
  function unignore(name: string) {
    void run(() => toggleSkill(name, true, true));
  }

  return (
    <>
      <div className="mb-4 flex items-center gap-2">
        <h1 className="text-base font-semibold">技能</h1>
        <Button
          variant="primary"
          size="sm"
          className="ml-auto"
          onClick={() => setEditing(null)}
        >
          <Plus size={13} />
          新建技能
        </Button>
      </div>

      <PageIntro>
        技能从 <span className="mono">~/.carter/skills/&lt;名称&gt;/SKILL.md</span>{" "}
        与下面这些扫描路径里<strong className="font-medium">取并集</strong>发现：本地优先、
        按名字去重。frontmatter 里的 <span className="mono">name</span> /{" "}
        <span className="mono">description</span> /{" "}
        <span className="mono">allowed-tools</span> 三个键会被读取。
      </PageIntro>

      <AsyncBody
        state={state}
        empty={
          <>
            没有发现任何技能。
            <br />
            点右上角「新建技能」创建一个，或者加一条扫描路径。
          </>
        }
      >
        {(listing: SkillListing) => {
          const externalRoots = listing.roots.filter((r) => r.source === "external");
          // 分组顺序跟着扫描根走（本地在最前）—— 优先级是**读出来**的。
          const groups = [
            { title: "本地", items: listing.skills.filter((s) => !s.readonly) },
            ...externalRoots.map((r) => ({
              title: r.path,
              items: listing.skills.filter((s) => s.readonly && s.source === r.path),
            })),
          ].filter((g) => g.items.length > 0);

          return (
            <>
              <div className="mb-4">
                <Card className="flex items-center gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="text-sm text-ink">全局启用技能</div>
                    <div className="text-[11px] text-dim">
                      关掉之后整个技能目录都不加载（写进配置的{" "}
                      <span className="mono">[skills].enabled</span>）
                    </div>
                  </div>
                  <Switch
                    checked={listing.globally_enabled}
                    label="全局启用技能"
                    onChange={(v) =>
                      void run(() => setSkillsEnabled(v).then(() => void 0))
                    }
                  />
                </Card>
                {!listing.globally_enabled && (
                  <div className="mt-2">
                    <Banner kind="warn">
                      全局开关是关的 —— 下面这些路径照常可以编辑，但技能一个都不会生效。
                    </Banner>
                  </div>
                )}
              </div>

              <div className="mb-4">
                <ScanRootsCard
                  listing={listing}
                  busy={busy}
                  onAdd={(p) =>
                    void run(() =>
                      addSkillRoot(p).then((s) => {
                        toastOk(`已加入扫描路径：${p}`);
                        return s;
                      }),
                    )
                  }
                  onRemove={(p) =>
                    void run(() =>
                      removeSkillRoot(p).then((s) => {
                        toastOk(`已移除：${p}`);
                        return s;
                      }),
                    )
                  }
                  onUnignore={(n) => unignore(n)}
                />
              </div>

              <div className="mb-2 text-[11px] leading-relaxed text-dim">
                本地技能的启停是把目录在{" "}
                <span className="mono">~/.carter/skills/</span> 与{" "}
                <span className="mono">~/.carter/skills.disabled/</span> 之间搬；
                外部路径里的技能搬不得（那是别的工具的目录），所以开关记的是「按名字忽略」。
              </div>

              {groups.map((g) => (
                <div key={g.title} className="mb-4">
                  <div className="mono mb-2 truncate text-[10px] text-dim" title={g.title}>
                    {g.title}
                  </div>
                  <div className="space-y-2">
                    {g.items.map((s) => (
                      <SkillCard
                        key={`${s.source}/${s.name}`}
                        s={s}
                        onEdit={() => setEditing(s)}
                        onToggle={(v) => void run(() => toggleSkill(s.name, v, s.readonly))}
                        onDelete={() =>
                          void run(() =>
                            deleteSkill(s.name).then(() => {
                              toastOk(`已删除技能 ${s.name}`);
                            }),
                          )
                        }
                      />
                    ))}
                  </div>
                </div>
              ))}

              {listing.skills.length === 0 && (
                <div className="text-[11px] text-dim">
                  {externalRoots.length === 0
                    ? "还没有技能。技能是纯 Markdown —— 一个目录加一份 SKILL.md 就够了。"
                    : "扫描路径都在这儿，但里面没有 SKILL.md。上面的计数会说明它们扫到了什么。"}
                </div>
              )}
            </>
          );
        }}
      </AsyncBody>

      {editing !== undefined && (
        <SkillEditor
          editing={editing}
          onClose={(changed) => {
            setEditing(undefined);
            if (changed) state.reload();
            else setNotice(null);
          }}
        />
      )}
    </>
  );
}
