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
    /// 是不是**副模型**（`agent.fast_model`）：压缩上下文、生成标题、总结用的那个。
    /// 没配就是主模型自己上（`false`）。
    pub is_fast: bool,
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
    fast_ref: Option<&str>,
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
        is_fast: fast_ref == Some(reference.as_str()),
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
    let fast_ref = config.agent.fast_model.clone();
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
                fast_ref.as_deref(),
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

// ---------------------------------------------------------------------------
// 按服务商浏览 models.dev 目录
//
// 官方渠道（deepseek / anthropic / openai …）本该只填一个密钥就能用。卡住人的是
// `meta`：它必须和 models.dev 的 `id` 一字不差，而 id 和展示名常常对不上
// （「DeepSeek V4.1 Flash」的 id 是 `deepseek-flash`）。所以界面不再让人手写，
// 而是从这张表里选。
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    /// models.dev 的模型 id —— `meta` 的后半截，也是官方端点认的模型名。
    pub id: String,
    /// 展示名，如「DeepSeek V4.1 Flash」。
    pub name: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub reasoning: bool,
    /// models.dev 标了 `status = "deprecated"`。
    pub deprecated: bool,
    pub release_date: Option<String>,
}

fn parse_cache(cache_json: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(cache_json).map_err(|e| format!("models.dev 缓存无法解析：{e}"))
}

fn entries_of(root: &serde_json::Value, provider_id: &str) -> Vec<CatalogEntry> {
    let Some(models) = root
        .get(provider_id)
        .and_then(|p| p.get("models"))
        .and_then(|m| m.as_object())
    else {
        return Vec::new();
    };
    let mut out: Vec<CatalogEntry> = models
        .iter()
        .map(|(id, m)| {
            let s = |k: &str| m.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let limit = |k: &str| m.get("limit").and_then(|l| l.get(k)).and_then(|v| v.as_u64());
            CatalogEntry {
                id: id.clone(),
                name: s("name").unwrap_or_else(|| id.clone()),
                context_window: limit("context"),
                max_output_tokens: limit("output"),
                reasoning: m.get("reasoning").and_then(|v| v.as_bool()).unwrap_or(false),
                deprecated: s("status").as_deref() == Some("deprecated"),
                release_date: s("release_date"),
            }
        })
        .collect();
    // 能用的排前面，同组里新的排前面 —— 下拉框第一项基本就是想要的那个。
    out.sort_by(|a, b| {
        a.deprecated
            .cmp(&b.deprecated)
            .then_with(|| b.release_date.cmp(&a.release_date))
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// 某个 models.dev 服务商下的全部模型。缓存里没有这个服务商时返回空表（不是错误）。
pub fn provider_models(provider_id: &str) -> Result<Vec<CatalogEntry>, String> {
    let cache = fetch::read_cache().map_err(|e| format!("还没有 models.dev 缓存：{e}"))?;
    Ok(entries_of(&parse_cache(&cache)?, provider_id))
}

/// 校验一个 `meta` 能不能在缓存里查到。查不到时尽量给出正确写法。
///
/// 常见的写错方式是照着展示名或 `canonical_model_id` 填（`deepseek/deepseek-v4.1-flash`），
/// 而查找用的是 `id`（`deepseek/deepseek-flash`）—— 这两种都能反查出来。
pub fn check_meta(root: &serde_json::Value, meta: &str) -> Result<(), String> {
    let meta = meta.trim();
    let Some((pid, mid)) = meta.split_once('/') else {
        return Err(format!("meta `{meta}` 格式应为 `服务商/模型id`，如 deepseek/deepseek-flash"));
    };
    let Some(models) = root
        .get(pid)
        .and_then(|p| p.get("models"))
        .and_then(|m| m.as_object())
    else {
        return Err(format!("models.dev 里没有服务商 `{pid}`（meta `{meta}`）"));
    };
    if models.contains_key(mid) {
        return Ok(());
    }
    let norm = |s: &str| s.to_lowercase().replace([' ', '_', '.'], "-");
    let suggestion = models.iter().find_map(|(id, m)| {
        let canonical = m.get("canonical_model_id").and_then(|v| v.as_str());
        let name = m.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        let hit = canonical == Some(meta)
            || canonical.and_then(|c| c.split_once('/')).map(|(_, c)| c) == Some(mid)
            || norm(name) == norm(mid)
            || id.eq_ignore_ascii_case(mid);
        hit.then(|| format!("{pid}/{id}"))
    });
    Err(match suggestion {
        Some(s) => format!("models.dev 里查不到 `{meta}` —— 应该写成 `{s}`"),
        None => format!(
            "models.dev 里查不到 `{meta}`；`{pid}` 下可用的有：{}",
            entries_of(root, pid)
                .iter()
                .filter(|e| !e.deprecated)
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>()
                .join("、")
        ),
    })
}

/// 保存前校验一组 meta。**还没有缓存时放行** —— 那种情况下查不了，不该挡住保存，
/// 模型页会把「去刷新元数据」挂在那几行上。
pub fn check_metas<'a>(metas: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let Ok(cache) = fetch::read_cache() else {
        return Ok(());
    };
    let root = parse_cache(&cache)?;
    metas.into_iter().try_for_each(|m| check_meta(&root, m))
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
            None,
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
            None,
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
            None,
            "anthropic",
        );
        let bad = resolve_one(
            &config_with("anthropic/does-not-exist"),
            &cache,
            &none,
            "ws",
            "sonnet",
            "ws/sonnet",
            None,
            "anthropic",
        );

        assert!(good.error.is_none());
        assert!(bad.error.is_some(), "查不到的 meta 应当在这一行上报错");
        assert_eq!(bad.meta, "anthropic/does-not-exist", "报错行也要显示 meta");
    }

    fn dev_cache() -> serde_json::Value {
        serde_json::from_str(r#"{"deepseek":{"models":{
            "deepseek-flash":{"id":"deepseek-flash","name":"DeepSeek V4.1 Flash",
              "canonical_model_id":"deepseek/deepseek-v4.1-flash","reasoning":true,
              "release_date":"2026-09-10","limit":{"context":1000000,"output":393216}},
            "deepseek-v4-flash":{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash",
              "status":"deprecated","release_date":"2026-09-10"},
            "deepseek-v4-pro":{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro",
              "release_date":"2026-08-12"}
        }}}"#)
        .unwrap()
    }

    /// 照着 canonical id 填的 meta 要能反查出正确写法 —— 这正是用户真实踩过的坑。
    #[test]
    fn a_canonical_meta_is_rejected_with_the_right_suggestion() {
        let root = dev_cache();
        assert!(check_meta(&root, "deepseek/deepseek-flash").is_ok());
        let err = check_meta(&root, "deepseek/deepseek-v4.1-flash").unwrap_err();
        assert!(err.contains("deepseek/deepseek-flash"), "{err}");
    }

    #[test]
    fn an_unknown_meta_lists_the_live_models() {
        let err = check_meta(&dev_cache(), "deepseek/nope").unwrap_err();
        assert!(err.contains("deepseek-v4-pro"), "{err}");
        assert!(!err.contains("deepseek-v4-flash"), "弃用的不该推荐：{err}");
    }

    #[test]
    fn catalog_puts_live_and_newest_models_first() {
        let ids: Vec<String> = entries_of(&dev_cache(), "deepseek").into_iter().map(|e| e.id).collect();
        assert_eq!(ids, ["deepseek-flash", "deepseek-v4-pro", "deepseek-v4-flash"]);
        assert!(entries_of(&dev_cache(), "nobody").is_empty());
    }
}
