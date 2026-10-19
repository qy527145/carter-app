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

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
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
      onSelect: () => void;
    }
  | "separator";

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
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  // 先量真实尺寸再定位：菜单在光标处弹出是正常的，弹到窗口外面就点不到了。
  // 用 layout effect 是为了在绘制前算好，不会先闪一下再跳。
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const margin = 8;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const left = align === "end" ? x - w : x;
    setPos({
      left: Math.max(margin, Math.min(left, window.innerWidth - w - margin)),
      top: Math.max(margin, Math.min(y, window.innerHeight - h - margin)),
    });
  }, [x, y, align]);

  // 焦点落到菜单上，方向键 / Esc 才有地方可去。
  useEffect(() => {
    ref.current?.focus();
  }, []);

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
    else if (e.key === "ArrowUp") next = cur < 0 ? buttons.length - 1 : (cur - 1 + buttons.length) % buttons.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = buttons.length - 1;
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
          e.preventDefault();
          onClose();
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
          visibility: pos ? "visible" : "hidden",
        }}
        className="carter-menu fixed z-50 min-w-44 rounded-lg border border-edge bg-card py-1 shadow-lift outline-none"
      >
        {entries.map((entry, i) =>
          entry === "separator" ? (
            <div key={i} className="my-1 h-px bg-edge" />
          ) : (
            <button
              key={i}
              type="button"
              role="menuitem"
              disabled={entry.disabled}
              onClick={() => {
                // 先收起再执行：动作可能弹确认框，菜单不该压在它上面。
                onClose();
                entry.onSelect();
              }}
              className={`flex w-full items-center gap-2 px-2.5 py-1.5 text-left text-xs transition-colors outline-none hover:bg-base focus:bg-base disabled:cursor-not-allowed disabled:opacity-40 ${
                entry.danger
                  ? "text-dim hover:text-[var(--danger)] focus:text-[var(--danger)]"
                  : "text-ink"
              }`}
            >
              {entry.icon && (
                <span
                  className={`shrink-0 ${
                    entry.danger
                      ? "text-[var(--danger)] opacity-70"
                      : "text-dim"
                  }`}
                >
                  {entry.icon}
                </span>
              )}
              <span className="min-w-0 flex-1 truncate">{entry.label}</span>
              {entry.shortcut && (
                <span className="shrink-0 text-[10px] text-dim/70">{entry.shortcut}</span>
              )}
            </button>
          ),
        )}
      </div>
    </>
  );
}
