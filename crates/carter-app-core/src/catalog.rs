//! 模型目录：把所有渠道下的 `provider/model` 摊平成一张带元数据的表。
//!
//! 界面上「模型」页要一次看到**全部**可用的模型引用（而不只是某个渠道下的），
//! 并且知道每一个的上下文窗口、能力与定价 —— 这些都是 models.dev 缓存里的数据，
//! 经 `carter::registry::resolve_model` 解出来。
//!
//! 一条解析不了**不能**让整页失败：最常见的原因是 `meta` 写错了、或者本地还没有
//! models.dev 缓存。那种情况下把错误挂在该行上，其余行照常显示。

use serde::Serialize;

use carter::config::Config;
use carter::registry::{Capability, ModelInfo, Pricing, fetch, resolve_model};

#[derive(Debug, Clone, Serialize)]
pub struct PricingRow {
    /// 每百万 token 的美元价。
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

impl From<&Pricing> for PricingRow {
    fn from(p: &Pricing) -> Self {
        Self {
            input: p.input,
            output: p.output,
            cache_read: p.cache_read,
            cache_write: p.cache_write,
        }
    }
}

fn cap_str(c: &Capability) -> &'static str {
    match c {
        Capability::Tools => "tools",
        Capability::Vision => "vision",
        Capability::Thinking => "thinking",
        Capability::Pdf => "pdf",
        Capability::Streaming => "streaming",
        Capability::PromptCache => "prompt_cache",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelRow {
    /// `provider/model` —— 配置里 `agent.model` 用的就是这种形式。
    pub reference: String,
    pub provider: String,
    /// 配置块名。
    pub name: String,
    /// 真正下发给端点的模型名。
    pub api_name: String,
    /// models.dev 的查找 id。
    pub meta: String,
    pub is_default: bool,
    /// 渠道走哪种线协议（kind 可能是别名）。
    pub protocol: String,
    /// 配置里给这个模型指定的默认推理档位，**原样**（字符串或 `{ budget = n }`）。
    pub default_reasoning: Option<serde_json::Value>,

    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub capabilities: Vec<&'static str>,
    pub pricing: Option<PricingRow>,

    /// 解析元数据失败的原因。有值时上面那几个字段是空的，但这一行仍然有效 ——
    /// 用户还需要能删掉它、或者改它的 meta。
    pub error: Option<String>,
}

fn resolve_one(
    config: &Config,
    cache: &Result<String, String>,
    reasoning: &std::collections::HashMap<(String, String), serde_json::Value>,
    provider: &str,
    name: &str,
    default_ref: &str,
    protocol: &str,
) -> ModelRow {
    let reference = format!("{provider}/{name}");
    let reasoning_raw = reasoning.get(&(provider.to_string(), name.to_string())).cloned();
    let mut row = ModelRow {
        reference: reference.clone(),
        provider: provider.to_string(),
        name: name.to_string(),
        api_name: name.to_string(),
        meta: String::new(),
        is_default: reference == default_ref,
        protocol: protocol.to_string(),
        default_reasoning: reasoning_raw,
        context_window: None,
        max_output_tokens: None,
        capabilities: Vec::new(),
        pricing: None,
        error: None,
    };

    // 先补上配置里的原始值，好让「解析失败」的那几行也显示得出 meta。
    if let Some(entry) = config
        .providers
        .get(provider)
        .and_then(|p| p.models.get(name))
    {
        row.meta = entry.meta.clone();
        row.api_name = entry.api_name.clone().unwrap_or_else(|| name.to_string());
        row.error = Some(format!(
            "models.dev 缓存里没有 `{}` —— 检查 meta，或点「刷新模型元数据」",
            entry.meta
        ));
    }

    let cache = match cache {
        Ok(c) => c,
        Err(e) => {
            row.error = Some(e.clone());
            return row;
        }
    };

    match resolve_model(config, cache, &reference) {
        Ok(info) => {
            fill(&mut row, &info);
            row.error = None;
        }
        Err(e) => row.error = Some(e.to_string()),
    }
    row
}

fn fill(row: &mut ModelRow, info: &ModelInfo) {
    row.api_name = info.api_name.clone();
    row.context_window = Some(info.context_window);
    row.max_output_tokens = Some(info.max_output_tokens);
    row.capabilities = info.capabilities.iter().map(cap_str).collect();
    row.pricing = Some((&info.pricing).into());
}

/// 全部模型。顺序：按渠道名，再按配置里的模型名。
pub fn list(config: &Config) -> Vec<ModelRow> {
    // 缓存读不到不算致命：照样把模型列出来，只是元数据那几栏空着。
    let cache = fetch::read_cache().map_err(|e| e.to_string());
    let default_ref = config.agent.model.clone();
    // `default_reasoning` 取配置里的**原值** —— `ModelInfo` 上那个字段已经被解析成
    // `ReasoningEffort`，`{ budget = 1024 }` 这种写法反推不回来。
    let reasoning = crate::config_edit::raw_default_reasoning();

    let mut providers: Vec<&String> = config.providers.keys().collect();
    providers.sort();

    let mut rows = Vec::new();
    for pname in providers {
        let pcfg = &config.providers[pname];
        let protocol = pcfg.protocol().to_string();
        let mut names: Vec<&String> = pcfg.models.keys().collect();
        names.sort();
        for mname in names {
            rows.push(resolve_one(
                config,
                &cache,
                &reasoning,
                pname,
                mname,
                &default_ref,
                &protocol,
            ));
        }
    }
    rows
}

/// 拉一次 models.dev 目录并落盘。返回模型条数。
pub async fn refresh() -> Result<usize, String> {
    let json = fetch::fetch_models_dev()
        .await
        .map_err(|e| format!("拉取 models.dev 失败：{e}"))?;
    fetch::write_cache(&json).map_err(|e| format!("写缓存失败：{e}"))?;

    // 数一下缓存里有多少模型，好给用户一个「确实拉到了东西」的反馈。
    let v: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("目录内容无法解析：{e}"))?;
    let count = v
        .as_object()
        .map(|providers| {
            providers
                .values()
                .filter_map(|p| p.get("models").and_then(|m| m.as_object()))
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(meta: &str) -> Config {
        let toml = format!(
            r#"
[agent]
model = "ws/sonnet"

[providers.ws]
kind = "anthropic"

  [providers.ws.models.sonnet]
  meta = "{meta}"
"#
        );
        toml_edit::de::from_str(&toml).unwrap()
    }

    /// 缓存读不到时，列表仍然要有内容 —— 用户至少得能看到自己配了哪些模型、
    /// 以及那条能把人引向「刷新」的错误。
    #[test]
    fn a_missing_cache_still_produces_rows_with_an_explanation() {
        let cfg = config_with("anthropic/claude-sonnet-4-5");
        let cache: Result<String, String> = Err("还没有 models.dev 缓存".into());
        let row = resolve_one(
            &cfg,
            &cache,
            &Default::default(),
            "ws",
            "sonnet",
            "ws/sonnet",
            "anthropic",
        );

        assert_eq!(row.reference, "ws/sonnet");
        assert!(row.is_default);
        assert_eq!(row.meta, "anthropic/claude-sonnet-4-5");
        assert_eq!(row.error.as_deref(), Some("还没有 models.dev 缓存"));
        assert!(row.context_window.is_none());
    }

    /// 元数据解析成功时，各栏要真的填上。
    #[test]
    fn a_resolvable_model_gets_its_metadata_filled_in() {
        // 形状照 models.dev 实际的字段来：`vision` 要 `attachment` 与 image 模态**同时**成立
        // （见 carter 的 `registry/models_dev.rs` 能力推导），少一个就推不出 Vision。
        let cache = Ok(r#"{"anthropic":{"models":{"claude-sonnet-4-5":{
            "id":"claude-sonnet-4-5","tool_call":true,"reasoning":true,"attachment":true,
            "modalities":{"input":["text","image"]},
            "limit":{"context":200000,"output":64000},
            "cost":{"input":3,"output":15,"cache_read":0.3}
        }}}}"#
            .to_string());
        let cfg = config_with("anthropic/claude-sonnet-4-5");
        let row = resolve_one(
            &cfg,
            &cache,
            &Default::default(),
            "ws",
            "sonnet",
            "ws/sonnet",
            "anthropic",
        );

        assert!(row.error.is_none(), "不该报错：{:?}", row.error);
        assert_eq!(row.context_window, Some(200_000));
        assert_eq!(row.max_output_tokens, Some(64_000));
        assert!(row.capabilities.contains(&"tools"));
        assert!(row.capabilities.contains(&"vision"));
        assert!(row.capabilities.contains(&"thinking"));
        // 声明了 cache_read 价 → 推得出 prompt cache。
        assert!(row.capabilities.contains(&"prompt_cache"));
        assert_eq!(row.pricing.as_ref().unwrap().input, 3.0);
        // 块名是 sonnet，api_name 省略时回落到块名。
        assert_eq!(row.api_name, "sonnet");
    }

    /// 一条解析不了不该影响别的行 —— 这是「单行失败不整页失败」的落点。
    #[test]
    fn a_bad_meta_only_marks_its_own_row() {
        let cache = Ok(r#"{"anthropic":{"models":{"claude-sonnet-4-5":{
            "id":"claude-sonnet-4-5","limit":{"context":200000,"output":64000},
            "cost":{"input":3,"output":15}
        }}}}"#
            .to_string());

        let none = Default::default();
        let good = resolve_one(
            &config_with("anthropic/claude-sonnet-4-5"),
            &cache,
            &none,
            "ws",
            "sonnet",
            "ws/sonnet",
            "anthropic",
        );
        let bad = resolve_one(
            &config_with("anthropic/does-not-exist"),
            &cache,
            &none,
            "ws",
            "sonnet",
            "ws/sonnet",
            "anthropic",
        );

        assert!(good.error.is_none());
        assert!(bad.error.is_some(), "查不到的 meta 应当在这一行上报错");
        assert_eq!(bad.meta, "anthropic/does-not-exist", "报错行也要显示 meta");
    }
}
