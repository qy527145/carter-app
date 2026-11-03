// 成员相关的小件：头像、选人、加入团队。
//
// 这些在任务卡、任务抽屉、动态、指派框里反复出现。集中在一处，头像配色与「不认识时
// 显示什么」的说法才不会各处不一样。

import { useCallback, useEffect, useRef, useState } from "react";
import { Check, ChevronDown, LoaderCircle, UserPlus, UserRound } from "lucide-react";

import { setAppSettings, teamJoin, teamRenameMe, type TeamMember, type TeamPeople } from "../../lib/api";
import { avatarHue } from "../../lib/teamUi";
import { toastError, toastOk } from "../../lib/toast";
import { useStore } from "../../store";
import { Modal } from "../../components/ui/Panel";
import { usePeople } from "./people";

/** 成员 / 任意身份的头像。认得出成员就用成员 id 取色，换邮箱颜色也不变。 */
export function PersonAvatar({
  person,
  size = 18,
  title,
}: {
  /** 成员 id，或 v0 的 `名字 <邮箱>`。 */
  person: string | null | undefined;
  size?: number;
  title?: string;
}) {
  const { people } = usePeople();
  const member = find(people, person);
  const name = member?.name ?? shortName(person);
  if (!person || !name) return null;
  const key = member?.id ?? person;
  const hue = avatarHue(key);
  return (
    <span
      title={title ?? member?.emails.join(" / ") ?? person}
      className="inline-flex shrink-0 items-center justify-center rounded-full font-medium"
      style={{
        width: size,
        height: size,
        fontSize: size * 0.5,
        background: `hsl(${hue} 45% 48% / 0.18)`,
        color: `hsl(${hue} 60% 38%)`,
      }}
    >
      {[...name][0]?.toUpperCase() ?? "?"}
    </span>
  );
}

function find(people: TeamPeople | null, raw: string | null | undefined): TeamMember | null {
  if (!people || !raw) return null;
  const r = raw.trim();
  if (r.startsWith("m-")) return people.members.find((m) => m.id === r) ?? null;
  const email = /<([^>]+)>/.exec(r)?.[1];
  if (email) {
    const e = email.trim().toLowerCase();
    return people.members.find((m) => m.emails.some((x) => x.toLowerCase() === e)) ?? null;
  }
  return null;
}

function shortName(raw: string | null | undefined): string {
  if (!raw) return "";
  const at = raw.indexOf("<");
  return (at > 0 ? raw.slice(0, at) : raw).trim() || raw.trim();
}

/** 名字 + 头像，一行显示。 */
export function PersonChip({ person, size = 14 }: { person: string | null | undefined; size?: number }) {
  const { people } = usePeople();
  if (!person) return null;
  const name = find(people, person)?.name ?? shortName(person);
  return (
    <span className="flex items-center gap-1 text-[10px] text-dim">
      <PersonAvatar person={person} size={size} />
      {name}
    </span>
  );
}

/**
 * 选人。点开列出成员，顶部是「我自己」。
 *
 * 用弹出层而不是下拉框：要显示头像与「还没加入的人」（提交过代码但没加入），
 * 原生下拉放不下这些。
 */
export function PersonPicker({
  open,
  onClose,
  anchor,
  onPick,
  onClaim,
  footer,
}: {
  open: boolean;
  onClose: () => void;
  /** 弹出位置：按钮的屏幕坐标。 */
  anchor: { x: number; y: number } | null;
  onPick: (member: TeamMember) => void;
  /** 点了「我来做」。没给就不显示这一项。 */
  onClaim?: () => void;
  footer?: React.ReactNode;
}) {
  const { people } = usePeople();
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDown = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("pointerdown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, onClose]);
  if (!open || !anchor) return null;
  // 右侧空间不够时往左翻，底边不够时往上翻 —— 贴边的任务卡点开也不该被裁掉。
  const width = 220;
  const left = Math.min(anchor.x, window.innerWidth - width - 12);
  const est = Math.min(360, 120 + (people?.members.length ?? 0) * 30);
  const top = anchor.y + est > window.innerHeight ? Math.max(12, anchor.y - est - 32) : anchor.y;
  return (
    <div
      ref={ref}
      style={{ left, top, width }}
      className="fixed z-50 max-h-[60vh] overflow-y-auto rounded-xl bg-elevated py-1 shadow-lift"
    >
      {onClaim && (
        <button
          onClick={() => {
            onClaim();
            onClose();
          }}
          className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-ink transition-colors hover:bg-ink/5"
        >
          <UserRound size={13} className="shrink-0 text-tone-blue" />
          我来做
        </button>
      )}
      <div className="px-3 pb-1 pt-1.5 text-[10px] font-medium uppercase tracking-wider text-dim">
        交给
      </div>
      {(people?.members ?? []).map((m) => (
        <button
          key={m.id}
          onClick={() => {
            onPick(m);
            onClose();
          }}
          className="flex w-full items-center gap-2 px-3 py-1.5 text-left transition-colors hover:bg-ink/5"
        >
          <PersonAvatar person={m.id} size={16} />
          <span className="min-w-0 flex-1 truncate text-xs text-ink">{m.name}</span>
          {people?.me === m.id && <span className="shrink-0 text-[10px] text-dim">我</span>}
        </button>
      ))}
      {(people?.members.length ?? 0) === 0 && (
        <div className="px-3 py-2 text-[11px] leading-relaxed text-dim">
          团队里还没有人加入。在右上角点头像加入，之后就能互相指派。
        </div>
      )}
      {(people?.pending.length ?? 0) > 0 && (
        <div className="mt-1 border-t border-edge px-3 py-1.5 text-[10px] leading-relaxed text-dim">
          提交过代码、还没加入：{people!.pending.slice(0, 4).join("、")}
        </div>
      )}
      {footer && <div className="border-t border-edge px-2 py-1.5">{footer}</div>}
    </div>
  );
}

/**
 * 加入团队。第一次在这个团队里需要身份时弹一次。
 *
 * 两种加入方式：给自己取个称呼（新成员），或者「我在这台机器上是 X」（把本机邮箱
 * 挂到已有成员名下）—— 一个人公司机器与个人机器上的 git 邮箱常常不一样，
 * 不认这一层的话同一个人会变成两个成员，「我的任务」在其中一台机器上是空的。
 */
export function JoinDialog({ cwd, onClose }: { cwd: string; onClose: () => void }) {
  const { people, reload } = usePeople();
  const settings = useStore((s) => s.settings);
  const setSettings = useStore((s) => s.setSettings);
  const gitName = shortName(people?.identity ?? "");
  // 预填顺序：全局昵称 → git 的 user.name。全局昵称是「我在所有团队里都叫这个」。
  const [name, setName] = useState(settings?.nickname || gitName);
  const [asExisting, setAsExisting] = useState<string | null>(null);
  const [remember, setRemember] = useState(true);
  const [busy, setBusy] = useState(false);

  const submit = useCallback(async () => {
    setBusy(true);
    try {
      const out = asExisting
        ? await teamJoin(cwd, { memberId: asExisting })
        : await teamJoin(cwd, { name });
      toastOk(
        asExisting
          ? `已以「${out.member.name}」的身份加入（本机邮箱挂到了这个名下）`
          : `已以「${out.member.name}」的身份加入`,
      );
      // 观察源没建成只影响「后台提醒」，团队页里照样看得到消息 —— 说一句，不算失败。
      if (out.inbox_source) toastError(`加入了，但消息提醒没开起来：${out.inbox_source}`);
      if (remember && name.trim() && settings) {
        const next = { ...settings, nickname: name.trim() };
        setSettings(next);
        // 存不下只是下次加入别的团队时不预填，不值得打断。
        void setAppSettings(next).catch(() => {});
      }
      await reload();
      onClose();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  }, [asExisting, cwd, name, onClose, reload, remember, settings, setSettings]);

  const candidates = people?.members ?? [];
  return (
    <Modal
      title="加入团队"
      onClose={onClose}
      width={460}
      footer={
        <>
          <button
            onClick={onClose}
            className="rounded-md px-3 py-1.5 text-sm text-dim transition-colors hover:bg-ink/5 hover:text-ink"
          >
            以后再说
          </button>
          <button
            onClick={() => void submit()}
            disabled={busy || (!asExisting && !name.trim())}
            className="flex items-center gap-1.5 rounded-md bg-ink px-3 py-1.5 text-sm font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-40"
          >
            {busy && <LoaderCircle size={13} className="animate-spin" />}
            加入
          </button>
        </>
      }
    >
      <p className="mb-3 text-xs leading-relaxed text-dim">
        团队里认的是人，不是 git 邮箱 —— 邮箱只用来认出「这台机器上的你」。
        {people?.identity && (
          <>
            <br />
            本机身份：<span className="mono text-ink/80">{people.identity}</span>
          </>
        )}
      </p>

      <label className="mb-1 block text-[11px] font-medium text-dim">在这个团队里怎么称呼你</label>
      <input
        autoFocus
        value={name}
        onChange={(e) => {
          setName(e.target.value);
          setAsExisting(null);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") void submit();
        }}
        placeholder={gitName || "比如：徐侨"}
        className="carter-field mb-1 w-full rounded-md border border-edge bg-base px-3 py-2 text-sm text-ink outline-none"
      />
      <label className="mb-4 flex items-center gap-1.5 text-[11px] text-dim">
        <input
          type="checkbox"
          checked={remember}
          onChange={(e) => setRemember(e.target.checked)}
          className="accent-ink"
        />
        以后加入别的团队也用这个昵称
      </label>

      {candidates.length > 0 && (
        <>
          <div className="mb-1 text-[11px] font-medium text-dim">
            已经在这个团队里？这是你的另一台机器的话，认领它 —— 本机邮箱会挂上去
          </div>
          <div className="max-h-48 overflow-y-auto rounded-lg bg-card">
            {candidates.map((m) => {
              const on = asExisting === m.id;
              return (
                <button
                  key={m.id}
                  onClick={() => setAsExisting(on ? null : m.id)}
                  className={`flex w-full items-center gap-2 px-2.5 py-1.5 text-left transition-colors hover:bg-ink/5 ${
                    on ? "bg-ink/5" : ""
                  }`}
                >
                  <PersonAvatar person={m.id} size={18} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-xs text-ink">{m.name}</span>
                    <span className="mono block truncate text-[10px] text-dim">
                      {m.emails.join(" · ")}
                    </span>
                  </span>
                  {on && <Check size={13} className="shrink-0 text-ink" />}
                </button>
              );
            })}
          </div>
        </>
      )}
    </Modal>
  );
}

/**
 * 团队页右上角的「我是谁」：没加入时是一个「加入团队」入口，加入后点头像可以改称呼。
 * 不加入也能用团队页（只看不认），所以它不挡任何东西。
 */
export function IdentityChip({ cwd, onJoin }: { cwd: string; onJoin: () => void }) {
  const { people, reload } = usePeople();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const me = people?.me ? people.members.find((m) => m.id === people.me) : null;
  const [busy, setBusy] = useState(false);

  if (!people) return null;
  if (!me) {
    return (
      <button
        onClick={onJoin}
        title="加入之后才能被指派、被 @"
        className="flex items-center gap-1 rounded-full bg-tone-orange-soft px-2 py-1 text-[10px] text-tone-orange transition-opacity hover:opacity-80"
      >
        <UserPlus size={11} />
        加入团队
      </button>
    );
  }

  const save = async () => {
    const name = draft.trim();
    if (!name || name === me.name) {
      setEditing(false);
      return;
    }
    setBusy(true);
    try {
      await teamRenameMe(cwd, name);
      await reload();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
      setEditing(false);
    }
  };

  if (editing) {
    return (
      <div className="flex items-center gap-1">
        <input
          autoFocus
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={() => void save()}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
            if (e.key === "Escape") setEditing(false);
          }}
          className="w-28 rounded-full bg-card px-2 py-1 text-[11px] outline-none"
        />
        {busy && <LoaderCircle size={11} className="animate-spin text-dim" />}
      </div>
    );
  }
  return (
    <button
      onClick={() => {
        setDraft(me.name);
        setEditing(true);
      }}
      title={`我是 ${me.name}\n${me.emails.join("\n")}\n点击改称呼`}
      className="flex items-center gap-1 rounded-full bg-card px-2 py-1 text-[10px] text-dim transition-colors hover:text-ink"
    >
      <PersonAvatar person={me.id} size={14} />
      {me.name}
      <ChevronDown size={10} className="opacity-60" />
    </button>
  );
}
