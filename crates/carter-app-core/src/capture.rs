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

/// 按会话分文件的报文仓库。
pub struct CaptureStore {
    root: PathBuf,
}

impl CaptureStore {
    pub fn open(root: PathBuf) -> Self {
        Self { root }
    }

    /// `~/.carter-app/captures`。
    pub fn default_root() -> PathBuf {
        let base = std::env::var_os("CARTER_APP_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .map(|h| PathBuf::from(h).join(".carter-app"))
            })
            .unwrap_or_else(|| PathBuf::from(".carter-app"));
        base.join("captures")
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

    pub fn get(&self, session_id: &str, id: &str) -> Option<ExchangeRecord> {
        self.load(session_id).into_iter().find(|r| r.id == id)
    }
}

/// 进行中的交换。
struct Pending {
    rec: ExchangeRecord,
    started: Instant,
    first_chunk_at: Option<Instant>,
}

/// 把交换写进 [`CaptureStore`] 的钩子。
///
/// 配对键是「关联标签 + URL」而不是顺序：钩子会同时服务并发请求
/// （并行子 agent），靠「上一条请求」那种假设配对，并发一上来就错。
pub struct CaptureHook {
    store: Arc<CaptureStore>,
    pending: Mutex<HashMap<String, Pending>>,
    seq: std::sync::atomic::AtomicU64,
}

impl CaptureHook {
    pub fn new(store: Arc<CaptureStore>) -> Self {
        Self {
            store,
            pending: Mutex::new(HashMap::new()),
            seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn key(req: &HttpRequest) -> String {
        let corr = req
            .correlation
            .as_ref()
            .map(|c| c.to_string())
            .unwrap_or_default();
        format!("{corr}|{}", req.url)
    }

    fn flush(&self, key: &str, fill: impl FnOnce(&mut ExchangeRecord)) {
        let Some(mut p) = self.pending.lock().unwrap().remove(key) else {
            return;
        };
        fill(&mut p.rec);
        p.rec.total_ms = Some(p.started.elapsed().as_millis());
        let sid = p.rec.meta.as_ref().and_then(|m| m.session_id.clone());
        self.store
            .append(sid.as_deref().unwrap_or("_orphan"), &p.rec);
    }
}

impl RequestHook for CaptureHook {
    fn before_request(&self, req: &HttpRequest) {
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            },
        );
    }

    fn after_raw_response(&self, req: &HttpRequest, resp: &RawHttpResponse) {
        let body = text_of(&resp.body);
        self.flush(&Self::key(req), |rec| {
            rec.status = Some(resp.status);
            rec.response_headers = redact(&resp.headers);
            rec.response_body = body;
        });
    }

    fn on_stream_chunk(&self, req: &HttpRequest, chunk: &Bytes) {
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

    fn on_stream_end(&self, req: &HttpRequest) {
        self.flush(&Self::key(req), |_| {});
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
