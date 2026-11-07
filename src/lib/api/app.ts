import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// 应用信息与设置
// ---------------------------------------------------------------------------

export type ThemePref = "system" | "light" | "dark";

/**
 * 对话主区的呈现密度。
 *
 * `detailed` = 现在的样子：思考、每一次工具调用都各占一张卡片；
 * `compact` = 只留正文，中间过程折成一行可展开的摘要。
 *
 * **只影响界面**，不改变发给模型的东西 —— 两种模式下工具照常执行、结果照常回灌。
 */
export type ConversationMode = "detailed" | "compact";

/** `~/.carter-app/settings.json`。**carter CLI 不读它。** */
export type AppSettings = {
  theme: ThemePref;
  /** 不注册给模型的本地工具名。 */
  disabled_tools: string[];
  sidebar_width: number;
  /** 对话主区的呈现密度。随时可切，来回切不碰会话历史。 */
  conversation_mode: ConversationMode;
  /**
   * 额外的技能扫描父目录（规范化后的绝对路径）。技能从当前项目的 `.carter/skills`、
   * `~/.carter/skills` 与这些目录**取并集**，项目优先、然后用户、再往后是这里。
   * 改动请走 `addSkillRoot` / `removeSkillRoot` ——
   * `setAppSettings` 是整体覆盖写入，拿旧值写回会把这些路径抹掉。
   */
  skill_roots: string[];
  /** 按名字忽略的技能。**只作用于外部扫描路径**（项目 / 用户技能靠搬目录启停）。 */
  ignored_skills: string[];
  /**
   * 变更条额外忽略的规则，每条一个 glob，相对会话 cwd。
   *
   * 含 `/` 按相对路径匹配（`dist/**`），不含 `/` 按任意层级的名字匹配（`*.log`）。
   * 内置目录规则与 `.gitignore` 不在这里，它们始终生效。
   */
  changes_ignore: string[];
  /** 全局昵称：加入新团队时预填的称呼。空 = 用 git 的 `user.name`。 */
  nickname: string;
  /** 从 GitHub 下载（连接器安装、更新）时允许走加速站。 */
  github_accel: boolean;
  /** 显式代理，如 `http://127.0.0.1:7897`。空 = 直连。 */
  http_proxy: string;
  /** models.dev 的额外源，一行一个完整 URL，排在内置源之后试。 */
  model_sources: string;
  /** 启动后自动检查新版本。关掉只剩「关于」页的手动入口。 */
  auto_check_update: boolean;
};

/** models.dev 缓存是哪一次、从哪个源拉的。镜像 `catalog::CacheMeta`。 */
export type ModelsMeta = {
  source: string;
  fetched_at: number;
  count: number;
  snapshot_date: string;
};

export type AppInfo = {
  name: string;
  version: string;
  default_cwd: string;
  default_model: string;
  /** 配置文件位置 —— 有些事只能去文件里做，界面要把路径写出来。 */
  config_path: string;
  config_exists: boolean;
  models_cache_exists: boolean;
  settings: AppSettings;
  /** models.dev 缓存的来源记录；从没刷新过是 `null`。 */
  models_meta: ModelsMeta | null;
  /** 编进安装包的目录快照是哪天的（`YYYY-MM-DD`）。 */
  models_snapshot_date: string;
};

export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>("app_info");
}

export function setAppSettings(settings: AppSettings): Promise<void> {
  return invoke<void>("set_app_settings", { settings });
}

/**
 * 开发用：打开 WebView 的开发者工具。
 *
 * 应用接管右键菜单之后，默认菜单里那个「检查元素」也没了 —— 这个命令把同一个入口
 * 留给 dev 构建（见 components/ui/ContextMenu.tsx），正式包里不会挂出来。
 */
export function openDevtools(): Promise<void> {
  return invoke<void>("open_devtools");
}
