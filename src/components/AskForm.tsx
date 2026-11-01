// 模型提问的表单：一次问一组，支持多选。
//
// ## 为什么是一次一组
//
// 之前是一问一答：模型问一个问题，用户点一下，模型再问下一个。三个问题就是三个
// 完整的模型往返 —— 慢、贵，而且用户看不到全貌就没法做取舍（「要 PostgreSQL 吗」
// 的答案取决于「部署在哪」）。一次把问题摆出来，用户能看见它们之间的关系。
//
// ## 为什么提交前不自动清空
//
// 提交后前端**立即**收起表单（响应要快），但发送失败时要把表单放回去 ——
// 否则用户刚填的东西没了，还得重新点一遍。所以答案在提交时先留一份。

import { useState } from "react";
import { Check, ChevronDown, Info, LoaderCircle } from "lucide-react";

import { sendCommand, type AskQuestion } from "../lib/api";
import { useStore } from "../store";

export function AskForm() {
  const form = useStore((s) => s.pendingForm);
  const clear = useStore((s) => s.clearPendingForm);
  const setNotice = useStore((s) => s.setNotice);
  const [submitting, setSubmitting] = useState(false);

  if (!form) return null;

  const submit = (answers: string[][]) => {
    const sid = useStore.getState().sessionId;
    setSubmitting(true);
    // 先收起表单：模型等着，响应要快。
    clear();
    if (!sid) return;
    sendCommand(sid, { type: "answer_form", id: form.id, answers })
      .catch((e) => {
        setNotice(String(e));
      })
      .finally(() => setSubmitting(false));
  };

  return (
    <div className="px-4 pb-2">
      <div className="mx-auto max-w-3xl rounded-xl bg-elevated p-3 shadow-soft">
        <div className="mb-2.5 flex items-center gap-2 text-xs text-dim">
          {submitting ? (
            <LoaderCircle size={13} className="animate-spin" />
          ) : (
            <Info size={13} />
          )}
          模型在等你回答
          {form.questions.length > 1 && (
            <span className="text-dim/70">（{form.questions.length} 个问题，可一次答完）</span>
          )}
        </div>
        <QuestionSet key={form.id} questions={form.questions} onSubmit={submit} />
      </div>
    </div>
  );
}

function QuestionSet({
  questions,
  onSubmit,
}: {
  questions: AskQuestion[];
  onSubmit: (answers: string[][]) => void;
}) {
  // 一题一组答案。`key={form.id}` 保证新一次提问会重置这份状态。
  const [answers, setAnswers] = useState<string[][]>(() => questions.map(() => []));
  const [free, setFree] = useState<string[]>(() => questions.map(() => ""));

  const toggle = (qi: number, label: string) => {
    const q = questions[qi];
    setAnswers((prev) =>
      prev.map((a, i) => {
        if (i !== qi) return a;
        if (q.multi_select) {
          return a.includes(label) ? a.filter((x) => x !== label) : [...a, label];
        }
        return a.includes(label) ? [] : [label];
      }),
    );
  };

  const setFreeAt = (qi: number, v: string) => {
    setFree((prev) => prev.map((x, i) => (i === qi ? v : x)));
  };

  /** 把每一题的「选项 + 自由输入」合成后端要的形状。 */
  const collect = (): string[][] =>
    answers.map((a, i) => {
      const f = free[i].trim();
      return f ? [...a, f] : a;
    });

  const anyAnswer = answers.some((a, i) => a.length > 0 || free[i].trim() !== "");

  return (
    <>
      <div className="space-y-3">
        {questions.map((q, qi) => (
          <div key={qi}>
            <div className="mb-1.5 flex items-baseline gap-2">
              {q.header && (
                <span className="shrink-0 rounded bg-bubble px-1.5 py-0.5 text-[10px] text-dim">
                  {q.header}
                </span>
              )}
              <span className="text-sm text-ink">{q.question}</span>
              {q.multi_select && (
                <span className="shrink-0 text-[10px] text-dim">可多选</span>
              )}
            </div>

            {q.options.length > 0 && (
              <div className="mb-1.5 flex flex-wrap gap-1.5">
                {q.options.map((o) => {
                  const picked = answers[qi].includes(o.label);
                  return (
                    <button
                      key={o.label}
                      onClick={() => toggle(qi, o.label)}
                      title={o.description ?? undefined}
                      className={`inline-flex max-w-full items-center gap-1.5 rounded-md border px-2.5 py-1.5 text-left text-xs transition-colors ${
                        picked
                          ? "border-accent bg-accent text-[var(--accent-contrast)]"
                          : "border-edge text-ink hover:border-dim"
                      }`}
                    >
                      {picked && <Check size={11} className="shrink-0" />}
                      <span className="min-w-0">
                        <span className="block truncate">{o.label}</span>
                        {o.description && (
                          <span
                            className={`block truncate text-[10px] ${
                              picked ? "opacity-70" : "text-dim"
                            }`}
                          >
                            {o.description}
                          </span>
                        )}
                      </span>
                    </button>
                  );
                })}
              </div>
            )}

            <input
              value={free[qi]}
              onChange={(e) => setFreeAt(qi, e.target.value)}
              placeholder={q.options.length > 0 ? "或直接输入回答…" : "输入回答…"}
              className="carter-field w-full rounded-md border border-edge bg-base px-3 py-1.5 text-xs text-ink outline-none transition-[border-color,box-shadow] placeholder:text-dim"
            />
          </div>
        ))}
      </div>

      <div className="mt-3 flex items-center gap-2">
        <button
          onClick={() => onSubmit(collect())}
          disabled={!anyAnswer}
          className="inline-flex items-center gap-1.5 rounded-md bg-accent px-3 py-1.5 text-sm font-medium text-[var(--accent-contrast)] transition-opacity hover:opacity-90 disabled:opacity-40"
        >
          提交
        </button>
        {/* 全部跳过也是合法选择 —— 模型那边会拿到「用户跳过了」并继续。 */}
        <button
          onClick={() => onSubmit(questions.map(() => []))}
          className="rounded-md bg-ink/5 px-3 py-1.5 text-sm text-dim transition-colors hover:bg-ink/10 hover:text-ink"
        >
          跳过
        </button>
        {questions.length > 1 && (
          <span className="ml-auto inline-flex items-center gap-1 text-[11px] text-dim">
            <ChevronDown size={11} />
            答完一次提交，模型不用再等一轮
          </span>
        )}
      </div>
    </>
  );
}
