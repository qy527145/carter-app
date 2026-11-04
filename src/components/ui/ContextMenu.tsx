// 右键菜单：应用自己的，替换 WebView 默认那套（Dev 下是 Reload / Inspect Element）。
//
// 默认菜单在正式包里本来就不该出现，但 dev 下它每处右键都弹，和这个应用的观感
// 完全两回事；而且「Reload」对用户是有害的 —— 一按就把界面状态清空。所以：
//
//   · 输入框与选中文字**放行**给系统 —— 剪切/复制/粘贴由 WebKit 原生实现，
//     自己实现粘贴要额外权限，还会丢掉系统语言与输入法行为；
//   · 其余地方一律压掉默认菜单，由组件按上下文调用 `useContextMenu().open(...)`；
//   · 没接菜单的区域右键不弹任何东西（原生桌面应用就是这样）。
//
// 判断「组件是否已经处理过」用的是 `defaultPrevented`：React 的事件挂在 #root 上，
// 冒泡到 window 时组件的 handler 已经跑完了。

import { Check } from "lucide-react";
import {
  createContext,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";

import { openDevtools } from "../../lib/api";

export type MenuEntry =
  | {
      label: string;
      icon?: ReactNode;
      /** 右侧的快捷键提示。纯展示 —— 目前没有真正绑定快捷键。 */
      shortcut?: string;
      danger?: boolean;
      disabled?: boolean;
      /** 勾选态（单选/开关类条目）。给了就在图标位画对勾，不给图标。 */
      checked?: boolean;
      onSelect: () => void;
    }
  | "separator"
  /** 分组小标题。不可点，键盘导航跳过它。 */
  | { heading: string };

/** 菜单左上角的落点。`align: "end"` 表示 x 是右边缘（用于从按钮右侧弹出）。 */
export type OpenAt = { x: number; y: number; align?: "start" | "end" };

type MenuApi = {
  open: (at: OpenAt, entries: MenuEntry[]) => void;
  close: () => void;
};

const MenuContext = createContext<MenuApi | null>(null);

export function useContextMenu(): MenuApi {
  const api = useContext(MenuContext);
  if (!api) throw new Error("useContextMenu 必须在 <ContextMenuProvider> 里使用");
  return api;
}

/** 当前有没有选中文字。有选中时右键交给系统菜单（复制选中的那一段）。 */
export function hasSelection(): boolean {
  const sel = window.getSelection();
  return !!sel && !sel.isCollapsed && sel.toString().trim().length > 0;
}

function isEditable(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.closest !== "function") return false;
  return !!el.closest('input, textarea, [contenteditable=""], [contenteditable="true"]');
}

type OpenState = OpenAt & { entries: MenuEntry[]; key: number };

export function ContextMenuProvider({ children }: { children: ReactNode }) {
  const [menu, setMenu] = useState<OpenState | null>(null);
  // 同一位置连开两次也要重新测尺寸（上一次的 entries 可能已经换了），
  // 所以用递增 key 强制重挂载，而不是依赖坐标是否变化。
  const seq = useRef(0);

  const close = useCallback(() => setMenu(null), []);

  const open = useCallback((at: OpenAt, entries: MenuEntry[]) => {
    if (entries.length === 0) return;
    setMenu({ ...at, entries, key: ++seq.current });
  }, []);

  // 压掉 WebView 的默认菜单。放行输入框与选中文字 —— 见模块头。
  useEffect(() => {
    const onContextMenu = (e: MouseEvent) => {
      if (e.defaultPrevented) return; // 组件已经开了自己的菜单
      if (isEditable(e.target) || hasSelection()) return;
      e.preventDefault();
      // 开发期唯一的例外：默认菜单里那个「检查元素」还有用，留一个入口。
      // 正式包里不挂 —— 用户不需要看见 WebView 的内部概念。
      if (import.meta.env.DEV) {
        setMenu({
          x: e.clientX,
          y: e.clientY,
          entries: [
            {
              label: "开发者工具",
              shortcut: "⌥⌘I",
              onSelect: () => void openDevtools().catch(() => {}),
            },
          ],
          key: ++seq.current,
        });
      }
    };
    window.addEventListener("contextmenu", onContextMenu);
    return () => window.removeEventListener("contextmenu", onContextMenu);
  }, []);

  // 菜单里写着 ⌥⌘I，那就得真的能用 —— 只在 dev 构建里绑。
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey && e.altKey && e.key.toLowerCase() === "i") {
        e.preventDefault();
        void openDevtools().catch(() => {});
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // value 不稳定的话，每次开/关菜单都会让所有消费者（每一行会话、每一条消息）
  // 跟着重渲染一遍。open/close 本身是稳定的，这里把它们固定成一个对象。
  const api = useMemo(() => ({ open, close }), [open, close]);

  return (
    <MenuContext.Provider value={api}>
      {children}
      {menu &&
        createPortal(
          <Menu
            key={menu.key}
            x={menu.x}
            y={menu.y}
            align={menu.align}
            entries={menu.entries}
            onClose={close}
          />,
          document.body,
        )}
    </MenuContext.Provider>
  );
}

function Menu({
  x,
  y,
  align = "start",
  entries,
  onClose,
}: OpenAt & { entries: MenuEntry[]; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number; origin: string } | null>(null);
  // 打开后的一小段时间里忽略 mouseup：右键按下弹出菜单、手一松，光标正好落在
  // 第一项上 —— 不挡的话这一下松手就成了「点了第一项」。
  const openedAt = useRef(performance.now());
  // 键盘首字母跳转：连续敲几个字母在 500ms 内拼成前缀去匹配。
  const typeahead = useRef({ text: "", at: 0 });

  // 先量真实尺寸再定位：菜单在光标处弹出是正常的，弹到窗口外面就点不到了。
  // 放不下时**翻到光标另一侧**而不是贴边挤 —— 贴边会让菜单盖住光标指着的那一行。
  // 用 layout effect 是为了在绘制前算好，不会先闪一下再跳。
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const margin = 8;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    let left = align === "end" ? x - w : x;
    let fromRight = align === "end";
    if (!fromRight && left + w > window.innerWidth - margin && x - w >= margin) {
      left = x - w;
      fromRight = true;
    }
    let top = y;
    let fromBottom = false;
    if (top + h > window.innerHeight - margin && y - h >= margin) {
      top = y - h;
      fromBottom = true;
    }
    setPos({
      left: Math.max(margin, Math.min(left, window.innerWidth - w - margin)),
      top: Math.max(margin, Math.min(top, window.innerHeight - h - margin)),
      // 缩放从光标那个角长出来 —— 菜单是「从这里弹出的」，不是凭空出现在中间。
      origin: `${fromBottom ? "bottom" : "top"} ${fromRight ? "right" : "left"}`,
    });
  }, [x, y, align]);

  // 焦点落到菜单上，方向键 / Esc 才有地方可去；收起时还给原来那个元素 ——
  // 用键盘唤起的菜单按 Esc 退出后，焦点不该掉到 body 上。
  //
  // 首帧菜单还是 `visibility: hidden`（在量尺寸），对隐藏元素 focus() 是静默失败的 ——
  // 所以等定位好、真正可见了再聚焦。另挂一个 document 级的 Esc 兜底：焦点万一
  // 没进来（比如被别的元素抢走），Esc 也照样能关。
  const prevFocus = useRef(document.activeElement as HTMLElement | null);
  // 唤起时焦点是否落在某个具体元素上且正显示键盘焦点环 —— 据此判断是不是键盘唤起的。
  const byKeyboard = useRef(prevFocus.current?.matches(":focus-visible") ?? false).current;
  const visible = pos !== null;
  useEffect(() => {
    if (visible) ref.current?.focus({ preventScroll: true });
  }, [visible]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !ref.current?.contains(document.activeElement)) {
        e.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      // 只有键盘唤起时才把焦点还回去：鼠标右键之后程序化 focus 会让那一行
      // 挂上 focus-visible 的描边，看起来像被「选中」了。
      const prev = prevFocus.current;
      const lost = !document.activeElement || document.activeElement === document.body;
      if (byKeyboard && prev?.isConnected && lost) prev.focus({ preventScroll: true });
    };
  }, [onClose]);

  // 滚动、缩放、切窗口都会让菜单和它指向的东西错位，直接收起。
  useEffect(() => {
    window.addEventListener("resize", onClose);
    window.addEventListener("blur", onClose);
    document.addEventListener("scroll", onClose, true);
    return () => {
      window.removeEventListener("resize", onClose);
      window.removeEventListener("blur", onClose);
      document.removeEventListener("scroll", onClose, true);
    };
  }, [onClose]);

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key === "Tab") {
      // 菜单不是模态：Tab 应该离开它，而不是把焦点困在里面。
      onClose();
      return;
    }
    const buttons = Array.from(
      ref.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [],
    );
    if (buttons.length === 0) return;
    const cur = buttons.indexOf(document.activeElement as HTMLButtonElement);
    let next: number | null = null;
    if (e.key === "ArrowDown") next = cur < 0 ? 0 : (cur + 1) % buttons.length;
    else if (e.key === "ArrowUp")
      next = cur < 0 ? buttons.length - 1 : (cur - 1 + buttons.length) % buttons.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = buttons.length - 1;
    else if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey) {
      const now = performance.now();
      const t = typeahead.current;
      t.text = (now - t.at < 500 ? t.text : "") + e.key.toLowerCase();
      t.at = now;
      // 从当前项之后找起，同一个字母连按就在同首字母的几项间轮转。
      const start = t.text.length === 1 ? cur + 1 : Math.max(cur, 0);
      for (let k = 0; k < buttons.length; k++) {
        const b = buttons[(start + k) % buttons.length];
        if ((b.dataset.label ?? "").toLowerCase().startsWith(t.text)) {
          next = (start + k) % buttons.length;
          break;
        }
      }
    }
    if (next !== null) {
      e.preventDefault();
      buttons[next].focus();
    }
  };

  return (
    <>
      {/* 点任何地方都收起。放在 portal 里而不是组件内：菜单可能被任何层级的元素唤起。 */}
      <div
        className="fixed inset-0 z-50"
        onMouseDown={onClose}
        onContextMenu={(e) => {
          // 菜单开着时在别处再右键：收起这一份，并把这次右键「穿透」给底下的元素，
          // 让它弹自己的菜单 —— 原生应用里换个地方右键是一步，不是两步。
          e.preventDefault();
          onClose();
          const under = document.elementsFromPoint(e.clientX, e.clientY)[1];
          under?.dispatchEvent(
            new MouseEvent("contextmenu", {
              bubbles: true,
              cancelable: true,
              clientX: e.clientX,
              clientY: e.clientY,
              button: 2,
            }),
          );
        }}
      />
      <div
        ref={ref}
        role="menu"
        tabIndex={-1}
        onKeyDown={onKeyDown}
        // 定位算好之前先藏着，避免在最角落闪一帧。
        style={{
          left: pos?.left ?? x,
          top: pos?.top ?? y,
          transformOrigin: pos?.origin,
          visibility: pos ? "visible" : "hidden",
        }}
        // 外层有 1px 的内衬环：深色主题里阴影托不起来，靠这一圈把菜单从底上勾出来。
        className="carter-menu fixed z-50 min-w-48 max-w-72 rounded-lg bg-elevated p-1 shadow-lift ring-1 ring-edge outline-none"
      >
        {entries.map((entry, i) =>
          entry === "separator" ? (
            <div key={i} role="separator" className="mx-1.5 my-1 h-px bg-edge" />
          ) : "heading" in entry ? (
            <div
              key={i}
              className="px-2 pb-0.5 pt-1.5 text-[10px] font-medium text-dim/80 select-none"
            >
              {entry.heading}
            </div>
          ) : (
            <button
              key={i}
              type="button"
              role={entry.checked === undefined ? "menuitem" : "menuitemcheckbox"}
              aria-checked={entry.checked}
              data-label={entry.label}
              disabled={entry.disabled}
              // 悬停 = 聚焦：鼠标与方向键共用**同一个**高亮，不会出现
              // 「鼠标停在第三项、键盘高亮还在第一项」两处同时亮的状态。
              onMouseMove={(e) => {
                if (document.activeElement !== e.currentTarget) e.currentTarget.focus();
              }}
              onMouseLeave={() => ref.current?.focus()}
              onMouseUp={(e) => {
                // 右键按住拖到某一项再松手 = 选中它（macOS 原生菜单的手势）。
                // 刚弹出的那一下松手不算。
                if (e.button === 2 && performance.now() - openedAt.current > 250) {
                  e.currentTarget.click();
                }
              }}
              onClick={() => {
                // 先收起再执行：动作可能弹确认框，菜单不该压在它上面。
                onClose();
                entry.onSelect();
              }}
              className={`flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs outline-none disabled:cursor-not-allowed disabled:opacity-40 ${
                entry.danger
                  ? "text-[var(--danger)] focus:bg-[var(--danger)]/10"
                  : "text-ink focus:bg-ink/[0.07]"
              }`}
            >
              {(entry.icon || entry.checked !== undefined) && (
                <span
                  className={`flex w-3 shrink-0 justify-center ${
                    entry.danger ? "opacity-80" : "text-dim"
                  }`}
                >
                  {entry.checked !== undefined ? (
                    entry.checked ? (
                      <Check size={12} className="text-ink" />
                    ) : null
                  ) : (
                    entry.icon
                  )}
                </span>
              )}
              <span className="min-w-0 flex-1 truncate">{entry.label}</span>
              {entry.shortcut && (
                <kbd className="shrink-0 font-sans text-[10px] tracking-wider text-dim/70">
                  {entry.shortcut}
                </kbd>
              )}
            </button>
          ),
        )}
      </div>
    </>
  );
}
