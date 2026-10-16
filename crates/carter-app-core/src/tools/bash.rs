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

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};

use carter::tools::{Tool, ToolResult};

use crate::platform::detect_shell;

const MAX_OUTPUT: usize = 30_000;
const DEFAULT_TIMEOUT_SECS: u64 = 120;

pub struct PlatformBash {
    /// 会话的工作目录。
    cwd: std::path::PathBuf,
}

impl PlatformBash {
    pub fn new(cwd: std::path::PathBuf) -> Self {
        Self { cwd }
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
                "timeout_secs": { "type": "integer", "description": "超时秒数（缺省 120）" }
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

        // 工作目录不存在就直接说清楚。让 spawn 去报 "program not found" 会把
        // 「目录没了」伪装成「命令不存在」，很难查。
        if !self.cwd.is_dir() {
            return ToolResult::err(format!(
                "工作目录不存在：{}（在输入框下方改一个有效目录，或新开会话）",
                self.cwd.display()
            ));
        }

        let shell = detect_shell();
        let warning = sniff_dangerous(command);

        let mut cmd = tokio::process::Command::new(&shell.program);
        cmd.arg(shell.flag)
            .arg(command)
            .current_dir(&self.cwd)
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

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::err(format!(
                    "启动 shell 失败：{e}\n（探测到的 shell：{} = {}）",
                    shell.label(),
                    shell.program.display()
                ));
            }
        };

        #[cfg(unix)]
        let pgid: Option<i32> = child.id().map(|p| p as i32);

        let output = match tokio::time::timeout(Duration::from_secs(timeout), child.wait_with_output())
            .await
        {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return ToolResult::err(format!("执行出错：{e}")),
            Err(_) => {
                #[cfg(unix)]
                if let Some(pid) = pgid {
                    unsafe {
                        libc::kill(-pid, libc::SIGTERM);
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                }
                return ToolResult::err(format!("命令超过 {timeout}s 未结束（进程树已清理）"));
            }
        };

        let stdout = decode(&output.stdout);
        let stderr = decode(&output.stderr);
        let code = output.status.code().unwrap_or(-1);

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
        if output.status.success() {
            ToolResult::ok(report)
        } else {
            ToolResult::err(report)
        }
    }
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

    #[tokio::test]
    async fn runs_a_trivial_command_in_the_session_cwd() {
        let dir = std::env::temp_dir();
        let tool = PlatformBash::new(dir.clone());
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
        let tool = PlatformBash::new(std::path::PathBuf::from("/definitely/not/here/xyz"));
        let res = tool.execute(json!({"command": "echo hi"})).await;
        assert!(!res.ok);
        assert!(res.content.contains("工作目录不存在"), "{}", res.content);
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_exit_code() {
        let tool = PlatformBash::new(std::env::temp_dir());
        // 两个平台都认的「一定失败」的写法不一样，用 exit 1 之外的通用办法：
        // 调用一个几乎不可能存在的程序名。
        let res = tool
            .execute(json!({"command": "definitely-not-a-real-program-xyz"}))
            .await;
        assert!(!res.ok, "非零退出应当作为错误回灌：{}", res.content);
        assert!(res.content.contains("exit_code="), "{}", res.content);
    }

    #[test]
    fn dangerous_commands_are_flagged() {
        assert!(sniff_dangerous("rm -rf /tmp/x").is_some());
        assert!(sniff_dangerous("ls -la").is_none());
        assert!(sniff_dangerous("cargo build").is_none());
    }
}
