//! 后台命令：把「跑很久、又不该卡住这一轮」的命令丢到后台，给模型一个句柄。
//!
//! ## 为什么不是「超时了自动转后台」
//!
//! 自动转后台看起来很省事，但模型的请求里**没有**「这个命令会跑很久」这个信息，
//! 它拿到一个超时结果时不会知道去做别的事 —— 于是还是会立刻 `bash_output` 死等，
//! 只是把一次 120 秒的阻塞换成了二十次轮询。真正有用的做法与 Claude Code 一样：
//! 让模型显式地写 `run_in_background: true`（起服务、跑长构建、`tail -f`），
//! 拿到 `shell_id` 之后由它决定什么时候回来看。
//!
//! ## 输出怎么拿回来
//!
//! 后台进程的 stdout/stderr 直接重定向到日志文件，`bash_output` 每次从**上次读到的
//! 位置**往后读。不留在管道里是有原因的：管道有内核缓冲上限，没人读就会把子进程
//! 写阻塞住 —— 一个「后台」服务跑一会儿自己卡死，排查起来极其费劲。
//!
//! ## 生命周期
//!
//! 一个会话一份 [`BackgroundShells`]。会话关掉时 [`BackgroundShells::kill_all`] 把
//! 它们整组收掉 —— 不留后台孤儿进程。这也意味着后台命令**不跨会话**：想让它长久
//! 活着，那是 systemd / launchd 该管的事，不是这里。

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::platform::Shell;

/// 单次读回的上限。与 bash 的结果上限一致：够模型看清，又不至于把上下文撑爆。
const MAX_READ: usize = 30_000;

/// `bash_output` 最多等多久。再长就等于把「后台」又变回前台了。
const MAX_WAIT_MS: u64 = 60_000;

#[derive(Debug, Clone, PartialEq)]
pub enum ShellStatus {
    Running,
    /// 自己结束的，带退出码。
    Exited(i32),
    /// 被 `kill_shell` / 会话关闭收掉的。
    Killed,
}

impl ShellStatus {
    pub fn as_str(&self) -> String {
        match self {
            ShellStatus::Running => "running".to_string(),
            ShellStatus::Exited(code) => format!("exited(code={code})"),
            ShellStatus::Killed => "killed".to_string(),
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, ShellStatus::Running)
    }
}

struct ShellState {
    log: PathBuf,
    /// 下次 `bash_output` 从哪个字节开始读。
    offset: u64,
    status: ShellStatus,
    /// 进程组 id（Unix 上就是子进程 pid，因为 setsid 之后它自成一个组）。
    pid: Option<u32>,
}

/// 接管过来的一条命令（前台转后台）。见 [`BackgroundShells::adopt`]。
pub(crate) struct AdoptedShell {
    pub id: String,
    pub log: PathBuf,
    /// 继续往日志里追加输出用的句柄（读取任务持有它）。
    pub file: std::fs::File,
    state: Arc<Mutex<ShellState>>,
}

impl AdoptedShell {
    /// 进程退出了，把状态写回后台表。
    pub fn mark_exited(&self, code: i32) {
        self.state.lock().unwrap().status = ShellStatus::Exited(code);
    }
}

/// 一条会话的后台命令表。
pub struct BackgroundShells {
    shells: Mutex<HashMap<String, Arc<Mutex<ShellState>>>>,
    seq: AtomicU64,
    /// 日志根目录。生产是 `~/.carter-app/shells`；测试给一个自己的临时目录 ——
    /// 测试之间共享 `CARTER_APP_HOME` 会互相删掉对方的文件（那是进程级环境变量）。
    root: PathBuf,
}

impl BackgroundShells {
    pub fn new() -> Arc<Self> {
        Self::at(crate::paths::app_home().join("shells"))
    }

    pub fn at(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            shells: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
            root,
        })
    }

    fn log_path(&self, session_id: &str, id: &str) -> PathBuf {
        self.root.join(session_id).join(format!("{id}.log"))
    }

    /// 起一条后台命令。返回 `(shell_id, 日志路径)`。
    pub fn spawn(
        &self,
        command: &str,
        cwd: &std::path::Path,
        shell: &Shell,
        session_id: &str,
    ) -> Result<(String, PathBuf), String> {
        let id = format!("sh{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1);
        let log = self.log_path(session_id, &id);
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("建后台命令的日志目录失败：{e}"))?;
        }
        let out = std::fs::File::create(&log).map_err(|e| format!("建日志文件失败：{e}"))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("复制日志句柄失败：{e}"))?;

        let mut cmd = tokio::process::Command::new(&shell.program);
        cmd.arg(shell.flag)
            .arg(command)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        #[cfg(windows)]
        crate::platform::hide_console_async(&mut cmd);
        // 独立进程组：`kill_shell` 与关会话时能一次收掉整棵树。
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("启动后台命令失败：{e}（shell：{}）", shell.label()))?;
        let pid = child.id();

        let state = Arc::new(Mutex::new(ShellState {
            log: log.clone(),
            offset: 0,
            status: ShellStatus::Running,
            pid,
        }));
        let watcher = state.clone();
        tokio::spawn(async move {
            let code = match child.wait().await {
                Ok(st) => st.code().unwrap_or(-1),
                Err(_) => -1,
            };
            let mut s = watcher.lock().unwrap();
            // 被 kill 过的不要被「进程确实退出了」覆盖成 Exited —— 界面上
            // 「我杀掉了它」和「它自己跑完了」是两件事。
            if s.status.is_running() {
                s.status = ShellStatus::Exited(code);
            }
        });

        self.shells.lock().unwrap().insert(id.clone(), state);
        Ok((id, log))
    }

    //// 接管一条**已经在跑**的命令（前台转后台）。
    ///
    /// 与 [`BackgroundShells::spawn`] 的区别只有一个：进程不是这里起的，输出也不在
    /// 日志文件里 —— 调用方把已经攒下的输出写进返回的 `file`，之后继续往里追加。
    /// 退出状态由调用方的监视任务回填（`AdoptedShell::mark_exited`）。
    pub(crate) fn adopt(
        &self,
        command: &str,
        session_id: &str,
        pid: Option<u32>,
    ) -> Result<AdoptedShell, String> {
        use std::io::Write as _;

        let id = format!("sh{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1);
        let log = self.log_path(session_id, &id);
        if let Some(dir) = log.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("建后台命令的日志目录失败：{e}"))?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|e| format!("建日志文件失败：{e}"))?;
        // 日志开头写清楚它是哪条命令 —— 之后 `bash_output` 只给模型看增量，
        // 没有这一行它就分不清这是哪条命令的输出。
        let _ = writeln!(file, "（这条命令先在前台跑，已转到后台）\n$ {command}");

        let state = Arc::new(Mutex::new(ShellState {
            log: log.clone(),
            offset: 0,
            status: ShellStatus::Running,
            pid,
        }));
        self.shells
            .lock()
            .unwrap()
            .insert(id.clone(), state.clone());
        Ok(AdoptedShell {
            id,
            log,
            file,
            state,
        })
    }

    /// 从上次读到的位置往后读。没有新输出时返回空串。
    pub fn read(&self, id: &str) -> Result<(String, ShellStatus), String> {
        let state = self
            .shells
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("没有这个后台命令：{id}（可能已经结束并被清理）"))?;
        let mut s = state.lock().unwrap();
        let mut file = std::fs::File::open(&s.log)
            .map_err(|e| format!("读不到日志文件 {}：{e}", s.log.display()))?;
        file.seek(SeekFrom::Start(s.offset))
            .map_err(|e| format!("定位日志失败：{e}"))?;
        let mut buf = Vec::new();
        file.take(MAX_READ as u64)
            .read_to_end(&mut buf)
            .map_err(|e| format!("读日志失败：{e}"))?;
        // 别把一个多字节字符切成两半：只推进到最后一个完整字符处。
        let boundary = match std::str::from_utf8(&buf) {
            Ok(_) => buf.len(),
            Err(e) => e.valid_up_to(),
        };
        let text = String::from_utf8_lossy(&buf[..boundary]).to_string();
        s.offset += boundary as u64;
        Ok((text, s.status.clone()))
    }

    pub fn status(&self, id: &str) -> Option<ShellStatus> {
        self.shells
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.lock().unwrap().status.clone())
    }

    /// 结束一条后台命令（整棵进程树）。
    pub fn kill(&self, id: &str) -> Result<ShellStatus, String> {
        let state = self
            .shells
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| format!("没有这个后台命令：{id}"))?;
        let mut s = state.lock().unwrap();
        if !s.status.is_running() {
            return Ok(s.status.clone());
        }
        if let Some(pid) = s.pid {
            kill_process_tree(pid);
        }
        s.status = ShellStatus::Killed;
        Ok(s.status.clone())
    }

    /// 关会话时把还活着的全部收掉。
    pub fn kill_all(&self) {
        let ids: Vec<String> = self.shells.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.kill(&id);
        }
    }

    /// 还活着的命令数（测试与诊断用）。
    pub fn running_count(&self) -> usize {
        self.shells
            .lock()
            .unwrap()
            .values()
            .filter(|s| s.lock().unwrap().status.is_running())
            .count()
    }
}

/// 杀掉一整棵进程树。
///
/// Unix 用进程组（`setsid` 之后子进程自成一个组）；Windows 用 `taskkill /T`，
/// 那是系统自带的、能递归杀子树的东西。
pub(crate) fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        // 负号 = 整个进程组。
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        // 给进程一次体面退出的机会（关连接、删临时文件），然后强杀。
        // 这段是同步阻塞的：调用点都在「用户点了停止 / 关会话」上，慢 150ms
        // 无所谓；换成异步反而要求调用方保证处在 runtime 里。
        std::thread::sleep(Duration::from_millis(150));
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // 没有控制台的 GUI 进程起 `taskkill.exe` 也会闪一个窗口。
        crate::platform::hide_console(&mut cmd);
        let _ = cmd.status();
    }
}

// ---------------------------------------------------------------------------
// 两个工具
// ---------------------------------------------------------------------------

/// `bash_output`：读后台命令**自上次读之后**的新输出。
pub struct BashOutputTool {
    shells: Arc<BackgroundShells>,
}

impl BashOutputTool {
    pub fn new(shells: Arc<BackgroundShells>) -> Self {
        Self { shells }
    }
}

#[async_trait::async_trait]
impl Tool for BashOutputTool {
    fn name(&self) -> &str {
        "bash_output"
    }

    fn description(&self) -> &str {
        "读取 `bash` 以 `run_in_background: true` 启动的命令的输出。\
         每次只返回**上次之后的新内容**。命令还在跑时可以带 `wait_ms` 等一会儿\
         再取（最多 60 秒），适合等构建/测试结束。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "shell_id": {
                    "type": "string",
                    "description": "后台命令的 id（`bash` 返回的 shell_id）"
                },
                "wait_ms": {
                    "type": "integer",
                    "description": "没有新输出时最多等多久（毫秒，缺省 0 = 立刻返回）"
                }
            },
            "required": ["shell_id"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("shell_id").and_then(Value::as_str) else {
            return ToolResult::err("缺少 shell_id");
        };
        let wait = args
            .get("wait_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(MAX_WAIT_MS);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait);

        loop {
            let (text, status) = match self.shells.read(id) {
                Ok(v) => v,
                Err(e) => return ToolResult::err(e),
            };
            let done = !status.is_running();
            if !text.is_empty() || done || tokio::time::Instant::now() >= deadline {
                let body = if text.is_empty() {
                    if done {
                        "（没有新输出）".to_string()
                    } else {
                        "（还没有新输出，命令仍在运行）".to_string()
                    }
                } else {
                    text
                };
                return ToolResult::ok(format!(
                    "shell_id={id}\nstatus={}\n--- 新输出 ---\n{body}",
                    status.as_str()
                ));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
}

/// `kill_shell`：结束一条后台命令。
pub struct KillShellTool {
    shells: Arc<BackgroundShells>,
}

impl KillShellTool {
    pub fn new(shells: Arc<BackgroundShells>) -> Self {
        Self { shells }
    }
}

#[async_trait::async_trait]
impl Tool for KillShellTool {
    fn name(&self) -> &str {
        "kill_shell"
    }

    fn description(&self) -> &str {
        "结束一条用 `run_in_background: true` 启动的后台命令（连同它的子进程）。\
         已经结束的命令会原样返回它的状态。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "shell_id": { "type": "string", "description": "要结束的后台命令 id" }
            },
            "required": ["shell_id"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("shell_id").and_then(Value::as_str) else {
            return ToolResult::err("缺少 shell_id");
        };
        match self.shells.kill(id) {
            Ok(status) => ToolResult::ok(format!("shell_id={id}\nstatus={}", status.as_str())),
            Err(e) => ToolResult::err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh() -> Shell {
        crate::platform::detect_shell()
    }

    /// 每个测试一个独立目录：日志是真实文件，不能落在共享的临时目录里。
    fn shells() -> Arc<BackgroundShells> {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "carter-bg-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        BackgroundShells::at(dir)
    }

    #[tokio::test]
    async fn a_background_command_reports_output_and_exit() {
        let shells = shells();
        let (id, log) = shells
            .spawn(
                "echo hello-bg",
                &std::env::temp_dir(),
                &sh(),
                "test-session",
            )
            .unwrap();
        assert!(log.exists());
        // 等它跑完（短命令，通常几十毫秒）。
        for _ in 0..50 {
            if shells.status(&id).is_some_and(|s| !s.is_running()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (text, status) = shells.read(&id).unwrap();
        assert!(matches!(status, ShellStatus::Exited(0)), "{status:?}");
        assert!(text.contains("hello-bg"), "{text}");
    }

    /// 读是**增量**的：第二次读不该把第一次的内容再返回一遍。
    #[tokio::test]
    async fn reading_advances_past_what_was_already_delivered() {
        let shells = shells();
        let (id, _) = shells
            .spawn("echo first", &std::env::temp_dir(), &sh(), "test-session")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (first, _) = shells.read(&id).unwrap();
        assert!(first.contains("first"));
        let (second, _) = shells.read(&id).unwrap();
        assert!(!second.contains("first"), "第二次不该重复：{second:?}");
    }

    #[tokio::test]
    async fn killing_a_running_shell_marks_it_killed() {
        let shells = shells();
        let (id, _) = shells
            .spawn("sleep 30", &std::env::temp_dir(), &sh(), "test-session")
            .unwrap();
        assert!(shells.running_count() >= 1);
        let st = shells.kill(&id).unwrap();
        assert_eq!(st, ShellStatus::Killed);
        // 杀完之后不再算「活着」。
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(shells.running_count(), 0);
        assert!(shells.read(&id).is_ok(), "日志仍然读得到");
    }

    #[tokio::test]
    async fn an_unknown_shell_id_is_a_clear_error() {
        let shells = shells();
        let err = shells.read("sh404").unwrap_err();
        assert!(err.contains("sh404"), "{err}");
    }
}
