// 判断一次按键是不是「输入法正在组字」——是的话就只是上屏/选词，不该当成快捷键。
//
// 两处都要判：
// 1. `isComposing`：Chrome / Firefox 组字期间一直为 true。
// 2. `keyCode === 229`：WebKit 上确认候选那一下，`isComposing` 可能已经翻成 false，
//    但 `keyCode` 仍是 229。只判前者的话，打英文按回车选词就会把半截话发出去。
//
// 接受的形状同时兼容 React 的合成事件（组字状态在 `nativeEvent` 里）与原生事件
// （在事件自己身上），这样 document 级的 keydown 监听也能用同一个判断。

type KeyLike = {
  keyCode?: number;
  isComposing?: boolean;
  nativeEvent?: { isComposing?: boolean };
};

export function isImeComposing(e: KeyLike): boolean {
  return e.nativeEvent?.isComposing === true || e.isComposing === true || e.keyCode === 229;
}
