// 设置页共用：异步三态（加载 / 出错 / 空）的统一渲染。
//
// 每个页都写一遍「先判断 loading 再判断 error」的结果，是三个页三种写法、
// 错误样式各不相同。收成一个组件之后，出错时「重试」按钮也只有一个实现。

import type { ReactNode } from "react";

import type { AsyncState } from "../../lib/useAsync";
import { Banner, EmptyState, Loading } from "../../components/ui/Panel";
import { Button } from "../../components/ui/Button";

export function AsyncBody<T>({
  state,
  empty,
  children,
}: {
  state: AsyncState<T>;
  empty: ReactNode;
  children: (data: T) => ReactNode;
}) {
  if (state.loading) return <Loading />;

  if (state.error) {
    return (
      <Banner
        kind="error"
        action={
          <Button size="sm" variant="secondary" onClick={state.reload}>
            重试
          </Button>
        }
      >
        {state.error}
      </Banner>
    );
  }

  const data = state.data;
  if (data === null || (Array.isArray(data) && data.length === 0)) {
    return <EmptyState>{empty}</EmptyState>;
  }
  return <>{children(data)}</>;
}

/** 页面顶部的一句说明。设置项里有些事只能在文件里做，那些地方要把路径写出来。 */
export function PageIntro({ children }: { children: ReactNode }) {
  return <p className="mb-5 text-xs leading-relaxed text-dim">{children}</p>;
}
