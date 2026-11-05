//! 会话标题：首轮用户消息之后，用副模型起一个名字。
//!
//! ## 为什么要在 carter 的 `generate_title` 外面再套一层
//!
//! 标题仍然由 carter 的 [`carter::agent::generate_title`] 生成（同一套提示词、
//! 同一套「取首行、去引号」的后处理，CLI 与 GUI 的名字才长得一样）。问题出在它的
//! `max_tokens` 上：那里定的是 **32**（标题就几个字，够用了）。
//!
//! 推理模型不是这么算账的 —— 它**先思考、再输出**，两边共用一个 `max_tokens`。
//! 32 个 token 会被思考整个吃掉，正文一个字都不剩。DeepSeek 上实测：
//!
//! ```text
//! finish = Length   output_tokens = 32   reasoning_tokens = 32   content = ""
//! → carter 报 "summary returned empty text"，标题永远出不来
//! ```
//!
//! 关掉思考，32 个 token 对标题绰绰有余（同一条件实测：`finish = Stop`，
//! `content = "工具功能一览"`）。顺带省掉一次推理的钱 —— 起标题本来就不需要推理。
//!
//! ## 关思考怎么表达
//!
//! 各家关思考的写法不一样，有的渠道对不认识的字段直接 400。所以分成两层：
//!
//! 1. **厂商自己的开关**（[`THINKING_SWITCHES`]）：按渠道种类写该厂商官方文档里的
//!    「别思考」字段。按厂商写、不按模型写 —— 挑哪一个模型起标题是用户的事，
//!    我们不该因为「以为它不是推理模型」就少关一次。
//! 2. **`reasoning_effort: "none"`**（[`NO_THINKING_KINDS`] 白名单）：OpenAI 系
//!    渠道常见的开关，但**只在实测过的渠道上发** —— 不认它的厂商可能因为多一个
//!    字段直接 400，那就把本来能用的标题弄坏了。
//!
//! 万一渠道挂着某厂商的 kind 却不认那套开关（中转、自建），[`SwitchOff`] 会去掉
//! 字段把**同一个请求**原样重发一次。
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

/// 官方接口的渠道 kind —— 请求体的形状是公开且稳定的，按厂商写开关是安全的。
const ANTHROPIC_KINDS: &[&str] = &["anthropic", "claude"];
const OPENAI_KINDS: &[&str] = &["openai"];
const DEEPSEEK_KINDS: &[&str] = &["deepseek"];
const GEMINI_KINDS: &[&str] = &["gemini", "google"];
const QWEN_KINDS: &[&str] = &["qwen", "dashscope"];
const GLM_KINDS: &[&str] = &["glm", "zhipu", "bigmodel"];
const KIMI_KINDS: &[&str] = &["kimi", "moonshot"];
const DOUBAO_KINDS: &[&str] = &["doubao", "volcengine", "ark"];
const MINIMAX_KINDS: &[&str] = &["minimax"];

/// 一组关思考字段：`(请求体里的键, JSON 字面量或普通字符串)`。
type SwitchFields = &'static [(&'static str, &'static str)];

/// 「别思考」在各家请求体里的写法。
///
/// 不在这张表里的渠道（`custom` / 中转 / 完全没见过的 kind）一个字都不加 ——
/// 请求体的形状我们不知道，乱加字段比多等几秒更糟。
const THINKING_SWITCHES: &[(&[&str], SwitchFields)] = &[
    (ANTHROPIC_KINDS, &[("thinking", r#"{"type":"disabled"}"#)]),
    (OPENAI_KINDS, &[("reasoning_effort", "none")]),
    (DEEPSEEK_KINDS, &[("thinking", r#"{"type":"disabled"}"#)]),
    (
        GEMINI_KINDS,
        &[("thinkingConfig", r#"{"thinkingBudget":0}"#)],
    ),
    (QWEN_KINDS, &[("enable_thinking", "false")]),
    (GLM_KINDS, &[("thinking", r#"{"type":"disabled"}"#)]),
    (KIMI_KINDS, &[("thinking", r#"{"type":"disabled"}"#)]),
    (DOUBAO_KINDS, &[("thinking", r#"{"type":"disabled"}"#)]),
    (MINIMAX_KINDS, &[("thinking", r#"{"type":"disabled"}"#)]),
];

/// 认 `reasoning_effort: "none"` 的渠道 kind。
///
/// 判据只有一个：拿真实报文试过，关掉思考之后标题能出、且请求没被拒。
/// 目前只有 DeepSeek 试过（`kind = "deepseek"`）。
///
/// 与 [`THINKING_SWITCHES`] 分开：那里每一条都是该厂商官方文档里的字段，这里是
/// 「额外再补一个常见开关」—— 后者没有文档背书，所以仍然只发给试过的渠道。
const NO_THINKING_KINDS: &[&str] = &["deepseek"];

/// 这个渠道关思考的 `reasoning_effort` 参数是不是被验证过。
pub fn supports_no_thinking(kind: &str) -> bool {
    let kind = kind.trim().to_ascii_lowercase();
    NO_THINKING_KINDS.contains(&kind.as_str())
}

/// 这个渠道关思考用哪些字段（`None` = 这个渠道我们不碰请求体）。
pub fn thinking_switches(kind: &str) -> Option<&'static [(&'static str, &'static str)]> {
    let kind = kind.trim().to_ascii_lowercase();
    THINKING_SWITCHES
        .iter()
        .find(|(kinds, _)| kinds.contains(&kind.as_str()))
        .map(|(_, fields)| *fields)
}

/// 这一次请求要额外带上的关思考字段：厂商自己的写法 + 白名单渠道的
/// `reasoning_effort`。
fn extra_fields(kind: &str) -> Vec<(&'static str, &'static str)> {
    let mut fields = thinking_switches(kind).unwrap_or(&[]).to_vec();
    if supports_no_thinking(kind) && !fields.iter().any(|(k, _)| *k == "reasoning_effort") {
        fields.push(("reasoning_effort", "none"));
    }
    fields
}

/// 给 provider 套一层：请求里多带几个「别思考」字段；渠道不认这些字段时，
/// 去掉它们把**同一个请求**原样重发一次。
///
/// 只包标题这一次调用 —— 主对话是要思考的，那个走原来的 provider。
///
/// 加字段与去字段必须在**同一层**：分成两个包装器的话，外层摘掉的只是它自己那份
/// 请求副本，内层在每次调用时都会把字段重新加回去，重发等于没改。
pub struct SwitchOff {
    inner: Arc<dyn LlmProvider>,
    fields: Vec<(&'static str, serde_json::Value)>,
}

impl SwitchOff {
    pub fn new(inner: Arc<dyn LlmProvider>, fields: &[(&'static str, &'static str)]) -> Self {
        let fields = fields
            .iter()
            .map(|(k, v)| {
                // 值可能是 JSON 字面量（`{"type":"disabled"}`），也可能是普通字符串。
                let value = serde_json::from_str(v)
                    .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
                (*k, value)
            })
            .collect();
        Self { inner, fields }
    }
}

#[async_trait::async_trait]
impl LlmProvider for SwitchOff {
    async fn stream(&self, req: ChatRequest) -> carter::Result<EventStream> {
        // 用 `extra` 而不是 IR 的 `thinking` 字段：IR 里那个是「用多少思考」，
        // 表达不了「一点都不要」，而且 chat completions 的编码器根本不读它。
        // `extra` 是原样并进请求体的通道 —— 这正是它存在的理由。
        let without = req.clone();
        let mut with = req;
        for (key, value) in &self.fields {
            with.wire.extra.insert((*key).into(), value.clone());
        }
        match self.inner.stream(with).await {
            // 只有「请求本身不被接受」才值得去掉字段再试。空输出 / 超时 / 限流
            // 之类去掉字段也没用，原样交回给上层的重试逻辑。
            Err(carter::CarterError::Llm(ref le)) if is_rejected(le) => {
                self.inner.stream(without).await
            }
            other => other,
        }
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
/// 它决定要不要关思考、怎么关，见模块文档。`session_id` 让起标题的请求也带上
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
    let fields = extra_fields(kind);
    let provider: Arc<dyn LlmProvider> = if fields.is_empty() {
        provider
    } else {
        Arc::new(SwitchOff::new(provider, &fields))
    };

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

    /// 关思考是**加几个字段**，不是改别的：请求的其余部分要原样过去。
    #[tokio::test]
    async fn the_wrapper_adds_the_fields_and_changes_nothing_else() {
        let inner = Counting::new();
        let wrapped = SwitchOff::new(inner.clone(), thinking_switches("deepseek").unwrap());
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

    /// 渠道拒了带开关的请求：去掉开关**把同一个请求**再发一次。
    ///
    /// 其余字段（`max_tokens`、消息）在重发时也要原样 —— 重发不是「另发一个空请求」。
    #[tokio::test]
    async fn the_wrapper_retries_without_switches_when_rejected() {
        struct RejectsSwitch {
            seen: Mutex<Vec<ChatRequest>>,
        }
        #[async_trait::async_trait]
        impl LlmProvider for RejectsSwitch {
            async fn stream(&self, req: ChatRequest) -> carter::Result<EventStream> {
                let rejected = req.wire.extra.contains_key("thinking");
                self.seen.lock().unwrap().push(req);
                if rejected {
                    return Err(bad_request());
                }
                Ok(text_stream("工具功能一览"))
            }
        }
        let inner = Arc::new(RejectsSwitch {
            seen: Mutex::new(Vec::new()),
        });
        let wrapped = SwitchOff::new(inner.clone(), thinking_switches("anthropic").unwrap());
        let mut req = request();
        req.wire.max_tokens = Some(32);
        let _ = wrapped.stream(req).await;

        let seen = inner.seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "被拒后应当重发一次");
        assert!(seen[0].wire.extra.contains_key("thinking"));
        assert!(seen[1].wire.extra.is_empty(), "重发时开关要摘掉");
        assert_eq!(seen[1].wire.max_tokens, Some(32), "其余字段原样");
        assert_eq!(seen[1].wire.messages.len(), 1);
    }

    /// 白名单只认实测过的渠道，其余一律不加 `reasoning_effort`。
    ///
    /// 这条测试挡的是「顺手给所有渠道都加上」—— 不认这个字段的厂商会直接 400，
    /// 那等于把本来能用的标题弄坏（Ollama 之类对未知字段很严）。
    #[test]
    fn only_verified_kinds_get_the_effort_field() {
        assert!(supports_no_thinking("deepseek"));
        assert!(
            supports_no_thinking("DeepSeek "),
            "大小写与空格不该影响判断"
        );
        for other in ["anthropic", "openai", "ollama", "gemini", "groq", "custom"] {
            assert!(
                !supports_no_thinking(other),
                "{other} 没实测过，不该发这个字段"
            );
        }
    }

    /// 关思考 = 写厂商自己的开关（不是「以为它是推理模型才关」）：
    /// 挑哪一个模型起标题是用户的事，标题不该被思考吃掉。
    #[test]
    fn known_vendors_always_get_their_switch() {
        for kind in [
            "anthropic",
            "claude",
            "openai",
            "deepseek",
            "gemini",
            "google",
            "qwen",
            "dashscope",
            "glm",
            "zhipu",
            "bigmodel",
            "kimi",
            "moonshot",
            "doubao",
            "volcengine",
            "ark",
            "minimax",
        ] {
            assert!(thinking_switches(kind).is_some(), "{kind} 应当有关思考开关");
        }
        assert!(
            thinking_switches("Anthropic").is_some(),
            "大小写不该影响判断"
        );
        for kind in ["custom", "ollama", "groq", "", "中转"] {
            assert!(
                thinking_switches(kind).is_none(),
                "{kind} 不认识它的请求体，不该乱加字段"
            );
        }
    }

    /// 总入口给不同渠道套的开关：厂商自己的写法，外加（实测过的）`reasoning_effort`。
    #[tokio::test]
    async fn generate_lines_up_the_switches_by_vendor() {
        async fn seen_fields(kind: &str) -> Vec<String> {
            let p = Counting::new();
            let out = generate("你好", "s", p.clone(), &model(), kind).await;
            assert!(!out.degraded);
            let mut keys: Vec<String> = p
                .last
                .lock()
                .unwrap()
                .as_ref()
                .expect("请求应到达渠道")
                .wire
                .extra
                .keys()
                .cloned()
                .collect();
            keys.sort();
            keys
        }
        assert_eq!(seen_fields("anthropic").await, vec!["thinking"]);
        assert_eq!(
            seen_fields("deepseek").await,
            vec!["reasoning_effort", "thinking"],
            "只有实测过的渠道才多补一个 reasoning_effort"
        );
        assert!(
            seen_fields("custom").await.is_empty(),
            "不认识的渠道一个字段都不加"
        );
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
