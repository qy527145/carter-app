//! 模型目录：把所有渠道下的 `provider/model` 摊平成一张带元数据的表。
//!
//! 界面上「模型」页要一次看到**全部**可用的模型引用（而不只是某个渠道下的），
//! 并且知道每一个的上下文窗口、能力与定价 —— 这些都是 models.dev 缓存里的数据，
//! 经 `carter::registry::resolve_model` 解出来。
//!
//! 一条解析不了**不能**让整页失败：最常见的原因是 `meta` 写错了、或者本地还没有
//! models.dev 缓存。那种情况下把错误挂在该行上，其余行照常显示。

use std::collections::{HashMap, HashSet};

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
    /// 目录里的展示名（「Claude Opus 5.5」）。界面上的模型标签用它 —— 用户配的是
    /// `opus`，请求里发的是 `claude-opus-5-5`，两者都不如这个名字好认。
    /// 没绑 meta、或目录查不到时是 `None`，界面退回显示引用本身。
    pub label: Option<String>,
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
    // 已经解析好的缓存，只用来取展示名（`label`）。解析一次给整张表用 ——
    // 每个模型都重新解析一遍近万条的 JSON 会明显拖慢「模型」页。
    root: Option<&serde_json::Value>,
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
        label: None,
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

    // 展示名在缓存里查得到就先填上 —— 解析失败的那一行也需要它（界面上那个
    // 标签不能因为缓存里少了一条就退回成 `ws/sonnet` 这种内部写法）。
    if let (Some(root), Some((pid, mid))) = (root, row.meta.split_once('/')) {
        if let Some((pid, mid)) = canonical_key(root, pid, mid) {
            row.label = model_label(root, &pid, &mid);
        }
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
    // 展示名要另外拿一份解析好的缓存：`resolve_model` 只认「这条能解析」，而
    // 解析失败的那几行同样需要展示名（界面上的标签不能退回成 `ws/sonnet`）。
    let root = cache.as_ref().ok().and_then(|c| parse_cache(c).ok());
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
                root.as_ref(),
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
    /// 能力标记，与 [`ModelRow::capabilities`] 同一套字符串。挑模型时要一眼看出
    /// 「这个能不能看图 / 会不会思考」，光有名字只能靠猜。
    pub capabilities: Vec<&'static str>,
    /// 同一系列的名字（`claude-opus` / `deepseek-v4`）。目录里按它分组显示 ——
    /// 官方目录动辄几十个快照，平铺一张表看不出谁和谁是同一代。
    pub family: Option<String>,
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
                capabilities: catalog_capabilities(m),
                family: s("family"),
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

// ---------------------------------------------------------------------------
// 上游模型名 → 目录条目
//
// 中转站（new-api / one-api / OpenRouter）的 `/v1/models` 给出的是**上游**模型名，
// 跟 models.dev 的 id 经常对不上：`deepseek-v4.1-flash`（目录里的 id 是
// `deepseek-flash`）、`gpt_oss_120b`（id 是 `gpt-oss-120b`）、
// `anthropic/claude-sonnet-4.5`（带厂商前缀、点号写法）。
//
// 这里做的是**纯推断**：名字对不上时给出最可能的目录条目，并标清楚这条推断有多可信
// （见 [`MetaMatch::match_kind`]）。宁可标「不确定」，也不假装确信 —— 绑错元数据会让
// 上下文窗口与定价全错，而且错得无声无息。
// ---------------------------------------------------------------------------

/// 一条「上游模型名 → 目录条目」的推断结果。
#[derive(Debug, Clone, Serialize)]
pub struct MetaMatch {
    /// 上游给的模型名，原样。
    pub name: String,
    /// `models.dev` 的 `服务商/模型id`。推断不出来是 `None`，界面要让人手动绑。
    pub meta: Option<String>,
    /// 推断的可靠度：
    /// - `exact` —— 名字与目录 id 一致（大小写不敏感）；
    /// - `alias` —— 靠归一化写法或 `canonical_model_id` 对上的；
    /// - `reseller` —— 只有转售商条目，原厂没有（价格/窗口可能和原厂不同）；
    /// - `none` —— 没对上。
    pub match_kind: String,
    /// 目录里的展示名。
    pub label: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// 能力标记，与 [`ModelRow::capabilities`] 同一套字符串。勾选时靠它区分
    /// 「能看图的」和「只会聊天的」。
    pub capabilities: Vec<&'static str>,
    pub deprecated: bool,
    /// 看起来像不像对话模型。界面上「只选对话模型」那一下用它 ——
    /// embedding / 语音 / 生图的条目混进模型列表只会占地方。
    pub chat_capable: bool,
}

/// 目录里的一个可选项（全局搜索用，跨服务商）。
#[derive(Debug, Clone, Serialize)]
pub struct CatalogHit {
    /// `服务商/模型id`，可以直接写进 meta。
    pub meta: String,
    pub provider: String,
    pub id: String,
    pub name: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// 能力标记。搜索结果的排序不靠它，靠它是为了让人在「绑元数据」时看得出差别。
    pub capabilities: Vec<&'static str>,
    pub deprecated: bool,
}

/// 目录索引：一次建好，供一轮批量匹配 / 搜索复用。
///
/// 缓存有近万条模型、同一模型名常常出现在几十家转售商下，所以每条都存
/// `(服务商, id)` 而不是只存模型定义 —— 没有服务商就拼不出 meta。
struct Index {
    by_id: HashMap<String, Vec<(String, String)>>,
    by_norm: HashMap<String, Vec<(String, String)>>,
    /// 被**别家**用 `canonical_model_id` 指到过的服务商 = 原厂。
    ///
    /// 指向自己（`canonical_model_id = 自己/自己`）不算 —— 目录里不少转售商也这么写，
    /// 把它们当成原厂会让同名模型挑到转售价上。
    vendors: HashSet<String>,
}

impl Index {
    fn build(root: &serde_json::Value) -> Self {
        let mut idx = Index {
            by_id: HashMap::new(),
            by_norm: HashMap::new(),
            vendors: HashSet::new(),
        };
        let Some(providers) = root.as_object() else {
            return idx;
        };
        for (pid, p) in providers {
            let Some(models) = p.get("models").and_then(|m| m.as_object()) else {
                continue;
            };
            for (mid, m) in models {
                let pair = (pid.clone(), mid.clone());
                idx.by_id
                    .entry(mid.to_ascii_lowercase())
                    .or_default()
                    .push(pair.clone());
                idx.by_norm
                    .entry(normalize_id(mid))
                    .or_default()
                    .push(pair.clone());
                if let Some((vendor, _)) = m
                    .get("canonical_model_id")
                    .and_then(|v| v.as_str())
                    .and_then(|c| c.split_once('/'))
                    && vendor != pid
                {
                    idx.vendors.insert(vendor.to_string());
                }
            }
        }
        idx
    }
}

/// 模型 id 的归一化：去掉厂商前缀、`:free` 之类的后缀，点号/下划线/`@` 一律当连字符，
/// 小写。`gpt_oss_120b`、`openai/gpt-oss-120b`、`gpt-oss-120b` 归一到同一个键。
///
/// 与 cc-switch 的 `normalizeModelsDevModelId` 同规则 —— 两边对同一个名字得出不同的
/// 判断，只会让人怀疑是不是绑错了。
fn normalize_id(id: &str) -> String {
    let after_slash = id.rsplit('/').next().unwrap_or(id);
    let before_colon = after_slash.split(':').next().unwrap_or(after_slash);
    let mut s: String = before_colon
        .trim()
        .chars()
        .map(|c| {
            if c == '@' || c == '_' || c == '.' {
                '-'
            } else {
                c
            }
        })
        .collect::<String>()
        .to_ascii_lowercase();
    if let Some(stripped) = s.strip_suffix("[1m]") {
        s = stripped.trim().to_string();
    }
    s
}

fn model_node<'a>(
    root: &'a serde_json::Value,
    pid: &str,
    mid: &str,
) -> Option<&'a serde_json::Value> {
    root.get(pid)?.get("models")?.get(mid)
}

fn model_label(root: &serde_json::Value, pid: &str, mid: &str) -> Option<String> {
    model_node(root, pid, mid)?
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn model_context(root: &serde_json::Value, pid: &str, mid: &str) -> Option<u64> {
    model_node(root, pid, mid)?
        .get("limit")?
        .get("context")?
        .as_u64()
}

fn model_deprecated(root: &serde_json::Value, pid: &str, mid: &str) -> bool {
    model_node(root, pid, mid)
        .and_then(|m| m.get("status"))
        .and_then(|v| v.as_str())
        == Some("deprecated")
}

/// 看起来是不是一个对话模型。
///
/// `models.dev` 没有「这是 embedding / 语音 / 生图」这种分类字段，但模态里有：对话模型的
/// **输出**一定含 text，而 embedding / whisper / tts / 生图要么没有 text 输出，要么名字里
/// 明说了自己是干什么的。两层都过一遍 —— 光看名字会漏掉 `text-embedding-3-small`
/// （输出确实是 text），光看模态会漏掉 `gpt-image-*`（输出也带 text）。
pub fn is_chat_like(id: &str) -> bool {
    const NON_CHAT: &[&str] = &[
        "embed",
        "whisper",
        "tts",
        "audio",
        "asr",
        "rerank",
        "moderation",
        "image",
        "video",
        "lyria",
        "guard",
        "transcribe",
        "-ocr",
    ];
    let low = id.to_ascii_lowercase();
    !NON_CHAT.iter().any(|k| low.contains(k))
}

fn chat_capable(root: &serde_json::Value, pid: &str, mid: &str, name: &str) -> bool {
    if !is_chat_like(name) {
        return false;
    }
    // 目录里有条目就按输出模态判断；没有（推断失败）时只按名字算。
    match model_node(root, pid, mid) {
        None => true,
        Some(m) => {
            let out = m
                .get("modalities")
                .and_then(|x| x.get("output"))
                .and_then(|x| x.as_array());
            match out {
                None => true,
                Some(list) => list.iter().any(|v| v.as_str() == Some("text")),
            }
        }
    }
}

/// 从同名候选里挑一个「最该相信的」。
///
/// 优先级（与 cc-switch 一致）：
/// 1. 正好来自提示的服务商（渠道的 base_url 认出来的那家）；
/// 2. 原厂自己的条目（供应商被别的条目的 `canonical_model_id` 指到过）；
/// 3. 按服务商名定序取第一个 —— 要的是**确定性**，不是"更对"。
///
/// 转售商条目「指向原厂」的情况由 [`match_one`] 在挑完之后处理：那条候选只是名字的
/// 另一种写法，meta 该落在它指的地方。
fn pick_candidate<'a>(
    index: &Index,
    cands: &'a [(String, String)],
    hint: Option<&str>,
) -> &'a (String, String) {
    let hinted = hint.and_then(|h| cands.iter().find(|(p, _)| p == h));
    if let Some(hit) = hinted {
        return hit;
    }
    if let Some(best) = cands.iter().find(|(p, _)| index.vendors.contains(p)) {
        return best;
    }
    &cands[0]
}

/// 一条名字的匹配。`hint` 是渠道认出来的目录服务商（可以为空）。
fn match_one(root: &serde_json::Value, index: &Index, name: &str, hint: Option<&str>) -> MetaMatch {
    let mut out = MetaMatch {
        name: name.to_string(),
        meta: None,
        match_kind: "none".into(),
        label: None,
        context_window: None,
        max_output_tokens: None,
        capabilities: Vec::new(),
        deprecated: false,
        chat_capable: is_chat_like(name),
    };

    let lower = name.to_ascii_lowercase();
    let exact_hit = index.by_id.get(&lower).map(|v| v.as_slice());
    let norm_hit = index.by_norm.get(&normalize_id(name)).map(|v| v.as_slice());
    let (cands, via) = match (exact_hit, norm_hit) {
        (Some(c), _) => (c, "exact"),
        (None, Some(c)) => (c, "alias"),
        (None, None) => {
            // 去掉厂商前缀再试一层：`openai/gpt-oss-120b` 在有的目录里叫
            // `gpt-oss-120b`。只剥一层，避免 `a/b/c` 无限递归。
            if let Some((_, tail)) = name.split_once('/') {
                return match_one(root, index, tail, hint);
            }
            return out;
        }
    };

    let (pid, mid) = pick_candidate(index, cands, hint);
    // 转售商写法 → 改用它指向的原厂条目（`deepseek-v4.1-flash` → `deepseek-flash`）。
    // 指过去的那条不存在时保留原样：至少元数据是这一家自己的。
    let resolved = model_node(root, pid, mid)
        .and_then(|v| v.get("canonical_model_id"))
        .and_then(|v| v.as_str())
        .and_then(|c| c.split_once('/'))
        .filter(|(vp, vm)| model_node(root, vp, vm).is_some())
        .map(|(vp, vm)| (vp.to_string(), vm.to_string()));
    let (pid, mid) = resolved.unwrap_or_else(|| (pid.clone(), mid.clone()));

    let reseller = !index.vendors.contains(&pid) && Some(pid.as_str()) != hint;
    out.match_kind = if reseller {
        "reseller".into()
    } else {
        via.into()
    };
    out.label = model_label(root, &pid, &mid).or_else(|| Some(mid.clone()));
    out.context_window = model_context(root, &pid, &mid);
    out.max_output_tokens = model_max_output(root, &pid, &mid);
    out.capabilities = model_node(root, &pid, &mid)
        .map(catalog_capabilities)
        .unwrap_or_default();
    out.deprecated = model_deprecated(root, &pid, &mid);
    out.chat_capable = chat_capable(root, &pid, &mid, name);
    out.meta = Some(format!("{pid}/{mid}"));
    out
}

/// 把一批上游模型名对上目录。`hint` 是 [`detect_provider`] 认出来的服务商。
///
/// 没有缓存时**不报错**，每条都返回 `meta: None` —— 界面照常能让人手写 meta。
pub fn match_metas(names: &[String], hint: Option<&str>) -> Vec<MetaMatch> {
    let cache = match fetch::read_cache() {
        Ok(c) => c,
        Err(_) => {
            return names
                .iter()
                .map(|n| MetaMatch {
                    name: n.clone(),
                    meta: None,
                    match_kind: "none".into(),
                    label: None,
                    context_window: None,
                    max_output_tokens: None,
                    capabilities: Vec::new(),
                    deprecated: false,
                    chat_capable: is_chat_like(n),
                })
                .collect();
        }
    };
    let Ok(root) = parse_cache(&cache) else {
        return Vec::new();
    };
    let index = Index::build(&root);
    names
        .iter()
        .map(|n| match_one(&root, &index, n, hint))
        .collect()
}

/// 跨服务商搜索模型，给「手动绑定元数据」那个搜索框用。
///
/// 排序：`服务商/id` 或 id 完全相等 > 前缀命中 > 包含；同组里未弃用的在前，其余按
/// `服务商/id` 定序（要的是确定性，不是"更相关"）。空查询返回空表 —— 把近万条全
/// 倒给下拉框没有任何用处。
pub fn search_models(query: &str, limit: usize) -> Result<Vec<CatalogHit>, String> {
    let cache = fetch::read_cache().map_err(|e| format!("还没有 models.dev 缓存：{e}"))?;
    Ok(search_in(&parse_cache(&cache)?, query, limit))
}

fn search_in(root: &serde_json::Value, query: &str, limit: usize) -> Vec<CatalogHit> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(u8, CatalogHit)> = Vec::new();
    let Some(providers) = root.as_object() else {
        return Vec::new();
    };
    for (pid, p) in providers {
        let Some(models) = p.get("models").and_then(|m| m.as_object()) else {
            continue;
        };
        for (mid, m) in models {
            let name = m.get("name").and_then(|v| v.as_str()).unwrap_or(mid);
            let low_id = mid.to_ascii_lowercase();
            let low_name = name.to_ascii_lowercase();
            let rank = if low_id == q || format!("{}/{}", pid.to_ascii_lowercase(), low_id) == q {
                0
            } else if low_id.starts_with(&q) || low_name.starts_with(&q) {
                1
            } else if low_id.contains(&q) || low_name.contains(&q) {
                2
            } else {
                continue;
            };
            // 「包含」这一档会把 `anthropic/claude-sonnet-4-5` 这类带厂商前缀的
            // 转售条目全捞出来（OpenRouter 一家就有 300 多条）。搜索词本身没带 `/`
            // 时把它们压到完全命中 / 前缀命中之外，否则下拉框里全是重复项。
            if rank == 2 && !q.contains('/') && mid.contains('/') {
                continue;
            }
            hits.push((
                rank,
                CatalogHit {
                    meta: format!("{pid}/{mid}"),
                    provider: pid.clone(),
                    id: mid.clone(),
                    name: name.to_string(),
                    context_window: model_context(root, pid, mid),
                    max_output_tokens: model_max_output(root, pid, mid),
                    capabilities: catalog_capabilities(m),
                    deprecated: model_deprecated(root, pid, mid),
                },
            ));
        }
    }
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.deprecated.cmp(&b.1.deprecated))
            .then_with(|| a.1.meta.cmp(&b.1.meta))
    });
    hits.truncate(limit.clamp(1, 200));
    hits.into_iter().map(|(_, h)| h).collect()
}

/// 一个服务商下「值得预先填进渠道」的几个模型。
///
/// 规则：只留对话模型、未弃用；同一 `family` 只留最新的一条。官方渠道的目录本来就按
/// family 组织（`claude-opus` / `claude-sonnet` / `claude-haiku`），这条规则把 17 个
/// 快照收敛成用户真正会用的那 3~5 个。
pub fn recommended_models(provider_id: &str, limit: usize) -> Result<Vec<CatalogEntry>, String> {
    let cache = fetch::read_cache().map_err(|e| format!("还没有 models.dev 缓存：{e}"))?;
    Ok(recommended_in(&parse_cache(&cache)?, provider_id, limit))
}

fn recommended_in(root: &serde_json::Value, provider_id: &str, limit: usize) -> Vec<CatalogEntry> {
    let Some(models) = root
        .get(provider_id)
        .and_then(|p| p.get("models"))
        .and_then(|m| m.as_object())
    else {
        return Vec::new();
    };

    // family → (release_date, id)：同名家族只要最新的那条。
    let mut best: HashMap<String, (String, String)> = HashMap::new();
    for (mid, m) in models {
        if model_deprecated(root, provider_id, mid) || !chat_capable(root, provider_id, mid, mid) {
            continue;
        }
        let family = m
            .get("family")
            .and_then(|v| v.as_str())
            .unwrap_or(mid)
            .to_string();
        let release = m
            .get("release_date")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if best.get(&family).map(|(r, _)| release > *r).unwrap_or(true) {
            best.insert(family, (release, mid.clone()));
        }
    }
    let mut picked: Vec<(String, String)> = best.into_values().collect();
    // 新的家族排在前面，同一天按 id 定序（要的是确定性）。
    picked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let ids: HashSet<String> = picked.into_iter().map(|(_, id)| id).collect();
    entries_of(root, provider_id)
        .into_iter()
        .filter(|e| ids.contains(&e.id))
        .take(limit.max(1))
        .collect()
}

/// 目录条目的「可读摘要」—— 编辑框在每一行 meta 下面显示它。
///
/// 能力与价格是给**绑定模型时**看的：绑 meta 这件事的意义就是让界面知道这个模型
/// 能干什么、多贵，只给一个展示名等于没绑。
#[derive(Debug, Clone, Serialize)]
pub struct MetaInfo {
    pub meta: String,
    /// 目录里的展示名，如「Claude Opus 5.5」。
    pub label: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// 能力标记，与 [`ModelRow::capabilities`] 同一套字符串。
    pub capabilities: Vec<&'static str>,
    /// 每百万 token 的美元价。目录条目没写 `cost` 时是 `None` —— 那不一定是免费，
    /// 更可能只是没录（本地模型、精简条目），界面不能把它显示成「免费」。
    pub pricing: Option<PricingRow>,
    pub deprecated: bool,
}

/// 从目录条目推能力标记。
///
/// 规则与 carter 的 `registry/models_dev.rs` 一一对应（那边是权威）：`tool_call`
/// → 工具；`attachment` 且输入含 image → 图片；`reasoning` → 思考；输入含 pdf →
/// PDF；流式是默认；声明了 `cache_read` 价 → 提示缓存。这里没直接调那边的
/// `lookup()`，是因为它只吃缓存字符串 —— 那样每个 meta 都要把整个缓存重新解析一遍。
fn catalog_capabilities(m: &serde_json::Value) -> Vec<&'static str> {
    let flag = |k: &str| m.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let inputs: Vec<&str> = m
        .get("modalities")
        .and_then(|x| x.get("input"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut caps = Vec::new();
    if flag("tool_call") {
        caps.push("tools");
    }
    if flag("attachment") && inputs.contains(&"image") {
        caps.push("vision");
    }
    if flag("reasoning") {
        caps.push("thinking");
    }
    if inputs.contains(&"pdf") {
        caps.push("pdf");
    }
    // models.dev 没有显式的 streaming 字段：现代云模型默认支持。
    caps.push("streaming");
    if m.get("cost")
        .and_then(|c| c.get("cache_read"))
        .and_then(|v| v.as_f64())
        .is_some()
    {
        caps.push("prompt_cache");
    }
    caps
}

fn catalog_pricing(m: &serde_json::Value) -> Option<PricingRow> {
    let cost = m.get("cost")?;
    let f = |k: &str| cost.get(k).and_then(|v| v.as_f64());
    // 一个价格字段都没有 = 没录价格，不是免费。
    f("input").or_else(|| f("output"))?;
    Some(PricingRow {
        input: f("input").unwrap_or(0.0),
        output: f("output").unwrap_or(0.0),
        cache_read: f("cache_read"),
        cache_write: f("cache_write"),
    })
}

fn model_max_output(root: &serde_json::Value, pid: &str, mid: &str) -> Option<u64> {
    model_node(root, pid, mid)?
        .get("limit")?
        .get("output")?
        .as_u64()
}

/// 把用户写的 `服务商/模型id` 归一成目录里**实际存在的**那个键。
///
/// 目录 id 全小写，但用户从别处抄来的 meta 不一定 —— 大小写写错时按不敏感再找一次。
/// 找不到就是 `None`（写错了、或者目录里没这条）。
fn canonical_key(root: &serde_json::Value, pid: &str, mid: &str) -> Option<(String, String)> {
    if model_node(root, pid, mid).is_some() {
        return Some((pid.to_string(), mid.to_string()));
    }
    let models = root.get(pid)?.get("models")?.as_object()?;
    let hit = models.keys().find(|k| k.eq_ignore_ascii_case(mid))?.clone();
    Some((pid.to_string(), hit))
}

/// 在已经解析好的缓存上查一个 meta。查不到是 `None`（不是错误）。
fn meta_info_in(root: &serde_json::Value, meta: &str) -> Option<MetaInfo> {
    let (pid, mid) = meta.trim().split_once('/')?;
    let (pid, mid) = canonical_key(root, pid, mid)?;
    let node = model_node(root, &pid, &mid);
    Some(MetaInfo {
        meta: format!("{pid}/{mid}"),
        label: model_label(root, &pid, &mid).unwrap_or_else(|| mid.clone()),
        context_window: model_context(root, &pid, &mid),
        max_output_tokens: model_max_output(root, &pid, &mid),
        capabilities: node.map(catalog_capabilities).unwrap_or_default(),
        pricing: node.and_then(catalog_pricing),
        deprecated: model_deprecated(root, &pid, &mid),
    })
}

/// 某个 meta 在目录里查得到吗。查不到是 `None`（不是错误）—— 界面据此把那一行
/// 标成「未绑定 / 写错了」，而不是让保存时才炸。
pub fn meta_info(meta: &str) -> Option<MetaInfo> {
    let cache = fetch::read_cache().ok()?;
    let root = parse_cache(&cache).ok()?;
    meta_info_in(&root, meta)
}

/// 批量版的 [`meta_info`]。界面一次拿一整个渠道的模型，省掉逐条往返 ——
/// 缓存也只读一次、解析一次。
pub fn meta_infos<'a>(metas: impl IntoIterator<Item = &'a str>) -> Vec<Option<MetaInfo>> {
    let metas: Vec<&str> = metas.into_iter().collect();
    let root = fetch::read_cache().ok().and_then(|c| parse_cache(&c).ok());
    match root {
        Some(root) => metas.iter().map(|m| meta_info_in(&root, m)).collect(),
        None => metas.iter().map(|_| None).collect(),
    }
}

// ---------------------------------------------------------------------------
// 从 base_url 认服务商
// ---------------------------------------------------------------------------

/// 目录里没写 `api` 的几家官方服务商 —— 它们的端点太好认，没必要联网比对。
const KNOWN_ENDPOINTS: &[(&str, &str)] = &[
    ("anthropic", "api.anthropic.com"),
    ("openai", "api.openai.com"),
    ("google", "generativelanguage.googleapis.com"),
    ("groq", "api.groq.com"),
    ("xai", "api.x.ai"),
    ("deepseek", "api.deepseek.com"),
];

/// 把一个端点归一成「域名 + 路径」，并去掉末尾的版本段与请求路径。
///
/// `https://openrouter.ai/api/v1` 与 `https://openrouter.ai/api` 是同一个接口 ——
/// 用户抄文档时带不带 `/v1` 全凭运气。但 `opencode.ai/zen/v1` 与 `/zen/go/v1`
/// 是同域名下的不同套餐，不能因为一方是另一方的前缀就混用，所以只去**末尾**那几段。
fn endpoint_key(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let rest = with_scheme.split_once("://")?.1;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h, p),
        None => (rest, ""),
    };
    let mut segs: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    while let Some(last) = segs.last() {
        let is_request = last == "completions" && segs.len() >= 2 && segs[segs.len() - 2] == "chat";
        if is_request {
            segs.truncate(segs.len() - 2);
            continue;
        }
        // v1 / v4 / v1beta 这类版本段
        let is_version = last.len() >= 2
            && last.starts_with('v')
            && last[1..].chars().next().is_some_and(|c| c.is_ascii_digit());
        if is_version || last == "responses" || last == "messages" {
            segs.pop();
            continue;
        }
        break;
    }
    let host = host.to_ascii_lowercase();
    Some(if segs.is_empty() {
        host
    } else {
        format!("{host}/{}", segs.join("/"))
    })
}

/// 这个渠道的 base_url 对应 models.dev 里的哪一家？认不出来是 `None`。
///
/// 用途是给「自动匹配模型元数据」一个提示（同样叫 `grok-4.5`，在 xAI 名下和在某家
/// 转售商名下是两个价），不是用来决定请求发去哪 —— 请求永远走配置里的 base_url。
pub fn detect_provider(base_url: &str) -> Option<String> {
    let target = endpoint_key(base_url)?;
    // 目录条目里写了 `api` 的（绝大多数）按地址认。目录里那几家官方偏偏没写，
    // 所以下面还有一张手写的域名表 —— 没缓存时也照样认得出。
    if let Some(root) = fetch::read_cache().ok().and_then(|c| parse_cache(&c).ok())
        && let Some(providers) = root.as_object()
    {
        for (pid, p) in providers {
            let api = p.get("api").and_then(|v| v.as_str());
            if api.and_then(endpoint_key).as_deref() == Some(target.as_str()) {
                return Some(pid.clone());
            }
        }
    }
    KNOWN_ENDPOINTS
        .iter()
        .find(|(_, host)| endpoint_key(&format!("https://{host}")).as_deref() == Some(target.as_str()))
        .map(|(pid, _)| (*pid).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 给测试用的目录片段：一家原厂（`deepseek`）、一家转售（`nano-gpt`，指向原厂
    /// 条目）、一个 embedding、一个生图。形状照 models.dev 实际的字段来。
    const MINI_CACHE: &str = r#"{
      "deepseek": {"api":"https://api.deepseek.com","models":{
        "deepseek-flash":{"id":"deepseek-flash","name":"DeepSeek V4.1 Flash",
          "family":"deepseek-flash","reasoning":true,"tool_call":true,
          "modalities":{"input":["text"],"output":["text"]},
          "canonical_model_id":"deepseek/deepseek-v4.1-flash",
          "release_date":"2026-10-17","limit":{"context":1048576,"output":393216}},
        "deepseek-v4-flash":{"id":"deepseek-v4-flash","name":"DeepSeek V4 Flash",
          "family":"deepseek-flash","status":"deprecated","tool_call":true,
          "modalities":{"output":["text"]},
          "canonical_model_id":"deepseek/deepseek-v4.1-flash","release_date":"2026-10-17"},
        "deepseek-v4-pro":{"id":"deepseek-v4-pro","name":"DeepSeek V4 Pro",
          "family":"deepseek-thinking","tool_call":true,
          "modalities":{"output":["text"]},"release_date":"2026-08-13"}
      }},
      "nano-gpt": {"api":"https://nano-gpt.com/v1","models":{
        "deepseek-v4.1-flash":{"id":"deepseek-v4.1-flash","name":"DeepSeek V4.1 Flash",
          "tool_call":true,"modalities":{"output":["text"]},
          "canonical_model_id":"deepseek/deepseek-flash","limit":{"context":128000}}
      }},
      "anthropic": {"models":{
        "claude-opus-5-5":{"id":"claude-opus-5-5","name":"Claude Opus 5.5",
          "family":"claude-opus","tool_call":true,"modalities":{"input":["text","image"],"output":["text"]},
          "release_date":"2026-09-22","limit":{"context":200000}},
        "claude-haiku-4-5":{"id":"claude-haiku-4-5","name":"Claude Haiku 4.5",
          "family":"claude-haiku","tool_call":true,"modalities":{"output":["text"]},
          "release_date":"2025-10-15","limit":{"context":200000}}
      }},
      "openai": {"api":"https://api.openai.com/v1","models":{
        "text-embedding-3-small":{"id":"text-embedding-3-small","name":"Embedding 3 Small",
          "tool_call":false,"modalities":{"input":["text"],"output":["text"]}},
        "gpt-image-2":{"id":"gpt-image-2","name":"GPT Image 2",
          "tool_call":false,"modalities":{"input":["text"],"output":["image"]}}
      }}
    }"#;

    fn mini() -> serde_json::Value {
        parse_cache(MINI_CACHE).unwrap()
    }

    fn index() -> Index {
        Index::build(&mini())
    }

    /// 上游给的模型名与目录 id 一致时，要认出来并带上元数据。
    #[test]
    fn an_identical_name_matches_exactly() {
        let root = mini();
        let m = match_one(&root, &index(), "deepseek-v4-pro", None);
        assert_eq!(m.meta.as_deref(), Some("deepseek/deepseek-v4-pro"));
        assert_eq!(m.match_kind, "exact");
        assert!(m.chat_capable);
    }

    /// 转售商写法（`deepseek-v4.1-flash`）要指回原厂条目 —— 否则定价会按转售商算，
    /// 而用户配的其实是官方渠道。
    #[test]
    fn a_reseller_spelling_points_back_at_the_vendor_entry() {
        let root = mini();
        let m = match_one(&root, &index(), "deepseek-v4.1-flash", None);
        assert_eq!(m.meta.as_deref(), Some("deepseek/deepseek-flash"));
        assert_eq!(m.context_window, Some(1_048_576), "要拿原厂条目的窗口");
    }

    /// 写法差异（下划线 / 点号 / 厂商前缀）归一到同一个键。
    #[test]
    fn spelling_variants_normalise_to_the_same_entry() {
        let root = mini();
        let idx = index();
        for name in [
            "DeepSeek-V4-Pro",
            "deepseek.v4.pro",
            "deepseek/deepseek-v4-pro",
        ] {
            let m = match_one(&root, &idx, name, None);
            assert_eq!(
                m.meta.as_deref(),
                Some("deepseek/deepseek-v4-pro"),
                "name={name}"
            );
        }
    }

    /// 认不出来就老实说认不出来 —— 绑一个错的元数据比不绑更糟。
    #[test]
    fn an_unknown_name_stays_unbound() {
        let root = mini();
        let m = match_one(&root, &index(), "qwen-image-3.0-pro", None);
        assert!(m.meta.is_none());
        assert_eq!(m.match_kind, "none");
    }

    /// 有一家提示（base_url 认出来的服务商）时优先用它的条目。
    #[test]
    fn the_provider_hint_wins_when_the_name_exists_there() {
        let root = mini();
        let m = match_one(&root, &index(), "deepseek-v4.1-flash", Some("nano-gpt"));
        // 提示的那家名下确实有这个名字 —— 用它，且它的 canonical 指回原厂。
        assert_eq!(m.meta.as_deref(), Some("deepseek/deepseek-flash"));
    }

    /// embedding / 生图要能被认出来，好在界面上默认筛掉。
    #[test]
    fn non_chat_models_are_flagged() {
        assert!(!is_chat_like("text-embedding-3-small"));
        assert!(!is_chat_like("whisper-large-v3"));
        assert!(!is_chat_like("gpt-image-2"));
        assert!(is_chat_like("deepseek-v4-pro"));
        let root = mini();
        let idx = index();
        assert!(!match_one(&root, &idx, "text-embedding-3-small", None).chat_capable);
        assert!(!match_one(&root, &idx, "gpt-image-2", None).chat_capable);
    }

    /// 预填的模型列表：同 family 只要最新的那条，弃用与非对话模型不要。
    #[test]
    fn recommended_models_collapses_families() {
        let root = mini();
        let picked = recommended_in(&root, "deepseek", 10);
        let ids: Vec<&str> = picked.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["deepseek-flash", "deepseek-v4-pro"]);
        // 同 family 的弃用条目（deepseek-v4-flash）不能出现。
        assert!(!ids.contains(&"deepseek-v4-flash"));

        let anthropic = recommended_in(&root, "anthropic", 10);
        assert_eq!(
            anthropic.len(),
            2,
            "{:?}",
            anthropic.iter().map(|e| &e.id).collect::<Vec<_>>()
        );
        // embedding / 生图不进推荐。
        assert!(recommended_in(&root, "openai", 10).is_empty());
        assert!(recommended_in(&root, "nobody", 10).is_empty());
    }

    /// 手动绑定元数据时的全局搜索：完全相等排最前，转售条目别把结果淹掉。
    #[test]
    fn search_ranks_exact_hits_first_and_drops_prefixed_noise() {
        let root = mini();
        let hits = search_in(&root, "deepseek-v4-pro", 10);
        assert_eq!(
            hits.first().map(|h| h.meta.as_str()),
            Some("deepseek/deepseek-v4-pro")
        );
        // 搜索词不带 `/` 时，`厂商/模型` 这种转售写法只在前缀命中里出现。
        let hits = search_in(&root, "flash", 10);
        assert!(hits.iter().all(|h| !h.id.contains('/')), "{hits:?}");
    }

    #[test]
    fn endpoint_keys_ignore_version_segments_but_not_real_paths() {
        let same = [
            ("https://api.deepseek.com", "https://api.deepseek.com/v1"),
            ("https://openrouter.ai/api/v1", "https://openrouter.ai/api"),
            ("https://x/v1beta/openai", "https://x/v1beta/openai/"),
        ];
        for (a, b) in same {
            assert_eq!(endpoint_key(a), endpoint_key(b), "{a} vs {b}");
        }
        // 同域名下的不同套餐不能混为一谈。
        assert_ne!(
            endpoint_key("https://opencode.ai/zen/v1"),
            endpoint_key("https://opencode.ai/zen/go/v1")
        );
    }

    /// 没有缓存也要能认出这几家官方 —— 模板里填上密钥就该直接能用。
    #[test]
    fn known_official_endpoints_are_recognised_without_a_cache() {
        assert_eq!(
            detect_provider("https://api.anthropic.com").as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            detect_provider("https://api.deepseek.com/v1").as_deref(),
            Some("deepseek")
        );
        assert_eq!(detect_provider("http://192.168.31.1:3000").as_deref(), None);
    }

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
            None,
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
            None,
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

    /// 展示名要从目录里取：界面上那个模型标签写的应该是「Claude Sonnet 4.5」，
    /// 而不是配置里的块名 `sonnet`，更不是请求里的 `claude-sonnet-4-5`。
    ///
    /// 解析失败的行同样要填 —— 名字对不上目录的原因恰恰是 meta 写错了，那时
    /// 标签能显示对的展示名，用户才看得出「这条本来该是什么」。
    #[test]
    fn a_row_carries_the_catalog_label_even_when_it_cannot_resolve() {
        let json = r#"{"anthropic":{"models":{"claude-sonnet-4-5":{
            "id":"claude-sonnet-4-5","name":"Claude Sonnet 4.5",
            "limit":{"context":200000,"output":64000}
        }}}}"#;
        let root = parse_cache(json).unwrap();
        let cache = Ok(json.to_string());

        let good = resolve_one(
            &config_with("anthropic/claude-sonnet-4-5"),
            &cache,
            Some(&root),
            &Default::default(),
            "ws",
            "sonnet",
            "ws/sonnet",
            None,
            "anthropic",
        );
        assert_eq!(good.label.as_deref(), Some("Claude Sonnet 4.5"));

        // 大小写写错的 meta：解析不了，但按不敏感查得到展示名。
        let odd = resolve_one(
            &config_with("anthropic/Claude-Sonnet-4-5"),
            &cache,
            Some(&root),
            &Default::default(),
            "ws",
            "sonnet",
            "ws/sonnet",
            None,
            "anthropic",
        );
        assert!(odd.error.is_some());
        assert_eq!(odd.label.as_deref(), Some("Claude Sonnet 4.5"));

        // 目录里压根没有这一条：没有展示名，界面退回显示引用本身。
        let missing = resolve_one(
            &config_with("anthropic/nope"),
            &cache,
            Some(&root),
            &Default::default(),
            "ws",
            "sonnet",
            "ws/sonnet",
            None,
            "anthropic",
        );
        assert!(missing.label.is_none());
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
            None,
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
            None,
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
              "tool_call":true,"attachment":true,
              "modalities":{"input":["text","image","pdf"]},
              "limit":{"context":1000000,"output":393216},
              "cost":{"input":0.28,"output":0.42,"cache_read":0.028},
              "release_date":"2026-09-10"},
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

    /// 绑 meta 的意义就是让人看见「这个模型能干什么、多贵」。摘要里只有展示名
    /// 等于没绑 —— 能力与价格必须一起回来。
    #[test]
    fn a_meta_summary_carries_capabilities_and_pricing() {
        let root = dev_cache();
        let info = meta_info_in(&root, "deepseek/deepseek-flash").expect("能查到");
        assert_eq!(info.label, "DeepSeek V4.1 Flash");
        assert_eq!(info.context_window, Some(1_000_000));
        assert_eq!(info.max_output_tokens, Some(393_216));
        for cap in ["tools", "vision", "thinking", "pdf", "streaming", "prompt_cache"] {
            assert!(info.capabilities.contains(&cap), "缺 {cap}：{:?}", info.capabilities);
        }
        assert_eq!(info.pricing.as_ref().map(|p| p.input), Some(0.28));
        assert_eq!(info.pricing.as_ref().and_then(|p| p.cache_read), Some(0.028));
    }

    /// 没有 `cost` / 模态 / 能力字段的条目（本地模型、精简条目）不能炸，也不能
    /// 凭空造出能力或价格：只有流式是默认的。
    #[test]
    fn a_bare_catalog_entry_only_gets_streaming() {
        let root = dev_cache();
        let info = meta_info_in(&root, "deepseek/deepseek-v4-pro").expect("能查到");
        assert_eq!(info.capabilities, vec!["streaming"]);
        assert!(info.pricing.is_none(), "没录价格 ≠ 免费");
    }

    /// 大小写写错的 meta 要在摘要里纠正成目录里的规范写法 —— 用户抄来的 id 常常
    /// 是展示名的大小写。
    #[test]
    fn a_meta_summary_normalises_the_case() {
        let root = dev_cache();
        assert_eq!(
            meta_info_in(&root, "deepseek/DeepSeek-Flash").map(|i| i.meta),
            Some("deepseek/deepseek-flash".to_string())
        );
        assert!(meta_info_in(&root, "deepseek/nope").is_none());
        assert!(meta_info_in(&root, "no-slash").is_none());
    }
}
