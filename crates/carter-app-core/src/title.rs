//! 会话标题：首轮用户消息之后，用副模型起一个名字。
//!
//! ## 为什么不用 carter 现成的 `generate_title`
//!
//! 提示词与后处理由 carter 的 [`carter::agent::generate_title`] 提供（同一套
//! 「取首行、去引号」，CLI 与 GUI 的名字才长得一样），`max_tokens` 是 **32**
//! （标题就几个字，够用了）。
//!
//! 推理模型不是这么算账的 —— 它**先思考、再输出**，两边共用一个 `max_tokens`。
//! 32 个 token 会被思考整个吃掉，正文一个字都不剩。DeepSeek 上实测：
//!
//! ```text
//! finish = Length   output_tokens = 32   reasoning_tokens = 32   content = ""
//! → 正文一个字都没有，标题永远出不来
//! ```
//!
//! 关掉思考，32 个 token 对标题绰绰有余（同一条件实测：`finish = Stop`，
//! `content = "工具功能一览"`）。顺带省掉一次推理的钱 —— 起标题本来就不需要推理。
//!
//! ## 关思考怎么表达
//!
//! **不管什么渠道、什么模型，标题请求都要关思考。** 不按厂商或模型名判断：模板建的
//! 渠道 `kind` 只写线协议名（DeepSeek 也是 `openai_chat`），中转站更认不出背后是谁，
//! 而且挑哪个模型起标题是用户的事，我们猜「它不是推理模型」只会猜漏。
//!
//! 能决定写法的只有线协议，见 [`switch_steps`]。Chat Completions 没有统一写法，就把
//! 各家的字段一起发；严格的渠道会因为不认识的字段 400，[`SwitchOff`] 再逐个单独试。
//! 所有写法都被拒时才不带字段发一次 —— 有些模型根本不接受这类参数（比如不支持
//! `reasoning_effort` 的非推理模型），它们本来就不思考，不该因此丢掉标题。
//!
//! ## 失败要说话
//!
//! 这里只负责「造一次标题」；重试与兜底在 [`generate`]：可重试的错误退避重来，
//! 实在拿不到就回一个从首句里截出来的名字（见 [`fallback_from_prompt`]），并带上
//! [`Outcome::degraded`] —— 侧栏永远不该停在「未命名会话」上。

use std::sync::Arc;
use std::time::Duration;

use carter::provider::{ChatRequest, EventStream, LlmProvider};
use carter::registry::ModelInfo;

/// 最多试几次（含第一次）。
const MAX_ATTEMPTS: u32 = 3;
/// 第 n 次重试前等多久（毫秒）。
const BACKOFF_MS: [u64; 2] = [800, 2_000];

/// 一组关思考字段：`(请求体里的键, JSON 字面量或普通字符串)`。
type SwitchFields = &'static [(&'static str, &'static str)];

const THINKING_DISABLED: &str = r#"{"type":"disabled"}"#;

const ANTHROPIC_STEPS: &[SwitchFields] = &[&[("thinking", THINKING_DISABLED)]];

const RESPONSES_STEPS: &[SwitchFields] = &[&[("reasoning", r#"{"effort":"none"}"#)]];

/// Chat Completions 各家的写法：`thinking`（DeepSeek、GLM、Kimi、豆包等）、
/// `reasoning_effort`（OpenAI、DeepSeek、Gemini 兼容端点等）、`enable_thinking`（千问）。
const CHAT_STEPS: &[SwitchFields] = &[
    &[
        ("thinking", THINKING_DISABLED),
        ("reasoning_effort", "none"),
        ("enable_thinking", "false"),
    ],
    &[("reasoning_effort", "none")],
    &[("thinking", THINKING_DISABLED)],
    &[("enable_thinking", "false")],
];

/// 这个渠道关思考的写法，按顺序试。
///
/// 协议由 carter 从 `kind` 解析（认别名、未知值归到 Chat Completions），这里不再维护
/// 一份映射，所以任何 `kind` 都有写法。
pub fn switch_steps(kind: &str) -> &'static [SwitchFields] {
    let config = carter::config::ProviderConfig {
        kind: kind.to_string(),
        ..Default::default()
    };
    let protocol = config.protocol();
    match protocol.as_str() {
        llm_adapt_core::ProtocolId::ANTHROPIC => ANTHROPIC_STEPS,
        llm_adapt_core::ProtocolId::OPENAI_RESPONSES => RESPONSES_STEPS,
        _ => CHAT_STEPS,
    }
}

/// 给 provider 套一层：请求里带上「别思考」字段；渠道不认这种写法就换下一种，
/// 都不认才去掉字段把**同一个请求**原样发一次。
///
/// 只包标题这一次调用 —— 主对话是要思考的，那个走原来的 provider。
pub struct SwitchOff {
    inner: Arc<dyn LlmProvider>,
    steps: Vec<Vec<(&'static str, serde_json::Value)>>,
}

impl SwitchOff {
    pub fn new(inner: Arc<dyn LlmProvider>, steps: &[SwitchFields]) -> Self {
        let steps = steps
            .iter()
            .map(|fields| {
                fields
                    .iter()
                    .map(|(k, v)| {
                        // 值可能是 JSON 字面量（`{"type":"disabled"}`），也可能是普通字符串。
                        let value = serde_json::from_str(v)
                            .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
                        (*k, value)
                    })
                    .collect()
            })
            .collect();
        Self { inner, steps }
    }
}

#[async_trait::async_trait]
impl LlmProvider for SwitchOff {
    async fn stream(&self, req: ChatRequest) -> carter::Result<EventStream> {
        // 用 `extra` 而不是 IR 的 `thinking` 字段：IR 里那个是「用多少思考」，
        // 表达不了「一点都不要」，而且 chat completions 的编码器根本不读它。
        // `extra` 是原样并进请求体的通道 —— 这正是它存在的理由。
        for fields in &self.steps {
            let mut with = req.clone();
            for (key, value) in fields {
                with.wire.extra.insert((*key).into(), value.clone());
            }
            match self.inner.stream(with).await {
                // 只有「请求本身不被接受」才值得换写法。空输出 / 超时 / 限流之类
                // 换了也没用，原样交回给上层的重试逻辑。
                Err(carter::CarterError::Llm(ref le)) if is_rejected(le) => {}
                other => return other,
            }
        }
        tracing::warn!(
            model = %req.wire.model,
            "渠道拒绝了所有关思考的写法，标题请求不带这些字段重发"
        );
        self.inner.stream(req).await
    }
}

/// 一次标题生成的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub title: String,
    /// `true` = 这不是模型起的名字，而是从首句话里截出来的兜底标题。
    pub degraded: bool,
    /// 一共试了几次（含第一次）。
    pub attempts: u32,
}

/// 生成会话标题 —— **总会**返回一个可用的标题。
///
/// `kind` 是渠道种类（`config.provider_for(model.provider).kind`）——
/// 它决定用哪种线协议的写法关思考，见模块文档。`session_id` 让起标题的请求也带上
/// 会话 id，网关靠它把这次请求归进同一条对话。
///
/// 重试：可重试的错误（限流、网络、超时、5xx）与空标题按指数退避重来；
/// 鉴权错、请求非法、IO 之类重试也没用，直接兜底。都失败就回
/// [`fallback_from_prompt`] 的截断标题，并标 `degraded`。
pub async fn generate(
    first_prompt: &str,
    session_id: &str,
    provider: Arc<dyn LlmProvider>,
    model: &ModelInfo,
    kind: &str,
) -> Outcome {
    let provider: Arc<dyn LlmProvider> = Arc::new(SwitchOff::new(provider, switch_steps(kind)));

    let mut attempts = 1;
    for attempt in 0..MAX_ATTEMPTS {
        attempts = attempt + 1;
        let retry =
            match carter::agent::generate_title(first_prompt, Some(session_id), &*provider, model)
                .await
            {
                Ok(t) if !t.trim().is_empty() => {
                    return Outcome {
                        title: t,
                        degraded: false,
                        attempts,
                    };
                }
                // 空标题：多半是思考把 token 吃光了（见模块文档），值得再试一次。
                Ok(_) => true,
                // 限流、网络、超时、5xx：值得重试。
                Err(carter::CarterError::Llm(ref le)) => worth_retrying(le),
                // 非模型侧的错误（解析配置、IO）重试不会变好。
                Err(_) => false,
            };
        if !retry || attempt + 1 == MAX_ATTEMPTS {
            break;
        }
        let wait = BACKOFF_MS[attempt as usize % BACKOFF_MS.len()];
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
    Outcome {
        title: fallback_from_prompt(first_prompt),
        degraded: true,
        attempts,
    }
}

/// 上游的拒绝是「请求体/参数不被接受」，而不是限流或网络问题。
fn is_rejected(err: &llm_adapt_core::LLMError) -> bool {
    use llm_adapt_core::LLMError;
    match err {
        LLMError::Validation { .. } => true,
        LLMError::Provider { ctx, .. } => matches!(ctx.http_status, Some(400 | 422)),
        _ => false,
    }
}

/// 这个错误值得原样再发一次吗。
///
/// `is_retryable()` 之外还要看**错误类别**：它读的是上游给的
/// `ErrorContext.retryable`，而流中途出的错（读 SSE 时断线、超时）上那一位未必
/// 填对。限流、网络、超时这三类按类别直接认。
fn worth_retrying(err: &llm_adapt_core::LLMError) -> bool {
    use llm_adapt_core::LLMError;
    err.is_retryable()
        || matches!(
            err,
            LLMError::RateLimit { .. } | LLMError::Network { .. } | LLMError::Timeout { .. }
        )
}

/// 兜底标题：从首句话里截一段给人看的文字。
///
/// 规则刻意简单、可预期：跳过空白与 markdown 标记（`#`、`>`、列表符、代码围栏），
/// 取第一行有内容的，行内空白压成一个空格，按**字符**（不是字节）截到
/// [`FALLBACK_MAX_CHARS`]，截断了就补省略号。再不行（整句话都是符号）用
/// [`DEFAULT_TITLE`]。
pub fn fallback_from_prompt(prompt: &str) -> String {
    let line = prompt
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '>', '-', '*', '+', '`', ' '])
                .trim()
        })
        .find(|l| l.chars().any(char::is_alphanumeric))
        .unwrap_or("");
    // 标题只占一行：行内多余空白压成一个空格。
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return DEFAULT_TITLE.to_string();
    }
    let mut out: String = line.chars().take(FALLBACK_MAX_CHARS).collect();
    if line.chars().count() > FALLBACK_MAX_CHARS {
        out.push('…');
    }
    out
}

/// 兜底标题最多几个字。
const FALLBACK_MAX_CHARS: usize = 20;
/// 连首句话都截不出东西时用这个。
pub const DEFAULT_TITLE: &str = "新会话";

#[cfg(test)]
mod tests {
    use super::*;
    use carter::provider::{Block, Role, StreamChunk, WireMessage};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn model() -> ModelInfo {
        serde_json::from_str(
            r#"{"provider":"p","api_name":"m","context_window":1000,
                "max_output_tokens":100,"tokenizer":"t",
                "pricing":{"input":0.0,"output":0.0}}"#,
        )
        .expect("ModelInfo 的 JSON 形状")
    }

    fn request() -> ChatRequest {
        let mut wire = carter::provider::WireRequest::new("deepseek-flash");
        wire.system = vec![Block::text("起个标题")];
        wire.messages = vec![WireMessage::new(Role::User, vec![Block::text("你好")])];
        ChatRequest {
            protocol: None,
            wire,
        }
    }

    fn text_stream(text: &str) -> EventStream {
        Box::pin(futures::stream::iter(vec![Ok(StreamChunk::TextDelta {
            index: 0,
            text: text.to_string(),
        })]))
    }

    /// 每次都成功，记下收到的请求与调用次数。
    struct Counting {
        last: Mutex<Option<ChatRequest>>,
        seen: Mutex<Vec<ChatRequest>>,
        calls: AtomicU32,
    }

    impl Counting {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                last: Mutex::new(None),
                seen: Mutex::new(Vec::new()),
                calls: AtomicU32::new(0),
            })
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for Counting {
        async fn stream(&self, req: ChatRequest) -> carter::Result<EventStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last.lock().unwrap() = Some(req.clone());
            self.seen.lock().unwrap().push(req);
            Ok(text_stream("工具功能一览"))
        }
    }

    /// 前 `failures` 次返回错误，之后回固定文本。
    struct Flaky {
        failures: u32,
        calls: AtomicU32,
        error: fn() -> carter::CarterError,
        text: &'static str,
    }

    fn flaky(failures: u32, error: fn() -> carter::CarterError) -> Arc<Flaky> {
        Arc::new(Flaky {
            failures,
            calls: AtomicU32::new(0),
            error,
            text: "工具功能一览",
        })
    }

    #[async_trait::async_trait]
    impl LlmProvider for Flaky {
        async fn stream(&self, _req: ChatRequest) -> carter::Result<EventStream> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.failures {
                return Err((self.error)());
            }
            Ok(text_stream(self.text))
        }
    }

    fn rate_limited() -> carter::CarterError {
        carter::CarterError::Llm(llm_adapt_core::LLMError::RateLimit {
            message: "slow down".into(),
            retry_after_ms: None,
            ctx: Default::default(),
        })
    }

    fn bad_request() -> carter::CarterError {
        carter::CarterError::Llm(llm_adapt_core::LLMError::Validation {
            message: "unknown field".into(),
        })
    }

    fn unauthorized() -> carter::CarterError {
        carter::CarterError::Llm(llm_adapt_core::LLMError::Auth {
            message: "bad key".into(),
            ctx: Default::default(),
        })
    }

    /// 标题请求的形状 —— 这是与 carter 的约定，也是曾经出过的 bug 的回归测试：
    /// 用户那句话必须**原样**发。carter 0.1.6 里它和压缩共用 `run_summary`，
    /// 会加「【待压缩历史】」前缀；0.1.7 起改由调用方自己给用户消息。
    /// 另外 32 个 token 全留给正文，并带上会话 id 与「这是起标题」的用途标记。
    #[tokio::test]
    async fn the_title_request_carries_the_prompt_as_is() {
        let p = Counting::new();
        let out = generate("你好啊", "s-1", p.clone(), &model(), "anthropic").await;
        assert_eq!(out.title, "工具功能一览");

        let seen = p.last.lock().unwrap().take().expect("请求应到达渠道");
        let text = seen.wire.messages[0].content[0].as_text().unwrap_or("");
        assert_eq!(text, "你好啊", "用户消息不能被加上压缩用的前缀");
        assert_eq!(seen.wire.max_tokens, Some(32), "标题只要 32 个 token");
        assert_eq!(seen.wire.meta.session_id.as_deref(), Some("s-1"));
        assert_eq!(
            seen.wire.meta.purpose,
            carter::provider::RequestPurpose::Title
        );
        assert!(
            seen.wire.system_text().contains("3-6 词中文标题"),
            "system 提示仍是起标题那一句"
        );
    }

    /// 关思考是**加几个字段**，不是改别的：请求的其余部分要原样过去。
    #[tokio::test]
    async fn the_wrapper_adds_the_fields_and_changes_nothing_else() {
        let inner = Counting::new();
        let wrapped = SwitchOff::new(inner.clone(), switch_steps("anthropic"));
        let mut req = request();
        req.wire.max_tokens = Some(32);
        let _ = wrapped.stream(req).await;

        let seen = inner.last.lock().unwrap().take().expect("请求应到达内层");
        assert_eq!(
            seen.wire.extra["thinking"],
            serde_json::json!({"type": "disabled"})
        );
        assert_eq!(seen.wire.model, "deepseek-flash");
        assert_eq!(seen.wire.max_tokens, Some(32), "其余字段不能被改动");
        assert_eq!(seen.wire.messages.len(), 1, "消息不能被改动");
    }

    /// 只接受某几个字段的渠道：记下每次请求，带了 `banned` 里的字段就 400。
    struct Strict {
        banned: &'static [&'static str],
        seen: Mutex<Vec<ChatRequest>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for Strict {
        async fn stream(&self, req: ChatRequest) -> carter::Result<EventStream> {
            let rejected = self.banned.iter().any(|k| req.wire.extra.contains_key(*k));
            self.seen.lock().unwrap().push(req);
            if rejected {
                return Err(bad_request());
            }
            Ok(text_stream("工具功能一览"))
        }
    }

    fn strict(banned: &'static [&'static str]) -> Arc<Strict> {
        Arc::new(Strict {
            banned,
            seen: Mutex::new(Vec::new()),
        })
    }

    /// 严格的渠道不认合并的写法时，要落到它认的那一种上 —— 而不是直接放弃关思考。
    #[tokio::test]
    async fn the_wrapper_falls_through_to_the_form_the_channel_accepts() {
        let inner = strict(&["thinking", "enable_thinking"]);
        let wrapped = SwitchOff::new(inner.clone(), switch_steps("openai_chat"));
        let _ = wrapped.stream(request()).await;

        let seen = inner.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let last = &seen[1].wire.extra;
        assert_eq!(last.len(), 1);
        assert_eq!(last["reasoning_effort"], serde_json::json!("none"));
    }

    /// 所有写法都被拒：去掉字段把**同一个请求**再发一次。
    ///
    /// 其余字段（`max_tokens`、消息）在重发时也要原样 —— 重发不是「另发一个空请求」。
    #[tokio::test]
    async fn the_wrapper_drops_the_fields_when_every_form_is_rejected() {
        let inner = strict(&["thinking", "reasoning_effort", "enable_thinking"]);
        let wrapped = SwitchOff::new(inner.clone(), switch_steps("openai_chat"));
        let mut req = request();
        req.wire.max_tokens = Some(32);
        let _ = wrapped.stream(req).await;

        let seen = inner.seen.lock().unwrap();
        assert_eq!(seen.len(), CHAT_STEPS.len() + 1);
        let last = seen.last().unwrap();
        assert!(last.wire.extra.is_empty(), "最后一次不带字段");
        assert_eq!(last.wire.max_tokens, Some(32), "其余字段原样");
        assert_eq!(last.wire.messages.len(), 1);
    }

    /// 限流之类不是「不认字段」，不能拿来换写法：那会把一次限流放大成好几个请求。
    #[tokio::test]
    async fn the_wrapper_only_changes_form_on_rejection() {
        let inner = flaky(u32::MAX, rate_limited);
        let wrapped = SwitchOff::new(inner.clone(), switch_steps("openai_chat"));
        assert!(wrapped.stream(request()).await.is_err());
        assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    }

    /// 不管什么渠道、什么模型，第一次发出去的请求都带着关思考的字段。
    #[tokio::test]
    async fn every_channel_and_model_turns_thinking_off() {
        for kind in [
            "anthropic",
            "claude",
            "openai_responses",
            "openai_chat",
            "OpenAI_Chat ",
            "openai",
            "openai_compat",
            "custom",
            "deepseek",
            "gemini",
            "ollama",
            "groq",
            "",
            "中转",
        ] {
            for api_name in ["deepseek-flash", "gpt-4o-mini", "some-unknown-model"] {
                let p = Counting::new();
                let mut m = model();
                m.api_name = api_name.to_string();
                let out = generate("你好", "s", p.clone(), &m, kind).await;
                assert!(!out.degraded);
                let seen = p.last.lock().unwrap().take().expect("请求应到达渠道");
                assert!(!seen.wire.extra.is_empty(), "{kind} / {api_name} 没关思考");
            }
        }
    }

    /// 写法跟着线协议走，且字段确实落进了发出去的请求体。
    #[tokio::test]
    async fn the_form_follows_the_wire_protocol() {
        assert_eq!(switch_steps("anthropic"), ANTHROPIC_STEPS);
        assert_eq!(switch_steps("openai_responses"), RESPONSES_STEPS);
        assert_eq!(switch_steps("openai_chat"), CHAT_STEPS);
        assert_eq!(switch_steps("deepseek"), CHAT_STEPS);

        let p = Counting::new();
        let out = generate("你好", "s", p.clone(), &model(), "openai_chat").await;
        assert!(!out.degraded);
        let seen = p.last.lock().unwrap().take().expect("请求应到达渠道");
        let body = llm_adapt_core::protocol::oai_chat::build_body(&seen.wire).unwrap();
        assert_eq!(body["thinking"], serde_json::json!({"type": "disabled"}));
        assert_eq!(body["reasoning_effort"], serde_json::json!("none"));
        assert_eq!(body["enable_thinking"], serde_json::json!(false));
        assert_eq!(body["max_tokens"], serde_json::json!(32));
    }

    /// 可重试的错误要重试，重试成功就拿到真标题。
    #[tokio::test]
    async fn retries_retryable_errors() {
        let p = flaky(1, rate_limited);
        let out = generate("帮我看看这个 bug", "s", p.clone(), &model(), "custom").await;
        assert!(!out.degraded);
        assert_eq!(out.title, "工具功能一览");
        assert_eq!(out.attempts, 2);
        assert_eq!(p.calls.load(Ordering::SeqCst), 2);
    }

    /// 重试都用完了：兜底标题必须顶上，而不是让会话没名字。
    #[tokio::test]
    async fn falls_back_after_every_attempt_fails() {
        let p = flaky(u32::MAX, rate_limited);
        let out = generate("帮我看看这个 bug", "s", p.clone(), &model(), "custom").await;
        assert!(out.degraded);
        assert_eq!(out.title, "帮我看看这个 bug");
        assert_eq!(out.attempts, MAX_ATTEMPTS);
        assert_eq!(p.calls.load(Ordering::SeqCst), MAX_ATTEMPTS);
    }

    /// 鉴权错重试也没用：只试一次就兜底，白等两次没意义。
    #[tokio::test]
    async fn does_not_retry_hopeless_errors() {
        let p = flaky(u32::MAX, unauthorized);
        let out = generate("你好", "s", p.clone(), &model(), "custom").await;
        assert!(out.degraded);
        assert_eq!(out.attempts, 1, "鉴权错重试也没用，只该试一次");
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fallback_skips_markup_and_truncates_by_chars() {
        assert_eq!(fallback_from_prompt("帮我看看这个 bug"), "帮我看看这个 bug");
        assert_eq!(fallback_from_prompt("## 标题\n正文"), "标题");
        assert_eq!(fallback_from_prompt("- 第一项\n- 第二项"), "第一项");
        assert_eq!(fallback_from_prompt("  多   空格  "), "多 空格");
        assert_eq!(fallback_from_prompt("   "), DEFAULT_TITLE);
        assert_eq!(fallback_from_prompt("!!!"), DEFAULT_TITLE);
        let long = "很".repeat(50);
        let out = fallback_from_prompt(&long);
        assert_eq!(out.chars().count(), FALLBACK_MAX_CHARS + 1);
        assert!(out.ends_with('…'));
    }
}
