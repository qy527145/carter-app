//! 网络：GitHub 加速站、多源下载、子进程的代理 / 加速环境变量。
//!
//! 这个应用要联网拿的东西几乎都在境外，而国内网络对它们的可达性差别很大：
//!
//! * `raw.githubusercontent.com` / `github.com` 经常整段不通；
//! * `models.dev` 挂在 Cloudflare 美国节点，6 MB 的目录能跑到几十秒、还会中途断掉。
//!
//! 所以凡是「从外网取一份东西」的地方都走同一条路：**官方源优先，失败逐个试
//! 加速站，全失败再落到本地的兜底**。兜底各不相同（模型目录有内置快照、下载包
//! 有签名校验），但「怎么试」只在这一处写。
//!
//! ## 为什么每个源都 `no_proxy`
//!
//! 用户开着系统代理时，`reqwest` 默认会让**所有**请求绕代理 —— 加速站在国内
//! 直连，绕一圈去境外再回来只会更慢。这里每个候选源都是显式地址，直连即可；
//! 真要代理，用 [`proxy`]（设置里那一个输入框）显式给。
//!
//! ## 给子进程的变量
//!
//! 连接器安装脚本、`git fetch`、`npm install` 都是外部进程，没法让它们「自己
//! 挨个试」。能做的只有：选一个**可达的**加速地址或用户代理，通过环境变量塞给
//! 它们（[`apply_to_env`]）。选的依据是一次很短的本机探测 —— 探测的是加速站的
//! 首页，不依赖任何具体仓库，失败只会让某个子进程慢一点，不会让主流程报错。

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// GitHub 加速站前缀，**按优先级**。空串 = 直连官方。
///
/// 只收「把完整 URL 接在后面的反向代理」这一种形态（`https://gh-proxy.com/https://…`），
/// 它们对 raw / releases / codeload 都通用。改动这张表时同步
/// `scripts/models-snapshot.ts` 里的同名表。
pub const GITHUB_ACCELS: &[&str] = &[
    "",
    "https://gh-proxy.com/",
    "https://ghproxy.net/",
    "https://ghfast.top/",
    "https://cdn.jsdelivr.net/",
];

/// 「把一个 GitHub URL 接在我后面」的加速站 —— `jsDelivr` 不是这种形态（它要
/// `cdn.jsdelivr.net/gh/<owner>/<repo>@<ref>/<path>`），只用在列目录那一处
/// （见 [`jsdelivr_tree`]），所以从这张表里排除。
fn is_prefix_accel(accel: &str) -> bool {
    accel.is_empty() || !accel.contains("jsdelivr")
}

/// models.dev 目录的候选源，**按优先级**。空串代表官方。
///
/// 镜像取的是 models.dev 源仓库（`anomalyco/models.dev`）里提交好的 `models.json`
/// —— provider 无关的元数据（上下文、能力、发布日期），正是解析模型要用到的
/// 那部分，而且只有几十 KB。仓库根目录的 `models.json` 每次合并都会更新，
/// 加速站再慢也有一份十几分钟内的。
pub const MODEL_SOURCES: &[&str] = &[
    "https://models.dev/api.json",
    "https://gh-proxy.com/https://raw.githubusercontent.com/anomalyco/models.dev/dev/models.json",
    "https://ghproxy.net/https://raw.githubusercontent.com/anomalyco/models.dev/dev/models.json",
    "https://ghfast.top/https://raw.githubusercontent.com/anomalyco/models.dev/dev/models.json",
];

/// 一次源尝试的超时。模型目录走慢的时候 30 秒能到；到不了的源本来也不该等。
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(30);

/// 整轮「挨个源试」的上限。用户点的是「刷新」，不是「挂在这里等网络」。
pub const ROUND_TIMEOUT: Duration = Duration::from_secs(60);

/// 一个候选源和它失败的原因，给人看。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceFailure {
    pub source: String,
    pub error: String,
}

/// 一轮下载的结果。
#[derive(Debug, Clone)]
pub struct Fetched {
    /// 实际拿到内容的那个源（出现的是加速站地址时，界面上可以说明「走的是镜像」）。
    pub source: String,
    pub bytes: Vec<u8>,
    /// 排在它前面、失败了的源。
    pub failed: Vec<SourceFailure>,
}

/// 网络偏好：用户在设置里选的加速 / 代理。全部有默认值，没配也能用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetPrefs {
    /// 允许走 GitHub 加速站。关掉之后只试官方源。
    pub accel: bool,
    /// 显式代理（`http://127.0.0.1:7897`、`socks5://…`）。空 = 不用代理。
    pub proxy: String,
}

impl Default for NetPrefs {
    fn default() -> Self {
        Self {
            accel: true,
            proxy: String::new(),
        }
    }
}

impl NetPrefs {
    /// 从「原始设置值」构造。`accel` 已经有默认值；这里再认一下环境变量
    /// `CARTER_ACCEL=0/1`（排障时临时关掉加速，不必去改界面）。
    pub fn from_raw(accel: bool, proxy: &str) -> Self {
        let accel = match std::env::var("CARTER_ACCEL") {
            Ok(v) => !matches!(v.trim(), "0" | "false" | "off" | "no"),
            Err(_) => accel,
        };
        Self {
            accel,
            proxy: proxy.trim().to_string(),
        }
    }

    /// 一组「基础地址」按偏好展开成候选 URL，按优先级排。
    ///
    /// * 非 GitHub 地址（models.dev 官方接口、用户自己填的镜像）原样保留 ——
    ///   加速站只认 GitHub，套上去只会 404；
    /// * GitHub 地址在开着加速时展开成「官方 + 各家加速站」，否则只剩官方；
    /// * `base` 里排在前面的地址，它的全部展开都排在后面的地址之前 —— 用户写
    ///   的顺序就是优先级。
    pub fn candidates(&self, base: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for url in base {
            if is_github(url) && self.accel {
                for accel in GITHUB_ACCELS {
                    push_once(&mut out, &accelerate(url, accel));
                }
            } else {
                push_once(&mut out, url);
            }
        }
        out
    }
}

const GITHUB_UPSTREAM: &str = "https://github.com/";
const RAW_UPSTREAM: &str = "https://raw.githubusercontent.com/";
const CODELOAD_UPSTREAM: &str = "https://codeload.github.com/";

fn push_once(out: &mut Vec<String>, url: &str) {
    if !out.iter().any(|u| u == url) {
        out.push(url.to_string());
    }
}

/// 这个地址能不能被加速站代理（GitHub 的几类下载地址）。
pub fn is_github(url: &str) -> bool {
    [GITHUB_UPSTREAM, RAW_UPSTREAM, CODELOAD_UPSTREAM]
        .iter()
        .any(|p| url.starts_with(p))
}

/// 把一个 GitHub 地址套到加速站前缀下。非 GitHub 地址原样返回 —— 加速站只
/// 认 GitHub，别把 API 请求也送过去。
pub fn accelerate(url: &str, accel: &str) -> String {
    if accel.is_empty() || !is_github(url) || !is_prefix_accel(accel) {
        url.to_string()
    } else {
        format!("{accel}{url}")
    }
}

/// jsDelivr 的 GitHub 地址形态：`https://cdn.jsdelivr.net/gh/<owner>/<repo>@<ref>/<path>`。
pub fn jsdelivr_url(owner_repo: &str, git_ref: &str, path: &str) -> String {
    format!(
        "https://cdn.jsdelivr.net/gh/{owner_repo}@{git_ref}/{}",
        path.trim_start_matches('/')
    )
}

/// 把源仓库的 Git 提交号「钉」进 jsDelivr：`gh/<repo>@<提交>`。
///
/// 为什么不在每次请求时现问提交号：`data.jsdelivr.com/v1/package/gh/<repo>` 的
/// `versions` 只列 Git **tag**，没有 tag 的仓库问不到分支指向哪里。而按分支名
/// 取会被 CDN 缓存 —— 实测 `@dev/flat` 的清单比分支现状晚了好几个提交
/// （新模型刚好落在差的那几个提交里）。所以要钉提交号，只能从别处拿到它：
/// 调用方传的是 GitHub 树接口上的 `sha`（一个请求，拿不到就退回按分支取）。
pub fn jsdelivr_pinned(owner_repo: &str, commit: &str, path: &str) -> String {
    jsdelivr_url(owner_repo, commit, path)
}

/// 人看的源名字：`gh-proxy.com` / `github.com` / `models.dev`。
pub fn source_label(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(url)
        .to_string()
}

/// 建一个请求客户端：按偏好直连或走显式代理，每个源单独超时。
fn client(prefs: &NetPrefs, timeout: Duration) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder().timeout(timeout);
    if prefs.proxy.is_empty() {
        b = b.no_proxy();
    } else {
        b = b.proxy(reqwest::Proxy::all(&prefs.proxy).map_err(|e| format!("代理地址不对：{e}"))?);
    }
    b.build().map_err(|e| format!("建 HTTP 客户端失败：{e}"))
}

/// 挨个源试，返回第一个成功的。
///
/// * `base`：按优先级排的基础地址（GitHub 地址会自动展开加速站）；
/// * `cancel`：外部取消开关（更新下载要能被用户取消）。取消后**不再**试下一个源，
///   直接把「已取消」报上去 —— 用户说停就是停。
type Cancel<'a> = Option<&'a std::sync::atomic::AtomicBool>;
type Progress<'a> = Option<&'a (dyn Fn(u64, Option<u64>) + Sync)>;

pub async fn get_first(
    prefs: &NetPrefs,
    base: &[&str],
    timeout: Duration,
    cancel: Cancel<'_>,
    progress: Progress<'_>,
) -> Result<Fetched, String> {
    let urls = prefs.candidates(base);
    if urls.is_empty() {
        return Err("没有可用的下载源".into());
    }
    // 整轮上限：单个源的超时之和可能很长，用户已经等够了就收。
    let deadline = Instant::now() + ROUND_TIMEOUT.max(timeout);
    let client = client(prefs, timeout)?;
    let mut failed = Vec::new();
    for url in urls {
        if let Some(c) = cancel
            && c.load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err("已取消".into());
        }
        if Instant::now() >= deadline {
            failed.push(SourceFailure {
                source: source_label(&url),
                error: "整体超时，没再试后面的源".into(),
            });
            break;
        }
        match fetch_one(&client, &url, cancel, progress).await {
            Ok(bytes) => {
                return Ok(Fetched {
                    source: url,
                    bytes,
                    failed,
                });
            }
            Err(e) if e == CANCELLED => return Err("已取消".into()),
            Err(e) => failed.push(SourceFailure {
                source: source_label(&url),
                error: e,
            }),
        }
    }
    Err(format_failures(&failed))
}

const CANCELLED: &str = "__cancelled__";

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    cancel: Cancel<'_>,
    progress: Progress<'_>,
) -> Result<Vec<u8>, String> {
    use futures::StreamExt;

    let res = client
        .get(url)
        .header("user-agent", "carter-app")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "连接超时".to_string()
            } else if e.is_connect() {
                "连不上".to_string()
            } else {
                e.to_string()
            }
        })?;
    let status = res.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let total = res.content_length();
    let mut out = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if let Some(c) = cancel
            && c.load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(CANCELLED.into());
        }
        let chunk = chunk.map_err(|e| e.to_string())?;
        out.extend_from_slice(&chunk);
        if let Some(p) = progress {
            p(out.len() as u64, total);
        }
    }
    Ok(out)
}

fn format_failures(failed: &[SourceFailure]) -> String {
    if failed.is_empty() {
        return "没有可用的下载源".into();
    }
    let list = failed
        .iter()
        .map(|f| format!("{}：{}", f.source, f.error))
        .collect::<Vec<_>>()
        .join("；");
    format!("所有源都失败（{list}）")
}

// ---------------------------------------------------------------------------
// 子进程用的加速 / 代理地址
// ---------------------------------------------------------------------------

/// 给外部命令用的环境变量集合。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildEnv {
    /// `https_proxy` / `HTTPS_PROXY` / `http_proxy` / `HTTP_PROXY`。
    pub proxy: Option<String>,
    /// 子进程该用的 GitHub 加速前缀（形如 `https://gh-proxy.com/`）。`None` = 直连官方。
    pub github: Option<String>,
}

impl ChildEnv {
    pub fn is_empty(&self) -> bool {
        self.proxy.is_none() && self.github.is_none()
    }

    /// 给界面的一句话：「直连」「走代理 127.0.0.1:7897」「走 gh-proxy.com」。
    pub fn describe(&self) -> String {
        match (&self.proxy, &self.github) {
            (Some(p), _) => format!("走代理 {p}"),
            (None, Some(g)) => format!("走 {}", source_label(g)),
            (None, None) => "直连官方".into(),
        }
    }
}

/// 探测结果缓存：每次启动只探一次，之后十分钟内复用。
fn probe_cache() -> &'static Mutex<Option<(Instant, NetPrefs, ChildEnv)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, NetPrefs, ChildEnv)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 为子进程挑一个可达的加速站。
///
/// * 用户配了代理 → 只给代理，子进程直连 GitHub 走代理是稳的；
/// * 否则挨个探测加速站的首页（单个 3 秒），第一个能回的用作 GitHub 加速前缀；
///   GitHub 官方本身能回就不给前缀 —— 原生地址比任何镜像都快。
///
/// 探测失败不算错误：返回空的 [`ChildEnv`]，子进程按原样跑。
pub async fn probe_child_env(prefs: &NetPrefs) -> ChildEnv {
    if !prefs.proxy.is_empty() {
        return ChildEnv {
            proxy: Some(prefs.proxy.clone()),
            github: None,
        };
    }
    // 设置变了（打开/关掉加速、改了代理）就重新探测：缓存按偏好区分，
    // 不能拿"上一次没开加速"的结果糊弄刚打开加速的用户。
    if let Ok(cache) = probe_cache().lock()
        && let Some((at, cached_prefs, env)) = cache.as_ref()
        && at.elapsed() < Duration::from_secs(600)
        && cached_prefs == prefs
    {
        return env.clone();
    }
    let mut env = ChildEnv {
        // 探测失败也照常用官方地址，只是记成「直连」。
        ..ChildEnv::default()
    };
    let client = client(prefs, Duration::from_secs(3)).ok();
    if let Some(client) = client {
        for accel in GITHUB_ACCELS {
            let root = if accel.is_empty() {
                "https://github.com"
            } else {
                accel.trim_end_matches('/')
            };
            // 探测的只是「这个站首页能不能回 200」，不碰任何具体仓库。
            if let Ok(res) = client.get(root).send().await
                && res.status().is_success()
            {
                env.github = if accel.is_empty() {
                    None
                } else {
                    Some(accel.to_string())
                };
                break;
            }
        }
    }
    if let Ok(mut cache) = probe_cache().lock() {
        *cache = Some((Instant::now(), prefs.clone(), env.clone()));
    }
    env
}

/// 把加速 / 代理写进一条将要执行的外部命令。
///
/// 变量名的选择是按「这几种程序各自认什么」来的：
///
/// * `curl` / `git` / `npm` / `sh`（安装脚本里的 curl）认 `https_proxy`，
///   顺便给 `ALL_PROXY`（curl 与 npm 都认，http 和 socks5 都行）；
/// * `powershell` 的 `Invoke-WebRequest` 只认 `HTTPS_PROXY`，两个大小写都给。
pub fn apply_to_env(env: &ChildEnv, cmd: &mut tokio::process::Command) {
    apply_to_std_env(env, cmd.as_std_mut());
}

/// 同 [`apply_to_env`]，给同步的 `std::process::Command`（团队 git 用）。
pub fn apply_to_std_env(env: &ChildEnv, cmd: &mut std::process::Command) {
    if let Some(proxy) = &env.proxy {
        for k in [
            "https_proxy",
            "HTTPS_PROXY",
            "http_proxy",
            "HTTP_PROXY",
            "ALL_PROXY",
        ] {
            cmd.env(k, proxy);
        }
    }
    if let Some(github) = &env.github {
        cmd.env("CARTER_GITHUB_ACCEL", github);
    }
}

/// 把命令字符串里**所有**出现的 GitHub 地址改写成走加速站。
///
/// 连接器的安装命令是写死的字符串（`curl -fsSL https://raw.githubusercontent.com/… | sh`、
/// `irm https://github.com/…/install.ps1 | iex`）。这些脚本自己还会下载别的 GitHub
/// 文件、跑 `npm install`，所以变量注入也要做 —— 这里改的是脚本里**明写**的那几个地址，
/// 实际下载走的是加速站，不需要脚本内部再懂镜像。
pub fn rewrite_github_urls(command: &str, github: Option<&str>) -> String {
    let Some(accel) = github.filter(|a| !a.is_empty()) else {
        return command.to_string();
    };
    let mut out = String::with_capacity(command.len() + 32);
    let mut rest = command;
    while let Some((at, _)) = [GITHUB_UPSTREAM, RAW_UPSTREAM]
        .iter()
        .filter_map(|p| rest.find(p).map(|i| (i, *p)))
        .min_by_key(|(i, _)| *i)
    {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| {
                c.is_whitespace() || c == '"' || c == '\'' || c == ')' || c == '|' || c == ';'
            })
            .unwrap_or(tail.len());
        let url = &tail[..end];
        out.push_str(accel.trim_end_matches('/'));
        out.push('/');
        out.push_str(url);
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// 一次检查的汇总（给界面看的）
// ---------------------------------------------------------------------------

/// 记录「这一次实际用了谁」，给状态栏 / 关于页显示。
#[derive(Debug, Clone, Default)]
pub struct LastFetch {
    pub source: String,
    pub failures: Vec<SourceFailure>,
    pub at: Option<u64>,
}

/// 进程内共享的最近一次取数结果：`models.dev` 刷新后写一份，界面拉状态时读。
pub fn last_fetch() -> &'static Mutex<HashMap<String, LastFetch>> {
    static LAST: OnceLock<Mutex<HashMap<String, LastFetch>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn note_last_fetch(key: &str, fetches: &[LastFetch]) {
    if let Ok(mut m) = last_fetch().lock() {
        for f in fetches {
            m.insert(key.to_string(), f.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_urls_expand_to_official_then_accels() {
        let prefs = NetPrefs::default();
        let out = prefs.candidates(&["https://github.com/o/r/releases/download/tool.tar.gz"]);
        assert_eq!(
            out[0],
            "https://github.com/o/r/releases/download/tool.tar.gz"
        );
        // jsDelivr 不是「前缀拼接」形态，不能出现在 GitHub 地址的候选里。
        assert!(out.iter().all(|u| !u.contains("jsdelivr")));
        assert_eq!(
            out[1],
            "https://gh-proxy.com/https://github.com/o/r/releases/download/tool.tar.gz"
        );
        assert_eq!(out.len(), GITHUB_ACCELS.len() - 1);
    }

    #[test]
    fn turning_accel_off_leaves_only_the_official_url() {
        let prefs = NetPrefs {
            accel: false,
            proxy: String::new(),
        };
        let out = prefs.candidates(&["https://raw.githubusercontent.com/o/r/dev/x.json"]);
        assert_eq!(
            out,
            vec!["https://raw.githubusercontent.com/o/r/dev/x.json"]
        );
    }

    #[test]
    fn jsdelivr_urls_pin_the_commit() {
        assert_eq!(
            jsdelivr_url(
                "anomalyco/models.dev",
                "abc123",
                "/providers/anthropic/provider.toml"
            ),
            "https://cdn.jsdelivr.net/gh/anomalyco/models.dev@abc123/providers/anthropic/provider.toml"
        );
    }

    #[test]
    fn non_github_urls_are_never_accelerated() {
        let prefs = NetPrefs::default();
        let out = prefs.candidates(&["https://models.dev/api.json"]);
        assert_eq!(out, vec!["https://models.dev/api.json"]);
        assert_eq!(
            accelerate("https://models.dev/api.json", "https://gh-proxy.com/"),
            "https://models.dev/api.json"
        );
    }

    #[test]
    fn model_sources_put_the_official_api_first() {
        let prefs = NetPrefs::default();
        let out = prefs.candidates(MODEL_SOURCES);
        assert_eq!(out[0], "https://models.dev/api.json");
        assert!(out.len() > 1, "开着加速时要有镜像兜底");
        assert!(
            out.iter()
                .all(|u| !u.contains("gh-proxy.com/https://models.dev"))
        );
    }

    #[test]
    fn rewrite_only_touches_github_urls() {
        let cmd = "curl -fsSL https://raw.githubusercontent.com/a/b/main/i.sh | sh";
        let out = rewrite_github_urls(cmd, Some("https://gh-proxy.com/"));
        assert_eq!(
            out,
            "curl -fsSL https://gh-proxy.com/https://raw.githubusercontent.com/a/b/main/i.sh | sh"
        );
        // PowerShell 里地址常写成字符串参数，后面的引号不算进 URL。
        let ps = "irm \"https://github.com/a/b/releases/latest/download/i.ps1\"; iex";
        let out = rewrite_github_urls(ps, Some("https://ghfast.top/"));
        assert_eq!(
            out,
            "irm \"https://ghfast.top/https://github.com/a/b/releases/latest/download/i.ps1\"; iex"
        );
    }

    #[test]
    fn rewrite_is_a_noop_without_an_accel() {
        let cmd = "curl https://github.com/a/b";
        assert_eq!(rewrite_github_urls(cmd, None), cmd);
    }

    #[test]
    fn apply_passes_proxy_to_both_cases() {
        let env = ChildEnv {
            proxy: Some("http://127.0.0.1:7897".into()),
            github: Some("https://gh-proxy.com/".into()),
        };
        let mut cmd = std::process::Command::new("true");
        apply_to_std_env(&env, &mut cmd);
        let vars: HashMap<_, _> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
            .collect();
        assert_eq!(
            vars.get("HTTPS_PROXY").map(String::as_str),
            Some("http://127.0.0.1:7897")
        );
        assert_eq!(
            vars.get("https_proxy").map(String::as_str),
            Some("http://127.0.0.1:7897")
        );
        assert_eq!(
            vars.get("CARTER_GITHUB_ACCEL").map(String::as_str),
            Some("https://gh-proxy.com/")
        );
    }
}
