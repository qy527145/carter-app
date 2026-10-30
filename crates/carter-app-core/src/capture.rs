//! 原始报文捕获。
//!
//! 需求 4 的落点：对话里的某条内容，要能查到产生它的那次 HTTP 请求的**原始报文**。
//!
//! ## 为什么是 JSONL 而不是 SQLite
//!
//! 数据天然按会话分片（一次会话的所有交换在一个文件里），查询只有两种：
//! 「按会话取全部」和「按 id 取一条」。对这两种查询，扫描一个文件与走一次索引
//! 没有可感差异，而换来的好处很实在：零额外依赖，且文件能直接用文本工具看 ——
//! 排障时最想做的事就是拿编辑器打开它。
//!
//! ## 存的是什么
//!
//! 钩子拿到的 `HttpRequest.body` 就是**最终要发出去的字节**，所以存下来的是真原始报文，
//! 不是归一化视图。流式响应逐块拼起来（网络块，不是 SSE 帧 —— 帧边界不保证与块边界
//! 对齐，但两者都保留下来，界面按行切一下就是帧）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use llm_adapt_core::{Bytes, HttpRequest, RawHttpResponse, RequestHook};
use serde::{Deserialize, Serialize};

/// 一次完整的 HTTP 交换。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeRecord {
    pub id: String,
    pub ts: u64,
    /// 归属：哪条对话的哪一轮。没有会话（一次性调用）时为 None。
    pub meta: Option<llm_adapt_core::RequestMeta>,

    pub method: String,
    pub url: String,
    /// 这次交换说的是哪个线协议（`anthropic` / `openai_chat` / `openai_responses`）。
    ///
    /// 存的是**最终字节**，把字节解回内容块必须知道按哪套语法解 —— 而 URL 反推
    /// 不可靠（自建镜像换个路径就猜错）。会话建起来时从渠道配置里取，回填给钩子。
    /// 老记录里没有这个字段（`None`），界面按 URL 兜底。
    #[serde(default)]
    pub protocol: Option<String>,
    /// 这次请求走的**上游渠道**（配置里 `[providers.X]` 的 X）。
    ///
    /// 同一个模型可以同时配在多个渠道上（官方源、中转、自建镜像），而 URL 未必
    /// 分得出来 —— 很多中转把域名配得和官方一样。排障时第一个要问的就是
    /// 「这条走的是哪个渠道」，所以解析 provider 时回填给钩子，跟着报文一起存。
    ///
    /// 与 `protocol` 同理，老记录里没有这个字段（`None`），界面按「未知」处理。
    #[serde(default)]
    pub channel: Option<String>,
    /// 本地模型引用（`provider/model`，如 `ws/sonnet`）。
    ///
    /// 不用请求体里的 `model`：那是下发给上游的 `api_name`，与用户配置里的块名
    /// 不一定相同（`api_name` 可以覆盖），换了就对不回配置。
    #[serde(default)]
    pub model: Option<String>,
    pub request_headers: Vec<(String, String)>,
    /// 原始请求体（UTF-8 文本；非 UTF-8 时给一段占位说明）。
    pub request_body: String,

    /// 非流式的响应状态与正文；流式时为 None / 空（正文在 `stream_frames`）。
    pub status: Option<u16>,
    pub response_headers: Vec<(String, String)>,
    pub response_body: String,
    /// 流式响应收到的原始字节块（按到达顺序）。
    pub stream_frames: Vec<String>,
    /// 上游在流里报的错（如果有）。
    pub stream_error: Option<String>,

    /// 首字节耗时（毫秒）。流式：接到第一块的时间；非流式：整个响应到达的时间。
    pub ttfb_ms: Option<u128>,
    /// 总耗时（毫秒）。流式在流结束时填。
    pub total_ms: Option<u128>,
}

/// 列表用的轻量投影：**不带**请求体与响应帧。
///
/// 一条流式交换的帧加起来可能几 MB，而列表在面板打开时每秒刷新一次 ——
/// 每次都把完整记录读一遍、拼一遍，面板一开就卡。摘要由
/// [`CaptureStore::briefs`] 增量维护，第一次查询后不再碰文件。
#[derive(Debug, Clone, Serialize)]
pub struct ExchangeBrief {
    pub id: String,
    pub ts: u64,
    pub meta: Option<llm_adapt_core::RequestMeta>,
    pub method: String,
    pub url: String,
    pub protocol: Option<String>,
    pub channel: Option<String>,
    pub model: Option<String>,
    pub status: Option<u16>,
    pub ttfb_ms: Option<u128>,
    pub total_ms: Option<u128>,
    pub frame_count: usize,
}

impl ExchangeRecord {
    pub fn brief(&self) -> ExchangeBrief {
        ExchangeBrief {
            id: self.id.clone(),
            ts: self.ts,
            meta: self.meta.clone(),
            method: self.method.clone(),
            url: self.url.clone(),
            protocol: self.protocol.clone(),
            channel: self.channel.clone(),
            model: self.model.clone(),
            status: self.status,
            ttfb_ms: self.ttfb_ms,
            total_ms: self.total_ms,
            frame_count: self.stream_frames.len(),
        }
    }
}

/// 进行中的交换。
struct Pending {
    rec: ExchangeRecord,
    started: Instant,
    first_chunk_at: Option<Instant>,
    /// 开始顺序。pending 存在 HashMap 里，取出来要按发起先后排 ——
    /// `ts` 只到毫秒，并发请求会撞。
    seq: u64,
    /// 建档的钩子。钩子被丢弃（换模型、关会话）时把它的孤儿清掉 ——
    /// 否则一条没等到收尾的回调会永远挂在列表里显示「进行中」。
    owner: u64,
}

/// 解析 provider 时回填给钩子的「这条请求走的是谁」。
#[derive(Debug, Clone, Default)]
struct Route {
    protocol: Option<String>,
    channel: Option<String>,
    model: Option<String>,
}

/// 按会话分文件的报文仓库。
pub struct CaptureStore {
    root: PathBuf,
    /// 进行中的交换。
    ///
    /// 放在**仓库**里而不是钩子里：钩子是按 provider 建的（换模型就换一个），
    /// 而界面问的是「这条会话现在有哪些请求在跑」—— 那是跨钩子的问题。
    ///
    /// 有了它，请求一发出（`before_request` 就已经知道 URL、渠道、模型、请求体）
    /// 就能被列表看见，不必等响应收完。
    pending: Mutex<HashMap<String, Pending>>,
    /// 全局递增序号。同样因为会同时存在多个钩子：各自从 0 数，id 会撞车，
    /// 而界面拿 id 来选中一条报文。
    seq: AtomicU64,
    /// 各会话已完成交换的摘要，首次查询时从文件建一次、之后随收尾增量追加。
    briefs: Mutex<HashMap<String, Vec<ExchangeBrief>>>,
}

impl CaptureStore {
    pub fn open(root: PathBuf) -> Self {
        Self {
            root,
            pending: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
            briefs: Mutex::new(HashMap::new()),
        }
    }

    /// `~/.carter-app/captures`。
    pub fn default_root() -> PathBuf {
        crate::paths::captures_dir()
    }

    fn path_for(&self, session_id: &str) -> PathBuf {
        // 会话 id 是 uuid，但外部写入不该能靠 `../` 逃出这个目录。
        let safe: String = session_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
            .collect();
        self.root.join(format!("{safe}.jsonl"))
    }

    fn append(&self, session_id: &str, rec: &ExchangeRecord) {
        if std::fs::create_dir_all(&self.root).is_err() {
            return;
        }
        let Ok(line) = serde_json::to_string(rec) else {
            return;
        };
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path_for(session_id))
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// 某个会话的全部交换，按时间顺序。
    pub fn load(&self, session_id: &str) -> Vec<ExchangeRecord> {
        let Ok(raw) = std::fs::read_to_string(self.path_for(session_id)) else {
            return Vec::new();
        };
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// 某个会话已完成交换的摘要（不含正文），按写入顺序。
    ///
    /// 第一次会解析一遍文件，之后一直走内存 —— 面板打开期间每秒轮询一次，
    /// 不能每次都把几 MB 的帧重新解出来。
    pub fn briefs(&self, session_id: &str) -> Vec<ExchangeBrief> {
        let mut cache = self.briefs.lock().unwrap();
        if let Some(list) = cache.get(session_id) {
            return list.clone();
        }
        let list: Vec<ExchangeBrief> = self.load(session_id).iter().map(|r| r.brief()).collect();
        cache.insert(session_id.to_string(), list.clone());
        list
    }

    pub fn get(&self, session_id: &str, id: &str) -> Option<ExchangeRecord> {
        // 进行中的也要能取到：点开一条还在跑的请求，请求体、渠道、模型现在就有，
        // 响应部分随轮询慢慢长出来。
        if let Some(rec) = self
            .pending
            .lock()
            .unwrap()
            .values()
            .find(|p| {
                p.rec.id == id
                    && p.rec.meta.as_ref().and_then(|m| m.session_id.as_deref())
                        == Some(session_id)
            })
            .map(|p| p.rec.clone())
        {
            return Some(rec);
        }
        self.load(session_id).into_iter().find(|r| r.id == id)
    }

    /// 某个会话**进行中**的交换摘要，按发起顺序。
    ///
    /// 返回摘要而不是完整记录：列表每秒取一次，而进行中的流帧正在长，
    /// 没必要每秒把它整个复制一遍（详情走 `get`）。
    pub fn active_briefs(&self, session_id: &str) -> Vec<ExchangeBrief> {
        let mut items: Vec<(u64, ExchangeBrief)> = self
            .pending
            .lock()
            .unwrap()
            .values()
            .filter(|p| {
                p.rec.meta.as_ref().and_then(|m| m.session_id.as_deref()) == Some(session_id)
            })
            .map(|p| (p.seq, p.rec.brief()))
            .collect();
        // HashMap 无序，而列表是给人按时间看的。
        items.sort_by_key(|(seq, _)| *seq);
        items.into_iter().map(|(_, brief)| brief).collect()
    }

    /// 配对键：关联标签 + URL。
    ///
    /// 不用顺序配对：钩子会同时服务并发请求（并行子 agent），「上一条请求」那种
    /// 假设并发一上来就错。放在仓库上是因为多个钩子共用一个 pending 表。
    fn key(req: &HttpRequest) -> String {
        let corr = req
            .correlation
            .as_ref()
            .map(|c| c.to_string())
            .unwrap_or_default();
        format!("{corr}|{}", req.url)
    }

    /// 请求发出时就建档。此刻还不知道响应，但 URL、渠道、模型、请求体全都有 ——
    /// 这些正是排障时最先想看的东西。
    fn begin(&self, req: &HttpRequest, route: &Route, owner: u64) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let meta: Option<llm_adapt_core::RequestMeta> = req
            .correlation
            .as_ref()
            .and_then(|c| serde_json::from_value(c.clone()).ok());
        let rec = ExchangeRecord {
            id: format!("x{seq}"),
            ts: crate::protocol::now_ms(),
            meta,
            method: format!("{:?}", req.method),
            url: req.url.clone(),
            protocol: route.protocol.clone(),
            channel: route.channel.clone(),
            model: route.model.clone(),
            request_headers: redact(&req.headers),
            request_body: text_of(&req.body),
            status: None,
            response_headers: Vec::new(),
            response_body: String::new(),
            stream_frames: Vec::new(),
            stream_error: None,
            ttfb_ms: None,
            total_ms: None,
        };
        self.pending.lock().unwrap().insert(
            Self::key(req),
            Pending {
                rec,
                started: Instant::now(),
                first_chunk_at: None,
                seq,
                owner,
            },
        );
    }

    fn chunk(&self, req: &HttpRequest, chunk: &Bytes) {
        let mut guard = self.pending.lock().unwrap();
        let Some(p) = guard.get_mut(&Self::key(req)) else {
            return;
        };
        if p.first_chunk_at.is_none() {
            p.first_chunk_at = Some(Instant::now());
            p.rec.ttfb_ms = Some(p.started.elapsed().as_millis());
        }
        // 首块就带上响应状态 —— 状态码在 `execute_stream` 里就拿到了，
        // 但那时没有钩子时机；第一块是能补上的最早时刻。
        p.rec.stream_frames.push(text_of(chunk));
    }

    fn finish(&self, req: &HttpRequest, fill: impl FnOnce(&mut ExchangeRecord)) {
        let Some(mut p) = self.pending.lock().unwrap().remove(&Self::key(req)) else {
            return;
        };
        fill(&mut p.rec);
        p.rec.total_ms = Some(p.started.elapsed().as_millis());
        let sid = p.rec.meta.as_ref().and_then(|m| m.session_id.clone());
        let sid = sid.as_deref().unwrap_or("_orphan");
        self.append(sid, &p.rec);
        // 摘要缓存已在用的话跟着追加；还没查询过就不用管 —— 首次 `briefs()`
        // 会把这份记录一并从文件里读出来。
        if let Ok(mut cache) = self.briefs.lock() {
            if let Some(list) = cache.get_mut(sid) {
                // 与 `briefs()` 首次建表并发时可能已经包含这一条。
                if !list.iter().any(|b| b.id == p.rec.id) {
                    list.push(p.rec.brief());
                }
            }
        }
    }
}

/// 把交换写进 [`CaptureStore`] 的钩子。
///
/// 同一份数据可能被多个钩子共写：主对话一个、切模型/副模型各一个。所以
/// 进行中的交换和序号都放在 [`CaptureStore`] 里，钩子只带自己那条路线的标签。
pub struct CaptureHook {
    store: Arc<CaptureStore>,
    /// 自己建档的交换用它标记 `Pending::owner`，丢弃时据此清理。
    id: u64,
    /// 这条路线（provider）的标签：协议、渠道、模型引用。
    ///
    /// 钩子先建、路线后知道（解析 provider 时才定下来），所以留一个回填口子 ——
    /// 回填发生在任何请求之前，钩子读到的一定是已填好的值。
    route: Mutex<Route>,
}

/// 钩子编号。钩子可能不止一个（主对话、切模型、副模型），编号用来认领 pending。
static NEXT_HOOK_ID: AtomicU64 = AtomicU64::new(0);

impl CaptureHook {
    pub fn new(store: Arc<CaptureStore>) -> Self {
        Self {
            store,
            id: NEXT_HOOK_ID.fetch_add(1, Ordering::Relaxed),
            route: Mutex::new(Route::default()),
        }
    }

    /// 告诉钩子这条路线用的是哪个协议、哪个渠道、哪个模型。每次解析 provider
    /// 之后都要调一次（换模型可能换协议、换渠道）。
    pub fn set_route(
        &self,
        protocol: impl Into<String>,
        channel: impl Into<String>,
        model: impl Into<String>,
    ) {
        *self.route.lock().unwrap() = Route {
            protocol: Some(protocol.into()),
            channel: Some(channel.into()),
            model: Some(model.into()),
        };
    }
}

impl Drop for CaptureHook {
    fn drop(&mut self) {
        // 钩子随 provider 一起丢弃（换模型 / 关会话）。正常情况下没跑完的请求
        // 会先拿到 `on_stream_end`；这里兜的是回调没来的情况 —— 别让幽灵条目
        // 永远留在「进行中」。
        if let Ok(mut pending) = self.store.pending.lock() {
            pending.retain(|_, p| p.owner != self.id);
        }
    }
}

impl RequestHook for CaptureHook {
    fn before_request(&self, req: &HttpRequest) {
        let route = self.route.lock().unwrap().clone();
        self.store.begin(req, &route, self.id);
    }

    fn after_raw_response(&self, req: &HttpRequest, resp: &RawHttpResponse) {
        let body = text_of(&resp.body);
        self.store.finish(req, |rec| {
            rec.status = Some(resp.status);
            rec.response_headers = redact(&resp.headers);
            rec.response_body = body;
        });
    }

    fn on_stream_chunk(&self, req: &HttpRequest, chunk: &Bytes) {
        self.store.chunk(req, chunk);
    }

    fn on_stream_end(&self, req: &HttpRequest) {
        self.store.finish(req, |_| {});
    }
}

/// 密钥不落盘。报文是给人看的，不该顺手把 key 抄一份到磁盘上。
fn redact(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(k, v)| {
            let lower = k.to_ascii_lowercase();
            let sensitive = lower == "authorization"
                || lower == "x-api-key"
                || lower.contains("api-key")
                || lower.contains("token");
            if sensitive {
                (k.clone(), "***".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

fn text_of(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes)
        .map(str::to_string)
        .unwrap_or_else(|_| format!("<{} 字节的二进制>", bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_adapt_core::{HttpMethod, RequestMeta, RequestPurpose};

    fn req(url: &str, sid: Option<&str>) -> HttpRequest {
        HttpRequest {
            method: HttpMethod::Post,
            url: url.to_string(),
            headers: vec![("authorization".into(), "Bearer sk-secret".into())],
            body: br#"{"model":"m"}"#.to_vec(),
            correlation: Some(
                serde_json::to_value(RequestMeta {
                    session_id: sid.map(str::to_string),
                    turn: 3,
                    round: 1,
                    purpose: RequestPurpose::MainTurn,
                })
                .unwrap(),
            ),
        }
    }

    fn tmp_store() -> (Arc<CaptureStore>, PathBuf) {
        // 并发跑测试时毫秒级时间戳会撞，必须再加个递增序号。
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "carter-app-cap-{}-{n}",
            crate::protocol::now_ms()
        ));
        (Arc::new(CaptureStore::open(dir.clone())), dir)
    }

    #[test]
    fn non_streaming_exchange_is_persisted_with_correlation() {
        let (store, dir) = tmp_store();
        let hook = CaptureHook::new(store.clone());
        let r = req("https://api.example/v1/messages", Some("sess-1"));
        hook.before_request(&r);
        hook.after_raw_response(
            &r,
            &RawHttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: Bytes::from_static(br#"{"ok":true}"#),
            },
        );

        let recs = store.load("sess-1");
        assert_eq!(recs.len(), 1, "落盘路径应按会话分文件");
        let rec = &recs[0];
        assert_eq!(rec.status, Some(200));
        assert_eq!(rec.request_body, r#"{"model":"m"}"#);
        assert_eq!(rec.response_body, r#"{"ok":true}"#);
        let meta = rec.meta.as_ref().unwrap();
        assert_eq!(meta.turn, 3);
        assert_eq!(meta.round, 1);
        // 密钥不落盘。
        assert_eq!(rec.request_headers[0].1, "***");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 流式：分块到达、结束时收尾，且首块时间被记为 ttfb。
    #[test]
    fn streaming_exchange_flushes_on_stream_end() {
        let (store, dir) = tmp_store();
        let hook = CaptureHook::new(store.clone());
        let r = req("https://api.example/v1/responses", Some("sess-2"));
        hook.before_request(&r);
        hook.on_stream_chunk(&r, &Bytes::from_static(b"data: {\"a\":1}\n\n"));
        hook.on_stream_chunk(&r, &Bytes::from_static(b"data: [DONE]\n\n"));
        // 流没结束前不该落盘 —— 落早了后续分块就丢了。
        assert!(store.load("sess-2").is_empty());
        hook.on_stream_end(&r);

        let recs = store.load("sess-2");
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].stream_frames.len(), 2);
        assert!(recs[0].ttfb_ms.is_some());
        assert!(recs[0].total_ms.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 请求一发出就能被列表看见，且带着渠道/模型 —— 不必等响应。
    #[test]
    fn in_flight_exchange_is_visible_before_response() {
        let (store, dir) = tmp_store();
        let hook = CaptureHook::new(store.clone());
        hook.set_route("anthropic", "官方", "官方/claude");
        let r = req("https://api.example/v1/messages", Some("sess-3"));
        hook.before_request(&r);

        let active = store.active_briefs("sess-3");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].channel.as_deref(), Some("官方"));
        assert_eq!(active[0].model.as_deref(), Some("官方/claude"));
        assert_eq!(active[0].protocol.as_deref(), Some("anthropic"));
        assert!(active[0].total_ms.is_none());
        // 还没收完，不该落盘。
        assert!(store.load("sess-3").is_empty());
        // 进行中的也能按 id 取到（点开详情不能等它结束），请求体此刻就有。
        let rec = store.get("sess-3", &active[0].id).unwrap();
        assert_eq!(rec.request_body, r#"{"model":"m"}"#);

        hook.on_stream_end(&r);
        assert!(store.active_briefs("sess-3").is_empty());
        assert_eq!(store.load("sess-3").len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 摘要缓存先建好之后，后续收尾的记录也要能增量进去（列表每秒查询）。
    #[test]
    fn briefs_pick_up_exchanges_finished_after_the_cache_is_built() {
        let (store, dir) = tmp_store();
        assert!(store.briefs("s1").is_empty());

        let hook = CaptureHook::new(store.clone());
        hook.set_route("openai_chat", "中转", "中转/gpt");
        let r = req("https://api.example/v1/chat/completions", Some("s1"));
        hook.before_request(&r);
        hook.after_raw_response(
            &r,
            &RawHttpResponse {
                status: 200,
                headers: vec![],
                body: Bytes::from_static(br#"{"ok":true}"#),
            },
        );

        let briefs = store.briefs("s1");
        assert_eq!(briefs.len(), 1);
        assert_eq!(briefs[0].channel.as_deref(), Some("中转"));
        assert_eq!(briefs[0].model.as_deref(), Some("中转/gpt"));
        assert_eq!(briefs[0].status, Some(200));
        assert!(briefs[0].total_ms.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 多个钩子共用一个仓库：id 不能各自从 0 数（界面上按 id 选中报文）。
    #[test]
    fn ids_are_unique_across_hooks() {
        let (store, dir) = tmp_store();
        let a = CaptureHook::new(store.clone());
        let b = CaptureHook::new(store.clone());
        let ra = req("https://api.example/a", Some("s1"));
        let rb = req("https://api.example/b", Some("s1"));
        a.before_request(&ra);
        b.before_request(&rb);

        let ids: Vec<_> = store.active_briefs("s1").into_iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 并发请求必须各归各的，不能靠「上一条请求」配对。
    #[test]
    fn concurrent_requests_do_not_cross_pair() {
        let (store, dir) = tmp_store();
        let hook = CaptureHook::new(store.clone());
        let a = req("https://api.example/a", Some("s1"));
        let b = req("https://api.example/b", Some("s1"));
        // 两个请求都先发出去，再陆续收响应（交错的顺序）。
        hook.before_request(&a);
        hook.before_request(&b);
        hook.after_raw_response(
            &b,
            &RawHttpResponse {
                status: 500,
                headers: vec![],
                body: Bytes::from_static(b"B"),
            },
        );
        hook.after_raw_response(
            &a,
            &RawHttpResponse {
                status: 200,
                headers: vec![],
                body: Bytes::from_static(b"A"),
            },
        );

        let recs = store.load("s1");
        let by_url: std::collections::HashMap<_, _> =
            recs.iter().map(|r| (r.url.clone(), r.clone())).collect();
        assert_eq!(by_url["https://api.example/a"].status, Some(200));
        assert_eq!(by_url["https://api.example/b"].status, Some(500));
        assert_eq!(by_url["https://api.example/a"].response_body, "A");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn session_id_with_path_separators_cannot_escape_the_root() {
        let (store, dir) = tmp_store();
        let hook = CaptureHook::new(store.clone());
        let r = req("https://api.example/x", Some("../../evil"));
        hook.before_request(&r);
        hook.after_raw_response(
            &r,
            &RawHttpResponse {
                status: 200,
                headers: vec![],
                body: Bytes::new(),
            },
        );
        // 文件名被清洗过，仍然落在这个目录里。
        let written: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(written.len(), 1);
        assert!(!written[0].file_name().to_string_lossy().contains(".."));
        let _ = std::fs::remove_dir_all(dir);
    }
}
