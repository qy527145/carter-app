import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// 自更新
//
// 查、下、装全在后端（`crates/carter-app-core/src/update.rs`）。Release 里放的是
// 三平台的原生安装包，不是 Tauri 更新器那一套 `latest.json` + 归档 —— 这里没有
// 「签名」这一步，装之前由界面把文件名与大小报给用户确认。
// ---------------------------------------------------------------------------

/** 下载进度事件。与 `src-tauri/src/lib.rs` 的 `UPDATE_PROGRESS` 必须一致。 */
export const UPDATE_PROGRESS = "carter://update-progress";

/** 「发现新版本」事件（启动后自动检查时发出）。与 `UPDATE_AVAILABLE` 一致。 */
export const UPDATE_AVAILABLE = "carter://update-available";

/** GitHub Release 里挑出来的那个安装包。 */
export type ReleaseInfo = {
  /** `v0.1.2`。 */
  tag: string;
  /** 去掉 `v` 的版本号。 */
  version: string;
  /** Release 页面，界面上的「更新说明」跳这里。 */
  html_url: string;
  /** 安装包文件名，如 `Carter_0.1.2_aarch64.dmg`。 */
  asset_name: string;
  /** 下载地址（后端按仓库名拼出来的，前端只回显，不能改）。 */
  asset_url: string;
  asset_size: number;
};

export type UpdateCheck = {
  has_update: boolean;
  /** 当前运行的版本。 */
  current: string;
  /** 有新版时才有；没有新版时是 `null`。 */
  release: ReleaseInfo | null;
};

export function checkUpdate(): Promise<UpdateCheck> {
  return invoke<UpdateCheck>("check_update");
}

/** 更新进行到哪一步（`UPDATE_PROGRESS` 事件的载荷）。 */
export type UpdateProgress = {
  /**
   * `downloading` 带字节数；`installing` / `restarting` 不带。
   * 界面按它切换文案：下载时显示进度，之后只显示「正在安装 / 即将重启」。
   */
  phase: "downloading" | "installing" | "restarting";
  downloaded?: number;
  total?: number | null;
};

/**
 * 一键更新：查 → 下 → 装 → 自动重启。进度走 [`UPDATE_PROGRESS`]。
 *
 * 装完之后**本进程会自己退出**，由后端把新版拉起来 —— 所以正常情况下这个
 * Promise 的返回值前端根本看不到（窗口先一步没了）。它 Reject 才是真的失败。
 */
export function applyUpdate(): Promise<void> {
  return invoke<void>("apply_update");
}

export function cancelUpdateDownload(): Promise<void> {
  return invoke<void>("cancel_update_download");
}

/** 重启应用（macOS 用 `ditto` 覆盖后在跑的这一份已经是新的了）。 */
export function restartApp(): Promise<void> {
  return invoke<void>("restart_app");
}
