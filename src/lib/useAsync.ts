// 只拉一次数据的小 hook。
//
// `load` 每次渲染都是新函数，所以不放依赖 —— 靠 `deps + nonce` 控制重取。
// 另外自动把 `configNonce` 算进去：界面里改完配置（写回 config.toml 并热重载）
// 会后端会广播 `config_changed`，各页据此重拉。少了这一条，改完渠道再点开「模型」
// 页会看到一份过期数据 —— 那种自相矛盾的界面比不刷新更糟。

import { useEffect, useState } from "react";

import { useStore } from "../store";

export type AsyncState<T> = {
  data: T | null;
  error: string | null;
  loading: boolean;
  reload: () => void;
};

export function useAsync<T>(load: () => Promise<T>, deps: unknown[] = []): AsyncState<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);
  const configNonce = useStore((s) => s.configNonce);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    load()
      .then((d) => {
        if (!cancelled) {
          setData(d);
          setLoading(false);
        }
      })
      .catch((e) => {
        if (!cancelled) {
          setError(typeof e === "string" ? e : String(e));
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce, configNonce]);

  return { data, error, loading, reload: () => setNonce((n) => n + 1) };
}
