// 从设置页之外跳进设置里的某一页（「去填密钥」之类的提示条用）。
//
// 设置页挂载时从 localStorage 读这个键决定停在哪一页（见 SettingsPage 的 `readTab`），
// 所以先写键、再切视图就够了，不用另开一份 store 状态。

import { useStore } from "../store";

export const SETTINGS_TAB_KEY = "carter.settings.tab";

export function openSettings(tab: string): void {
  try {
    localStorage.setItem(SETTINGS_TAB_KEY, tab);
  } catch {
    /* 存不下就停在上次那一页 */
  }
  useStore.getState().setView("settings");
}
