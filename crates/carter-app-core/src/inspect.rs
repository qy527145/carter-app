//! 抓到的报文 → **可读的语义视图**（报文面板「可视化」那一页的数据）。
//!
//! ## 为什么不把字节直接摊给界面
//!
//! 一次流式回答抓下来是一百多个网络字节块、几百个 SSE 事件，全是
//! `{"choices":[{"delta":{"content":"好"}}]}` 这种一片一个字的碎片。摊开来看不出
//! 「模型答了什么」，只能看出「传输有多碎」—— 那是传输层的问题，不是用户的问题。
//!
//! 所以这里**用与真实链路同一套 codec 把字节解回 IR**（`llm_adapt` 的
//! `Codec::decode_request` / `Codec::decode_response` / `new_stream_decoder` +
//! `BlockAssembler`），界面按语义渲染：系统提示词、工具清单、对话上下文、回答与
//! 思考、token 消耗。原始字节仍然在「原始」页 —— 排障时偶尔要拿它比对上游。
//!
//! 用**同一个 codec** 还有个副作用是好的：界面显示的就是模型实际收到/说出的话，
//! 而不是我们对它的一层猜测。协议层要是把哪个字段丢了（比如 `reasoning_content`），
//! 这里也会一起没有 —— 那正是应该被发现的事。
//!
//! ## 解不出来怎么办
//!
//! 网关换了形状、自建镜像改了 JSON、抓包截了一半 —— 都可能解不出来。这时**不猜**：
//! 把原因放进 `decode_error`，界面回到「原始」页并说明为什么。半截的语义视图比
//! 原始字节更骗人。
//!
//! ## 这些类型是 IPC 契约
//!
//! `protocol.rs` 写着「只有它定义跨进程边界的形状」，这里是它承认的例外：
//! 这些类型是 [`crate::capture::ExchangeRecord`] 的投影，跟着记录一起走，
//! 放在解码的地方才读得懂。前端镜像在 `src/lib/api.ts`。

use serde::Serialize;

use llm_adapt_core::{
    Block, BlockAssembler, Bytes, FinishReason, ProtocolId, ProtocolRegistry, SseFrame, SseMapper,
    ToolChoice, WireRequest, WireResponse,
};

use crate::capture::ExchangeRecord;
use crate::protocol::UsageView;
use crate::view::{self, BlockView, MessageView};

/// 一次交换解出来的全部东西。
#[derive(Debug, Clone, Serialize)]
pub struct InspectedExchange {
    /// 实际用的协议（记录里没有时按 URL 兜底猜，猜不到为空串）。
    pub protocol: String,
    /// 协议是不是按 URL 猜的 —— 猜的要告诉界面，别让人以为这是权威结论。
    pub protocol_guessed: bool,
    pub request: Option<InspectedRequest>,
    pub response: Option<InspectedResponse>,
    pub stream: StreamStats,
    /// 解码失败的原因（有值时 `request` / `response` 可能为空）。
    pub decode_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectedRequest {
    pub model: String,
    pub stream: bool,
    pub params: Vec<Param>,
    /// 系统提示词（多个块拼起来，界面上当一段看）。
    pub system: String,
    /// 对话上下文。**与历史消息同一个形状**（`view.rs`），所以渲染可以共用一套。
    pub messages: Vec<MessageView>,
    pub tools: Vec<ToolView>,
    /// 没建模的顶层字段名。值本身在「格式化」页里看 —— 这里只说明「还有别的」。
    pub extra_keys: Vec<String>,
}

/// 一个请求参数，摊成键值给人看。
#[derive(Debug, Clone, Serialize)]
pub struct Param {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolView {
    pub name: String,
    pub description: Option<String>,
    /// JSON Schema 的美化文本。不解析成对象 —— 界面只需要把它当一段 JSON 摊开。
    pub schema: String,
    /// 厂商自己执行的工具（`web_search` 之类），不是我们能跑的函数。
    pub server_side: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct InspectedResponse {
    /// 按事件流里的**原始顺序**排的内容块：思考 → 回答 → 工具调用。
    /// 按类型分桶重排会把「模型先想、再答、最后决定调工具」这个因果顺序抹掉。
    pub blocks: Vec<BlockView>,
    /// 收尾原因，已经翻成人话。
    pub finish: Option<String>,
    pub usage: Option<UsageView>,
    pub extra_keys: Vec<String>,
}

/// 流式响应有多碎。**这一页只有统计，没有逐帧** —— 逐帧在「原始」页。
#[derive(Debug, Clone, Serialize)]
pub struct StreamStats {
    pub streaming: bool,
    /// 网络字节块数（`stream_frames` 的长度）。
    pub frames: usize,
    /// 拼起来之后能切出多少个 SSE 事件。
    pub events: usize,
    /// 事件名 → 次数，按次数降序。Anthropic 有 `message_delta` 这类命名事件，
    /// OpenAI 只有匿名的 `data:`（记作 `message`）。
    pub event_kinds: Vec<EventKind>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventKind {
    pub name: String,
    pub count: usize,
}

/// 解一条交换。**不返回 Result**：解不出来也是一种结果（带上原因），
/// 让界面统一处理，不必区分「命令失败」与「报文解不开」两种红灯。
pub fn inspect(rec: &ExchangeRecord) -> InspectedExchange {
    let (protocol, guessed) = resolve_protocol(rec);
    let mut out = InspectedExchange {
        protocol: protocol.clone(),
        protocol_guessed: guessed,
        request: None,
        response: None,
        stream: stats(rec),
        decode_error: None,
    };

    let codec = match codec_for(&protocol) {
        Some(c) => c,
        None => {
            out.decode_error = Some(format!("认不出这是哪个协议：{protocol:?}"));
            return out;
        }
    };

    // 请求体：解不出不影响响应那一半 —— 两条各自尽力。
    match codec.decode_request(rec.request_body.as_bytes()) {
        Ok(req) => out.request = Some(request_view(&req)),
        Err(e) => out.decode_error = Some(format!("请求体解不开：{e}")),
    }

    match response_view(codec.as_ref(), rec) {
        Ok(v) => out.response = v,
        Err(e) => {
            let msg = format!("响应解不开：{e}");
            out.decode_error = Some(match out.decode_error.take() {
                Some(prev) => format!("{prev}；{msg}"),
                None => msg,
            });
        }
    }

    out
}

/// 协议从记录里取；老记录没这个字段时按 URL 猜一把（并标明是猜的）。
fn resolve_protocol(rec: &ExchangeRecord) -> (String, bool) {
    if let Some(p) = rec.protocol.as_ref().filter(|p| !p.is_empty()) {
        return (p.clone(), false);
    }
    (sniff_protocol(&rec.url).unwrap_or_default(), true)
}

/// 按 URL 猜协议。**只在记录里没有协议时用**（2026-10 之前抓的老报文）。
///
/// 三个内置协议各自的端点路径在 `llm_adapt` 里是固定的，所以常见情形猜得准；
/// 但渠道可以用 `path` 覆盖路径（自建镜像很常见），那时就会猜错 ——
/// 所以猜出来的结果要在界面上标明。
fn sniff_protocol(url: &str) -> Option<String> {
    let url = url.to_ascii_lowercase();
    if url.contains("/chat/completions") {
        Some(ProtocolId::OPENAI_CHAT.to_string())
    } else if url.contains("/responses") {
        Some(ProtocolId::OPENAI_RESPONSES.to_string())
    } else if url.contains("/messages") {
        Some(ProtocolId::ANTHROPIC.to_string())
    } else {
        None
    }
}

fn codec_for(protocol: &str) -> Option<std::sync::Arc<dyn llm_adapt_core::Codec>> {
    let registry = ProtocolRegistry::new();
    llm_adapt_core::protocol::register_defaults(&registry);
    registry.get(&ProtocolId::new(protocol)).ok()
}

fn request_view(req: &WireRequest) -> InspectedRequest {
    let system = req
        .system
        .iter()
        .filter_map(|b| match b {
            Block::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    InspectedRequest {
        model: req.model.clone(),
        stream: req.stream,
        params: params(req),
        system,
        messages: view::to_views(&req.messages),
        tools: req
            .tools
            .iter()
            .map(|t| ToolView {
                name: t.name.clone(),
                description: t.description.clone(),
                schema: serde_json::to_string_pretty(&t.input_schema).unwrap_or_default(),
                server_side: !matches!(t.kind, llm_adapt_core::ToolKind::Function),
            })
            .collect(),
        extra_keys: req.extra.keys().cloned().collect(),
    }
}

/// 把请求参数摊成人能读的键值。**空值不列** —— 一屏 `temperature: null` 没有意义。
fn params(req: &WireRequest) -> Vec<Param> {
    let mut out = Vec::new();
    let mut push = |k: &str, v: String| {
        out.push(Param {
            key: k.to_string(),
            value: v,
        })
    };

    push("model", req.model.clone());
    push("stream", req.stream.to_string());
    if let Some(n) = req.max_tokens {
        push("max_tokens", n.to_string());
    }
    if let Some(t) = req.temperature {
        push("temperature", t.to_string());
    }
    if let Some(p) = req.top_p {
        push("top_p", p.to_string());
    }
    if !req.stop.is_empty() {
        push("stop", req.stop.join(" | "));
    }
    if let Some(tc) = describe_tool_choice(&req.tool_choice) {
        push("tool_choice", tc);
    }
    if let Some(t) = &req.thinking {
        if let Some(b) = t.budget_tokens {
            push("thinking budget", b.to_string());
        }
        if let Some(e) = &t.effort {
            push("reasoning effort", e.clone());
        }
    }
    push("tools", req.tools.len().to_string());
    push("messages", req.messages.len().to_string());
    out
}

/// `tool_choice` 说人话，比 `{"type":"tool","name":"bash"}` 直观。
fn describe_tool_choice(tc: &ToolChoice) -> Option<String> {
    match tc {
        // 默认值不列 —— 它出现在每一次请求里，列出来只是噪声。
        ToolChoice::Auto => None,
        ToolChoice::None => Some("禁止调用".to_string()),
        ToolChoice::Required => Some("必须调用".to_string()),
        ToolChoice::Specific { name } => Some(format!("必须调用 {name}")),
    }
}

fn response_view(
    codec: &dyn llm_adapt_core::Codec,
    rec: &ExchangeRecord,
) -> Result<Option<InspectedResponse>, String> {
    if !rec.stream_frames.is_empty() {
        let blocks = stream_blocks(codec, &rec.stream_frames.join(""))?;
        return Ok(Some(InspectedResponse {
            blocks: blocks.0,
            finish: blocks.1,
            usage: blocks.2,
            // 流式体积太大，逐字段摊开没有意义 —— 要看细节去「原始」页。
            extra_keys: Vec::new(),
        }));
    }
    if rec.response_body.trim().is_empty() {
        // 一条被中断的交换：还没收到响应体就断了。没什么可解的。
        return Ok(None);
    }
    let resp = codec
        .decode_response(Bytes::from(rec.response_body.clone()))
        .map_err(|e| e.to_string())?;
    Ok(Some(response_from(&resp)))
}

fn response_from(resp: &WireResponse) -> InspectedResponse {
    InspectedResponse {
        blocks: blocks_view(&resp.content),
        finish: finish_label(&resp.finish_reason),
        usage: (!resp.usage.is_empty()).then(|| UsageView::from(&resp.usage)),
        extra_keys: resp.extra.keys().cloned().collect(),
    }
}

/// 流式响应：拼字节 → 切 SSE 帧 → 喂 mapper → 装配回内容块。
///
/// 返回 `(块, 收尾原因, 用量)`。用量与收尾原因由装配器记着 —— 它们是**流级**的
/// 信息，不属于任何一块。
type StreamedResponse = (Vec<BlockView>, Option<String>, Option<UsageView>);

fn stream_blocks(codec: &dyn llm_adapt_core::Codec, raw: &str) -> Result<StreamedResponse, String> {
    let mut mapper = codec.new_stream_decoder();
    let mut asm = BlockAssembler::new();
    for frame in split_frames(raw) {
        let outcome = mapper.map(frame).map_err(|e| e.to_string())?;
        for c in &outcome.chunks {
            asm.push(c);
        }
        // 终止帧（OpenAI 的 `[DONE]`、Anthropic 的 `message_stop`）之后不该再有内容；
        // 但**用量帧常常排在它后面**，所以这里不 break，继续吃完剩下的帧。
    }
    Ok((
        blocks_view(&asm.blocks()),
        finish_label(asm.finish_reason().unwrap_or(&FinishReason::Stop)),
        (!asm.usage().is_empty()).then(|| UsageView::from(asm.usage())),
    ))
}

/// 按空行切 SSE 帧。
///
/// 与 `llm_adapt` 内部的切帧是同一套判据（`\n\n` 与 `\r\n\r\n` 取**靠前者**），
/// 各写一份是因为那边是 `pub(crate)`。少一个尾分隔符的容错差异会让两边的帧数
/// 对不上 —— 所以这里也**丢掉末尾不完整的块**（它还没成事件）。
fn split_frames(raw: &str) -> Vec<SseFrame> {
    let mut frames = Vec::new();
    let mut rest = raw;

    loop {
        let crlf = rest.find("\r\n\r\n");
        let lf = rest.find("\n\n");
        let (pos, sep) = match (crlf, lf) {
            (Some(a), Some(b)) => {
                if a <= b {
                    (a, 4)
                } else {
                    (b, 2)
                }
            }
            (Some(a), None) => (a, 4),
            (None, Some(b)) => (b, 2),
            (None, None) => break,
        };
        frames.push(parse_frame(&rest[..pos]));
        rest = &rest[pos + sep..];
    }
    frames
}

/// 认事件名与 `data:` 内容。注释行（`: keep-alive` 心跳）按 SSE 规范忽略。
fn parse_frame(block: &str) -> SseFrame {
    let mut event = None;
    let mut data: Vec<&str> = Vec::new();
    for line in block.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    SseFrame {
        event,
        data: data.join("\n"),
    }
}

fn blocks_view(blocks: &[Block]) -> Vec<BlockView> {
    // 复用历史消息那套投影：同一块东西在两处长得不一样的话，
    // 「对话里是这么显示的、报文里是那么显示的」会让人怀疑哪边是错的。
    view::to_views(&[llm_adapt_core::WireMessage::new(
        llm_adapt_core::Role::Assistant,
        blocks.to_vec(),
    )])
    .into_iter()
    .next()
    .map(|m| m.blocks)
    .unwrap_or_default()
}

/// 收尾原因说人话。
fn finish_label(r: &FinishReason) -> Option<String> {
    Some(match r {
        FinishReason::Stop => "正常结束".to_string(),
        FinishReason::Length => "达到长度上限被截断".to_string(),
        FinishReason::ToolCall => "要调用工具".to_string(),
        FinishReason::Safety => "被内容策略拦截".to_string(),
        FinishReason::StopSequence => "命中停止词".to_string(),
        // 上游没给收尾原因时 `parse_opt(None)` 给的也是 `Other { value: "" }`，
        // 那种「什么都没有」不该在界面上占一行。
        FinishReason::Other { value } if value.is_empty() => return None,
        FinishReason::Other { value } => format!("其它（{value}）"),
    })
}

fn stats(rec: &ExchangeRecord) -> StreamStats {
    let streaming = !rec.stream_frames.is_empty();
    let raw = rec.stream_frames.join("");
    let mut kinds: Vec<EventKind> = Vec::new();
    let mut events = 0;
    if streaming {
        for frame in split_frames(&raw) {
            // 只有带 `data:` 的块才算一个事件 —— 心跳与纯 `event:` 的保活块不算。
            if frame.data.is_empty() {
                continue;
            }
            events += 1;
            let name = frame.event.unwrap_or_else(|| "message".to_string());
            match kinds.iter_mut().find(|k| k.name == name) {
                Some(k) => k.count += 1,
                None => kinds.push(EventKind { name, count: 1 }),
            }
        }
    }
    kinds.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    StreamStats {
        streaming,
        frames: rec.stream_frames.len(),
        events,
        event_kinds: kinds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::now_ms;

    fn record(protocol: &str, request: &str, response: &str) -> ExchangeRecord {
        ExchangeRecord {
            id: "x0".into(),
            ts: now_ms(),
            meta: None,
            method: "POST".into(),
            url: "https://api.deepseek.com/v1/chat/completions".into(),
            protocol: Some(protocol.into()),
            channel: None,
            model: None,
            request_headers: Vec::new(),
            request_body: request.into(),
            status: Some(200),
            response_headers: Vec::new(),
            response_body: response.into(),
            stream_frames: Vec::new(),
            stream_error: None,
            ttfb_ms: Some(120),
            total_ms: Some(900),
        }
    }

    /// 请求解出模型、系统提示词、上下文与工具。
    #[test]
    fn a_chat_request_decodes_into_readable_pieces() {
        let req = r#"{"model":"deepseek-flash","stream":true,"max_tokens":16000,
            "messages":[{"role":"system","content":"be brief"},
                        {"role":"user","content":"你好"}],
            "tools":[{"type":"function","function":{"name":"bash","description":"跑命令",
                      "parameters":{"type":"object"}}}]}"#;
        let rec = record("openai_chat", req, "");
        let v = inspect(&rec);
        let r = v.request.expect("请求应当解得开");

        assert_eq!(r.model, "deepseek-flash");
        assert!(r.stream);
        // Chat Completions 的 `role:"system"` 会被提到顶层 `system` 上（IR 与
        // Anthropic 一致），所以上下文里只剩那条用户消息。
        assert_eq!(r.system, "be brief");
        assert_eq!(r.messages.len(), 1);
        // 用户那条是第 0 轮 —— 界面靠这个数把回答对回报文。
        assert_eq!(r.messages[0].role, "user");
        assert_eq!(r.messages[0].turn, Some(0));
        assert_eq!(r.tools[0].name, "bash");
        assert!(!r.tools[0].server_side);
        assert!(r.params.iter().any(|p| p.key == "max_tokens" && p.value == "16000"));
        // 默认的 tool_choice 不列进参数里。
        assert!(!r.params.iter().any(|p| p.key == "tool_choice"));
    }

    /// 流式响应：一串碎片解成「思考 + 工具调用」，并带上收尾原因与用量。
    ///
    /// 这就是界面上「可视化」页要的那个形状 —— 逐帧的碎片在这一页不该出现。
    #[test]
    fn a_streaming_response_decodes_into_blocks_not_frames() {
        let mut rec = record("openai_chat", r#"{"model":"m","messages":[]}"#, "");
        // 故意切成**跨事件的碎片**：真实抓包就是这样，帧边界与事件边界无关。
        let sse = "data: {\"id\":\"c1\",\"model\":\"deepseek-flash\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"想一下\"}}]}\n\n\
                   data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]}}]}\n\n\
                   data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}}]}}]}\n\n\
                   data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
                   data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n\n\
                   data: [DONE]\n\n";
        rec.stream_frames = vec![sse[..80].to_string(), sse[80..].to_string()];

        let v = inspect(&rec);
        assert!(v.decode_error.is_none(), "{:?}", v.decode_error);
        assert_eq!(v.protocol, "openai_chat");
        assert!(!v.protocol_guessed);

        // 统计：2 个网络块，5 个事件（[DONE] 也算一个 —— 它就是上游发的一个事件）。
        assert_eq!(v.stream.frames, 2);
        assert_eq!(v.stream.events, 6);
        assert_eq!(v.stream.event_kinds[0].name, "message");
        assert_eq!(v.stream.event_kinds[0].count, 6);

        let r = v.response.expect("响应应当解得开");
        assert!(matches!(r.blocks[0], BlockView::Thinking { .. }), "{:?}", r.blocks);
        match &r.blocks[1] {
            // 名字不能被拼成 bashbash —— 同一份字节发两遍的老毛病（见 llm_adapt 的
            // oai_chat mapper）。这条从**界面看到的这一侧**再守一次。
            BlockView::ToolCall { name, args, .. } => {
                assert_eq!(name, "bash");
                assert!(args.contains("ls"));
            }
            other => panic!("期望工具调用，得到 {other:?}"),
        }
        assert_eq!(r.finish.as_deref(), Some("要调用工具"));
        assert_eq!(r.usage.map(|u| u.input_tokens), Some(12));
    }

    /// 非流式响应走 `decode_response`。
    #[test]
    fn a_non_streaming_response_decodes_from_the_body() {
        let body = r#"{"id":"c1","model":"m","choices":[{"message":{"role":"assistant",
            "content":"好了"},"finish_reason":"stop"}]}"#;
        let v = inspect(&record("openai_chat", r#"{"model":"m","messages":[]}"#, body));
        let r = v.response.expect("响应应当解得开");
        assert!(matches!(&r.blocks[0], BlockView::Text { text } if text == "好了"));
        assert_eq!(r.finish.as_deref(), Some("正常结束"));
    }

    /// 协议解不开时报出来，而不是给一个空白页。
    #[test]
    fn an_undecodable_payload_says_so() {
        let v = inspect(&record("no_such_protocol", "{}", "{}"));
        assert!(v.decode_error.is_some());
        assert!(v.request.is_none() && v.response.is_none());
    }

    /// 老记录（没有 protocol 字段）按 URL 猜，并标明是猜的。
    #[test]
    fn old_records_fall_back_to_url_sniffing_and_admit_it() {
        let mut rec = record("openai_chat", r#"{"model":"m","messages":[]}"#, "");
        rec.protocol = None;
        let v = inspect(&rec);
        assert_eq!(v.protocol, "openai_chat");
        assert!(v.protocol_guessed, "猜出来的要标出来");

        rec.url = "https://gw.example/v1/messages".into();
        assert_eq!(inspect(&rec).protocol, "anthropic");

        rec.url = "https://gw.example/custom/thing".into();
        let v = inspect(&rec);
        assert!(v.decode_error.is_some(), "猜不出来要说出来");
    }

    /// 还没收到响应体就断掉的交换：不该报错，只是没有响应可看。
    #[test]
    fn an_interrupted_exchange_has_no_response_and_no_error() {
        let v = inspect(&record("openai_chat", r#"{"model":"m","messages":[]}"#, ""));
        assert!(v.response.is_none());
        assert!(v.decode_error.is_none());
        assert!(!v.stream.streaming);
    }
}