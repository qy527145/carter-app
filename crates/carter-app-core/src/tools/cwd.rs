//! 把文件类工具的**相对路径**按会话的工作目录解析。
//!
//! ## 问题
//!
//! carter 的文件工具直接 `Path::new(&args["path"])`，没有任何 cwd 解析 ——
//! 相对路径落到**进程的** cwd 上。命令行下这没问题（进程 cwd 就是用户所在目录），
//! 桌面应用下必然错：进程 cwd 取决于从哪启动（资源管理器是 `C:\Windows\System32`，
//! IDE 是项目目录），而会话有自己的工作目录。
//!
//! 更糟的是它**不报错**。系统提示里写着「工作目录：C:\Users\xuqiao」，模型据此用
//! 相对路径，文件却读写到了别处 —— 用户看到的是 `read_file ../.carter/facts.md`
//! 这种和当前工作目录对不上的路径，而工具还以为成功了。
//!
//! ## 做法
//!
//! 在工具外面套一层，执行前把参数里的 `path` 改写成绝对路径：
//!
//! * 相对路径 → 拼上会话 cwd；
//! * 绝对路径 → 原样；
//! * **没有** `path` → 补上 cwd。`glob` / `grep` 的文档写着「缺省当前目录」，
//!   而那个「当前目录」在 GUI 里是进程 cwd，不是会话 cwd。
//!
//! 顺带展开开头的 `~` —— 模型很爱写 `~/.carter/facts.md`，而 Windows 的
//! `std::fs` 不认 `~`，不展开就会得到一个「文件不存在」，原因看不出来。
//!
//! 只改这一个字段就够：`glob` 的 `pattern`、`grep` 的 `glob` 都是模式而不是路径，
//! 它们会在 `path` 之下解析。改写整个参数对象是危险的 —— `edit_file` 的
//! `old_string` 里出现一个 `path` 键就会被误伤。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use carter::tools::{Tool, ToolResult};

/// 需要按会话 cwd 解析路径的工具。
///
/// 用白名单而不是「凡是有 path 参数的都改」：改动参数是有副作用的，
/// 只有这几个工具的 `path` 语义我们确定。
const SCOPED: &[&str] = &["read_file", "write_file", "edit_file", "glob", "grep"];

pub struct CwdScoped {
    inner: Arc<dyn Tool>,
    cwd: PathBuf,
}

impl CwdScoped {
    pub fn new(inner: Arc<dyn Tool>, cwd: PathBuf) -> Self {
        Self { inner, cwd }
    }

    /// 这个工具要不要按 cwd 解析路径。
    pub fn applies_to(name: &str) -> bool {
        SCOPED.contains(&name)
    }
}

/// 把一个路径参数解析成绝对路径。
pub fn resolve(cwd: &Path, raw: &str) -> PathBuf {
    let expanded = expand_tilde(raw);
    let p = Path::new(&expanded);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        // 用 join 而不是字符串拼接：它会处理 cwd 末尾有没有分隔符。
        let joined = cwd.join(p);
        // `join` 只在一处补分隔符，相对部分里的 `/` 会原样带进来 ——
        // 于是 `C:\work` + `src/main.rs` 得到 `C:\work\src/main.rs`。
        // Windows 认这种混排，但显示在卡片上很难看，统一一下。
        #[cfg(windows)]
        {
            PathBuf::from(joined.to_string_lossy().replace('/', "\\"))
        }
        #[cfg(not(windows))]
        {
            joined
        }
    }
}

/// 展开开头的 `~`。
///
/// 实现在 [`crate::paths`] —— 技能页加扫描路径时也要用同一份展开规则，
/// 抄第二份就是两份会漂的代码（尤其 Windows 上分隔符那段）。
fn expand_tilde(raw: &str) -> String {
    crate::paths::expand_tilde(raw)
}

#[async_trait::async_trait]
impl Tool for CwdScoped {
    // 元信息原样透传 —— `name` 一变，并发调度与子 agent 的工具白名单就跟着变。
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> Value {
        self.inner.parameters()
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let patched = match args {
            Value::Object(mut m) => {
                match m.get("path").and_then(Value::as_str) {
                    Some(p) if !p.trim().is_empty() => {
                        let abs = resolve(&self.cwd, p);
                        m.insert("path".into(), Value::String(abs.to_string_lossy().into_owned()));
                    }
                    // `None` = 没给 path。glob / grep 的「缺省当前目录」指的是进程的
                    // 当前目录，在 GUI 里那是错的地方 —— 补上会话的 cwd。
                    None => {
                        m.insert(
                            "path".into(),
                            Value::String(self.cwd.to_string_lossy().into_owned()),
                        );
                    }
                    // 空串按「没给」处理。
                    Some(_) => {
                        m.insert(
                            "path".into(),
                            Value::String(self.cwd.to_string_lossy().into_owned()),
                        );
                    }
                }
                Value::Object(m)
            }
            other => other,
        };
        self.inner.execute(patched).await
    }
}

/// 挡住 `save_memory` 的**项目级**作用域。
///
/// `save_memory` 没有 `path` 参数可以改写 —— 目标路径是它内部按 `scope` 算出来的
/// （`carter/tools/save_memory.rs::target_for`）：`kind=facts, scope=project` 直接
/// `PathBuf::from("CARTER.md")`，又是一个相对路径。命令行下那等于项目根目录，
/// 桌面应用下等于**进程**的 cwd（从资源管理器启动就是 `C:\Windows\System32`）。
///
/// 而且 `scope` 的**默认值就是 `project`** —— 模型只要省略这个参数就会踩中。
///
/// 这里选择**明确拒绝**而不是放过。写错地方的记忆比没有记忆更坏：用户以为记住了，
/// 下次却找不到。错误信息里直接把替代方案给出来（`scope=global`，或者用 `write_file`
/// 写 `<cwd>/CARTER.md` —— 后者是可行的，因为 `write_file` 已经被 [`CwdScoped`]
/// 按会话 cwd 解析了）。
pub struct ProjectMemoryGuard {
    inner: Arc<dyn Tool>,
}

impl ProjectMemoryGuard {
    pub fn new(inner: Arc<dyn Tool>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl Tool for ProjectMemoryGuard {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> Value {
        self.inner.parameters()
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("facts");
        // 默认值就是 "project" —— 省略参数也会走到这里。
        let scope = args.get("scope").and_then(Value::as_str).unwrap_or("project");

        if kind == "facts" && scope == "project" {
            return ToolResult::err(
                "桌面应用里 `scope = \"project\"` 不可用：项目记忆要写到工作目录下的 \
                 CARTER.md，而工具运行时的进程工作目录与会话的工作目录不是一回事 —— \
                 写进去多半会落在一个你完全没预期的位置。\
                 改用 `scope = \"global\"`（写进 ~/.carter/facts.md），\
                 或者直接用 write_file 写 `CARTER.md`（它已经按会话的工作目录解析）。",
            );
        }
        self.inner.execute(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 回显收到的参数 —— 这样测的是「改写对不对」，不掺进真实文件操作。
    struct Echo;
    #[async_trait::async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "read_file"
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> Value {
            json!({})
        }
        async fn execute(&self, args: Value) -> ToolResult {
            ToolResult::ok(args.to_string())
        }
    }

    fn scoped(cwd: &str) -> CwdScoped {
        CwdScoped::new(Arc::new(Echo), PathBuf::from(cwd))
    }

    /// 取回 echo 工具收到的参数表。
    ///
    /// 断言**解析后的值**而不是字符串：JSON 会把反斜杠转义成 `\`，
    /// 拿路径字面量去 contains 会在 Windows 上永远失败（而且失败信息很难看懂）。
    async fn args_of(cwd: &str, given: Value) -> serde_json::Map<String, Value> {
        let out = scoped(cwd).execute(given).await;
        serde_json::from_str::<Value>(&out.content)
            .expect("echo 应当回一段 JSON")
            .as_object()
            .expect("应当是个对象")
            .clone()
    }

    /// 相对路径必须落到会话的 cwd 下 —— 这是整件事的起因。
    #[tokio::test]
    async fn a_relative_path_resolves_against_the_session_cwd() {
        let cwd = if cfg!(windows) { r"C:\work\proj" } else { "/work/proj" };
        let a = args_of(cwd, json!({"path": "src/main.rs"})).await;
        let got = Path::new(a["path"].as_str().unwrap());
        assert_eq!(got, Path::new(cwd).join("src").join("main.rs"));
    }

    /// 绝对路径不能被改写 —— 模型显式指定的位置就是它要的位置。
    #[tokio::test]
    async fn an_absolute_path_is_left_alone() {
        let other = if cfg!(windows) { r"D:\otherile.txt" } else { "/other/file.txt" };
        let cwd = if cfg!(windows) { r"C:\work\proj" } else { "/work/proj" };
        let a = args_of(cwd, json!({"path": other})).await;
        assert_eq!(Path::new(a["path"].as_str().unwrap()), Path::new(other));
    }

    /// `glob` / `grep` 不给 path 时，carter 的「缺省当前目录」是**进程**的当前目录。
    /// 必须补上会话的 cwd，否则搜索范围完全不对。
    #[tokio::test]
    async fn a_missing_path_gets_the_cwd_injected() {
        let cwd = if cfg!(windows) { r"C:\work\proj" } else { "/work/proj" };
        let a = args_of(cwd, json!({"pattern": "**/*.rs"})).await;
        assert_eq!(Path::new(a["path"].as_str().unwrap()), Path::new(cwd));
        // 模式本身不能被改写。
        assert_eq!(a["pattern"], json!("**/*.rs"));
    }

    /// 其他字段一个都不能动 —— 改写整个参数对象是危险的，
    /// `edit_file` 的 old_string 里出现一个 `path` 键就会被误伤。
    #[tokio::test]
    async fn other_arguments_are_untouched() {
        let a = args_of(
            "/work",
            json!({
                "path": "a.txt",
                "old_string": "path: b.txt",
                "new_string": "path: c.txt",
                "replace_all": true
            }),
        )
        .await;
        assert_eq!(a["old_string"], json!("path: b.txt"));
        assert_eq!(a["new_string"], json!("path: c.txt"));
        assert_eq!(a["replace_all"], json!(true));
    }

    /// 模型很爱写 `~/...`，而 `std::fs` 不认 `~`。
    #[test]
    fn a_tilde_is_expanded() {
        let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok();
        if let Some(home) = home {
            let p = resolve(Path::new("/elsewhere"), "~/.carter/facts.md");
            assert_eq!(
                p,
                Path::new(home.trim_end_matches(['/', '\\']))
                    .join(".carter")
                    .join("facts.md"),
                "`~` 应当展开成家目录"
            );
        }
        // 不认识的形式原样留着，别自作主张。
        assert!(resolve(Path::new("/w"), "~bob/x")
            .to_string_lossy()
            .contains("~bob"));
    }

    #[test]
    fn only_the_known_file_tools_are_scoped() {
        for n in ["read_file", "write_file", "edit_file", "glob", "grep"] {
            assert!(CwdScoped::applies_to(n), "{n} 应当按 cwd 解析");
        }
        for n in ["bash", "todo_write", "task", "save_memory", "ask_user_question"] {
            assert!(!CwdScoped::applies_to(n), "{n} 不该被改写参数");
        }
    }

    // -----------------------------------------------------------------------
    // save_memory 的项目级作用域
    // -----------------------------------------------------------------------

    struct MemoryEcho;
    #[async_trait::async_trait]
    impl Tool for MemoryEcho {
        fn name(&self) -> &str {
            "save_memory"
        }
        fn description(&self) -> &str {
            ""
        }
        fn parameters(&self) -> Value {
            json!({})
        }
        async fn execute(&self, _: Value) -> ToolResult {
            ToolResult::ok("要是走到了这里，说明真的写盘了")
        }
    }

    /// **省略 `scope` 也会被挡住** —— carter 的默认值就是 `project`，
    /// 模型只要不写这个参数就会踩中「写到了进程 cwd」。
    #[tokio::test]
    async fn project_scope_is_refused_even_when_scope_is_omitted() {
        let t = ProjectMemoryGuard::new(Arc::new(MemoryEcho));
        let res = t.execute(json!({"content": "记住这件事"})).await;
        assert!(!res.ok, "项目级作用域不该放过去：{}", res.content);
        assert!(res.content.contains("scope"), "错误信息要给出替代方案：{}", res.content);
        assert!(res.content.contains("global"), "{}", res.content);
    }

    /// 全局作用域不受影响 —— 它写的是 `~/.carter/facts.md`，绝对路径，本来就没问题。
    #[tokio::test]
    async fn global_scope_passes_through() {
        let t = ProjectMemoryGuard::new(Arc::new(MemoryEcho));
        let res = t.execute(json!({"content": "x", "scope": "global"})).await;
        assert!(res.ok, "{}", res.content);
    }

    /// profile / skill 的目标路径本来就是绝对的，不该被误伤。
    #[tokio::test]
    async fn other_kinds_are_not_touched() {
        let t = ProjectMemoryGuard::new(Arc::new(MemoryEcho));
        for kind in ["profile", "skill"] {
            let res = t.execute(json!({"content": "x", "kind": kind})).await;
            assert!(res.ok, "{kind} 不该被挡：{}", res.content);
        }
    }
}
