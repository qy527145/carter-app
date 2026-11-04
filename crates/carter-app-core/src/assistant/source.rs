//! 观察源：定时跑一条命令，输出里**新出现的行**就是事件。
//!
//! ## 为什么是「命令 + 按行比对」
//!
//! 原型里每种来源（GitHub、飞书）各是一个插件，各写一套读接口、游标与去重。桌面上
//! 用户已经有现成的 CLI：`gh` 能列通知与 PR，`git` 能看提交，钉钉连接器装好了 `dws`。
//! 一条命令 + 一份「见过哪些行」就能覆盖它们，用户也看得懂自己配的是什么 ——
//! 想换个过滤条件，改的是一条自己会在终端里试的命令，不是一份插件配置。
//!
//! 命令的输出该是**一行一件事**（`gh ... --jq '.[] | "..."'` 这种）。一整段 JSON 的话
//! 每一行都是半个对象，模型照样读得懂，只是噪音大 —— 设置页的示例给的都是逐行的写法。
//!
//! ## 基线
//!
//! 第一次成功轮询只记下现有的行，不产生事件：原型的「重启不能重放事件」。不然
//! 新建一个「看 GitHub 通知」的来源，就会把积压的几十条通知一次性当成新事件。
//!
//! ## 指纹
//!
//! 存的是每行（去掉首尾空白之后）的 64 位哈希，不存原文 —— 原文可能很长，还可能带
//! 私人内容，没必要落盘第二份。最多留 [`MAX_SEEN`] 条，按最近出现的顺序淘汰。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use super::{Source, SourceState};
use crate::platform;
use crate::protocol::now_ms;

/// 指纹最多留多少条。
pub const MAX_SEEN: usize = 2000;
/// 一次轮询最多交给评估的新行。再多就是命令写得太宽了（或者第一次跑命令改了没重建
/// 基线），截断并在事件末尾说明，免得一次把几百行塞进评估请求。
pub const MAX_NEW_PER_POLL: usize = 20;
/// 单行最长保留多少字符。
const MAX_LINE_CHARS: usize = 500;
/// 命令最多跑多久。
const TIMEOUT: Duration = Duration::from_secs(60);
/// 连续失败几次才算「来源出问题」。
pub const FAILURES_TO_SHOW: u32 = 3;

pub fn fingerprint(line: &str) -> u64 {
    let mut h = DefaultHasher::new();
    line.trim().hash(&mut h);
    h.finish()
}

/// 拿这一次的输出和见过的比，返回新行，并更新 `state.seen` / `baselined`。
///
/// 纯函数（除了改 `state`），轮询与测试共用。
pub fn diff(state: &mut SourceState, output: &str) -> Vec<String> {
    let mut fresh = Vec::new();
    let mut current: Vec<u64> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fp = fingerprint(line);
        if current.contains(&fp) {
            continue; // 同一次输出里重复的行只算一次
        }
        current.push(fp);
        if state.baselined && !state.seen.contains(&fp) {
            fresh.push(clip(line));
        }
    }

    // 这一次见到的排到最后（最「新」），淘汰从最前面开始。
    state.seen.retain(|fp| !current.contains(fp));
    state.seen.extend(current);
    if state.seen.len() > MAX_SEEN {
        let extra = state.seen.len() - MAX_SEEN;
        state.seen.drain(..extra);
    }
    state.baselined = true;

    if fresh.len() > MAX_NEW_PER_POLL {
        let more = fresh.len() - MAX_NEW_PER_POLL;
        fresh.truncate(MAX_NEW_PER_POLL);
        fresh.push(format!(
            "（还有 {more} 条新行没有列出 —— 命令的范围可能太宽了）"
        ));
    }
    fresh
}

fn clip(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let mut s: String = line.chars().take(MAX_LINE_CHARS).collect();
    s.push('…');
    s
}

/// 内置命令的前缀。以它开头的「命令」不交给 shell，由应用自己执行。
///
/// 团队消息的送达（`carter:team-inbox`）就是这样一条：它要读 git 的事件链、对照成员
/// 名单，用 shell 拼的话得依赖 `jq`，而 Windows 上通常没有 —— 那正是上一版
/// `carter-team` 脚本在 Windows 上起不来的原因。做成内置命令，三平台行为一致，
/// 输出照样是「一行一件事」，观察源其余的机制（基线、去重、评估）一点不用改。
pub const BUILTIN_PREFIX: &str = "carter:";

/// 团队收件箱：拉取事件、筛出跟本机有关的，每条一行。
pub const TEAM_INBOX: &str = "carter:team-inbox";

/// 跑一次来源的命令，返回标准输出。非零退出码算失败，错误里带上 stderr 的末尾。
pub async fn run_command(command: &str, cwd: &Path) -> Result<String, String> {
    if !cwd.is_dir() {
        return Err(format!("工作目录 {} 不存在", cwd.display()));
    }
    if let Some(rest) = command.trim().strip_prefix(BUILTIN_PREFIX) {
        return run_builtin(rest.trim(), cwd).await;
    }
    let shell = platform::detect_shell();
    let mut cmd = tokio::process::Command::new(&shell.program);
    cmd.arg(shell.flag)
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    platform::hide_console_async(&mut cmd);

    let out = match tokio::time::timeout(TIMEOUT, cmd.output()).await {
        Err(_) => return Err(format!("命令超过 {} 秒没跑完", TIMEOUT.as_secs())),
        Ok(Err(e)) => return Err(format!("命令启动失败：{e}")),
        Ok(Ok(o)) => o,
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err
            .trim()
            .lines()
            .rev()
            .take(3)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" / ");
        return Err(match out.status.code() {
            Some(c) if tail.is_empty() => format!("命令退出码 {c}"),
            Some(c) => format!("命令退出码 {c}：{tail}"),
            None => format!("命令被信号终止：{tail}"),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn run_builtin(name: &str, cwd: &Path) -> Result<String, String> {
    match name {
        "team-inbox" => {
            let cwd = cwd.to_path_buf();
            // git 是阻塞调用；放进阻塞线程池，别卡住轮询它的那个运行时。
            tokio::task::spawn_blocking(move || crate::team::inbox::observe(&cwd, 50))
                .await
                .map_err(|e| format!("团队收件箱没跑完：{e}"))?
        }
        other => Err(format!("不认识的内置命令 `{BUILTIN_PREFIX}{other}`")),
    }
}

/// 轮询一次：跑命令、比对、把结果记进 `state`。返回新事件。
///
/// 下一次的时间在这里推：失败也推，否则一个坏掉的命令每 10 秒跑一次。
pub async fn poll(source: &Source) -> (SourceState, Result<Vec<String>, String>) {
    let mut state = source.state.clone();
    let now = now_ms();
    state.next_at = now + source.every_minutes.max(super::MIN_POLL_MINUTES) * 60_000;
    state.last_polled_at = Some(now);
    match run_command(&source.command, Path::new(&source.cwd)).await {
        Ok(out) => {
            let fresh = diff(&mut state, &out);
            state.last_new = fresh.len() as u32;
            state.last_error = None;
            state.failures = 0;
            (state, Ok(fresh))
        }
        Err(e) => {
            state.last_new = 0;
            state.failures += 1;
            state.last_error = Some(e.clone());
            (state, Err(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 第一次只建基线：已有的东西不算新事件。
    #[test]
    fn the_first_poll_only_records_a_baseline() {
        let mut st = SourceState::default();
        assert!(diff(&mut st, "a\nb\nc\n").is_empty());
        assert!(st.baselined);
        assert_eq!(st.seen.len(), 3);
    }

    #[test]
    fn only_new_lines_come_out_afterwards() {
        let mut st = SourceState::default();
        diff(&mut st, "a\nb\n");
        assert_eq!(diff(&mut st, "a\nb\nc\n"), vec!["c".to_string()]);
        // 已经见过的再出现不算新的；消失再回来也不算（还在指纹里）。
        assert!(diff(&mut st, "c\n").is_empty());
        assert!(diff(&mut st, "a\n").is_empty());
    }

    #[test]
    fn blank_lines_and_surrounding_spaces_are_ignored() {
        let mut st = SourceState::default();
        diff(&mut st, "  a  \n\n");
        assert!(diff(&mut st, "a\n   \n").is_empty());
        assert_eq!(
            diff(&mut st, "a\n x \n x\n"),
            vec!["x".to_string()],
            "同一次里重复的只算一次"
        );
    }

    #[test]
    fn a_flood_is_truncated_with_an_explanation() {
        let mut st = SourceState::default();
        diff(&mut st, "");
        let many: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let fresh = diff(&mut st, &many);
        assert_eq!(fresh.len(), MAX_NEW_PER_POLL + 1);
        assert!(fresh.last().unwrap().contains("30"));
    }

    #[test]
    fn the_fingerprint_list_is_bounded() {
        let mut st = SourceState::default();
        let many: String = (0..(MAX_SEEN + 100)).map(|i| format!("{i}\n")).collect();
        diff(&mut st, &many);
        assert_eq!(st.seen.len(), MAX_SEEN);
        // 留下的是最近的：最后一行还在。
        assert!(
            st.seen
                .contains(&fingerprint(&format!("{}", MAX_SEEN + 99)))
        );
    }

    #[test]
    fn long_lines_are_clipped() {
        let mut st = SourceState::default();
        diff(&mut st, "");
        let long = "字".repeat(MAX_LINE_CHARS + 10);
        let out = diff(&mut st, &long);
        assert_eq!(out[0].chars().count(), MAX_LINE_CHARS + 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_runs_in_its_directory() {
        let dir = std::env::temp_dir();
        let out = run_command("pwd", &dir).await.unwrap();
        let got = std::fs::canonicalize(out.trim()).unwrap();
        assert_eq!(got, std::fs::canonicalize(&dir).unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_command_reports_its_stderr() {
        let err = run_command("echo boom >&2; exit 3", &std::env::temp_dir())
            .await
            .unwrap_err();
        assert!(err.contains("3") && err.contains("boom"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn polling_advances_the_clock_even_on_failure() {
        let mut s = super::super::tests::source();
        s.command = "exit 1".into();
        s.cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let (st, res) = poll(&s).await;
        assert!(res.is_err());
        assert_eq!(st.failures, 1);
        assert!(st.next_at > now_ms());
    }
}
