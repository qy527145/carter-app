import { invoke } from "@tauri-apps/api/core";
import type { AppSettings } from "./app";

// ---------------------------------------------------------------------------
// 技能
// ---------------------------------------------------------------------------

/**
 * 技能根的三类归属。镜像 `skills.rs::RootKind`。
 *
 * - `project`：`<会话 cwd>/.carter/skills`，跟着项目走，可写；
 * - `local`：`~/.carter/skills`，用户自己的，可写；
 * - `external`：用户加进来的其它工具目录（Claude Code / Codex），只读。
 */
export type SkillRootKind = "project" | "local" | "external";

export type SkillRow = {
  name: string;
  description: string;
  allowed_tools: string[];
  /** 界面上那个开关的状态：本应用的技能看目录在哪一侧，外部技能看忽略名单。 */
  enabled: boolean;
  /** 这一份 `SKILL.md` 的绝对路径。 */
  path: string;
  /** 这一行来自哪个扫描根（绝对路径）。写操作照它定位。 */
  source: string;
  /** 这一行属于哪一类根。列表按它分组与标注。 */
  scope: SkillRootKind;
  /** 外部路径 → `true`：不能编辑、不能删除，开关也只能改忽略名单。 */
  readonly: boolean;
  /** 有没有进**并集** —— 也就是模型到底用不用得到它。 */
  effective: boolean;
  /** 被同名技能挡住时，指向挡住它的那一份 `SKILL.md`。 */
  shadowed_by: string | null;
};

/** 一条扫描路径，附带「为什么它扫到的技能没全生效」的几个计数。 */
export type SkillRoot = {
  path: string;
  /** `project` / `local` 是本应用的目录（不可移除）；`external` = 用户加进来的。 */
  source: SkillRootKind;
  exists: boolean;
  /** 直接子目录数。 */
  subdirs: number;
  /** 解析出 `SKILL.md` 的技能数。 */
  found: number;
  /** 进了并集的数量。 */
  effective: number;
  /** 因为同名被前面的根挡住的数量。 */
  shadowed: number;
  /** 因为忽略名单被排掉的数量。 */
  ignored: number;
};

/** 快捷选项：别的工具放技能的默认位置。 */
export type SkillPreset = {
  id: string;
  label: string;
  /** 这个预设包含的全部路径（Codex 是两条，见 `hint`）。 */
  paths: string[];
  /** 其中**还没加**的那些 —— 点一下只加这些，于是重复点也不会撞上「已经在列表里」。 */
  missing: string[];
  exists: boolean;
  hint: string;
};

export type SkillListing = {
  skills: SkillRow[];
  /** 全局开关（`[skills].enabled`）。关掉时整个技能目录都不加载。 */
  globally_enabled: boolean;
  /** 扫描根一览：项目 → 用户 → 附加路径（顺序 = 优先级）。 */
  roots: SkillRoot[];
  presets: SkillPreset[];
  /** 被按名字忽略的外部技能。有些名字可能已经没有对应的行了，靠它撤销。 */
  ignored_skills: string[];
};

export type SkillDraft = {
  name: string;
  description: string;
  allowed_tools: string[];
  body: string;
  /**
   * 写到哪个根（列表里那一行的 `source`）。不给 = 按名字找它现在所在的目录，
   * 新技能落用户目录。「新建」时由编辑框上的选择决定。
   */
  source?: string | null;
};

/**
 * 技能列表。`cwd` 是**当前会话**的工作目录 —— 项目级技能靠它定位
 * （`<cwd>/.carter/skills`，找不到就沿父目录往上找）。没有会话时传 `null`。
 */
export function listSkills(cwd?: string | null): Promise<SkillListing> {
  return invoke<SkillListing>("list_skills", { cwd: cwd ?? null });
}

/** 读技能正文。`source` 给了就只读那一行对应的根（同名技能可能有好几行）。 */
export function readSkill(
  name: string,
  source?: string | null,
  cwd?: string | null,
): Promise<string> {
  return invoke<string>("read_skill", { name, source: source ?? null, cwd: cwd ?? null });
}

/** 保存技能，返回实际写入的根目录。 */
export function saveSkill(draft: SkillDraft, cwd?: string | null): Promise<string> {
  return invoke<string>("save_skill", { draft, cwd: cwd ?? null });
}

/**
 * 启停单个技能：项目级 / 用户级的把目录在 `skills/` 与 `skills.disabled/` 之间搬，
 * 外部路径的（Claude Code / Codex 那边）只记进忽略名单 —— 本应用不碰别人的目录。
 *
 * `source` 必须传**点的是哪一行**（行上的 `source`）。同一个名字可能同时在项目、用户、
 * 外部三个根里出现，能区分它们的只有来源。从「已忽略」名单恢复一个名字时传 `null`。
 *
 * 回传新的 `AppSettings`，调用方要 `setSettings` 对齐，否则下一次「改主题」会用
 * 旧值整体覆盖，把忽略名单清掉。
 */
export function toggleSkill(
  name: string,
  enabled: boolean,
  source: string | null,
  cwd?: string | null,
): Promise<AppSettings> {
  return invoke<AppSettings>("toggle_skill", {
    name,
    enabled,
    source,
    cwd: cwd ?? null,
  });
}

/** 删技能。`source` 指到哪个根就删哪个根 —— 同名的可能在项目与用户两处都有。 */
export function deleteSkill(
  name: string,
  source?: string | null,
  cwd?: string | null,
): Promise<void> {
  return invoke<void>("delete_skill", { name, source: source ?? null, cwd: cwd ?? null });
}

export function setSkillsEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_skills_enabled", { enabled });
}

/** 加一条技能扫描路径。回传新的 `AppSettings`（理由同 `toggleSkill`）。 */
export function addSkillRoot(path: string): Promise<AppSettings> {
  return invoke<AppSettings>("add_skill_root", { path });
}

export function removeSkillRoot(path: string): Promise<AppSettings> {
  return invoke<AppSettings>("remove_skill_root", { path });
}
