// 「报文」设置页：抓包占了多少盘，以及一键清空。
//
// 抓包是只追加的：每次请求都存一份完整的原始报文（含当时的全部上下文），
// 不随会话压缩变小，也没有上限 —— 所以要有一处能看见它多大、并能清掉。
// 删会话会带走它自己的那份；这里清的是全部。

import { useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card, ConfirmDialog, Section } from "../../components/ui/Panel";
import { captureUsage, clearCaptures } from "../../lib/api";
import { fmtBytes } from "../../lib/format";
import { toastError, toastOk } from "../../lib/toast";
import { useAsync } from "../../lib/useAsync";
import { AsyncBody, PageIntro } from "./AsyncBody";

export function CapturesPage() {
  const usage = useAsync(captureUsage, []);
  const [confirming, setConfirming] = useState(false);
  const [clearing, setClearing] = useState(false);

  async function clear() {
    setClearing(true);
    try {
      const freed = await clearCaptures();
      toastOk(
        freed.files > 0 ? `已清理 ${fmtBytes(freed.bytes)} 的报文记录` : "没有可清理的报文记录",
      );
      setConfirming(false);
    } catch (e) {
      toastError(e);
    } finally {
      setClearing(false);
      // 成败都重新量一遍：删到一半失败时，界面上的数字也该是真的。
      usage.reload();
    }
  }

  return (
    <>
      <h1 className="mb-4 text-[16px] font-semibold">报文</h1>

      <PageIntro>
        对话右侧的报文面板能查到每次模型调用的
        <strong className="font-medium text-ink">原始 HTTP 报文</strong>
        。这些记录按会话存在 <span className="mono">~/.carter-app/captures/</span>
        ，每次请求都带着当时的完整上下文，所以只增不减 —— 长会话一条就可能有几百 MB。
        删除会话时会一并删掉它的报文记录。
      </PageIntro>

      <Section title="磁盘占用">
        <Card>
          <AsyncBody state={usage} empty={null}>
            {(u) => (
              <div className="flex items-center gap-3">
                <div className="min-w-0">
                  <div className="text-sm text-ink">{fmtBytes(u.bytes)}</div>
                  <div className="mt-0.5 text-[11px] text-dim">
                    {u.files > 0 ? `${u.files} 条会话的报文记录` : "还没有报文记录"}
                  </div>
                </div>
                <div className="ml-auto flex shrink-0 items-center gap-2">
                  <Button size="sm" variant="ghost" onClick={usage.reload}>
                    刷新
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    disabled={u.files === 0}
                    onClick={() => setConfirming(true)}
                  >
                    清理全部
                  </Button>
                </div>
              </div>
            )}
          </AsyncBody>
        </Card>
      </Section>

      {confirming && usage.data && (
        <ConfirmDialog
          title="清理全部报文记录"
          danger
          busy={clearing}
          confirmLabel={`清理 ${fmtBytes(usage.data.bytes)}`}
          message={
            <>
              将删除全部 {usage.data.files} 条会话的报文记录，
              <strong className="font-medium">不可恢复</strong>。
              <br />
              对话历史不受影响，只是这些历史回答的「查看报文」不再有内容；
              正在进行的请求会照常记录。
            </>
          }
          onConfirm={() => void clear()}
          onCancel={() => setConfirming(false)}
        />
      )}
    </>
  );
}
