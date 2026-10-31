// 「变更」设置页：回答末尾那张「本轮变更了 N 个文件」清单的规则。
//
// 三层忽略规则里，只有最后一层是可配的：
// 1. 固定规则（这里展示，不可改）—— 构建产物与依赖目录；
// 2. `.gitignore`（git 仓库里自动生效，不用也不该在这里配）；
// 3. 这里的额外规则 —— 这个项目特有的、不想看见的产物。
//
// 固定规则从后端拿，不在前端抄一份：界面上写着「会忽略」而实际没忽略，比
// 不展示更糟（与工具页同一个道理）。

import { useState } from "react";

import { changeRules, setAppSettings, type AppSettings } from "../../lib/api";
import { toastError } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { useStore } from "../../store";
import { Button } from "../../components/ui/Button";
import { Textarea } from "../../components/ui/Input";
import { Card, Section } from "../../components/ui/Panel";
import { AsyncBody, PageIntro } from "./AsyncBody";

export function ChangesPage() {
  const settings = useStore((s) => s.settings);
  const setSettings = useStore((s) => s.setSettings);
  const rules = useAsync(changeRules, []);
  // `null` = 跟着已保存的设置走；用户一编辑就变成草稿。保存成功后回到 `null`，
  // 免得界面上留着一份「看起来已保存、其实不是」的文本。
  const [draft, setDraft] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  if (!settings) return null;

  const saved = settings.changes_ignore.join("\n");
  const value = draft ?? saved;
  const dirty = value !== saved;

  async function save() {
    const patterns = value
      .split("\n")
      .map((s) => s.trim())
      .filter((s) => s !== "" && !s.startsWith("#"));
    const next: AppSettings = { ...settings!, changes_ignore: patterns };
    setSettings(next);
    setSaving(true);
    try {
      await setAppSettings(next);
      setDraft(null);
    } catch (e) {
      // 存不下就回滚，否则用户以为规则生效了，重启又回来。
      setSettings(settings!);
      toastError(e);
    } finally {
      setSaving(false);
    }
  }

  return (
    <>
      <h1 className="mb-4 text-base font-semibold">变更</h1>

      <PageIntro>
        对话里每次回答结尾会列出「本轮变更了哪些文件」。这份清单来自对工作目录的
        <strong className="font-medium text-ink">文件系统监听</strong>
        ，不是解析模型调用的参数 —— 所以 <code className="mono">bash</code>、脚本、
        子进程的写入都算在内。下面配置的是清单
        <strong className="font-medium text-ink">不看哪些文件</strong>
        ；点开清单里的文件还能看到本轮 diff。
      </PageIntro>

      <Section title="额外忽略规则">
        <Card>
          <div className="mb-2 text-xs leading-relaxed text-dim">
            一行一条，相对会话的工作目录。含 <code className="mono">/</code> 的按相对路径
            匹配（<code className="mono">dist/**</code>）；不含 <code className="mono">/</code> 的
            按任意层级的名字匹配（<code className="mono">*.log</code>、
            <code className="mono">__generated__</code>）。以 <code className="mono">#</code>{" "}
            开头的行是注释。
          </div>
          <Textarea
            mono
            rows={8}
            value={value}
            onChange={(e) => setDraft(e.target.value)}
            placeholder={"# 例如\n*.snap\ncoverage/**\nfixtures/large"}
          />
          <div className="mt-3 flex items-center justify-end gap-2">
            {dirty && <span className="text-[11px] text-dim">未保存</span>}
            <Button
              variant="primary"
              size="sm"
              disabled={!dirty || saving}
              onClick={() => void save()}
            >
              {saving ? "保存中…" : "保存"}
            </Button>
          </div>
        </Card>
      </Section>

      <Section title="始终忽略">
        <Card>
          <div className="mb-2 text-xs leading-relaxed text-dim">
            这些是固定规则，不需要也不能在这里配置。构建产物与依赖目录几乎总是噪音，
            让每个项目手写一遍没有意义。
          </div>
          <AsyncBody state={rules} empty={null}>
            {(r) => (
              <div className="space-y-2">
                <div>
                  <div className="mb-1 text-[11px] text-dim">目录（任意层级）</div>
                  <div className="flex flex-wrap gap-1">
                    {r.dirs.map((d) => (
                      <span key={d} className="mono rounded border border-edge px-1.5 py-0.5 text-[11px] text-ink">
                        {d}
                      </span>
                    ))}
                  </div>
                </div>
                {r.files.length > 0 && (
                  <div>
                    <div className="mb-1 text-[11px] text-dim">文件名</div>
                    <div className="flex flex-wrap gap-1">
                      {r.files.map((f) => (
                        <span key={f} className="mono rounded border border-edge px-1.5 py-0.5 text-[11px] text-ink">
                          {f}
                        </span>
                      ))}
                    </div>
                  </div>
                )}
              </div>
            )}
          </AsyncBody>
        </Card>
      </Section>

      <Section title="gitignore 与本轮 diff">
        <Card>
          <div className="space-y-2 text-xs leading-relaxed text-dim">
            <p>
              工作目录在 git 仓库里时，清单会尊重{" "}
              <code className="mono">.gitignore</code>（含嵌套的与全局 excludes）——
              装了 git 就用 <code className="mono">git check-ignore</code> 判断，
              没装则用进程内的同一套 ripgrep 引擎，两种情况下清单一致。
            </p>
            <p>
              预览里的
              <strong className="font-medium text-ink">「本轮改动」</strong>
              也来自 git：每轮开始时用 <code className="mono">git stash create</code>{" "}
              给当前工作区拍一张快照（只写对象，不动工作区），结束时用{" "}
              <code className="mono">git diff</code> 算这一轮真正改了什么。
            </p>
            <p>
              不是 git 仓库、或者本机没装 git 时，清单与文件预览仍然可用，但
              <strong className="font-medium text-ink">没有 diff</strong> ——
              没有基线就还原不了改动前的内容。
            </p>
          </div>
        </Card>
      </Section>
    </>
  );
}
