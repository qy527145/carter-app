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
//! ## 为什么只对一部分渠道发这个字段
//!
//! 用的是 `reasoning_effort: "none"`（OpenAI 系写法，DeepSeek 实测认）。
//! 但**不认这个字段的厂商可能因为多了一个字段直接 400** —— 那就把本来能用的标题
//! 弄坏了。所以按渠道 kind 发白名单，名单里的每一条都是**实测过**的；
//! 要加新的一条，先拿真实报文验一遍再说。
//!
//! 白名单之外的渠道一个字都不加：那里标题能不能出来，取决于模型自己是否推理
//! （非推理模型照常工作）。这是**保持现状**，不是新引入的缺陷。

use std::sync::Arc;

use carter::provider::{ChatRequest, EventStream, LlmProvider};
use carter::registry::ModelInfo;

/// 认 `reasoning_effort: "none"` 的渠道 kind。
///
/// 判据只有一个：拿真实报文试过，关掉思考之后标题能出、且请求没被拒。
/// 目前只有 DeepSeek 试过（`kind = "deepseek"`）。
const NO_THINKING_KINDS: &[&str] = &["deepseek"];

/// 这个渠道关思考的参数是不是被验证过。
pub fn supports_no_thinking(kind: &str) -> bool {
    let kind = kind.trim().to_ascii_lowercase();
    NO_THINKING_KINDS.contains(&kind.as_str())
}

/// 起标题用 `reasoning_effort` 的哪个值。
const NO_THINKING_VALUE: &str = "none";

/// 给 provider 套一层：每一次请求都带上「别思考」。
///
/// 只包一层、只为了标题这一次调用 —— 主对话是要思考的，那个走原来的 provider。
pub struct NoThinking {
    inner: Arc<dyn LlmProvider>,
}

impl NoThinking {
    pub fn new(inner: Arc<dyn LlmProvider>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl LlmProvider for NoThinking {
    async fn stream(&self, req: ChatRequest) -> carter::Result<EventStream> {
        let mut req = req;
        // 用 `extra` 而不是 IR 的 `thinking` 字段：IR 里那个是"用多少思考"，
        // 表达不了"一点都不要"，而且 chat completions 的编码器根本不读它。
        // `extra` 是原样并进请求体的通道 —— 这正是它存在的理由。
        req.wire
            .extra
            .insert("reasoning_effort".into(), serde_json::json!(NO_THINKING_VALUE));
        self.inner.stream(req).await
    }
}

/// 生成会话标题。
///
/// `kind` 是渠道种类（`config.provider_for(model.provider).kind`）——
/// 它决定要不要关思考，见模块文档。
pub async fn generate(
    first_prompt: &str,
    provider: Arc<dyn LlmProvider>,
    model: &ModelInfo,
    kind: &str,
) -> carter::Result<String> {
    let provider: Arc<dyn LlmProvider> = if supports_no_thinking(kind) {
        Arc::new(NoThinking::new(provider))
    } else {
        provider
    };
    carter::agent::generate_title(first_prompt, &*provider, model).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use carter::provider::{Block, Role, StreamChunk, WireMessage};
    use std::sync::Mutex;

    /// 记下收到的请求，回一个空流 —— 只为验证包装层往请求里塞了什么。
    struct Recorder {
        seen: Mutex<Option<ChatRequest>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for Recorder {
        async fn stream(&self, req: ChatRequest) -> carter::Result<EventStream> {
            *self.seen.lock().unwrap() = Some(req);
            Ok(Box::pin(futures::stream::iter(Vec::<carter::Result<StreamChunk>>::new())))
        }
    }

    fn request() -> ChatRequest {
        let mut wire = carter::provider::WireRequest::new("deepseek-flash");
        wire.system = vec![Block::text("起个标题")];
        wire.messages = vec![WireMessage::new(Role::User, vec![Block::text("你好")])];
        ChatRequest { protocol: None, wire }
    }

    /// 关思考是**加一个字段**，不是改别的：请求的其余部分要原样过去。
    #[tokio::test]
    async fn the_wrapper_adds_the_field_and_changes_nothing_else() {
        let inner = Arc::new(Recorder {
            seen: Mutex::new(None),
        });
        let wrapped = NoThinking::new(inner.clone());
        let _ = wrapped.stream(request()).await;

        let seen = inner.seen.lock().unwrap().take().expect("请求应到达内层");
        assert_eq!(seen.wire.extra["reasoning_effort"], serde_json::json!("none"));
        assert_eq!(seen.wire.model, "deepseek-flash");
        assert_eq!(seen.wire.messages.len(), 1, "消息不能被改动");
    }

    /// 白名单只认实测过的渠道，其余一律不加字段。
    ///
    /// 这条测试挡的是「顺手给所有渠道都加上」—— 不认这个字段的厂商会直接 400，
    /// 那等于把本来能用的标题弄坏（Ollama 之类对未知字段很严）。
    #[test]
    fn only_verified_kinds_get_the_field() {
        assert!(supports_no_thinking("deepseek"));
        assert!(supports_no_thinking("DeepSeek "), "大小写与空格不该影响判断");
        for other in ["anthropic", "openai", "ollama", "gemini", "groq", "custom"] {
            assert!(!supports_no_thinking(other), "{other} 没实测过，不该发这个字段");
        }
    }
}