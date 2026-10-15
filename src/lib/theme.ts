//! 主题：把设置里的 `theme` 落到 `<html class="dark">` 上。
//!
//! 三档：`system` / `light` / `dark`。`system` 不是「不管」—— 它要真的跟着系统
//! 变（用户在 Windows 里切深色，应用要跟着切），所以得挂一个 matchMedia 监听。
//!
//! 这里只做「谁决定最终是明是暗」这一件事；具体的颜色全在 index.css 的 token 里。

export type ThemePref = "system" | "light" | "dark";

const MEDIA = "(prefers-color-scheme: dark)";

/**
 * 主题偏好同时存一份到 localStorage。
 *
 * 真正的设置在 `~/.carter-app/settings.json` 里，但要等应用启动、`app_info` 回来
 * 才知道读什么 —— 那已经是首帧之后了，深色用户会看到一瞬白闪。`index.html` 里有
 * 一段内联脚本先按这里的值把 class 打上，等设置回来再以设置为准。
 */
export const THEME_KEY = "carter.theme";

/** 当前偏好下最终该是深色还是浅色。 */
export function resolveDark(pref: ThemePref): boolean {
  if (pref === "dark") return true;
  if (pref === "light") return false;
  return window.matchMedia(MEDIA).matches;
}

/** 把偏好落到 DOM 上。返回一个清理函数。 */
export function applyTheme(pref: ThemePref): () => void {
  const root = document.documentElement;

  try {
    localStorage.setItem(THEME_KEY, pref);
  } catch {
    // 无痕模式之类会抛 —— 那只是少了一次「不闪」，不该影响主题本身。
  }

  const sync = () => {
    root.classList.toggle("dark", resolveDark(pref));
  };
  sync();

  // 只有 system 档需要听系统变化；另外两档是用户显式选的，不该被系统带跑。
  if (pref !== "system") return () => {};

  const mq = window.matchMedia(MEDIA);
  mq.addEventListener("change", sync);
  return () => mq.removeEventListener("change", sync);
}
