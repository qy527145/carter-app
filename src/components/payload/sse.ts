// SSE 事件的切分与识别。
//
// 只做**最小识别**，不引入完整解析器：解析器会顺手丢掉注释行（`: keep-alive`
// 心跳）和不认识的字段，而那些恰恰是排障时要看的 —— 所以原始块原样留在
// `SseEvent.raw` 里，界面在两处都能给人看。
//
// 事件按**空行**切，这与 SSE 规范一致（也是后端 `inspect.rs` 切帧的判据）。

/** 一条 SSE 事件。 */
export type SseEvent = {
  /** 在整个流里从 0 起的序号（过滤之后仍然用它，保证「第几个事件」是全局的）。 */
  seq: number;
  name: string;
  /** `data:` 行拼起来的内容（多行 data 用换行连）。 */
  data: string;
  /** 原始事件块，含 `event:` / `data:` 行 —— 排障时要逐字节对照。 */
  raw: string;
  /** 这一片真正新增的内容；认不出就是 `null`（比如用量帧、控制帧）。 */
  fragment: string | null;
};

export type SseParse = {
  events: SseEvent[];
  /**
   * 没有 `data:` 的块（心跳、纯注释）数量。
   *
   * 不列进事件列表 —— 一条长回答里心跳几十条，混进去只会淹没真正的事件；
   * 但要报出这个数，否则"上游到底发没发保活"就无从判断。
   */
  emptyBlocks: number;
};

export function parseSse(raw: string): SseParse {
  const events: SseEvent[] = [];
  let emptyBlocks = 0;
  if (!raw) return { events, emptyBlocks };

  for (const block of raw.split(/\r?\n\r?\n/)) {
    if (!block.trim()) continue;
    let name = "message";
    const data: string[] = [];
    for (const line of block.split(/\r?\n/)) {
      if (line.startsWith(":")) continue; // 注释 / 心跳
      const idx = line.indexOf(":");
      const field = idx === -1 ? line : line.slice(0, idx);
      let value = idx === -1 ? "" : line.slice(idx + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (field === "event") name = value;
      else if (field === "data") data.push(value);
    }
    const joined = data.join("\n");
    // 没有 data 的块不是「模型说了什么」，只是保活。
    if (!joined) {
      emptyBlocks += 1;
      continue;
    }
    events.push({
      seq: events.length,
      name,
      data: joined,
      raw: block,
      fragment: fragmentOf(joined),
    });
  }
  return { events, emptyBlocks };
}

/**
 * 从事件 data 里取「这一片新增的内容」。
 *
 * 只认几种最常见的位置（Anthropic 的 `delta.text` / `delta.thinking` / `delta.partial_json`、
 * OpenAI 的 `choices[0].delta.content`、Responses 的顶层 `delta`）—— 认不出就不硬猜，
 * 事件详情里的 JSON 照样能看到全部字段。
 */
function fragmentOf(data: string): string | null {
  let v: unknown;
  try {
    v = JSON.parse(data);
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null) return null;
  const obj = v as Record<string, unknown>;
  const delta = obj.delta as Record<string, unknown> | undefined;
  const fromDelta =
    str(delta?.text) ?? str(delta?.thinking) ?? str(delta?.partial_json) ?? str(obj.delta);
  if (fromDelta) return fromDelta;
  const choices = Array.isArray(obj.choices) ? obj.choices : [];
  const first = choices[0] as Record<string, unknown> | undefined;
  const d = first?.delta as Record<string, unknown> | undefined;
  return str(d?.content) ?? str(d?.reasoning_content) ?? null;
}

function str(v: unknown): string | null {
  return typeof v === "string" && v.length > 0 ? v : null;
}

/** 事件名 → 次数，按次数降序。界面上拿来当筛选用。 */
export function kindsOf(events: SseEvent[]): { name: string; count: number }[] {
  const map = new Map<string, number>();
  for (const e of events) map.set(e.name, (map.get(e.name) ?? 0) + 1);
  return [...map.entries()]
    .map(([name, count]) => ({ name, count }))
    .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

/**
 * 列表行上的一句摘要：优先给「这一片新增的内容」，没有就给 data 压成一行。
 *
 * 一整条流里绝大多数事件都是「一个 token 一片」，摘要这一列让人一眼看出
 * 模型正在往外吐什么 —— 而不是一屏 `message_delta`。
 */
export function previewOf(ev: SseEvent): string {
  const text = ev.fragment ?? compactJson(ev.data) ?? ev.data;
  return text.replace(/\s+/g, " ").trim();
}

/** JSON 压成一行（保留字段，只是不缩进）。不是 JSON 返回 null。 */
function compactJson(data: string): string | null {
  try {
    return JSON.stringify(JSON.parse(data));
  } catch {
    return null;
  }
}