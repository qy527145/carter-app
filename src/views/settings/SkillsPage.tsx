// 技能页：查看 / 启停 / 新建 / 编辑 / 删除。
//
// ## 单个技能的启停为什么是把目录搬走
//
// carter 的 `parse_skill_md` 只认 frontmatter 的 name / description / allowed-tools，
// 多写一个 `enabled: false` 会被当成未知键**静默忽略**，技能照样加载。
// 所以单技能启停落在文件系统上：目录在 `~/.carter/skills/` 与
// `~/.carter/skills.disabled/` 之间搬（后端 `skills.rs` 做的）。页面上要说清这一点，
// 否则用户去文件管理器里会找不到自己刚停用的技能。

import { useEffect, useState } from "react";
import { FileText, Pencil, Plus, Sparkles, Trash } from "lucide-react";

import {
  deleteSkill,
  listSkills,
  readSkill,
  saveSkill,
  setSkillsEnabled,
  toggleSkill,
  type SkillListing,
  type SkillRow,
} from "../../lib/api";
import { toastError, toastOk } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { useStore } from "../../store";
import { Button, IconButton } from "../../components/ui/Button";
import { Field, Input, Switch, Textarea } from "../../components/ui/Input";
import { Badge, Banner, Card, ConfirmDialog, Modal } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

/** 技能编辑弹窗：frontmatter 的三个键 + 正文。 */
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
  return (
    <>
      <Card className="space-y-2">
        <div className="flex items-center gap-2">
          <Sparkles size={13} className="shrink-0 text-tone-violet" />
          <span className="text-sm font-medium text-ink">{s.name}</span>
          {s.allowed_tools.length > 0 && (
            <span className="flex flex-wrap gap-1">
              {s.allowed_tools.map((t) => (
                <Badge key={t}>{t}</Badge>
              ))}
            </span>
          )}
          <span className="ml-auto flex shrink-0 items-center gap-1.5">
            <Switch checked={s.enabled} onChange={onToggle} label={`启用 ${s.name}`} />
            <IconButton title="编辑" onClick={onEdit}>
              <Pencil size={12} />
            </IconButton>
            <IconButton title="删除" onClick={() => setConfirming(true)}>
              <Trash size={12} />
            </IconButton>
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
  const setNotice = useStore((s) => s.setNotice);

  async function run(fn: () => Promise<void>) {
    try {
      await fn();
      state.reload();
    } catch (e) {
      toastError(e);
    }
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
        发现，frontmatter 里的 <span className="mono">name</span> /{" "}
        <span className="mono">description</span> / <span className="mono">allowed-tools</span>{" "}
        三个键会被读取。
      </PageIntro>

      <AsyncBody
        state={state}
        empty={
          <>
            没有发现任何技能。
            <br />
            点右上角「新建技能」创建一个。
          </>
        }
      >
        {(listing: SkillListing) => (
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
                    全局开关是关的 —— 下面这些技能虽然显示为启用，实际都不会生效。
                  </Banner>
                </div>
              )}
            </div>

            <div className="mb-2 text-[11px] leading-relaxed text-dim">
              单个技能的启停是把目录在{" "}
              <span className="mono">~/.carter/skills/</span> 与{" "}
              <span className="mono">~/.carter/skills.disabled/</span> 之间搬 ——
              停用的技能在文件管理器里会出现在后者下面。
            </div>

            <div className="space-y-2">
              {listing.skills.map((s) => (
                <SkillCard
                  key={s.name}
                  s={s}
                  onEdit={() => setEditing(s)}
                  onToggle={(v) => void run(() => toggleSkill(s.name, v))}
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
          </>
        )}
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

      {state.data && state.data.skills.length === 0 && state.data.globally_enabled && (
        <div className="mt-4 flex items-start gap-2 text-[11px] text-dim">
          <FileText size={12} className="mt-0.5 shrink-0" />
          还没有技能。技能是纯 Markdown ── 一个目录加一份 SKILL.md 就够了。
        </div>
      )}
    </>
  );
}
