//! 跨平台的 bash 工具，顶掉 carter 那个硬编码 `sh -c` 的实现。
//!
//! ## 它和 carter 那个的区别
//!
//! | | carter `Bash` | 这里 |
//! |---|---|---|
//! | shell | 恒 `sh -c`（Windows 上多半找不到） | 探测：POSIX sh → `cmd /C` |
//! | 描述 | 固定的，不提平台 | 写明系统、shell、以及该写 `ls` 还是 `dir` |
//! | 工作目录 | 进程 cwd | 会话的 cwd（显式传入） |
//!
//! 保留的行为（与 carter 一致，不重新发明）：超时、大输出截断、危险命令软警告、
//! Unix 下用进程组起子进程以便超时时清理整棵进程树。
//!
//! ## 为什么要显式设工作目录
//!
//! 桌面的进程 cwd 取决于从哪启动的（`C:\Windows\System32` 之类）。会话有自己明确的
//! cwd，工具却跑在进程 cwd 上 —— 模型会读到完全无关的目录内容，而且看不出来为什么。
//! 这个 bug 在命令行下不会出现（cwd 就是用户启动的位置），在 GUI 里必现。
//!
//! ## 取消
//!
//! 工具里必须**自己**盯着取消信号。carter 的 `run_turn` 只在流式循环里检查取消，
//! 工具执行阶段它什么都做不了 —— 一个 `sleep 600` 会把「停止」按钮变成一个摆设，
//! 关掉会话之后旧任务还要在后台跑满 120 秒（期间用户重开同一条会话就是两个 runner
//! 写同一个文件，历史从此对不上）。所以这里：
//!
//! * 进入时已经取消 → 一条命令都不执行；
//! * 执行中取消 → 立刻杀掉整个进程组，返回「被中断」的结果。
//!
//! ## 后台执行
//!
//! 两种入口，最终都落到 [`super::background::BackgroundShells`]：
//!
//! * 模型写 `run_in_background: true` —— 起服务、跑长构建、`tail -f` 这类命令
//!   一开始就不该卡住整轮对话；
//! * 界面上那条「运行中…」的卡片被点了「转后台」—— 命令已经跑了一会儿，用户
//!   发现它不会很快结束。这时输出管道的读取任务和退出监视任务都留着，只是把
//!   目的地从内存缓冲换成日志文件（见 [`LiveOutput::start_logging`]），
//!   工具立刻带着 `shell_id` 返回。
//!
//! 卡片上另一颗按钮「中断命令」只结束这一条命令（见 [`super::control`]）：
//! 结果是回灌给模型的，这一轮继续 —— 与输入框上的「停止」不是一回事。

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use carter::agent::CancelToken;
use carter::tools::{Tool, ToolResult};

use crate::platform::detect_shell;

use super::background::BackgroundShells;

const MAX_OUTPUT: usize = 30_000;
const DEFAULT_TIMEOUT_SECS: u64 = 120;

pub struct PlatformBash {
    /// 会话的工作目录。
    cwd: std::path::PathBuf,
    /// 用户按「停止」/关会话时置位。见模块头。
    cancel: CancelToken,
    /// 后台命令表。`None` = 这个工具只用来展示（设置页的工具体验），不真跑后台。
    shells: Option<Arc<BackgroundShells>>,
    /// 日志按会话分目录，见 `background.rs`。
    session_id: String,
}

impl PlatformBash {
    pub fn new(
        cwd: std::path::PathBuf,
        cancel: CancelToken,
        shells: Option<Arc<BackgroundShells>>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            cwd,
            cancel,
            shells,
            session_id: session_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for PlatformBash {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        // 描述是**每次调用时现算**的：探测结果有缓存，代价只有一次字符串拼接。
        // 用 &str 的 trait 签名要求这里返回借用，所以 Description 走一个 leak ——
        // 描述在进程生命周期内不变，泄漏几十字节换掉一次 clone 是划算的。
        // 见 `bash_tool_description_leaked`。
        crate::platform::bash_tool_description_leaked()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的命令" },
                "timeout_secs": { "type": "integer", "description": "超时秒数（缺省 120，仅前台执行有效）" },
                "run_in_background": {
                    "type": "boolean",
                    "description": "后台执行，立刻返回 shell_id。适合起服务、跑长构建、tail -f 这类\
                                    不想卡住整轮对话的命令。之后用 bash_output 读输出、kill_shell 结束。"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return ToolResult::err("missing or non-string argument: command");
        };
        let timeout = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let background = args
            .get("run_in_background")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // 工作目录不存在就直接说清楚。让 spawn 去报 "program not found" 会把
        // 「目录没了」伪装成「命令不存在」，很难查。
        if !self.cwd.is_dir() {
            return ToolResult::err(format!(
                "工作目录不存在：{}（在输入框下方改一个有效目录，或新开会话）",
                self.cwd.display()
            ));
        }

        let shell = detect_shell();

        // 进入工具时这一轮已经被取消了：一条命令都别跑。否则用户按了停止之后，
        // 队列里剩下的工具还会一个个跑起来 —— 看起来就是「停止没生效」。
        if self.cancel.is_cancelled() {
            return ToolResult::err("这一轮已被用户中断，命令没有执行。");
        }

        if background {
            let Some(shells) = &self.shells else {
                return ToolResult::err("这个会话不支持后台命令。");
            };
            return match shells.spawn(command, &self.cwd, &shell, &self.session_id) {
                Ok((id, log)) => ToolResult::ok(format!(
                    "命令已在后台启动。\n\
                     shell_id={id}\n\
                     日志：{}\n\
                     用 bash_output(shell_id=\"{id}\") 读取输出（可以带 wait_ms 等一会儿），\
                     用 kill_shell(shell_id=\"{id}\") 结束它。",
                    log.display()
                )),
                Err(e) => ToolResult::err(e),
            };
        }

        let warning = sniff_dangerous(command);

        let mut cmd = tokio::process::Command::new(&shell.program);
        cmd.arg(shell.flag)
            .arg(command)
            .current_dir(&self.cwd)
            // 桌面进程的 stdin 不是终端。不显式给 null 的话，`cat` / `read`
            // 这类命令会一直等着一个永远不会来的输入 —— 用户看到的就是
            // 「运行中…」再也不会变。
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Windows 上不弹控制台窗口（否则每次 bash 都会闪一个黑框）。
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        // Unix：独立进程组，超时时能 kill 整棵树（含 `cmd & disown` 这种）。
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::err(format!(
                    "启动 shell 失败：{e}\n（探测到的 shell：{} = {}）",
                    shell.label(),
                    shell.program.display()
                ));
            }
        };

        let kill_pid = child.id();

        // 两条管道各交给一个读取任务。**不能等命令结束再收输出**：
        // 一是管道写满会把子进程卡死；二是「转后台」之后还得继续往日志里写。
        let live = Arc::new(LiveOutput::default());
        let mut readers = Some([
            spawn_reader(child.stdout.take(), live.clone(), Which::Stdout),
            spawn_reader(child.stderr.take(), live.clone(), Which::Stderr),
        ]);

        // 子进程退出由一个监视任务统一处理：转了后台之后这次调用已经返回，
        // 但后台表里的状态还得继续更新。
        let exit = Arc::new(Mutex::new(ExitSlot::default()));
        let (exit_tx, mut exit_rx) = tokio::sync::oneshot::channel();
        {
            let slot = exit.clone();
            tokio::spawn(async move {
                let status = child.wait().await;
                let code = status.as_ref().ok().and_then(|s| s.code());
                slot.lock().unwrap().finish(code);
                let _ = exit_tx.send(status);
            });
        }

        // 单条命令的控制（界面 bash 卡片上的「中断命令 / 转后台」）。
        // 没有经过记录包装层时是 `None`（设置页展示、单测直接调工具）。
        let control = super::control::current();
        let wait_interrupt = async {
            match control.as_ref() {
                Some(c) => c.interrupted().await,
                None => std::future::pending().await,
            }
        };
        let wait_background = async {
            match control.as_ref() {
                Some(c) => c.backgrounded().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(wait_interrupt);
        tokio::pin!(wait_background);
        let deadline = tokio::time::sleep(Duration::from_secs(timeout));
        tokio::pin!(deadline);

        enum How {
            Done(std::io::Result<std::process::ExitStatus>),
            Stop,
            Interrupt,
            Background,
            Timeout,
        }

        let how = tokio::select! {
            biased;
            // 整轮停止 / 关会话优先于单条命令上的操作。
            _ = self.cancel.cancelled() => How::Stop,
            _ = &mut wait_interrupt => How::Interrupt,
            _ = &mut wait_background => How::Background,
            r = &mut exit_rx => How::Done(match r {
                Ok(st) => st,
                Err(_) => return ToolResult::err("执行出错：等待子进程退出时通道断了"),
            }),
            _ = &mut deadline => How::Timeout,
        };

        let status = match how {
            How::Done(st) => match st {
                Ok(s) => s,
                Err(e) => return ToolResult::err(format!("执行出错：{e}")),
            },
            How::Stop => {
                kill_if_running(kill_pid);
                drain(readers.take()).await;
                return ToolResult::err(
                    "命令被用户中断了（进程树已清理）。需要长时间运行的命令请用 \
                     run_in_background: true。",
                );
            }
            How::Interrupt => {
                // 只结束**这一条**命令，不结束这一轮：结果回灌给模型，
                // 让它自己决定换种做法还是先问用户。
                kill_if_running(kill_pid);
                drain(readers.take()).await;
                let (out, err) = live.snapshot();
                let tail = merge_streams(&out, &err);
                return ToolResult::err(if tail.trim().is_empty() {
                    "命令被用户中断了（进程树已清理）。".to_string()
                } else {
                    format!(
                        "命令被用户中断了（进程树已清理）。中断前已产生的输出：\n{}",
                        truncate(&tail, MAX_OUTPUT)
                    )
                });
            }
            How::Timeout => {
                kill_if_running(kill_pid);
                drain(readers.take()).await;
                return ToolResult::err(format!("命令超过 {timeout}s 未结束（进程树已清理）"));
            }
            How::Background => {
                // 读取任务**不 join**：它们继续把输出写进日志。调用在这里返回，
                // `readers` 随之被 drop —— tokio 的 JoinHandle 是 detach 语义，
                // 任务照跑。
                return self.hand_over_to_background(command, &live, &exit, kill_pid);
            }
        };
        let code = status.code().unwrap_or(-1);

        // 子进程已退出：等两条管道读到 EOF，输出才完整。
        drain(readers.take()).await;

        let (stdout_b, stderr_b) = live.snapshot();
        let stdout = decode(&stdout_b);
        let stderr = decode(&stderr_b);

        let mut body = String::new();
        if let Some(w) = warning {
            body.push_str(&format!("[warning] {w}\n"));
        }
        if !stdout.is_empty() {
            body.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str("[stderr]\n");
            body.push_str(&stderr);
        }
        if body.is_empty() {
            // 命令成功但一声不吭（`mkdir` / `cd` 之类）。明确说出来，
            // 否则模型会以为输出丢了。
            body.push_str("(命令成功，没有输出)");
        }

        let report = format!("exit_code={code}\n{}", truncate(&body, MAX_OUTPUT));
        if status.success() {
            ToolResult::ok(report)
        } else {
            ToolResult::err(report)
        }
    }
}

impl PlatformBash {
    /// 把一条前台命令交给后台表，并立刻返回 `shell_id`。
    fn hand_over_to_background(
        &self,
        command: &str,
        live: &LiveOutput,
        exit: &Mutex<ExitSlot>,
        pid: Option<u32>,
    ) -> ToolResult {
        let Some(shells) = &self.shells else {
            return ToolResult::err("这个会话不支持后台命令。");
        };
        match shells.adopt(command, &self.session_id, pid) {
            Ok(mut adopted) => {
                if let Err(e) = live.start_logging(&mut adopted.file) {
                    return ToolResult::err(format!("转到后台失败：{e}"));
                }
                let (id, log) = (adopted.id.clone(), adopted.log.clone());
                // 进程可能已经在这几步之间退出了 —— `attach` 会把这个状态补上。
                exit.lock().unwrap().attach(adopted);
                ToolResult::ok(format!(
                    "命令已转到后台继续运行。\n\
                     shell_id={id}\n\
                     日志：{}\n\
                     用 bash_output(shell_id=\"{id}\") 读取输出（可以带 wait_ms 等一会儿），\
                     用 kill_shell(shell_id=\"{id}\") 结束它。",
                    log.display()
                ))
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

/// 前台命令的哪一路输出。
#[derive(Clone, Copy)]
enum Which {
    Stdout,
    Stderr,
}

/// 前台命令已经收到的输出。转到后台之后，同一份数据会继续写进日志文件。
#[derive(Default)]
struct LiveOutput {
    inner: Mutex<Live>,
}

#[derive(Default)]
struct Live {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// 转到后台之后才有值。有它之后就不再攒内存了 —— 一条跑几个小时的后台
    /// 服务不该把全部输出留在进程里。
    log: Option<std::fs::File>,
}

impl LiveOutput {
    fn push(&self, which: Which, bytes: &[u8]) {
        use std::io::Write as _;
        let mut g = self.inner.lock().unwrap();
        if let Some(f) = g.log.as_mut() {
            let _ = f.write_all(bytes);
            return;
        }
        match which {
            Which::Stdout => g.stdout.extend_from_slice(bytes),
            Which::Stderr => g.stderr.extend_from_slice(bytes),
        }
    }

    fn snapshot(&self) -> (Vec<u8>, Vec<u8>) {
        let g = self.inner.lock().unwrap();
        (g.stdout.clone(), g.stderr.clone())
    }

    /// 把已经攒下的输出写进日志，并从现在起把新输出也写进去。
    fn start_logging(&self, file: &mut std::fs::File) -> std::io::Result<()> {
        use std::io::Write as _;
        let mut g = self.inner.lock().unwrap();
        if !g.stdout.is_empty() {
            file.write_all(&g.stdout)?;
            if !g.stdout.ends_with(b"\n") {
                file.write_all(b"\n")?;
            }
        }
        if !g.stderr.is_empty() {
            file.write_all(&g.stderr)?;
        }
        g.log = Some(file.try_clone()?);
        Ok(())
    }
}

/// 子进程退出状态。转后台之后，这条记录会被 `ExitSlot.target` 拿去更新后台表。
#[derive(Default)]
struct ExitSlot {
    code: Option<i32>,
    target: Option<super::background::AdoptedShell>,
}

impl ExitSlot {
    fn finish(&mut self, code: Option<i32>) {
        self.code = code;
        if let Some(t) = &self.target {
            t.mark_exited(code.unwrap_or(-1));
        }
    }

    fn attach(&mut self, shell: super::background::AdoptedShell) {
        if let Some(code) = self.code {
            shell.mark_exited(code);
        }
        self.target = Some(shell);
    }
}

/// 起一个读取任务，把管道里的字节喂给 [`LiveOutput`]。
fn spawn_reader<R>(
    stream: Option<R>,
    live: Arc<LiveOutput>,
    which: Which,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncReadExt as _;
    tokio::spawn(async move {
        let Some(mut stream) = stream else { return };
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => live.push(which, &buf[..n]),
            }
        }
    })
}

/// 等读取任务把已产出的输出收完。带时限 —— 被杀的进程可能留下一个不关管道的
/// 孙进程，不能让它把整轮对话吊住。
async fn drain(readers: Option<[tokio::task::JoinHandle<()>; 2]>) {
    let Some(readers) = readers else { return };
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        for h in readers {
            let _ = h.await;
        }
    })
    .await;
}

fn kill_if_running(pid: Option<u32>) {
    if let Some(pid) = pid {
        super::background::kill_process_tree(pid);
    }
}

/// 中断时给模型看的一段现场：stdout 与 stderr 按区段拼起来。
fn merge_streams(stdout: &[u8], stderr: &[u8]) -> String {
    let mut s = decode(stdout);
    let e = decode(stderr);
    if !e.is_empty() {
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&e);
    }
    s
}

/// 解 stdout / stderr 的字节。
///
/// 先按 UTF-8 试。不是 UTF-8 的话，Windows 上按 **OEM 代码页**解 —— 中文环境下
/// `dir`、`ipconfig` 这类原生命令输出的是 GBK(936)，直接 `from_utf8_lossy` 会得到
/// 一片 `�`，模型和用户都读不出任何东西。
///
/// 用 `MultiByteToWideChar` 而不是自己写码表：GBK 与 Unicode 的映射不是能「朴素还原」
/// 的东西，手搓一个只会产出看着像字、其实是错的乱码 —— 那比一个诚实的 `�` 更坏。
fn decode(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    #[cfg(windows)]
    {
        if let Some(s) = decode_oem(bytes) {
            return s;
        }
    }
    String::from_utf8_lossy(bytes).to_string()
}

/// 按系统 OEM 代码页解码。解不出来返回 None，让调用方回落 lossy。
#[cfg(windows)]
fn decode_oem(bytes: &[u8]) -> Option<String> {
    use windows_sys::Win32::Globalization::{GetOEMCP, MultiByteToWideChar};

    if bytes.is_empty() {
        return Some(String::new());
    }
    // 控制台程序的输出走 OEM 代码页（中文 Windows 上是 936）。
    let cp = unsafe { GetOEMCP() };
    let src_len = i32::try_from(bytes.len()).ok()?;

    // 第一遍问长度。
    let need = unsafe {
        MultiByteToWideChar(
            cp,
            0,
            bytes.as_ptr(),
            src_len,
            std::ptr::null_mut(),
            0,
        )
    };
    if need <= 0 {
        return None;
    }
    let mut buf = vec![0u16; need as usize];
    let got = unsafe {
        MultiByteToWideChar(cp, 0, bytes.as_ptr(), src_len, buf.as_mut_ptr(), need)
    };
    if got <= 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..got as usize]))
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... [截断 {} 字节]", &s[..end], s.len() - end)
}

/// 危险命令软警告。不阻断 —— 让模型在结果里看到（错误即数据）。
fn sniff_dangerous(cmd: &str) -> Option<&'static str> {
    let lower = cmd.trim().to_ascii_lowercase();
    if (lower.contains("rm -rf") || lower.contains("rm -fr"))
        && (lower.contains(" /")
            || lower.contains(" ~")
            || lower.contains(" *")
            || lower.contains(" .git"))
    {
        return Some("检测到 `rm -rf` 指向敏感目标，请确认结果符合预期");
    }
    if lower.contains("dd ") && lower.contains("of=/dev/") {
        return Some("`dd` 写入 /dev/* 设备不可逆");
    }
    if (lower.starts_with("mkfs") || lower.contains(" mkfs")) && lower.contains("/dev/") {
        return Some("`mkfs` 会格式化块设备，不可逆");
    }
    if lower.starts_with("shutdown") || lower.starts_with("reboot") || lower.starts_with("halt") {
        return Some("系统电源命令，会影响宿主机");
    }
    // Windows 侧的高危操作。
    if lower.contains("format ") && (lower.contains(" /q") || lower.contains("c:")) {
        return Some("`format` 会格式化磁盘，不可逆");
    }
    if lower.contains("rd /s /q") && (lower.contains(" c:\\") || lower.contains(" %systemdrive%")) {
        return Some("`rd /s /q` 指向系统盘，请确认");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个「没有人会去取消」的工具。取消相关的测试自己另建令牌。
    fn tool(cwd: std::path::PathBuf) -> PlatformBash {
        PlatformBash::new(cwd, CancelToken::new(), None, "test")
    }

    #[tokio::test]
    async fn runs_a_trivial_command_in_the_session_cwd() {
        let dir = std::env::temp_dir();
        let tool = tool(dir.clone());
        // 用两边都有的命令：POSIX 下 `pwd` 在 cmd 下不存在，所以选一个绝对安全的。
        let res = tool.execute(json!({"command": "echo hello-from-bash"})).await;
        assert!(res.ok, "{}", res.content);
        assert!(res.content.contains("hello-from-bash"), "{}", res.content);
        assert!(res.content.contains("exit_code=0"), "{}", res.content);
    }

    /// 工作目录不存在时要**明说**，而不是伪装成「命令不存在」。
    ///
    /// 这条守的是 GUI 特有的坑：桌面进程的 cwd 取决于从哪启动，而会话有自己的 cwd。
    /// 两者不一致时如果直接 spawn，报出来的是 shell 找不到，查半天查不到原因。
    #[tokio::test]
    async fn a_missing_cwd_says_so_instead_of_failing_obscurely() {
        let tool = tool(std::path::PathBuf::from("/definitely/not/here/xyz"));
        let res = tool.execute(json!({"command": "echo hi"})).await;
        assert!(!res.ok);
        assert!(res.content.contains("工作目录不存在"), "{}", res.content);
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_exit_code() {
        let tool = tool(std::env::temp_dir());
        // 两个平台都认的「一定失败」的写法不一样，用 exit 1 之外的通用办法：
        // 调用一个几乎不可能存在的程序名。
        let res = tool
            .execute(json!({"command": "definitely-not-a-real-program-xyz"}))
            .await;
        assert!(!res.ok, "非零退出应当作为错误回灌：{}", res.content);
        assert!(res.content.contains("exit_code="), "{}", res.content);
    }

    /// 用户点「停止」时，一条正在跑的命令必须**立刻**结束。
    ///
    /// 这条守的是那个把会话历史写坏的根因：以前取消只在流式阶段被检查，
    /// 前台命令会一直跑到超时（默认 120 秒），期间会话关不掉、重开就会有两个
    /// runner 写同一个文件。
    #[tokio::test]
    async fn a_running_command_is_killed_promptly_on_cancel() {
        let cancel = CancelToken::new();
        let tool = PlatformBash::new(std::env::temp_dir(), cancel.clone(), None, "test");
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            c.set();
        });
        let started = std::time::Instant::now();
        // `sleep 30` 在 POSIX 与 cmd 下都存在（Windows 的 sleep 是 timeout，但
        // Git Bash 的 sleep 也多半有）。为了两边都稳，用 shell 内建的死循环写法：
        // 这个测试在 CI 上主要是 macOS / Linux。
        let res = tool.execute(json!({"command": "sleep 30"})).await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "取消之后不该继续等命令：{:?}",
            started.elapsed()
        );
        assert!(!res.ok, "被中断应当作为错误回灌：{}", res.content);
        assert!(res.content.contains("中断"), "{}", res.content);
    }

    /// 卡片上的「中断命令」只结束这一条命令：工具返回一个错误结果，**这一轮继续**。
    #[tokio::test]
    async fn the_card_button_interrupts_only_that_command() {
        use super::super::control::RunningTools;
        use crate::protocol::ToolActionKind;

        let running = RunningTools::new();
        let tool = Arc::new(tool(std::env::temp_dir()));
        let r = running.clone();
        let task = tokio::spawn(async move {
            r.scope("t0", async move { tool.execute(json!({"command": "sleep 30"})).await })
                .await
        });

        // 等命令真的开始跑，再按按钮。
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(running.apply("t0", ToolActionKind::Interrupt));

        let res = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("中断之后不该继续等命令")
            .unwrap();
        assert!(!res.ok, "被中断应当作为错误回灌：{}", res.content);
        assert!(res.content.contains("中断"), "{}", res.content);
    }

    /// 卡片上的「转后台」：这次调用立刻返回 `shell_id`，命令继续跑，
    /// 已经产出的输出也在日志里。
    #[tokio::test]
    async fn a_running_command_can_be_moved_to_the_background() {
        use super::super::control::RunningTools;
        use crate::protocol::ToolActionKind;

        let shells = BackgroundShells::at(std::env::temp_dir().join(format!(
            "carter-bg-adopt-{}",
            std::process::id()
        )));
        let tool = Arc::new(PlatformBash::new(
            std::env::temp_dir(),
            CancelToken::new(),
            Some(shells.clone()),
            "test-session",
        ));
        let running = RunningTools::new();
        let r = running.clone();
        let task = tokio::spawn(async move {
            r.scope(
                "t0",
                async move {
                    tool.execute(json!({"command": "echo first-line; sleep 30"}))
                        .await
                },
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(running.apply("t0", ToolActionKind::Background));

        let res = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("转后台必须立刻返回")
            .unwrap();
        assert!(res.ok, "{}", res.content);
        assert!(res.content.contains("shell_id=sh1"), "{}", res.content);

        // 给读取任务一点时间把输出写进日志。
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (text, status) = shells.read("sh1").unwrap();
        assert!(text.contains("first-line"), "日志里要有转后台之前的输出：{text}");
        assert!(status.is_running(), "命令应当还在跑：{status:?}");
        shells.kill_all();
    }

    /// 已经取消的轮次不许再启动新命令 —— 否则「停止」之后队列里剩下的工具
    /// 还会一个个跑起来。
    #[tokio::test]
    async fn a_cancelled_turn_does_not_start_the_command_at_all() {
        let cancel = CancelToken::new();
        cancel.set();
        let tool = PlatformBash::new(std::env::temp_dir(), cancel, None, "test");
        let res = tool
            .execute(json!({"command": "echo should-not-run"}))
            .await;
        assert!(!res.ok);
        assert!(res.content.contains("没有执行"), "{}", res.content);
    }

    /// `run_in_background` 立刻返回 shell_id，不等命令跑完。
    #[tokio::test]
    async fn background_mode_returns_immediately_with_a_shell_id() {
        let shells = BackgroundShells::at(
            std::env::temp_dir().join(format!("carter-bg-bash-{}", std::process::id())),
        );
        let tool = PlatformBash::new(
            std::env::temp_dir(),
            CancelToken::new(),
            Some(shells.clone()),
            "test-session",
        );
        let started = std::time::Instant::now();
        let res = tool
            .execute(json!({"command": "sleep 5", "run_in_background": true}))
            .await;
        assert!(res.ok, "{}", res.content);
        assert!(res.content.contains("shell_id=sh1"), "{}", res.content);
        assert!(started.elapsed() < Duration::from_secs(3), "不该阻塞");
        // 收尾：别把测试进程里的子进程留着。
        shells.kill_all();
    }

    #[test]
    fn dangerous_commands_are_flagged() {
        assert!(sniff_dangerous("rm -rf /tmp/x").is_some());
        assert!(sniff_dangerous("ls -la").is_none());
        assert!(sniff_dangerous("cargo build").is_none());
    }
}
