//! 从渠道的上游要一份模型清单（`GET {base_url}/…/models`），再对到目录元数据上。
//!
//! 新建渠道时最烦的一步是「把上游有哪些模型敲一遍」。中转站（new-api / one-api）、
//! OpenRouter、各家官方的 OpenAI 兼容端点都有这个只读接口，问一遍就能拿到。
//!
//! ## 拉取那一半在 carter
//!
//! URL 怎么拼（`.../v1` 结尾不重复补、`/v4`、`/v1beta/openai` 都算版本段）、三种协议
//! 各自的鉴权头、各家返回的几种形状（Gemini 的 `models/` 前缀、裸数组、字符串数组）
//! 全都在 `carter::provider::models_api` —— CLI 的向导与 `/model` 用的是同一份。
//! 这里只留 app 独有的一步：**把拉回来的名字对到 models.dev 的元数据上**。
//!
//! ## 代理
//!
//! 用 reqwest 的默认行为（`system-proxy`）：跑在代理后面的机器上，拉模型列表要是
//! 不走代理就会一直超时。注意 llm_adapt 是**显式禁用**系统代理的（它只认配置里的
//! `proxy`），两者口径不同是有意的 —— 那是请求链路的一部分，这里只是探测。

use crate::catalog::MetaMatch;

/// 拉取 + 对元数据。界面只要一次往返。
#[derive(Debug, serde::Serialize)]
pub struct UpstreamModels {
    /// 实际请求的 URL（`base_url` 留空用了协议默认地址时，这里是那个默认地址）。
    pub url: String,
    /// 从 `base_url` 认出来的 models.dev 服务商（认不出是 `None`）。
    /// 匹配元数据时当提示用：同样叫 `grok-4.5`，xAI 名下和转售商名下是两个价。
    pub provider_hint: Option<String>,
    pub models: Vec<MetaMatch>,
}

/// 拉一次上游模型清单，顺手把每条都对到 models.dev 的元数据上。
///
/// `base_url` 留空时用协议的默认地址（与真正发聊天请求时同一个来源）—— 否则
/// 「填了密钥、类型选 DeepSeek、地址留空」这种最省事的配置会在这里报「没填 Base URL」。
/// `api_key` 必须已经做过 `${ENV}` 展开，调用方负责（见 `src-tauri` 里那条命令）。
pub async fn fetch_matched(
    kind: &str,
    base_url: &str,
    api_key: &str,
) -> Result<UpstreamModels, String> {
    let provider = carter::provider::models_api::probe_provider(kind, base_url, api_key);
    let fetched = carter::provider::models_api::fetch_models(&provider).await?;

    // 认服务商要用**真正请求的地址**：`base_url` 留空时上面那份配置里也没有它，
    // 而协议默认地址一样能认出是哪一家。
    let provider_hint = crate::catalog::detect_provider(&fetched.url);
    let names: Vec<String> = fetched.models.into_iter().map(|m| m.id).collect();
    let models = crate::catalog::match_metas(&names, provider_hint.as_deref());
    Ok(UpstreamModels {
        url: fetched.url,
        provider_hint,
        models,
    })
}

#[cfg(test)]
mod tests {
    /// 认服务商要用实际请求的 URL —— 用户留空 `base_url` 时只有它知道打到了哪。
    /// 这条守的是 `fetch_matched` 里那个 `&fetched.url`。
    #[test]
    fn the_provider_is_detected_from_the_requested_url() {
        assert_eq!(
            crate::catalog::detect_provider("https://api.anthropic.com/v1/models"),
            Some("anthropic".to_string())
        );
        assert_eq!(
            crate::catalog::detect_provider("http://localhost:11434/v1/models"),
            None
        );
    }

    /// `probe_provider` 要接受「只有 kind 和密钥、地址留空」这种最省事的配置，
    /// 并让 carter 落回协议默认地址 —— 否则新建渠道第一步就报「没填 Base URL」。
    #[tokio::test]
    async fn an_empty_base_url_falls_back_to_the_protocol_default() {
        let p = carter::provider::models_api::probe_provider("deepseek", "", "");
        let base = p.to_client_config().map(|c| c.base_url).unwrap_or_default();
        assert!(
            base.contains("deepseek.com"),
            "应落回协议默认地址，实际是 `{base}`"
        );
    }
}
