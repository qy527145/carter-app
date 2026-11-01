//! 在界面上改 `~/.carter/config.toml`。
//!
//! ## 为什么不能「读进来 → 改 → 全量写回」
//!
//! `carter::config::Config` 是 **Deserialize-only**（没有 `save` / `to_toml`），
//! 而 `Config::load()` 会先对**整份原始文本**做 `${ENV_VAR}` 插值再解析。
//! 于是「反序列化 → 改字段 → 序列化回去」这条路会把用户写的
//!
//! ```toml
//! api_key = "${ANTHROPIC_API_KEY}"
//! ```
//!
//! 换成插值后的**明文密钥**写进磁盘。这不是理论风险 —— 用户第一次在界面上
//! 点保存就会发生，而且没有任何提示。
//!
//! ## 做法
//!
//! 用 `toml_edit` 在**原始文本**上做定点修改：只碰用户真正改过的那几个键，
//! 注释、缩进、空行、本应用还不认识的未来字段，以及 `${VAR}` 字面量全部原样留下。
//!
//! ## 写坏了的兜底
//!
//! 写完立刻用 `Config::load()` 读一遍；读不回来就把原内容还原回去再报错。
//! 一份「本来能用」的配置不能被界面换成「起不来」的配置。

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

/// TOML 裸键允许的字符集。渠道名与模型名会变成 `[providers.<名>]` 这样的表键，
/// 含空格或点号的键必须加引号才能表示 —— 与其在界面里做引号转义，不如直接挡住。
/// carter 的模型引用 `provider/model` 本来也是按 `/` 切的，名字里有怪字符迟早出事。
fn valid_key(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn check_key(kind: &str, name: &str) -> Result<(), String> {
    if valid_key(name) {
        return Ok(());
    }
    Err(format!(
        "{kind}名字 `{name}` 只能用字母、数字、`-`、`_`（它会变成配置里的表键）"
    ))
}

pub fn config_path() -> std::path::PathBuf {
    carter::config::paths::config_path()
}

/// 读原始配置文档。文件不存在时给一份空文档 —— 用户第一次配渠道时
/// `~/.carter/config.toml` 可能根本还没被建出来。
pub fn load_doc() -> Result<DocumentMut, String> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => raw
            .parse::<DocumentMut>()
            .map_err(|e| format!("解析 {} 失败：{e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(format!("读 {} 失败：{e}", path.display())),
    }
}

/// 写回并热重载。失败时把原内容还原。
pub fn write_and_reload(doc: &DocumentMut) -> Result<carter::config::Config, String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建目录失败：{e}"))?;
    }

    let previous = std::fs::read_to_string(&path).ok();
    std::fs::write(&path, doc.to_string()).map_err(|e| format!("写配置失败：{e}"))?;

    match carter::config::Config::load() {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            // 还原。原来的文件不存在（第一次创建）就把这份半成品删掉，
            // 否则用户会拿到一个「看着像配置、其实解析不了」的文件。
            match previous {
                Some(text) => {
                    let _ = std::fs::write(&path, text);
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                }
            }
            Err(format!("这份配置 carter 读不了，已还原上一版：{e}"))
        }
    }
}

// ---------------------------------------------------------------------------
// 表操作小工具
//
// 三态约定（贯穿本模块）：`None` = 不动那个键，`Some("")` = 删掉它，
// `Some(v)` = 设成 v。api_key 靠这个约定实现「用户没碰就绝不改写」。
// ---------------------------------------------------------------------------

fn child_table<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table, String> {
    let item = parent.entry(key).or_insert(Item::Table(Table::new()));
    item.as_table_mut()
        .ok_or_else(|| format!("配置里的 `{key}` 不是一个表段，界面改不了它"))
}

fn set_opt_str(t: &mut Table, key: &str, v: &Option<String>) {
    match v {
        Some(s) if !s.trim().is_empty() => {
            t.insert(key, value(s.as_str()));
        }
        Some(_) => {
            t.remove(key);
        }
        None => {}
    }
}

fn set_str_array(t: &mut Table, key: &str, vs: &[String]) {
    if vs.is_empty() {
        // 空列表就删键，而不是留一个 `server_tools = []`。
        t.remove(key);
        return;
    }
    let mut arr = Array::new();
    for v in vs {
        arr.push(v.as_str());
    }
    t.insert(key, value(arr));
}

fn set_str_map(t: &mut Table, key: &str, pairs: &[(String, String)]) {
    if pairs.is_empty() {
        t.remove(key);
        return;
    }
    let mut it = InlineTable::new();
    for (k, v) in pairs {
        it.insert(k, v.as_str().into());
    }
    t.insert(key, value(it));
}

// ---------------------------------------------------------------------------
// 草稿（前端 → 后端）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDraft {
    /// 模型块名，也是 `provider/model` 里的后半截。
    pub name: String,
    /// models.dev 的查找 id，形如 `anthropic/claude-sonnet-4-5`。
    #[serde(default)]
    pub meta: String,
    #[serde(default)]
    pub api_name: Option<String>,
    /// `none` / `low` / `medium` / `high` / `xhigh` / `max`。留空则不写这个键。
    /// （carter 的 `ReasoningEffort::Budget(n)` 这种带参数的形态表达不了。）
    #[serde(default)]
    pub default_reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDraft {
    pub name: String,
    /// 协议。可以是规范名，也可以是 carter 认的别名（`deepseek` / `ollama` …）。
    pub kind: String,
    /// **`None` = 保持原样。** 界面不会把已存的 key 回传给后端 ——
    /// 于是「不碰密钥」这个意图在协议层就是明确的，不靠界面自觉。
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub server_tools: Vec<String>,
    #[serde(default)]
    pub server_tool_max_uses: Option<u32>,
    /// 整表对齐：不在这个列表里的模型会被删掉。
    #[serde(default)]
    pub models: Vec<ModelDraft>,
}

pub fn save_provider(doc: &mut DocumentMut, d: &ProviderDraft) -> Result<(), String> {
    check_key("渠道", &d.name)?;
    if d.kind.trim().is_empty() {
        return Err("协议不能为空".into());
    }

    let providers = child_table(doc.as_table_mut(), "providers")?;
    let p = child_table(providers, &d.name)?;

    p.insert("kind", value(d.kind.trim()));
    set_opt_str(p, "api_key", &d.api_key);
    set_opt_str(p, "base_url", &d.base_url);
    set_str_array(p, "server_tools", &d.server_tools);
    match d.server_tool_max_uses {
        Some(n) => {
            p.insert("server_tool_max_uses", value(i64::from(n)));
        }
        None => {
            p.remove("server_tool_max_uses");
        }
    }

    // 模型整表对齐。先算出要删的，再写入 —— `models` 的借用不能跨过写入。
    let stale: Vec<String> = match p.get("models") {
        // 渠道是刚建出来的：还没有 models 键，没有要删的。
        None => Vec::new(),
        Some(item) => {
            let models = item
                .as_table()
                .ok_or_else(|| "配置里的 models 不是一个表段".to_string())?;
            let wanted: std::collections::HashSet<&str> =
                d.models.iter().map(|m| m.name.as_str()).collect();
            models
                .iter()
                .map(|(k, _)| k.to_string())
                .filter(|k| !wanted.contains(k.as_str()))
                .collect()
        }
    };
    let models = child_table(p, "models")?;
    for k in &stale {
        models.remove(k);
    }
    for m in &d.models {
        check_key("模型", &m.name)?;
        if m.meta.trim().is_empty() {
            return Err(format!("模型 `{}` 的 meta 不能为空（形如 anthropic/claude-sonnet-4-5）", m.name));
        }
        let mt = child_table(models, &m.name)?;
        mt.insert("meta", value(m.meta.trim()));
        set_opt_str(mt, "api_name", &m.api_name);
        set_opt_str(mt, "default_reasoning", &m.default_reasoning);
    }
    Ok(())
}

pub fn remove_provider(doc: &mut DocumentMut, name: &str) -> Result<(), String> {
    let Some(providers) = doc
        .as_table_mut()
        .get_mut("providers")
        .and_then(|i| i.as_table_mut())
    else {
        return Err("配置里没有 [providers] 段".into());
    };
    if providers.remove(name).is_none() {
        return Err(format!("找不到渠道 `{name}`"));
    }
    Ok(())
}

/// 默认模型（`agent.model`，形如 `渠道名/模型名`）。
pub fn set_default_model(doc: &mut DocumentMut, model_ref: &str) -> Result<(), String> {
    if !model_ref.contains('/') {
        return Err(format!("模型引用 `{model_ref}` 应为 `渠道/模型` 形式"));
    }
    child_table(doc.as_table_mut(), "agent")?.insert("model", value(model_ref));
    Ok(())
}

/// 副模型（`agent.fast_model`）：压缩上下文、生成标题、总结用的那个。
///
/// `None` = 删掉这个键，回落主模型。**删键而不是写空串**：配置里留一句
/// `fast_model = ""` 会让读的人以为配了个空模型，而 carter 那边它是 `Option<String>`，
/// 空串是个"配了但没法解析"的坏值。
pub fn set_fast_model(doc: &mut DocumentMut, model_ref: Option<&str>) -> Result<(), String> {
    let agent = child_table(doc.as_table_mut(), "agent")?;
    match model_ref {
        Some(r) => {
            if !r.contains('/') {
                return Err(format!("模型引用 `{r}` 应为 `渠道/模型` 形式"));
            }
            agent.insert("fast_model", value(r));
        }
        None => {
            // 没有这个键时 remove 返回 false，那不是错误。
            agent.remove("fast_model");
        }
    }
    Ok(())
}

/// 自动压缩（`agent.compact_enabled` / `agent.compact_threshold_ratio`）。
///
/// 比例收在 0.3–0.95：再低几乎每轮都在压，再高压完也装不下下一轮的输出。
pub fn set_compaction(doc: &mut DocumentMut, enabled: bool, ratio: f64) -> Result<(), String> {
    if !(0.3..=0.95).contains(&ratio) {
        return Err(format!("压缩阈值 {ratio} 应在 0.3 到 0.95 之间"));
    }
    let agent = child_table(doc.as_table_mut(), "agent")?;
    agent.insert("compact_enabled", value(enabled));
    // 两位小数足够，也免得 0.7 写成 0.7000000000000001。
    agent.insert("compact_threshold_ratio", value((ratio * 100.0).round() / 100.0));
    Ok(())
}

pub fn set_skills_enabled(doc: &mut DocumentMut, enabled: bool) -> Result<(), String> {
    child_table(doc.as_table_mut(), "skills")?.insert("enabled", value(enabled));
    Ok(())
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpDraft {
    pub name: String,
    pub enabled: bool,
    /// `stdio` | `http`。carter 的 `McpServerConfig` 没有 `Default`，
    /// 这个字段是必填的 —— 界面要挡住空值。
    pub transport: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

pub fn save_mcp_server(doc: &mut DocumentMut, d: &McpDraft) -> Result<(), String> {
    check_key("MCP server", &d.name)?;
    let is_stdio = match d.transport.as_str() {
        "stdio" => true,
        "http" => false,
        other => return Err(format!("未知的 transport `{other}`（只支持 stdio / http）")),
    };
    if is_stdio && d.command.as_deref().unwrap_or("").trim().is_empty() {
        return Err("stdio 必须填 command".into());
    }
    if !is_stdio && d.url.as_deref().unwrap_or("").trim().is_empty() {
        return Err("http 必须填 url".into());
    }

    let mcp = child_table(doc.as_table_mut(), "mcp")?;
    let servers = child_table(mcp, "servers")?;
    let s = child_table(servers, &d.name)?;

    s.insert("enabled", value(d.enabled));
    s.insert("transport", value(d.transport.as_str()));

    // 只写当前 transport 用得上的字段，另一个的键直接删掉。
    // 留着一个用不上的 url 会让人以为它生效 —— 表单里看到的就该是文件里的。
    if is_stdio {
        set_opt_str(s, "command", &d.command);
        set_str_array(s, "args", &d.args);
        set_str_map(s, "env", &d.env);
        s.remove("url");
        s.remove("headers");
    } else {
        set_opt_str(s, "url", &d.url);
        set_str_map(s, "headers", &d.headers);
        s.remove("command");
        s.remove("args");
        s.remove("env");
    }
    Ok(())
}

pub fn remove_mcp_server(doc: &mut DocumentMut, name: &str) -> Result<(), String> {
    let Some(servers) = doc
        .as_table_mut()
        .get_mut("mcp")
        .and_then(|i| i.as_table_mut())
        .and_then(|m| m.get_mut("servers"))
        .and_then(|i| i.as_table_mut())
    else {
        return Err("配置里没有 [mcp.servers] 段".into());
    };
    if servers.remove(name).is_none() {
        return Err(format!("找不到 MCP server `{name}`"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 写回 + 热重载
//
// 外壳层只调这几个函数，不碰 `DocumentMut` —— 它不该认识 TOML 编辑这套东西。
// ---------------------------------------------------------------------------

fn apply(shell: &crate::shell::AppShell, doc: &DocumentMut) -> Result<(), String> {
    let config = write_and_reload(doc)?;
    shell.config.store(std::sync::Arc::new(config));
    shell.emit(crate::protocol::AppEvent::ConfigChanged);
    Ok(())
}

/// 保存一个渠道并热重载。返回写回后的原始详情。
pub fn save_provider_reload(
    shell: &crate::shell::AppShell,
    d: &ProviderDraft,
) -> Result<ProviderDetail, String> {
    // meta 写错要在保存时就说，而不是等到开会话才报「缓存中找不到」。
    crate::catalog::check_metas(d.models.iter().map(|m| m.meta.as_str()))?;
    let mut doc = load_doc()?;
    save_provider(&mut doc, d)?;
    apply(shell, &doc)?;
    provider_detail(&d.name)
}

pub fn remove_provider_reload(shell: &crate::shell::AppShell, name: &str) -> Result<(), String> {
    let mut doc = load_doc()?;
    remove_provider(&mut doc, name)?;
    apply(shell, &doc)
}

pub fn set_default_model_reload(
    shell: &crate::shell::AppShell,
    model_ref: &str,
) -> Result<(), String> {
    let mut doc = load_doc()?;
    set_default_model(&mut doc, model_ref)?;
    apply(shell, &doc)
}

pub fn set_fast_model_reload(
    shell: &crate::shell::AppShell,
    model_ref: Option<&str>,
) -> Result<(), String> {
    let mut doc = load_doc()?;
    set_fast_model(&mut doc, model_ref)?;
    apply(shell, &doc)
}

pub fn set_compaction_reload(
    shell: &crate::shell::AppShell,
    enabled: bool,
    ratio: f64,
) -> Result<(), String> {
    let mut doc = load_doc()?;
    set_compaction(&mut doc, enabled, ratio)?;
    apply(shell, &doc)
}

pub fn set_skills_enabled_reload(
    shell: &crate::shell::AppShell,
    enabled: bool,
) -> Result<(), String> {
    let mut doc = load_doc()?;
    set_skills_enabled(&mut doc, enabled)?;
    apply(shell, &doc)
}

pub fn save_mcp_server_reload(
    shell: &crate::shell::AppShell,
    d: &McpDraft,
) -> Result<(), String> {
    let mut doc = load_doc()?;
    save_mcp_server(&mut doc, d)?;
    apply(shell, &doc)
}

pub fn remove_mcp_server_reload(shell: &crate::shell::AppShell, name: &str) -> Result<(), String> {
    let mut doc = load_doc()?;
    remove_mcp_server(&mut doc, name)?;
    apply(shell, &doc)
}

// ---------------------------------------------------------------------------
// 读原始值（给编辑弹窗用）
//
// 刻意读**原始文本**而不是 `Config`：`Config::load()` 已经把 `${ENV}` 插值掉了，
// 拿它回填表单会让界面显示真实密钥，而且一保存就把 `${VAR}` 覆盖成明文。
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(default)]
    providers: HashMap<String, RawProvider>,
}

#[derive(Debug, Deserialize)]
struct RawProvider {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    server_tools: Vec<String>,
    #[serde(default)]
    server_tool_max_uses: Option<u32>,
    #[serde(default)]
    models: BTreeMap<String, RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    #[serde(default)]
    meta: String,
    #[serde(default)]
    api_name: Option<String>,
    /// **原样的 TOML 值**，不是转成字符串的。
    ///
    /// `default_reasoning` 除了 `"medium"` 这类裸字符串，还能写成
    /// `{ budget = 1024 }`（carter 的 `ReasoningEffort::Budget(n)`）。把它读成
    /// `String` 会让后者要么丢、要么被摊平成一个既写不回去、又看着像合法的字符串。
    /// 界面见到非字符串的值就只展示、不改写。
    #[serde(default)]
    default_reasoning: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelDetail {
    pub name: String,
    pub meta: String,
    pub api_name: Option<String>,
    /// 字符串（`"medium"`）或表（`{ budget = 1024 }`），原样给前端。
    pub default_reasoning: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderDetail {
    pub name: String,
    pub kind: String,
    /// 原样返回，可能还是 `${ANTHROPIC_API_KEY}` 这种引用。
    pub api_key: String,
    pub has_api_key: bool,
    pub base_url: String,
    pub server_tools: Vec<String>,
    pub server_tool_max_uses: Option<u32>,
    pub models: Vec<ModelDetail>,
}

/// 把读出来的 `default_reasoning` 原值转成可写回的草稿值。
///
/// 字符串原样带回；表（`{ budget = 1024 }`）界面表达不了，返回 `None` ——
/// 那是「不动这个键」的意思，于是这种写法既不会丢、也不会被改写成别的东西。
pub fn reasoning_to_draft(v: &Option<serde_json::Value>) -> Option<String> {
    match v {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// 每个模型在配置里写的 `default_reasoning` 原值，键是 `(渠道名, 模型名)`。
///
/// 模型列表要用它把 `{ budget = 1024 }` 这类表达式原样显示出来 ——
/// `ModelInfo` 里那个字段已经被解析成 `ReasoningEffort`，反推不回原始写法。
pub fn raw_default_reasoning() -> std::collections::HashMap<(String, String), serde_json::Value> {
    let Ok(doc) = load_doc() else {
        return Default::default();
    };
    let Ok(file) = toml_edit::de::from_str::<RawFile>(&doc.to_string()) else {
        return Default::default();
    };
    let mut out = std::collections::HashMap::new();
    for (pname, p) in &file.providers {
        for (mname, m) in &p.models {
            if let Some(v) = &m.default_reasoning {
                out.insert((pname.clone(), mname.clone()), v.clone());
            }
        }
    }
    out
}

/// 取某个渠道的**原始**配置（含 `${VAR}` 字面量）。
pub fn provider_detail(name: &str) -> Result<ProviderDetail, String> {
    let doc = load_doc()?;
    let file: RawFile = toml_edit::de::from_str(&doc.to_string())
        .map_err(|e| format!("解析配置失败：{e}"))?;
    let p = file
        .providers
        .get(name)
        .ok_or_else(|| format!("找不到渠道 `{name}`"))?;

    let raw_key = p.api_key.clone().unwrap_or_default();
    Ok(ProviderDetail {
        name: name.to_string(),
        kind: p.kind.clone(),
        has_api_key: !raw_key.is_empty(),
        api_key: raw_key,
        base_url: p.base_url.clone().unwrap_or_default(),
        server_tools: p.server_tools.clone(),
        server_tool_max_uses: p.server_tool_max_uses,
        models: p
            .models
            .iter()
            .map(|(k, m)| ModelDetail {
                name: k.clone(),
                meta: m.meta.clone(),
                api_name: m.api_name.clone(),
                default_reasoning: m.default_reasoning.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份「典型」的用户配置：带注释、带 `${ENV}` 引用、带本应用不认识的字段。
    const SAMPLE: &str = r#"# 我的配置
[agent]
model = "ws/sonnet"   # 默认模型

[providers.ws]
# 密钥走环境变量，别写死在这里
api_key = "${ANTHROPIC_API_KEY}"
kind = "anthropic"
server_tools = ["web_search"]

  [providers.ws.models.sonnet]
  meta = "anthropic/claude-sonnet-4-5"
  api_name = "claude-sonnet-4-6"

[future_section]
whatever = true
"#;

    fn doc() -> DocumentMut {
        SAMPLE.parse().unwrap()
    }

    /// **这是本模块最重要的一条测试。**
    ///
    /// 保存渠道时如果用户没碰 API Key（`api_key: None`），文件里那个
    /// `${ANTHROPIC_API_KEY}` 必须原样留着。一旦这里退化成明文，
    /// 用户点一次保存密钥就落到磁盘上了，而且不会有任何提示。
    #[test]
    fn saving_without_touching_api_key_keeps_the_env_reference() {
        let mut d = doc();
        let draft = ProviderDraft {
            name: "ws".into(),
            kind: "anthropic".into(),
            api_key: None, // 界面没碰密钥
            base_url: Some("https://example.test".into()),
            server_tools: vec!["web_search".into()],
            server_tool_max_uses: None,
            models: vec![ModelDraft {
                name: "sonnet".into(),
                meta: "anthropic/claude-sonnet-4-5".into(),
                api_name: Some("claude-sonnet-4-6".into()),
                default_reasoning: None,
            }],
        };
        save_provider(&mut d, &draft).unwrap();
        let out = d.to_string();

        assert!(
            out.contains(r#"api_key = "${ANTHROPIC_API_KEY}""#),
            "环境变量引用被改写了：\n{out}"
        );
        assert!(!out.contains("sk-"), "核心里不该出现任何明文密钥");
        assert!(out.contains("https://example.test"), "改过的字段要写进去");
    }

    /// 自动压缩：两个键写进 `[agent]`，越界的比例挡在外面。
    #[test]
    fn compaction_settings_are_written_and_range_checked() {
        let mut d = doc();
        set_compaction(&mut d, false, 0.8).unwrap();
        let out = d.to_string();
        assert!(out.contains("compact_enabled = false"), "{out}");
        assert!(out.contains("compact_threshold_ratio = 0.8"), "{out}");
        assert!(out.contains(r#"model = "ws/sonnet""#), "{out}");
        assert!(set_compaction(&mut d, true, 0.1).is_err());
        assert!(set_compaction(&mut d, true, 1.0).is_err());
    }

    /// 副模型：写进去、清掉，而且两件事都不动别的键。
    #[test]
    fn fast_model_is_written_and_cleared_without_touching_the_main_model() {
        let mut d = doc();
        set_fast_model(&mut d, Some("local/haiku")).unwrap();
        let out = d.to_string();
        assert!(out.contains(r#"fast_model = "local/haiku""#), "{out}");
        // 主模型那行以及它的行尾注释都要原样留着。
        assert!(out.contains(r#"model = "ws/sonnet"   # 默认模型"#), "{out}");

        // 清除 = 删键，不是写空串（空串在 carter 那边是个"配了但没法解析"的坏值）。
        set_fast_model(&mut d, None).unwrap();
        let out = d.to_string();
        assert!(!out.contains("fast_model"), "{out}");
        assert!(out.contains(r#"model = "ws/sonnet""#), "{out}");

        // 引用要带渠道名 —— 与主模型同一个校验，写错了要当场说。
        assert!(set_fast_model(&mut d, Some("sonnet")).is_err());
    }

    /// 注释、空行与本应用不认识的段都要留住 —— 用户在文件里手写的东西
    /// 不该因为点了一次界面就消失。
    #[test]
    fn comments_and_unknown_sections_survive_a_save() {
        let mut d = doc();
        let draft = ProviderDraft {
            name: "ws".into(),
            kind: "anthropic".into(),
            api_key: None,
            base_url: None,
            server_tools: vec![],
            server_tool_max_uses: None,
            models: vec![ModelDraft {
                name: "sonnet".into(),
                meta: "anthropic/claude-sonnet-4-5".into(),
                api_name: None,
                default_reasoning: None,
            }],
        };
        save_provider(&mut d, &draft).unwrap();
        let out = d.to_string();

        assert!(out.contains("# 我的配置"), "顶部注释没了：\n{out}");
        assert!(out.contains("# 密钥走环境变量"), "字段注释没了：\n{out}");
        assert!(out.contains("[future_section]"), "未知段没了：\n{out}");
        assert!(out.contains("# 默认模型"), "行尾注释没了：\n{out}");
    }

    #[test]
    fn empty_lists_and_options_remove_their_keys() {
        let mut d = doc();
        let draft = ProviderDraft {
            name: "ws".into(),
            kind: "anthropic".into(),
            api_key: Some(String::new()), // 显式清空
            base_url: None,
            server_tools: vec![], // 清空
            server_tool_max_uses: None,
            models: vec![ModelDraft {
                name: "sonnet".into(),
                meta: "anthropic/claude-sonnet-4-5".into(),
                api_name: Some(String::new()), // 清空
                default_reasoning: None,
            }],
        };
        save_provider(&mut d, &draft).unwrap();
        let out = d.to_string();

        assert!(!out.contains("api_key"), "清空的键该被删掉：\n{out}");
        assert!(!out.contains("server_tools"), "空数组该删键而不是写 []：\n{out}");
        assert!(!out.contains("api_name"), "清空的 api_name 该被删掉：\n{out}");
    }

    /// 新增一个渠道时，`[providers.<名>]` 与 `models` 表都要现建出来。
    #[test]
    fn a_brand_new_provider_creates_its_tables() {
        let mut d = DocumentMut::new();
        let draft = ProviderDraft {
            name: "ds".into(),
            kind: "deepseek".into(),
            api_key: Some("${DEEPSEEK_API_KEY}".into()),
            base_url: None,
            server_tools: vec![],
            server_tool_max_uses: None,
            models: vec![ModelDraft {
                name: "chat".into(),
                meta: "deepseek/deepseek-chat".into(),
                api_name: None,
                default_reasoning: None,
            }],
        };
        save_provider(&mut d, &draft).unwrap();

        let back = provider_detail_from(&d, "ds");
        assert_eq!(back.kind, "deepseek");
        assert_eq!(back.models.len(), 1);
        assert_eq!(back.models[0].meta, "deepseek/deepseek-chat");
        // api_name 留空 → 不写这个键，carter 会回落到块名。
        assert_eq!(back.models[0].api_name, None);
    }

    /// 从草稿里去掉的模型，文件里也要跟着消失（界面上「删掉这一行」要真的生效）。
    #[test]
    fn models_removed_in_the_form_are_removed_from_the_file() {
        let mut d = doc();
        let draft = ProviderDraft {
            name: "ws".into(),
            kind: "anthropic".into(),
            api_key: None,
            base_url: None,
            server_tools: vec![],
            server_tool_max_uses: None,
            models: vec![ModelDraft {
                name: "haiku".into(),
                meta: "anthropic/claude-haiku-4-5".into(),
                api_name: None,
                default_reasoning: None,
            }],
        };
        save_provider(&mut d, &draft).unwrap();
        let back = provider_detail_from(&d, "ws");
        assert_eq!(back.models.len(), 1);
        assert_eq!(back.models[0].name, "haiku");
    }

    #[test]
    fn names_that_would_need_quoting_are_rejected() {
        let mut d = DocumentMut::new();
        let draft = ProviderDraft {
            name: "my provider".into(),
            kind: "anthropic".into(),
            api_key: None,
            base_url: None,
            server_tools: vec![],
            server_tool_max_uses: None,
            models: vec![],
        };
        let err = save_provider(&mut d, &draft).unwrap_err();
        assert!(err.contains("只能用字母"), "错误信息要说清规则，实际：{err}");
    }

    #[test]
    fn mcp_stdio_and_http_write_only_their_own_fields() {
        let mut d = DocumentMut::new();
        save_mcp_server(
            &mut d,
            &McpDraft {
                name: "fs".into(),
                enabled: true,
                transport: "stdio".into(),
                command: Some("npx".into()),
                args: vec!["-y".into(), "server-filesystem".into()],
                env: vec![("TOKEN".into(), "t".into())],
                url: Some("http://stale".into()),
                headers: vec![],
            },
        )
        .unwrap();
        let out = d.to_string();
        assert!(out.contains("command = \"npx\""));
        assert!(out.contains("TOKEN"), "env 要写成内联表");
        assert!(!out.contains("stale"), "stdio 不该带上 url");

        // 换成 http：stdio 的三个键要消失。
        save_mcp_server(
            &mut d,
            &McpDraft {
                name: "fs".into(),
                enabled: true,
                transport: "http".into(),
                command: Some("npx".into()),
                args: vec!["-y".into()],
                env: vec![],
                url: Some("https://mcp.test/sse".into()),
                headers: vec![("x-k".into(), "v".into())],
            },
        )
        .unwrap();
        let out = d.to_string();
        assert!(out.contains("https://mcp.test/sse"));
        assert!(!out.contains("command"), "http 不该留着 command：\n{out}");
        assert!(!out.contains("args"), "http 不该留着 args：\n{out}");
    }

    #[test]
    fn mcp_requires_the_mandatory_field_for_each_transport() {
        let mut d = DocumentMut::new();
        let base = McpDraft {
            name: "x".into(),
            enabled: true,
            transport: "stdio".into(),
            command: None,
            args: vec![],
            env: vec![],
            url: None,
            headers: vec![],
        };
        assert!(save_mcp_server(&mut d, &base).unwrap_err().contains("command"));

        let http = McpDraft {
            transport: "http".into(),
            ..base.clone()
        };
        assert!(save_mcp_server(&mut d, &http).unwrap_err().contains("url"));
    }

    #[test]
    fn default_model_requires_a_provider_slash_model_reference() {
        let mut d = DocumentMut::new();
        assert!(set_default_model(&mut d, "sonnet").is_err());
        set_default_model(&mut d, "ws/sonnet").unwrap();
        assert!(d.to_string().contains(r#"model = "ws/sonnet""#));
    }

    // -----------------------------------------------------------------------
    // 落盘路径
    //
    // 下面两条会改 `CARTER_HOME` —— 那是**进程级**的，而 cargo 并行跑测试。
    // 用**全 crate 共用**的那把锁（见 `paths::ENV_LOCK`）把这类测试串起来。
    // -----------------------------------------------------------------------

    /// 在临时 `CARTER_HOME` 下跑一段涉及真实配置文件的操作。
    fn with_temp_home<T>(tag: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::paths::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("carter-app-cfg-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: 只有本模块的顺序化测试会改这两个变量。
        unsafe { std::env::set_var("CARTER_HOME", &dir) };
        let out = f();
        unsafe { std::env::remove_var("CARTER_HOME") };
        let _ = std::fs::remove_dir_all(dir);
        out
    }

    /// 写回 → `Config::load()` 必须能读回来。这是「界面改的配置对 carter 有效」
    /// 的最终判据 —— 只把 TOML 写对还不够，得是 carter 认的那个形状。
    #[test]
    fn a_saved_config_is_readable_by_carter() {
        with_temp_home("roundtrip", || {
            let mut doc = DocumentMut::new();
            save_provider(
                &mut doc,
                &ProviderDraft {
                    name: "ws".into(),
                    kind: "anthropic".into(),
                    api_key: Some("${MY_KEY}".into()),
                    base_url: None,
                    server_tools: vec!["web_search".into()],
                    server_tool_max_uses: Some(5),
                    models: vec![ModelDraft {
                        name: "sonnet".into(),
                        meta: "anthropic/claude-sonnet-4-5".into(),
                        api_name: Some("claude-sonnet-4-6".into()),
                        default_reasoning: Some("medium".into()),
                    }],
                },
            )
            .unwrap();
            set_default_model(&mut doc, "ws/sonnet").unwrap();
            set_skills_enabled(&mut doc, true).unwrap();

            let cfg = write_and_reload(&doc).expect("写回的配置 carter 应当读得回来");

            let p = cfg.providers.get("ws").expect("渠道应当读回来");
            assert_eq!(p.kind, "anthropic");
            assert_eq!(p.base_url, None);
            assert_eq!(p.server_tools, vec!["web_search".to_string()]);
            assert_eq!(p.server_tool_max_uses, Some(5));
            let m = p.models.get("sonnet").expect("模型应当读回来");
            assert_eq!(m.meta, "anthropic/claude-sonnet-4-5");
            assert_eq!(m.api_name.as_deref(), Some("claude-sonnet-4-6"));
            assert_eq!(cfg.agent.model, "ws/sonnet");
            assert!(cfg.skills.enabled);
        });
    }

    /// **写坏了的兜底。** 构造一份 TOML 合法、但 carter 解析不了的文档：
    /// 写盘之后必须还原成上一版，而不是把用户原本能用的配置留在半坏状态。
    #[test]
    fn a_config_that_carter_cannot_parse_is_rolled_back() {
        with_temp_home("rollback", || {
            let path = config_path();
            std::fs::write(&path, "GOOD = 1
").unwrap();

            // TOML 层面完全合法，但 `agent.model` 要的是字符串。
            let mut bad = DocumentMut::new();
            bad["agent"] = toml_edit::Item::Table(toml_edit::Table::new());
            bad["agent"]["model"] = value(123_i64);

            let err = write_and_reload(&bad).unwrap_err();
            assert!(err.contains("已还原"), "错误信息要说清做了什么，实际：{err}");

            let after = std::fs::read_to_string(&path).unwrap();
            assert_eq!(after, "GOOD = 1
", "原内容必须原样回来");
        });
    }

    // 测试辅助：不落盘的 provider_detail。
    fn provider_detail_from(d: &DocumentMut, name: &str) -> ProviderDetail {
        let file: RawFile = toml_edit::de::from_str(&d.to_string()).unwrap();
        let p = file.providers.get(name).unwrap();
        let raw_key = p.api_key.clone().unwrap_or_default();
        ProviderDetail {
            name: name.into(),
            kind: p.kind.clone(),
            has_api_key: !raw_key.is_empty(),
            api_key: raw_key,
            base_url: p.base_url.clone().unwrap_or_default(),
            server_tools: p.server_tools.clone(),
            server_tool_max_uses: p.server_tool_max_uses,
            models: p
                .models
                .iter()
                .map(|(k, m)| ModelDetail {
                    name: k.clone(),
                    meta: m.meta.clone(),
                    api_name: m.api_name.clone(),
                    default_reasoning: m.default_reasoning.clone(),
                })
                .collect(),
        }
    }
}
