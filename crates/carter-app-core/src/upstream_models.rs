//! 从渠道的上游要一份模型清单（`GET {base_url}/…/models`）。
//!
//! 新建渠道时最烦的一步是「把上游有哪些模型敲一遍」。中转站（new-api / one-api）、
//! OpenRouter、各家官方的 OpenAI 兼容端点都有这个只读接口，问一遍就能拿到；
//! 拿回来再交给 [`crate::catalog::match_metas`] 去对 models.dev 的元数据。
//!
//! ## 为什么不复用 carter / llm_adapt 的 HTTP 客户端
//!
//! 它们只发**聊天**请求，`ClientConfig.path` 那条覆盖路径是给 `/chat/completions`
//! 用的；拿来拼 `/models` 要么伪造一个 `ClientConfig`，要么把这个 crate 对底座内部
//! 拼 URL 的细节（去尾斜杠、路径覆盖）了解得太深。这里只是 GET 一个 JSON，
//! 自己拼 URL 反而更短、更好读，也更容易在认错 URL 时给出可读的报错。
//!
//! ## 代理
//!
//! 用 reqwest 的默认行为（`system-proxy`）：跑在代理后面的机器上，拉模型列表要是
//! 不走代理就会一直超时。注意 llm_adapt 是**显式禁用**系统代理的（它只认配置里的
//! `proxy`），两者口径不同是有意的 —— 那是请求链路的一部分，这里只是探测。

use std::time::Duration;

use crate::catalog::MetaMatch;

/// 一次拉取的结果。
pub struct Fetched {
    /// 实际请求的 URL。出错、或者用户想确认「它到底打到了哪」时看这个。
    pub url: String,
    pub models: Vec<String>,
}

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

/// 按渠道的协议补鉴权头。
///
/// 两种都补上是刻意的：Anthropic 只认 `x-api-key`，OpenAI 兼容端点只认 Bearer，
/// 对方不认识的头直接忽略。于是同一份代码既能问官方 anthropic，也能问中转站。
fn apply_auth(
    req: reqwest::RequestBuilder,
    kind_protocol: &str,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if api_key.is_empty() {
        return req;
    }
    match kind_protocol {
        "anthropic" => req
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01"),
        _ => req.header("authorization", format!("Bearer {api_key}")),
    }
}

/// 模型清单的 URL。
///
/// 规则：`base_url` 末尾已经是版本段（`/v1`、`/v4`、`/v1beta/openai` 这种）就直接接
/// `/models`；否则补 `/v1/models`。这正是各家文档里写的那个地址，少补一次或多补一次
/// 都会得到 404，而 404 在日志里和「路径写错」长得一模一样。
pub fn models_url(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return String::new();
    }
    let last = base.rsplit('/').next().unwrap_or(base);
    let versioned = last.len() >= 2
        && last.starts_with('v')
        && last[1..].chars().next().is_some_and(|c| c.is_ascii_digit());
    // `…/v1beta/openai`、`…/compatible-mode/v1`、`…/api/paas/v4` 都算「已经到版本层」。
    if versioned || last == "openai" {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    }
}

/// 解析各种形状的模型列表响应。
///
/// - OpenAI 兼容：`{"data":[{"id":"…"}, …]}`
/// - Anthropic：`{"data":[{"id":"…"}], "has_more":…}`
/// - 少数实现只给 `{"models":["a","b"]}` 或裸数组 `["a","b"]`
///
/// `models/` 前缀要剥掉（Gemini 原生写法），带着它发给上游会 404。
fn parse_models(body: &[u8]) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("响应不是合法 JSON：{e}"))?;

    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let id = raw.strip_prefix("models/").unwrap_or(raw).trim();
        if !id.is_empty() && !out.iter().any(|x| x == id) {
            out.push(id.to_string());
        }
    };

    let items: Option<&Vec<serde_json::Value>> = value
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| value.get("models").and_then(|v| v.as_array()));
    match items {
        Some(arr) => {
            for item in arr {
                // `{"id": …}` 最常见；`{"name": …}` 是 Gemini / 少数中转站的写法。
                if let Some(s) = item.as_str() {
                    push(s);
                    continue;
                }
                let raw = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("name").and_then(|v| v.as_str()));
                if let Some(raw) = raw {
                    push(raw);
                }
            }
        }
        None => {
            // 裸数组，或者 `{"models":["a","b"]}` 这种字符串数组。
            let arr = value
                .as_array()
                .cloned()
                .or_else(|| value.get("models").and_then(|v| v.as_array()).cloned());
            let Some(arr) = arr else {
                return Err(
                    "认不出上游返回的模型列表格式（既没有 data 也没有 models 数组）".into(),
                );
            };
            for item in &arr {
                if let Some(s) = item.as_str() {
                    push(s);
                }
            }
        }
    }

    if out.is_empty() {
        return Err("上游返回了模型列表，但里面没有任何模型名".into());
    }
    out.sort();
    Ok(out)
}

fn describe(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "请求超时（检查 base_url 是否可达，或是否需要代理）".to_string()
    } else if e.is_connect() {
        format!("连不上上游：{e}")
    } else {
        e.to_string()
    }
}

/// 向一个渠道要模型清单。
///
/// `kind` 用来决定鉴权头（走 [`crate::protocol`] 同一套归一：`anthropic` 用
/// `x-api-key`，其余用 Bearer）。`api_key` 必须已经做过 `${ENV}` 展开 ——
/// 调用方负责，见 `src-tauri` 里那条命令。
pub async fn fetch(kind_protocol: &str, base_url: &str, api_key: &str) -> Result<Fetched, String> {
    let url = models_url(base_url);
    if url.is_empty() {
        return Err("还没填 Base URL —— 不知道去哪里问模型列表".into());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        // 有的中转站证书链不全，直接 TLS 失败时的报错比 404 更难懂。
        .user_agent(concat!("carter-app/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("建 HTTP 客户端失败：{e}"))?;

    let req = apply_auth(client.get(&url), kind_protocol, api_key);
    let resp = req.send().await.map_err(|e| describe(&e))?;

    let status = resp.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 | 403 => "密钥不对，或这个渠道不接受拉列表",
            404 => "这个地址下没有 /v1/models —— 检查 Base URL 是否多写/少写了 /v1",
            429 => "被限流了，稍等再试",
            _ => "上游拒绝了这次请求",
        };
        return Err(format!("{hint}（HTTP {}）：{url}", status.as_u16()));
    }

    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("读取响应失败：{}", describe(&e)))?;
    Ok(Fetched {
        url,
        models: parse_models(&body)?,
    })
}

/// 只填了 kind 的一份渠道配置 —— 借底座那套「kind → 线协议 / 默认端点」的表，
/// 不在这里抄第二份（那份表会漂）。
fn bare_provider(kind: &str) -> carter::config::ProviderConfig {
    carter::config::ProviderConfig {
        kind: kind.to_string(),
        api_key: None,
        base_url: None,
        models: Default::default(),
        server_tools: Vec::new(),
        server_tool_specs: Vec::new(),
        server_tool_max_uses: None,
    }
}

/// 拉一次上游模型清单，顺手把每条都对到 models.dev 的元数据上。
///
/// `base_url` 留空时用协议的默认地址（与真正发聊天请求时同一个来源）—— 否则
/// 「填了密钥、类型选 DeepSeek、地址留空」这种最省事的配置会在这里报「没填 Base URL」。
pub async fn fetch_matched(
    kind: &str,
    base_url: &str,
    api_key: &str,
) -> Result<UpstreamModels, String> {
    let provider = bare_provider(kind);
    let resolved = if base_url.trim().is_empty() {
        provider
            .to_client_config()
            .map(|c| c.base_url)
            .map_err(|_| "还没填 Base URL，这个协议也没有默认地址".to_string())?
    } else {
        base_url.trim().to_string()
    };
    let protocol = provider.protocol().to_string();

    let fetched = fetch(&protocol, &resolved, api_key).await?;
    let provider_hint = crate::catalog::detect_provider(&resolved);
    let models = crate::catalog::match_metas(&fetched.models, provider_hint.as_deref());
    Ok(UpstreamModels {
        url: fetched.url,
        provider_hint,
        models,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_gets_the_version_segment_exactly_once() {
        for (base, want) in [
            (
                "https://api.deepseek.com",
                "https://api.deepseek.com/v1/models",
            ),
            (
                "https://api.anthropic.com/",
                "https://api.anthropic.com/v1/models",
            ),
            (
                "https://openrouter.ai/api/v1",
                "https://openrouter.ai/api/v1/models",
            ),
            (
                "https://api.groq.com/openai/v1",
                "https://api.groq.com/openai/v1/models",
            ),
            (
                "https://open.bigmodel.cn/api/paas/v4",
                "https://open.bigmodel.cn/api/paas/v4/models",
            ),
            (
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "https://generativelanguage.googleapis.com/v1beta/openai/models",
            ),
            // 中转站：裸域名，要补 /v1。
            (
                "http://192.168.31.1:3000",
                "http://192.168.31.1:3000/v1/models",
            ),
        ] {
            assert_eq!(models_url(base), want, "base={base}");
        }
        assert_eq!(models_url("  "), "");
    }

    #[test]
    fn parses_the_shapes_upstreams_actually_return() {
        let openai = br#"{"object":"list","data":[{"id":"gpt-5.6"},{"id":"gpt-4.1"}]}"#;
        assert_eq!(parse_models(openai).unwrap(), ["gpt-4.1", "gpt-5.6"]);

        // Anthropic 的分页字段不影响解析。
        let anthropic = br#"{"data":[{"id":"claude-opus-5","type":"model"}],"has_more":false}"#;
        assert_eq!(parse_models(anthropic).unwrap(), ["claude-opus-5"]);

        // Gemini 原生会带 `models/` 前缀；带着它发回上游是 404。
        let gemini = br#"{"models":[{"name":"models/gemini-3.1-pro"}]}"#;
        assert_eq!(parse_models(gemini).unwrap(), ["gemini-3.1-pro"]);

        // 少数实现给字符串数组，还可能在两个字段里重复。
        let loose = br#"["kimi-k3", "kimi-k3", "glm-5.3"]"#;
        assert_eq!(parse_models(loose).unwrap(), ["glm-5.3", "kimi-k3"]);
    }

    #[test]
    fn an_unrecognisable_body_says_so_instead_of_returning_nothing() {
        assert!(parse_models(br#"{"error":"nope"}"#).is_err());
        assert!(parse_models(b"<html>502</html>").is_err());
        assert!(parse_models(br#"{"data":[]}"#).is_err());
    }
}
