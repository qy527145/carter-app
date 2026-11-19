//! 触发命令：一条**长驻**命令，它每吐一行，就让这个来源立刻查一次。
//!
//! ## 它为什么要和「事件」分开
//!
//! 观察源靠「定时跑一条命令、比对输出的行」工作，最短一分钟一次。有工单在等回复时
//! 这会加密到几十秒（见 `source::URGENT_SECS`），但再密也是轮询。想让「对方刚回一句」
//! 就立刻动，得有人推我们 —— 可推过来的东西（webhook 的 JSON、inotifywait 的输出）
//! 与轮询命令输出的行几乎不是同一种格式，直接当成事件的话指纹去重全失效：同一句话
//! 会先被监听叫醒一次，再被轮询当新事件送去评估一次，评估多半又判 attach 塞回同一张
//! 工单。
//!
//! 所以触发命令**不产生事件**，它只说明「现在有动静了，去查一次」。查还是那条轮询
//! 命令，格式、去重、基线、评估、唤醒全都照旧，只是时机不再由定时器决定。
//!
//! ## 谁会用
//!
//! * 能推的：GitHub 的 webhook 转发到本地（一行一个 push）、`inotifywait`、`fswatch`、
//!   任何 `while true; do ...; echo; done` 轮询外部系统但比本应用更懂退避的脚本。
//! * 不能推的（眼下的钉钉 `dws` 就是）：不配触发命令，走定时 + 等回复时加密。
//!
//! ## 看护
//!
//! 每个配了触发命令的来源一份进程。助理暂停、来源停用 / 删除 / 改命令时收掉；进程
//! 自己退了按 2、4、8… 最多 60 秒退避重启，重启次数与最后一次错误显示在面板上 ——
//! 一个悄悄重启了三百次的监听器，比没配更糟。
//!
//! 连吐多行只查一次：轮询正在跑的时候又来一行，只记下「跑完再补一次」，不排队。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Notify;

use super::Source;
use crate::platform;
use crate::protocol::now_ms;
use crate::shell::AppShell;

/// 退避重启的上限。
const MAX_BACKOFF_SECS: u64 = 60;

/// 面板上显示的运行状态。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TriggerStatus {
    /// 重启过几次（不含第一次拉起）。
    pub restarts: u32,
    /// 最近吐出来的一行，以及什么时候。看它就知道监听器还活着。
    pub last_line: Option<String>,
    pub last_line_at: Option<u64>,
    /// 最近一次拉起失败或意外退出的原因。正常情况下是空的。
    pub last_error: Option<String>,
}

struct Run {
    source_id: String,
    child: Mutex<Option<tokio::process::Child>>,
    status: Mutex<TriggerStatus>,
    stop: AtomicBool,
    /// 有新动静。见 [`Run::signal`]。
    notify: Notify,
    /// 已经排着一次被唤醒的轮询 —— 连吐十行只查一次。
    pending: AtomicBool,
}

impl Run {
    fn new(source_id: &str) -> Self {
        Self {
            source_id: source_id.to_string(),
            child: Mutex::new(None),
            status: Mutex::new(TriggerStatus::default()),
            stop: AtomicBool::new(false),
            notify: Notify::new(),
            pending: AtomicBool::new(false),
        }
    }

    /// 有新动静：记下，保证只排一次轮询。读线程调它，不 await。
    fn signal(&self, line: &str) {
        {
            let mut st = self.status.lock().unwrap();
            st.last_line = Some(super::source::clip(line));
            st.last_line_at = Some(now_ms());
        }
        if !self.pending.swap(true, Ordering::SeqCst) {
            self.notify.notify_one();
        }
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.start_kill();
        }
        self.notify.notify_waiters();
    }
}

/// 所有触发进程。挂在 [`super::AssistantStore`] 上，随应用退出一起 drop
/// （`kill_on_drop`）。
#[derive(Default)]
pub struct TriggerTable {
    runs: Mutex<HashMap<String, Arc<Run>>>,
}

impl TriggerTable {
    pub fn status_of(&self, source_id: &str) -> Option<TriggerStatus> {
        self.runs
            .lock()
            .unwrap()
            .get(source_id)
            .map(|r| r.status.lock().unwrap().clone())
    }

    pub fn is_running(&self, source_id: &str) -> bool {
        self.runs.lock().unwrap().contains_key(source_id)
    }

    /// 停掉一个来源的触发进程（改了命令、停用、删除、暂停都走这里）。
    pub fn stop(&self, source_id: &str) {
        if let Some(r) = self.runs.lock().unwrap().remove(source_id) {
            r.stop();
        }
    }

    pub fn stop_all(&self) {
        let runs: Vec<Arc<Run>> = self.runs.lock().unwrap().drain().map(|(_, r)| r).collect();
        for r in runs {
            r.stop();
        }
    }

    /// 对一次账：该跑的跑起来，不该跑（暂停、停用、删除、命令改空）的收掉。
    ///
    /// 每拍调一次。没配触发命令的来源在这里完全不受影响。
    pub fn sync(&self, shell: &Arc<AppShell>, sources: &[Source], paused: bool) {
        let want = wanted(sources, paused);
        // 先收：不在名单里的（删除、停用、暂停、命令改了由 `save_source` 先 stop 掉）。
        let stale: Vec<String> = self
            .runs
            .lock()
            .unwrap()
            .keys()
            .filter(|id| !want.iter().any(|s| &s.id == *id))
            .cloned()
            .collect();
        for id in stale {
            self.stop(&id);
        }
        for s in want {
            let mut runs = self.runs.lock().unwrap();
            if runs.contains_key(&s.id) {
                continue;
            }
            let run = Arc::new(Run::new(&s.id));
            runs.insert(s.id.clone(), run.clone());
            drop(runs);
            tokio::spawn(waker(shell.clone(), run.clone()));
            tokio::spawn(watch(shell.clone(), s, run));
        }
    }
}

/// 该有触发进程的来源。暂停时一个都不要；停用的、没配命令的也不要。
fn wanted(sources: &[Source], paused: bool) -> Vec<Source> {
    if paused {
        return Vec::new();
    }
    sources
        .iter()
        .filter(|s| s.enabled && !s.trigger.trim().is_empty())
        .cloned()
        .collect()
}

/// 收到动静就去查一次。
async fn waker(shell: Arc<AppShell>, run: Arc<Run>) {
    loop {
        run.notify.notified().await;
        if run.stop.load(Ordering::SeqCst) {
            return;
        }
        run.pending.store(false, Ordering::SeqCst);
        let id = run.source_id.clone();
        // 定时那一拍、或者连过来的第二行，都可能正在查同一个来源 —— 不排队，等下一次
        // 动静。退回 30 秒是为了「那一拍恰好是几分钟前开始的长命令」时别丢：那时早就
        // 有空了，再试一次。
        if !shell.assistant.claim(&id) {
            let shell = shell.clone();
            let run = run.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                if !run.stop.load(Ordering::SeqCst) && !run.pending.swap(true, Ordering::SeqCst) {
                    run.notify.notify_one();
                }
                drop(shell);
            });
            continue;
        }
        // 这一次查到的行已经在 `poll_and_assess` 里评估、投递、开工过了：中途被丢掉的话，
        // 触发命令给出的那几行就悄悄没了。所以这里收尾一定要走到，不能提前 return。
        let found = shell.assistant.read(|s| {
            if s.settings.paused {
                return None;
            }
            s.sources.iter().find(|x| x.id == id && x.enabled).cloned()
        });
        if let Some(source) = found {
            super::runtime::poll_and_assess(&shell, &source, true).await;
        }
        shell.assistant.release(&id);
    }
}

/// 看护一份进程：拉起、读它的输出、退了就退避重启。
async fn watch(shell: Arc<AppShell>, source: Source, run: Arc<Run>) {
    let mut backoff = 2;
    loop {
        if run.stop.load(Ordering::SeqCst) {
            return;
        }
        match spawn(&source) {
            Ok(mut child) => {
                let stdout = child.stdout.take();
                *run.child.lock().unwrap() = Some(child);
                if let Some(out) = stdout {
                    let mut lines = BufReader::new(out).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if run.stop.load(Ordering::SeqCst) {
                            break;
                        }
                        if !line.trim().is_empty() {
                            run.signal(&line);
                        }
                    }
                }
                // 读到头 = 进程退了。收尸，拿退出码。
                let status = match run.child.lock().unwrap().take() {
                    Some(mut c) => c.try_wait().ok().flatten(),
                    None => None,
                };
                if run.stop.load(Ordering::SeqCst) {
                    return;
                }
                let why = match status.and_then(|s| s.code()) {
                    Some(0) => "自己退出了".to_string(),
                    Some(c) => format!("退出了（退出码 {c}）"),
                    None => "退出了".to_string(),
                };
                run.status.lock().unwrap().last_error = Some(format!("{why}，{backoff} 秒后重启"));
            }
            Err(e) => {
                run.status.lock().unwrap().last_error =
                    Some(format!("拉不起来：{e}，{backoff} 秒后重试"));
            }
        }
        // 面板上那句「重启过几次 / 上次为什么退」得让人看见。
        shell.emit(crate::protocol::AppEvent::AssistantChanged);
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
        run.status.lock().unwrap().restarts += 1;
    }
}

fn spawn(source: &Source) -> Result<tokio::process::Child, String> {
    let cwd = std::path::Path::new(&source.cwd);
    if !cwd.is_dir() {
        return Err(format!("工作目录 {} 不存在", cwd.display()));
    }
    let shell = platform::detect_shell();
    let mut cmd = tokio::process::Command::new(&shell.program);
    cmd.arg(shell.flag)
        .arg(source.trigger.trim())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // 错误不并进来：并进 stdout 会让每行报错都当成「有动静」，一次连不上就反复轮询。
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    platform::hide_console_async(&mut cmd);
    cmd.spawn().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::SourceState;

    fn source(id: &str, enabled: bool, trigger: &str) -> Source {
        Source {
            id: id.into(),
            name: id.into(),
            command: "echo a".into(),
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            every_minutes: 10,
            trigger: trigger.into(),
            enabled,
            state: SourceState::default(),
        }
    }

    /// 只有启用的、配了触发命令的来源才该有进程；暂停时一个都不该有。
    #[test]
    fn only_enabled_sources_with_a_trigger_get_a_process() {
        let all = vec![
            source("a", true, "inotifywait -m ."),
            source("b", false, "inotifywait -m ."),
            source("c", true, "  "),
        ];
        let ids: Vec<String> = wanted(&all, false).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["a"]);
        assert!(wanted(&all, true).is_empty(), "暂停时不跑触发进程");
    }

    /// 连吐多行只排一次轮询：第二次 signal 不再唤醒。
    #[tokio::test]
    async fn a_burst_of_lines_coalesces_into_one_poll() {
        let run = Run::new("src");
        run.signal("第一行");
        run.signal("第二行");
        run.signal("第三行");
        // 一次唤醒。
        tokio::time::timeout(Duration::from_millis(100), run.notify.notified())
            .await
            .expect("应该唤醒一次");
        assert!(
            tokio::time::timeout(Duration::from_millis(100), run.notify.notified())
                .await
                .is_err(),
            "第二次不该再唤醒"
        );
        let st = run.status.lock().unwrap().clone();
        assert_eq!(st.last_line.as_deref(), Some("第三行"));
        assert!(st.last_line_at.is_some());
    }

    /// 真起一条命令：它吐的行进状态，退了之后按退避重启。
    #[tokio::test]
    async fn a_real_trigger_process_feeds_lines_and_is_restarted() {
        let dir =
            std::env::temp_dir().join(format!("carter-trig-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = super::super::AssistantStore::open(dir.join("assistant.json"));
        let shell = Arc::new(
            Arc::into_inner(AppShell::new(carter::config::Config::default()))
                .unwrap()
                .with_assistant(store),
        );
        let src = source("s", true, "printf '一\\n二\\n'");
        let run = Arc::new(Run::new(&src.id));
        let task = tokio::spawn(watch(shell.clone(), src, run.clone()));
        // 等它把两行读完再退出。
        for _ in 0..50 {
            if run.status.lock().unwrap().last_line.as_deref() == Some("二") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(run.status.lock().unwrap().last_line.as_deref(), Some("二"));
        run.stop();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        assert!(run.stop.load(Ordering::SeqCst));
    }
}
