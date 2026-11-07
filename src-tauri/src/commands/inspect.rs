use crate::prelude::*;

// ---------------------------------------------------------------------------
// 报文检视
// ---------------------------------------------------------------------------

/// 报文列表项。只给概览 —— 完整记录里带着几 MB 的 SSE 帧，
/// 列表里全带上会让界面一开就卡。
#[derive(Serialize)]
pub(crate) struct ExchangeSummary {
    id: String,
    ts: u64,
    /// 「第几轮 / 第几次调用 / 干什么用的」，供界面排序与分组。
    turn: Option<u32>,
    round: Option<u32>,
    purpose: Option<String>,
    method: String,
    url: String,
    /// 上游渠道（`[providers.X]` 的 X）与本地模型引用。同一个模型可能配在多个
    /// 渠道上，列表里必须能一眼分出这条走的是哪个。
    channel: Option<String>,
    model: Option<String>,
    status: Option<u16>,
    ttfb_ms: Option<u128>,
    total_ms: Option<u128>,
    frame_count: usize,
    /// 还在进行中（请求已发出、响应没收完）。界面据此显示「进行中」而不是「—」。
    in_flight: bool,
}

pub(crate) fn purpose_str(p: carter::provider::RequestPurpose) -> String {
    use carter::provider::RequestPurpose as P;
    match p {
        P::MainTurn => "main_turn",
        P::Compaction => "compaction",
        P::Title => "title",
        P::Subagent => "subagent",
    }
    .to_string()
}

pub(crate) fn summarize(r: &carter_app_core::ExchangeBrief, in_flight: bool) -> ExchangeSummary {
    ExchangeSummary {
        id: r.id.clone(),
        ts: r.ts,
        turn: r.meta.as_ref().map(|m| m.turn),
        round: r.meta.as_ref().map(|m| m.round),
        // 用 snake_case，与 `ExchangeRecord.meta.purpose` 的 serde 表示保持一致 ——
        // 两处拼法不同会让前端不得不写一个归一化器，那是纯负担。
        purpose: r.meta.as_ref().map(|m| purpose_str(m.purpose)),
        method: r.method.clone(),
        url: r.url.clone(),
        channel: r.channel.clone(),
        model: r.model.clone(),
        status: r.status,
        ttfb_ms: r.ttfb_ms,
        total_ms: r.total_ms,
        frame_count: r.frame_count,
        in_flight,
    }
}

#[tauri::command]
pub(crate) fn list_exchanges(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
) -> Vec<ExchangeSummary> {
    // 排序、合并进行中、去重都在 `CaptureStore::listing` 里 —— 那是业务口径，
    // 命令层只做投影。
    shell
        .capture
        .listing(&session_id)
        .iter()
        .map(|(r, in_flight)| summarize(r, *in_flight))
        .collect()
}

/// 取一条交换的**完整**原始报文。
#[tauri::command]
pub(crate) fn get_exchange(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
    exchange_id: String,
) -> Option<carter_app_core::ExchangeRecord> {
    shell.capture.get(&session_id, &exchange_id)
}

/// 把一条交换**解回 IR**（报文面板「可视化」那一页）。
///
/// 解码放在后端而不是前端：那是三个协议各自的 wire 语法，用 `llm_adapt` 现成的
/// codec 解一遍就够，前端再写一份必然与真实链路走样 —— 而「报文面板显示的」与
/// 「模型实际收到/说出的」不是同一份东西，这个面板就没有意义了。
///
/// 解码失败**不是命令失败**：`InspectExchange` 里带着原因，界面据此降级到「原始」页。
#[tauri::command]
pub(crate) fn inspect_exchange(
    shell: State<'_, Arc<AppShell>>,
    session_id: String,
    exchange_id: String,
) -> Option<carter_app_core::inspect::InspectedExchange> {
    shell
        .capture
        .get(&session_id, &exchange_id)
        .map(|rec| carter_app_core::inspect::inspect(&rec))
}
