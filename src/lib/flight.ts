// 跨页面的衔接动效：点下去的按钮化成一枚卡片，飞进下一页的输入框。
//
// 用裸 DOM 而不是 React：卡片要活过页面切换，而切换时两边的组件树都在卸载和挂载，
// 它不该属于任何一边。全程只动 transform 和 opacity。

const EASE_OUT = "cubic-bezier(0.23, 1, 0.32, 1)";
const FLIGHT_MS = 460;
/** 目的地迟迟没出现（新建会话失败）时，卡片自己退场，不能一直悬在屏幕上。 */
const GIVE_UP_MS = 4000;

type Flight = { outer: HTMLElement; inner: HTMLElement; x: number; y: number; timer: number };

let active: Flight | null = null;

function reducedMotion(): boolean {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function dismiss(f: Flight) {
  window.clearTimeout(f.timer);
  if (active === f) active = null;
  f.outer.animate({ opacity: [1, 0] }, { duration: 120, easing: "ease-out", fill: "forwards" }).onfinish =
    () => f.outer.remove();
}

/** 从 `from` 的位置升起一枚卡片，悬停等 `landFlight`。 */
export function launchFlight(from: Element | null, label: string): void {
  if (active) {
    active.outer.remove();
    window.clearTimeout(active.timer);
    active = null;
  }
  if (!from || !from.isConnected || reducedMotion()) return;

  const outer = document.createElement("div");
  outer.className = "pointer-events-none fixed left-0 top-0 z-[70] will-change-transform";
  outer.setAttribute("aria-hidden", "true");
  const inner = document.createElement("div");
  inner.className =
    "flex max-w-[20rem] items-center gap-1.5 whitespace-nowrap rounded-lg bg-elevated px-2.5 py-1.5 text-xs font-medium text-ink shadow-lift ring-1 ring-tone-blue/50 will-change-transform";
  const dot = document.createElement("span");
  dot.className = "size-1.5 shrink-0 rounded-full bg-tone-blue";
  const text = document.createElement("span");
  text.className = "truncate";
  text.textContent = label;
  inner.append(dot, text);
  outer.append(inner);
  document.body.append(outer);

  const src = from.getBoundingClientRect();
  const own = inner.getBoundingClientRect();
  const x = src.left + src.width / 2 - own.width / 2;
  const y = src.top + src.height / 2 - own.height / 2;
  outer.style.transform = `translate(${x}px, ${y}px)`;
  inner.animate(
    [
      { opacity: 0, transform: "scale(0.8)" },
      { opacity: 1, transform: "scale(1.06)" },
    ],
    { duration: 180, easing: EASE_OUT, fill: "forwards" },
  );

  const flight: Flight = {
    outer,
    inner,
    x,
    y,
    timer: window.setTimeout(() => dismiss(flight), GIVE_UP_MS),
  };
  active = flight;
}

/**
 * 让悬着的卡片落到 `target` 上。必须在 `target` 上屏之前调（layout effect 里）：
 * 目标先藏起来，落地那一刻才接手，看上去是同一个东西。
 *
 * 外层管横向、内层管纵向，两条曲线不同，走出来的是一道弧而不是直线。
 */
export function landFlight(target: Element | null): void {
  const f = active;
  if (!f) return;
  if (!(target instanceof HTMLElement)) {
    dismiss(f);
    return;
  }
  window.clearTimeout(f.timer);

  const to = target.getBoundingClientRect();
  // 用布局尺寸：此刻卡片还放大着，量外框会让落点偏几像素，落地时就跳一下。
  const own = { width: f.inner.offsetWidth, height: f.inner.offsetHeight };
  // 目标比卡片大得多（整个输入框）：落到左上角并融进去，而不是砸在正中间。
  const absorbed = to.width > own.width * 2.5;
  const endX = absorbed ? to.left : to.left + to.width / 2 - own.width / 2;
  const endY = absorbed ? to.top : to.top + to.height / 2 - own.height / 2;
  if (!absorbed) target.style.opacity = "0";

  f.outer.animate(
    [{ transform: `translate(${f.x}px, ${f.y}px)` }, { transform: `translate(${endX}px, ${f.y}px)` }],
    { duration: FLIGHT_MS, easing: "cubic-bezier(0.32, 0.72, 0, 1)", fill: "forwards" },
  );
  const fly = f.inner.animate(
    [
      { transform: "translateY(0) scale(1.06)", opacity: 1 },
      { transform: `translateY(${(endY - f.y) * 0.3}px) scale(1.1)`, opacity: 1, offset: 0.4 },
      {
        transform: `translateY(${endY - f.y}px) scale(${absorbed ? 0.7 : 1})`,
        opacity: absorbed ? 0 : 1,
      },
    ],
    { duration: FLIGHT_MS, easing: "cubic-bezier(0.5, 0, 0.2, 1)", fill: "forwards" },
  );
  fly.onfinish = () => {
    if (active === f) active = null;
    if (absorbed) {
      f.outer.remove();
      return;
    }
    target.style.opacity = "";
    // 落地的一圈涟漪：告诉人「东西到这儿了」。
    target.animate(
      [
        { boxShadow: "0 0 0 0 color-mix(in srgb, var(--tone-blue) 45%, transparent)" },
        { boxShadow: "0 0 0 7px transparent" },
      ],
      { duration: 480, easing: EASE_OUT },
    );
    f.outer.animate({ opacity: [1, 0] }, { duration: 90, fill: "forwards" }).onfinish = () =>
      f.outer.remove();
  };
}

/** 当前页先淡出（比进场快），再换页。新页面自带进场动画（`.carter-view`）。 */
export function leaveMain(next: () => void): void {
  const main = document.querySelector("main");
  if (!main || reducedMotion()) {
    next();
    return;
  }
  const out = main.animate(
    [
      { opacity: 1, transform: "none" },
      { opacity: 0, transform: "scale(0.985)" },
    ],
    { duration: 130, easing: "ease-in", fill: "forwards" },
  );
  out.onfinish = () => {
    next();
    // 等新页面画出第一帧（它从透明开始）再撤掉淡出，否则旧页面会闪回一帧。
    requestAnimationFrame(() => requestAnimationFrame(() => out.cancel()));
  };
}
