//! 按需补模型元数据：用户的渠道用了目录里没有的模型时，只拉那几家服务商的条目补进缓存。
//!
//! ## 为什么不是「每次刷新拉全量」
//!
//! `models.dev/api.json` 压缩后也有 600 KB，走加速站还有十几分钟的 CDN 缓存 ——
//! 新出的模型在官方那份里总是先有。用户真正缺的通常只是一两家：加了 OpenRouter
//! 或者某个新出的国产模型，日期新一点，目录里没收录。
//!
//! 所以这里按**服务商**去拉：models.dev 源仓库里 `providers/<id>/models/**/*.toml`
//! 是逐家存的，经加速站取某一家只有几十 KB；再和本地缓存合并，只补**缺的**条目
//! （已有的不动 —— 用户可能是因为旧条目更准才没刷新）。
//!
//! ## 什么时候跑
//!
//! 保存渠道之后（`config_edit::apply`）。后台跑、不阻塞保存；同一时间只跑一份；
//! 半小时内不重复补同一个服务商 —— 网络不好时不会变成每存一次渠道就挂一次请求。
//!
//! ## 拿不到「非对话」模型
//!
//! 只收输出含 text 的模型。生图 / 语音那几类在这个应用里用不上，界面本来也
//! 把 `meta` 标成「不是对话模型」，补进来反而让人以为能选。
//!
//! ## 上下文窗口在原厂那条里
//!
//! provider 文件通常只是「引用原厂 + 覆盖价格 / 缓存」（`base_model = "…"`），
//! `limit`、展示名、模态都在 `models/<lab>/<model>.toml` 里。只翻 provider 文件
//! 会补出一条「能查到、但没有上下文窗口」的模型 —— 那等于没补。所以拉完一家的
//! provider 文件之后，按 `base_model` 把缺的原厂文件也拉下来，合并时原厂缺的
//! 字段由 provider 补、provider 缺的由原厂补。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use carter::config::Config;

/// 补一家服务商最多花多久。超时不报错，下次保存渠道再试。
const PER_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
/// 一家网络并发。源仓库里一家最多几百个文件，8 路已经很客气了。
const CONCURRENCY: usize = 8;
/// 同一个服务商两次补全之间的最短间隔。
const COOLDOWN: Duration = Duration::from_secs(30 * 60);

fn cooldown() -> &'static Mutex<std::collections::HashMap<String, Instant>> {
    static M: OnceLock<Mutex<std::collections::HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 现在能不能补这个服务商（冷却期过了、且没在跑）。
fn claim(provider: &str) -> bool {
    let Ok(mut m) = cooldown().lock() else {
        return false;
    };
    match m.get(provider) {
        Some(at) if at.elapsed() < COOLDOWN => false,
        _ => {
            m.insert(provider.to_string(), Instant::now());
            true
        }
    }
}

/// 配置里哪些服务商「该补」。返回 models.dev 的 id 与它下面缺的模型名。
///
/// 判断「缺」的依据只有一条：缓存里这个服务商下一条都没有，或者模型名对不上。
/// 服务商 id 的认法沿用 [`crate::catalog::detect_provider`]（按 base_url），
/// 认不出来的（用户自建中转）不补 —— 目录里本来就没有它们。
pub fn missing_from(config: &Config, cache_json: &str) -> Vec<(String, Vec<String>)> {
    let root: serde_json::Value = match serde_json::from_str(cache_json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for pcfg in config.providers.values() {
        let base = pcfg.base_url.as_deref().unwrap_or_default();
        let Some(pid) = crate::catalog::detect_provider(base) else {
            continue;
        };
        let models = root.get(&pid).and_then(|p| p.get("models"));
        let missing: Vec<String> = pcfg
            .models
            .iter()
            .filter(|(_, m)| {
                let Some((_, id)) = m.meta.split_once('/') else {
                    return false;
                };
                models.and_then(|ms| ms.get(id)).is_none_or(|v| v.is_null())
            })
            // 只要 meta 指向这个服务商（或者 meta 写错但名字看着像它家的）的，
            // 不能靠猜 —— 只补 meta 已经写了这个服务商的。
            .filter(|(_, m)| m.meta.split_once('/').is_some_and(|(p, _)| p == pid))
            .filter_map(|(_, m)| Some(m.meta.split_once('/')?.1.to_string()))
            .collect();
        if !missing.is_empty() {
            out.push((pid, missing));
        }
    }
    out
}

/// 保存渠道之后的后台入口（`config_edit::apply` 调它）。
///
/// 立刻把配置快照拷进任务里再返回 —— 调用方在同步的写回流程里，不能等网络。
pub fn spawn_for_models_sync(shell: &crate::shell::AppShell) {
    if !crate::app_settings::AppSettings::load().network_ok() {
        return;
    }
    let config = shell.config.load_full();
    let sent = spawn_detached(async move {
        if let Err(e) = run_with(config).await {
            tracing::warn!(error = %e, "模型目录补全没做成");
        }
    });
    if !sent {
        // 配置已经写好了，补全只是锦上添花 —— 不能为了它把调用方崩掉。
        // 走到这里说明命令还是同步的（见 `config_edit::apply` 的说明）。
        tracing::warn!("模型目录补全要 tokio runtime，这次在同步上下文里，跳过");
    }
}

/// 把任务送进**当前** runtime；没有 runtime 时返回 `false`，不 panic。
///
/// 为什么不能直接 `tokio::spawn`：`config_edit::apply` 既被 async 命令调用，
/// 也被同步命令调用，而同步的 `#[tauri::command]` 跑在主线程上、那里没有
/// reactor —— `tokio::spawn` 会 panic，release 又是 `panic = "abort"`，
/// 表现就是「改一下主模型，应用当场闪退」。这类地方一律走这个入口。
pub(crate) fn spawn_detached<F>(f: F) -> bool
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(f);
            true
        }
        Err(_) => false,
    }
}

/// 真正干活：找出该补的 → 逐家拉 → 合并 → 落盘。返回补进来的条数。
pub async fn run_with(config: std::sync::Arc<Config>) -> Result<usize, String> {
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Ok(0);
    }
    let result = run_inner(&config).await;
    RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    match &result {
        Ok(n) if *n > 0 => tracing::info!(added = n, "模型目录补全：新补了 {n} 条"),
        Err(e) => tracing::warn!(error = %e, "模型目录补全没做成"),
        Ok(_) => {}
    }
    result
}

async fn run_inner(config: &Config) -> Result<usize, String> {
    if !crate::app_settings::AppSettings::load().network_ok() {
        return Ok(0);
    }
    let cache_json = crate::catalog::read_cache()?;
    let wanted = missing_from(config, &cache_json);
    let mut root: serde_json::Value =
        serde_json::from_str(&cache_json).map_err(|e| format!("目录解析失败：{e}"))?;
    let prefs = crate::app_settings::AppSettings::load().net_prefs();
    let mut added = 0usize;
    for (pid, missing) in wanted {
        if !claim(&pid) {
            continue;
        }
        match fetch_provider(&prefs, &pid).await {
            Ok(models) => {
                let n = merge_models(&mut root, &pid, &missing, models);
                added += n;
            }
            Err(e) => tracing::warn!(provider = %pid, error = %e, "这一家没补上"),
        }
    }
    if added > 0 {
        let json = serde_json::to_string(&root).map_err(|e| e.to_string())?;
        carter::registry::fetch::write_cache(&json).map_err(|e| format!("写缓存失败：{e}"))?;
    }
    Ok(added)
}

/// 把 `models` 里缺的那几条并进 `root[pid].models`，返回新增条数。
///
/// `root` 头里那个 `$generated_at`（内置快照的生成日期）合并时丢掉 —— 线上缓存
/// 不该带本地快照的标记。
fn merge_models(
    root: &mut serde_json::Value,
    pid: &str,
    missing: &[String],
    models: serde_json::Map<String, serde_json::Value>,
) -> usize {
    let Some(obj) = root.as_object_mut() else {
        return 0;
    };
    let entry = obj
        .entry(pid.to_string())
        .or_insert_with(|| serde_json::json!({ "id": pid, "models": {} }));
    if entry.get("models").and_then(|m| m.as_object()).is_none() {
        entry["models"] = serde_json::json!({});
    }
    let target = entry["models"].as_object_mut().expect("上面刚保证过");
    let mut added = 0;
    for id in missing {
        if target.contains_key(id) {
            continue;
        }
        if let Some(m) = models.get(id) {
            target.insert(id.clone(), m.clone());
            added += 1;
        }
    }
    added
}

// ---------------------------------------------------------------------------
// 从 models.dev 源仓库取一家服务商的模型
// ---------------------------------------------------------------------------

const REPO: &str = "anomalyco/models.dev";
const REF: &str = "dev";

/// 源仓库在 CDN 上的路径前缀（jsDelivr 的 GitHub 形态）。
const RAW: &str = "https://raw.githubusercontent.com/anomalyco/models.dev";

/// 列这家服务商在源仓库里的模型文件（完整路径），外加钉住的那次提交（如果拿到了）。
///
/// 两种列法各有各的短板，按代价从小到大试：
///
/// 1. **GitHub 树接口**：一次请求拿到全仓清单，顺手得到当前提交号（用来钉 jsDelivr）。
///    不带 token 每小时 60 次 —— 对「保存渠道时补一家」绰绰有余，但它要代理才稳；
/// 2. **jsDelivr `flat` 清单**：国内直连，走 CDN；代价是分支名给的清单会被缓存
///    十几分钟，刚合并的模型可能不在里面（拿不到钉住需要的提交号，只能按分支取）。
///
/// 两个都失败才算失败，交给调用方记录、下次保存渠道再试。
async fn list_provider_files(
    prefs: &crate::net::NetPrefs,
    client: &reqwest::Client,
    pid: &str,
) -> Result<(Vec<String>, Option<String>), String> {
    let prefix = format!("providers/{pid}/models/");
    let mut last_err = String::from("没有可用的列目录方式");

    if let Some((files, commit)) = list_from_tree(client, &prefix).await {
        return Ok((files, Some(commit)));
    }
    for base in jsdelivr_flat_apis() {
        let Ok(res) =
            crate::net::get_first(prefs, &[base.as_str()], Duration::from_secs(15), None, None)
                .await
        else {
            continue;
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&res.bytes) else {
            continue;
        };
        let files: Vec<String> = v
            .get("files")
            .and_then(|f| f.as_array())
            .into_iter()
            .flatten()
            .filter_map(|f| f.get("name").and_then(|n| n.as_str()))
            .filter(|n| n.starts_with(&format!("/{prefix}")) && n.ends_with(".toml"))
            .map(|n| n.trim_start_matches('/').to_string())
            .collect();
        if !files.is_empty() {
            return Ok((files, None));
        }
    }

    // 树接口的具体失败原因已经吞在上面了（只用来决定要不要换下一种），
    // 这里把 jsDelivr 的也带上一句，日志里看得出到底卡在哪。
    last_err.push_str("；jsDelivr 清单也没有拿到");
    Err(last_err)
}

/// GitHub 树接口：全仓文件 + 当前提交号。符号链接是别家的条目换了个名字，跳过
/// （原厂那条本来就在）。任何失败都返回 `None` —— 调用方还有 jsDelivr。
async fn list_from_tree(client: &reqwest::Client, prefix: &str) -> Option<(Vec<String>, String)> {
    let url = format!("https://api.github.com/repos/{REPO}/git/trees/{REF}?recursive=1");
    let res = client
        .get(url)
        .header("user-agent", "carter-app")
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let v: serde_json::Value = res.json().await.ok()?;
    let commit = v.get("sha").and_then(|s| s.as_str())?.to_string();
    let files = v
        .get("tree")
        .and_then(|t| t.as_array())?
        .iter()
        .filter(|node| {
            let path = node
                .get("path")
                .and_then(|p| p.as_str())
                .unwrap_or_default();
            let mode = node
                .get("mode")
                .and_then(|p| p.as_str())
                .unwrap_or_default();
            path.starts_with(prefix) && path.ends_with(".toml") && mode == "100644"
        })
        .filter_map(|node| Some(node.get("path")?.as_str()?.to_string()))
        .collect::<Vec<_>>();
    (!files.is_empty()).then_some((files, commit))
}

/// jsDelivr 的文件清单入口：主站与 fastly 节点（主站被墙时另一个常常还行）。
fn jsdelivr_flat_apis() -> [String; 2] {
    [
        format!("https://data.jsdelivr.com/v1/package/gh/{REPO}@{REF}/flat"),
        format!("https://data-jsdelivr-com.fastly.jsdelivr.net/v1/package/gh/{REPO}@{REF}/flat"),
    ]
}

/// 拉一家的全部模型，翻译成缓存条目（按模型 id 索引）。
async fn fetch_provider(
    prefs: &crate::net::NetPrefs,
    pid: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let mut builder = reqwest::Client::builder().timeout(PER_PROVIDER_TIMEOUT);
    builder = if prefs.proxy.is_empty() {
        builder.no_proxy()
    } else {
        builder.proxy(reqwest::Proxy::all(&prefs.proxy).map_err(|e| e.to_string())?)
    };
    let client = builder.build().map_err(|e| e.to_string())?;
    let (files, commit) = list_provider_files(prefs, &client, pid).await?;
    if files.is_empty() {
        return Err(format!("源仓库里没有 `{pid}` 的模型目录"));
    }

    let (defs, bases) = definitions(prefs, pid, &files, commit.as_deref()).await;

    // 合并：provider 文件缺的展示字段从它指的原厂条目补（`base_model`），
    // 原厂条目缺的用 provider 文件自己的。两边都没有 `modalities` 的无从判断
    // 是不是对话模型，按对话处理 —— 漏掉比多出一条更糟。
    let mut out = serde_json::Map::new();
    for (id, entry) in &defs {
        let merged = match bases.get(id).and_then(|b| defs.get(b)) {
            Some(base) => inherit(base, entry),
            None => entry.clone(),
        };
        if let Some(mods) = merged.get("modalities")
            && !mods
                .get("output")
                .and_then(|o| o.as_array())
                .is_some_and(|o| o.iter().any(|v| v.as_str() == Some("text")))
        {
            continue;
        }
        out.insert(id.clone(), merged);
    }
    Ok(out)
}

/// provider 条目继承原厂条目：原厂**有而自己没有**的字段照抄，自己有的优先。
///
/// 不递归 `base_model`（原厂条目自己不会再引用别人）；`id` / `canonical_model_id`
/// 这类身份字段永远用自己那份。
fn inherit(base: &serde_json::Value, own: &serde_json::Value) -> serde_json::Value {
    let (Some(b), Some(o)) = (base.as_object(), own.as_object()) else {
        return own.clone();
    };
    let mut out = o.clone();
    for (k, v) in b {
        if k == "id" || k == "canonical_model_id" {
            continue;
        }
        if !out.contains_key(k) || out.get(k).is_some_and(|x| x.is_null()) {
            out.insert(k.clone(), v.clone());
        }
    }
    serde_json::Value::Object(out)
}

/// 一条 provider 模型 TOML → 缓存里的一条。
///
/// 字段名与 models.dev `api.json` 一致（carter 与 `catalog` 都按这些名字取）。
/// 只保留对话模型（输出含 text）。
///
/// provider 文件常常只是「引用原厂条目 + 覆盖几项」（`base_model = "…"`），
/// 没有 `modalities` / `limit`。这些**不在这里补**：补的地方在 [`definitions`]，
/// 那里能同时看到原厂文件（见那里的说明）。
fn provider_model_entry(id: &str, text: &str) -> Option<serde_json::Value> {
    let t: toml_edit::DocumentMut = text.parse().ok()?;
    let d = t.as_table();

    let s = |k: &str| d.get(k).and_then(|i| i.as_str()).map(str::to_string);
    let b = |k: &str| d.get(k).and_then(|i| i.as_bool());
    if b("v2_only").unwrap_or(false) {
        return None;
    }
    // 有 modalities 才判「是不是对话模型」；没有的到合并原厂文件时再判。
    if let Some(mods) = d.get("modalities").and_then(|m| m.as_table_like())
        && let Some(out) = mods.get("output").and_then(|a| a.as_array())
        && !out.iter().any(|v| v.as_str() == Some("text"))
    {
        return None;
    }

    let mut out = serde_json::Map::new();
    out.insert("id".into(), id.into());
    if let Some(name) = s("name") {
        out.insert("name".into(), name.into());
    }
    if let Some(desc) = s("description") {
        out.insert("description".into(), desc.into());
    }
    if let Some(family) = s("family") {
        out.insert("family".into(), family.into());
    }
    if let Some(bm) = s("base_model") {
        out.insert("canonical_model_id".into(), bm.into());
    }
    if let Some(v) = b("open_weights") {
        out.insert("open_weights".into(), v.into());
    }
    for k in ["knowledge", "release_date", "last_updated", "status"] {
        if let Some(v) = s(k) {
            out.insert(k.into(), v.into());
        }
    }

    // 模态：v2 里具体 MIME 归回 v1 的大类（`application/pdf` → `pdf`）。
    if let Some(mods) = d.get("modalities").and_then(|m| m.as_table_like()) {
        let (input, output) = (modality_list(mods, "input"), modality_list(mods, "output"));
        out.insert(
            "modalities".into(),
            serde_json::json!({ "input": input, "output": output }),
        );
        out.insert(
            "attachment".into(),
            out.get("modalities")
                .and_then(|m| m.get("input"))
                .and_then(|i| i.as_array())
                .is_some_and(|a| a.iter().any(|v| v.as_str() != Some("text")))
                .into(),
        );
    }

    if let Some(caps) = d.get("capabilities").and_then(|c| c.as_table_like()) {
        let supported = |k: &str| {
            caps.get(k)
                .and_then(|r| r.as_table_like())
                .and_then(|r| r.get("supported"))
                .and_then(|v| v.as_bool())
        };
        if let Some(v) = supported("tools") {
            out.insert("tool_call".into(), v.into());
        }
        if let Some(v) = supported("reasoning") {
            out.insert("reasoning".into(), v.into());
        }
        for k in ["structured_output", "temperature"] {
            if let Some(v) = caps.get(k).and_then(|v| v.as_bool()) {
                out.insert(k.into(), v.into());
            }
        }
    }

    if let Some(limit) = d.get("limit").and_then(|l| l.as_table_like()) {
        let num = |k: &str| limit.get(k).and_then(|v| v.as_integer());
        if let Some(ctx) = num("context") {
            out.insert(
                "limit".into(),
                match num("output") {
                    Some(o) => serde_json::json!({ "context": ctx, "output": o }),
                    None => serde_json::json!({ "context": ctx }),
                },
            );
        }
    }

    Some(serde_json::Value::Object(out))
}

fn modality_list(mods: &dyn toml_edit::TableLike, key: &str) -> Vec<String> {
    let raw: Vec<String> = mods
        .get(key)
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|m| {
                    if m == "application/pdf" {
                        "pdf".to_string()
                    } else {
                        m.split('/').next().unwrap_or(m).to_string()
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut seen = HashSet::new();
    raw.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

/// jsDelivr 上能一次取一家的所有定义：`definitions` = 原厂条目 + 该服务商条目，
/// 以及「叫这个名字的那条文件路径」→ 它的 `base_model`（跟着链接走到原厂）。
///
/// ## 为什么要把原厂文件也拉下来
///
/// provider 文件常常只有「引用原厂 + 覆盖几项」（`base_model = "…"`，外加价格、
/// 缓存），`modalities` / `limit` / 展示名都在原厂那一条里。只翻 provider 文件
/// 得到的条目会缺上下文窗口，而上下文窗口正是「刷新元数据」最想补上的东西。
async fn definitions(
    prefs: &crate::net::NetPrefs,
    pid: &str,
    files: &[String],
    commit: Option<&str>,
) -> (
    serde_json::Map<String, serde_json::Value>,
    HashMap<String, String>,
) {
    let mut defs: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut bases: HashMap<String, String> = HashMap::new();
    let queue = std::sync::Arc::new(Mutex::new(files.to_vec()));
    let mut tasks = Vec::new();
    for _ in 0..CONCURRENCY {
        let queue = queue.clone();
        let prefs = prefs.clone();
        let commit = commit.map(str::to_string);
        tasks.push(tokio::spawn(async move {
            let mut got = Vec::new();
            while let Some(path) = queue.lock().ok().and_then(|mut q| q.pop()) {
                // 先拿这条自己：没有 `base_model`（或就是原厂文件）时它就是定义。
                let url = fetch_url(&commit, &path);
                let Ok(f) = crate::net::get_first(
                    &prefs,
                    &[url.as_str()],
                    crate::net::SOURCE_TIMEOUT,
                    None,
                    None,
                )
                .await
                else {
                    continue;
                };
                let text = String::from_utf8_lossy(&f.bytes).into_owned();
                let id = file_stem(&path).to_string();
                let Some(entry) = provider_model_entry(&id, &text) else {
                    continue;
                };
                let base = entry
                    .get("canonical_model_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                got.push((id, entry, base));
            }
            got
        }));
    }
    for t in tasks {
        if let Ok(got) = t.await {
            for (id, entry, base) in got {
                if let Some(base) = base {
                    bases.insert(id.clone(), base);
                }
                defs.entry(id).or_insert(entry);
            }
        }
    }

    // 缺的那几家原厂文件：`<lab>/<model>` → `models/<lab>/<model>.toml`。
    let needed: Vec<String> = bases
        .values()
        .filter(|b| !defs.contains_key(*b))
        .map(|b| format!("models/{b}.toml"))
        .collect();
    if !needed.is_empty() {
        let queue = std::sync::Arc::new(Mutex::new(needed));
        let mut tasks = Vec::new();
        for _ in 0..CONCURRENCY {
            let queue = queue.clone();
            let prefs = prefs.clone();
            let commit = commit.map(str::to_string);
            tasks.push(tokio::spawn(async move {
                let mut got = Vec::new();
                while let Some(path) = queue.lock().ok().and_then(|mut q| q.pop()) {
                    let url = fetch_url(&commit, &path);
                    let Ok(f) = crate::net::get_first(
                        &prefs,
                        &[url.as_str()],
                        crate::net::SOURCE_TIMEOUT,
                        None,
                        None,
                    )
                    .await
                    else {
                        continue;
                    };
                    let key = path
                        .strip_prefix("models/")
                        .and_then(|p| p.strip_suffix(".toml"))
                        .unwrap_or_default()
                        .to_string();
                    if let Some(entry) = provider_model_entry(
                        key.rsplit('/').next().unwrap_or(&key),
                        &String::from_utf8_lossy(&f.bytes),
                    ) {
                        got.push((key, entry));
                    }
                }
                got
            }));
        }
        for t in tasks {
            if let Ok(got) = t.await {
                for (key, entry) in got {
                    defs.entry(key).or_insert(entry);
                }
            }
        }
    }
    let _ = pid;
    (defs, bases)
}

/// 一条源仓库文件在哪个地址取：钉了提交就走 jsDelivr，否则按常规顺序试。
fn fetch_url(commit: &Option<String>, path: &str) -> String {
    match commit {
        Some(c) => crate::net::jsdelivr_pinned("anomalyco/models.dev", c, path),
        None => format!("{RAW}/{REF}/{path}"),
    }
}

fn file_stem(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .unwrap_or_default()
        .trim_end_matches(".toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 同步上下文（没有 tokio runtime）里必须安静跳过 —— 用户报的「改主模型就闪退」
    /// 就是这条路上 `tokio::spawn` panic（release 是 `panic = "abort"`）造成的。
    #[test]
    fn detached_spawn_outside_a_runtime_skips_instead_of_panicking() {
        assert!(!spawn_detached(async {}), "没有 runtime 时应当跳过");
    }

    /// 有 runtime 时照常跑 —— 否则上面那条「不 panic」会靠「永远不干活」通过。
    #[tokio::test]
    async fn detached_spawn_inside_a_runtime_runs_the_task() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        assert!(
            spawn_detached(async move { flag.store(true, Ordering::SeqCst) }),
            "有 runtime 时应当送进去"
        );
        for _ in 0..50 {
            if ran.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("任务没有跑起来");
    }

    const TOML: &str = r#"
schema = 2
name = "Claude Sonnet 5"
family = "claude-sonnet"
base_model = "anthropic/claude-sonnet-5"
knowledge = "2026-01"
release_date = "2026-06-29"

[modalities]
input = ["text", "image/png", "application/pdf"]
output = ["text"]

[capabilities]
tools = { supported = true }
reasoning = { supported = false }
temperature = true

[limit]
context = 1000000
output = 128000
"#;

    #[test]
    fn a_provider_model_becomes_a_cache_entry() {
        let e = provider_model_entry("claude-sonnet-5", TOML).unwrap();
        assert_eq!(e["id"], "claude-sonnet-5");
        assert_eq!(e["name"], "Claude Sonnet 5");
        assert_eq!(e["canonical_model_id"], "anthropic/claude-sonnet-5");
        assert_eq!(e["tool_call"], true);
        assert_eq!(e["reasoning"], false);
        assert_eq!(e["limit"]["context"], 1000000);
        assert_eq!(
            e["modalities"]["input"],
            serde_json::json!(["text", "image", "pdf"])
        );
        assert_eq!(e["attachment"], true);
    }

    #[test]
    fn provider_entries_inherit_the_lab_entry() {
        let base = provider_model_entry(
            "claude-opus-5-5",
            r#"
schema = 2
name = "Claude Opus 5.5"
open_weights = false
[modalities]
input = ["text", "image/png"]
output = ["text"]
[limit]
context = 1_000_000
output = 128_000
"#,
        )
        .unwrap();
        let own = provider_model_entry(
            "claude-opus-5-5",
            r#"
schema = 2
base_model = "anthropic/claude-opus-5-5"
[capabilities]
tools = { supported = true }
[cost.text.input]
price = 5
"#,
        )
        .unwrap();
        let merged = inherit(&base, &own);
        assert_eq!(merged["name"], "Claude Opus 5.5", "展示名从原厂来");
        assert_eq!(merged["limit"]["context"], 1_000_000, "上下文窗口从原厂来");
        assert_eq!(merged["tool_call"], true, "自己的能力优先");
        assert_eq!(merged["canonical_model_id"], "anthropic/claude-opus-5-5");
    }

    #[test]
    fn non_chat_models_are_skipped() {
        let image = TOML.replace(r#"output = ["text"]"#, r#"output = ["image/png"]"#);
        assert!(provider_model_entry("img", &image).is_none());
    }

    #[test]
    fn merging_only_adds_what_is_missing() {
        let mut root = serde_json::json!({
            "acme": { "id": "acme", "models": { "a": {"id": "a", "limit": {"context": 1}} } }
        });
        let mut incoming = serde_json::Map::new();
        incoming.insert(
            "a".into(),
            serde_json::json!({"id": "a", "limit": {"context": 2}}),
        );
        incoming.insert("b".into(), serde_json::json!({"id": "b"}));
        let n = merge_models(
            &mut root,
            "acme",
            &["a".into(), "b".into(), "c".into()],
            incoming,
        );
        assert_eq!(n, 1);
        assert_eq!(
            root["acme"]["models"]["a"]["limit"]["context"], 1,
            "已有的不动"
        );
        assert!(root["acme"]["models"].get("b").is_some());
        assert!(
            root["acme"]["models"].get("c").is_none(),
            "源里没有的不硬塞"
        );
    }

    #[test]
    fn a_new_provider_block_is_created_when_merging() {
        let mut root = serde_json::json!({});
        let mut incoming = serde_json::Map::new();
        incoming.insert("x".into(), serde_json::json!({"id": "x"}));
        assert_eq!(
            merge_models(&mut root, "newone", &["x".into()], incoming),
            1
        );
        assert_eq!(root["newone"]["models"]["x"]["id"], "x");
    }

    #[test]
    fn missing_from_only_looks_at_bounds_metas() {
        let cache = r#"{"anthropic":{"models":{"claude-opus-5-5":{}}}}"#;
        let toml = r#"
[providers.anth]
kind = "anthropic"
base_url = "https://api.anthropic.com"
[providers.anth.models.opus]
meta = "anthropic/claude-opus-5-5"
[providers.anth.models.newone]
meta = "anthropic/claude-opus-6"
[providers.anth.models.typo]
meta = "anthropic-official/claude-opus-5-5"
"#;
        let config: carter::config::Config = toml_edit::de::from_str(toml).unwrap();
        let missing = missing_from(&config, cache);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, "anthropic");
        assert_eq!(missing[0].1, vec!["claude-opus-6".to_string()]);
    }

    /// 真联网：只补一家、只要 `claude-opus-6` 这种一定存在的名字。目录不通时
    /// 跳过而不是失败（CI 与离线机器上这条不该变成红灯）。
    ///
    /// `cargo test -p carter-app-core --lib models_sync::tests::live -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn live_fetches_one_provider_through_jsdelivr() {
        let prefs = crate::net::NetPrefs::default();
        let mut root: serde_json::Value = serde_json::json!({"anthropic": {"models": {}}});
        // 用真实清单里一定有的一家：anthropic 的 provider.toml 与它的一半模型。
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let (files, commit) = match list_provider_files(&prefs, &client, "anthropic").await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("跳过：{e}");
                return;
            }
        };
        assert!(!files.is_empty());
        eprintln!("列目录用的提交：{commit:?}，{} 个文件", files.len());
        let map = match fetch_provider(&prefs, "anthropic").await {
            Ok(m) => m,
            Err(e) => {
                eprintln!("跳过：{e}");
                return;
            }
        };
        assert!(
            map.contains_key("claude-opus-5-5"),
            "拿到了 {:?}",
            map.keys().take(5).collect::<Vec<_>>()
        );
        let n = merge_models(
            &mut root,
            "anthropic",
            &["claude-opus-5-5".to_string()],
            map,
        );
        assert_eq!(n, 1);
        // 上下文窗口在**原厂条目**里（`anthropic/claude-opus-5-5`），要经由
        // `base_model` 继承过来 —— 这正是这次补全存在的意义。
        assert_eq!(
            root["anthropic"]["models"]["claude-opus-5-5"]["limit"]["context"],
            1_000_000
        );
    }
}
