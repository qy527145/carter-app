// 团队页的成员上下文：名单、本机是谁、怎么认人。
//
// 放进 context 而不是层层传参：任务卡、抽屉、动态、选人框都要「这个 id 是谁」，
// 而名单只该拉一次、改了（有人加入、改了称呼）一起刷新。

import { createContext, type ReactNode, useCallback, useContext, useEffect, useState } from "react";

import { type TeamPeople, teamPeople } from "../../lib/api";

type PeopleApi = {
  people: TeamPeople | null;
  reload: () => Promise<void>;
};

const Ctx = createContext<PeopleApi>({ people: null, reload: async () => {} });

export function PeopleProvider({
  cwd,
  nonce,
  children,
}: {
  cwd: string;
  nonce: number;
  children: ReactNode;
}) {
  const [people, setPeople] = useState<TeamPeople | null>(null);
  const reload = useCallback(async () => {
    if (!cwd) return;
    try {
      setPeople(await teamPeople(cwd));
    } catch {
      // 没有团队目录时团队页自己会显示初始化面板，这里不再弹一次错。
      setPeople(null);
    }
  }, [cwd]);
  // 换目录时先把旧名单清掉，别让人看到上一条目录的成员。
  // biome-ignore lint/correctness/useExhaustiveDependencies: cwd 是「该清了」的触发信号，正文只 setPeople
  useEffect(() => {
    setPeople(null);
  }, [cwd]);
  // 名单跟着 cwd 变；nonce 是团队页重读全景时的触发器，正文不读它。
  // biome-ignore lint/correctness/useExhaustiveDependencies: nonce 是触发器，reload 自己已经绑了 cwd
  useEffect(() => {
    void reload();
  }, [reload, nonce]);
  return <Ctx.Provider value={{ people, reload }}>{children}</Ctx.Provider>;
}

export function usePeople(): PeopleApi {
  return useContext(Ctx);
}
