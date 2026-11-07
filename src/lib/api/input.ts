import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// 输入框：斜杠命令、@ 文件、图片
// ---------------------------------------------------------------------------

/**
 * 一条能出现在 `/` 菜单里的命令。
 *
 * `builtin` 由本应用执行；`custom` 是 `~/.carter/commands/*.md` 里的命令，
 * 按回车后**不立即发送**，而是把它的展开交给后端（见下）——
 * 展开后的正文（含注入的文件内容）不该出现在输入框里。
 */
export type CommandRow = {
  name: string;
  description: string;
  argument_hint: string | null;
  kind: "builtin" | "custom";
};

export function listCommands(cwd?: string | null): Promise<CommandRow[]> {
  return invoke<CommandRow[]>("list_commands", { cwd: cwd ?? null });
}

/** `@` 菜单的文件列表：相对工作目录的路径。 */
export function listFiles(cwd: string | null, query: string, limit = 50): Promise<string[]> {
  return invoke<string[]>("list_files", { cwd, query, limit });
}

/**
 * 把粘贴进来的图片存进 carter 的图片库，返回可插进消息文本的 `[img:hash.ext]` 引用。
 *
 * `data` 是裸 base64 或 data URL 都行。服务端按 sha256 内容寻址去重，
 * 并在入库前把长边压到 1568px。
 */
export function saveImage(data: string, ext?: string | null): Promise<string> {
  return invoke<string>("save_image", { data, ext: ext ?? null });
}

/** 把一条图片引用（`[img:hash.ext]` 或裸 `hash.ext`）读成 data URL；读不到返回 null。 */
export function readImage(token: string): Promise<string | null> {
  return invoke<string | null>("read_image", { token });
}

/**
 * 文件预览：点击变更条里的一行时打开。
 *
 * `path` 可以是相对会话 cwd 的路径，也可以是绝对路径（工具可能写到 cwd 之外）。
 * 后端把「文件不在了 / 是二进制 / 是图片 / 读失败」都收成枚举变体，所以这里
 * 不会 reject —— 调用方按 `kind` 分别渲染即可。
 */
export function previewFile(cwd: string | null, path: string): Promise<FilePreview> {
  return invoke<FilePreview>("preview_file", { cwd, path });
}

/** 一个文件的预览结果。`kind` 决定弹窗里画什么。 */
/**
 * 某个文件从第 `since` 轮起累计的 diff（点开预览时按需算）。
 *
 * 只有 cwd 在 git 仓库里、且那几轮记下了轮首基线时才会有 `diff`；其余情况是
 * `unavailable` + 一句原因。
 */
export type FileDiff =
  | { kind: "diff"; path: string; text: string; truncated: boolean }
  | { kind: "empty"; path: string }
  | { kind: "unavailable"; path: string; reason: string };

export function fileDiff(
  sessionId: string,
  since: number,
  cwd: string,
  path: string,
): Promise<FileDiff> {
  return invoke<FileDiff>("file_diff", { sessionId, since, cwd, path });
}

/** 变更条的固定忽略名单。设置页展示用，真实来源在 core 里。 */
export type ChangeRules = { dirs: string[]; files: string[] };

export function changeRules(): Promise<ChangeRules> {
  return invoke<ChangeRules>("change_rules");
}

export type FilePreview =
  | {
      kind: "text";
      path: string;
      bytes: number;
      mtime: number | null;
      content: string;
      /** 文件过大，`content` 只是开头一段（后端已截断）。 */
      truncated: boolean;
    }
  | { kind: "image"; path: string; bytes: number; mtime: number | null; data_url: string }
  | { kind: "binary"; path: string; bytes: number; mtime: number | null }
  | { kind: "dir"; path: string }
  | { kind: "missing"; path: string }
  | { kind: "error"; path: string; message: string };
