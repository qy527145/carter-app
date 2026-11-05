//! 静默更新：后台检查 → 经加速站下载 → 验签 → 等用户点「重启更新」。
//!
//! ## 为什么不用插件的 `check()`
//!
//! `tauri-plugin-updater` 的 `check()` 会按 `endpoints` 逐个请求，只要有一份
//! 清单解析成功就停 —— 但 **版本不比当前新时它也算「成功」**（返回 `None`），
//! 后面的 endpoint 就不会再试。把加速站排在官方后面，官方那份被 CDN 缓存成
//! 旧的，就会一直看不到新版本。所以这里自己按 [`carter_app_core::net`] 的候选源
//! 挨个拿 `latest.json`，自己比版本，插件只用来**验签**与**装包**。
//!
//! ## 生命周期
//!
//! `Idle → Checking → Downloading{进度} → Ready{版本, 包} →（用户点击）安装并重启`。
//! 任何一步失败都记在 [`Status`] 里，界面能看到「哪个源、为什么」；
//! 「已就绪」的包连同清单一起记在 `~/.carter-app/updates/`，重启应用后不用重下。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::Emitter;

use carter_app_core::net;

/// 发布产物清单，每个版本一份，放在 Release 里。
///
/// 客户端拿到之后会把这条**整串**接到各个加速站前缀后面去取（见
/// `carter_app_core::net::accelerate`），所以这里必须是完整的 GitHub 地址。
pub const MANIFEST_URL: &str =
    "https://github.com/qy527145/carter-app/releases/latest/download/latest.json";

/// 启动后多久检查第一次、之后每隔多久检查。
const FIRST_CHECK: Duration = Duration::from_secs(30);

const EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// 一个更新包最多等多久（几十 MB，走加速站也就一两分钟）。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// `latest.json` 的形状（Tauri updater 静态清单）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub platforms: std::collections::HashMap<String, PlatformEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlatformEntry {
    pub signature: String,
    pub url: String,
}

/// 界面看到的更新状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Status {
    /// 没在做事。`latest` 是上一次检查知道的最高版本（没有新版本时也给界面看）。
    Idle {
        checked_at: Option<u64>,
        error: Option<String>,
    },
    Checking,
    Downloading {
        version: String,
        downloaded: u64,
        total: Option<u64>,
    },
    /// 包已下好、验签通过，等用户点「重启更新」。
    Ready {
        version: String,
        notes: Option<String>,
        downloaded: u64,
    },
    Installing {
        version: String,
    },
    Failed {
        version: Option<String>,
        error: String,
    },
}

/// 下载完成后落在磁盘上、重启后仍然可用的那份记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cached {
    version: String,
    notes: Option<String>,
    package: PathBuf,
    signature: String,
    /// `latest.json` 原文：`Update::restore_update` 用它重建「要装什么」。
    raw_json: serde_json::Value,
    size: u64,
}

/// 进程内共享的更新状态。
pub struct Updater {
    status: Mutex<Status>,
    cached: Mutex<Option<Cached>>,
    running: AtomicBool,
}

impl Updater {
    fn dir() -> PathBuf {
        carter_app_core::paths::updates_dir()
    }

    fn cache_file() -> PathBuf {
        Self::dir().join("ready.json")
    }

    /// 启动时调一次：读回上次「已就绪」的那份（如果包还在、签名记录还在）。
    pub fn load() -> Arc<Self> {
        let cached: Option<Cached> = std::fs::read_to_string(Self::cache_file())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .filter(|c: &Cached| c.package.is_file());
        let status = match &cached {
            Some(c) => Status::Ready {
                version: c.version.clone(),
                notes: c.notes.clone(),
                downloaded: c.size,
            },
            None => Status::Idle {
                checked_at: None,
                error: None,
            },
        };
        Arc::new(Self {
            status: Mutex::new(status),
            cached: Mutex::new(cached),
            running: AtomicBool::new(false),
        })
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .map(|s| s.clone())
            .unwrap_or(Status::Idle {
                checked_at: None,
                error: None,
            })
    }

    fn set(&self, app: &tauri::AppHandle, next: Status) {
        tracing::debug!(?next, "更新状态");
        if let Ok(mut s) = self.status.lock() {
            *s = next.clone();
        }
        if let Ok(status) = serde_json::to_value(&next) {
            let _ = app.emit(
                "carter://event",
                &carter_app_core::AppEvent::UpdateChanged { status },
            );
        }
    }

    fn prefs() -> net::NetPrefs {
        carter_app_core::app_settings::AppSettings::load().net_prefs()
    }

    /// 自动检查是否开着（设置 → 外观 → 更新）。手动检查不受它管。
    fn enabled() -> bool {
        carter_app_core::app_settings::AppSettings::load().auto_update
    }

    /// 后台检查 + 下载。已经在跑就直接返回。
    pub fn spawn_check(self: &Arc<Self>, app: tauri::AppHandle) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = me.check_and_download(&app).await;
            me.running.store(false, Ordering::SeqCst);
            match result {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "检查更新没做成");
                    let version = match me.status() {
                        Status::Downloading { version, .. } => Some(version),
                        _ => None,
                    };
                    me.set(&app, Status::Failed { version, error: e });
                }
            }
        });
    }

    /// 启动后的定时检查。设置里关掉自动更新就什么都不做（手动检查另有入口）。
    pub fn spawn_scheduler(self: &Arc<Self>, app: tauri::AppHandle) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            // 本机验证更新链路时用 `CARTER_DEBUG_FIRST_CHECK_SECS=5` 把第一次检查提前
            // （`config_edit` 保存渠道也会触发补全，想快速验证时用那个入口更直接）。
            let first = std::env::var("CARTER_DEBUG_FIRST_CHECK_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(FIRST_CHECK);
            if cfg!(debug_assertions) {
                tokio::time::sleep(first).await;
            } else {
                tokio::time::sleep(FIRST_CHECK).await;
            }
            loop {
                me.spawn_check(app.clone());
                tokio::time::sleep(EVERY).await;
            }
        });
    }

    async fn check_and_download(&self, app: &tauri::AppHandle) -> Result<(), String> {
        // 设置里关掉了就只剩手动检查：定时那一轮不动已有状态、不联网。
        if !Self::enabled() {
            tracing::debug!("自动更新已关闭，跳过检查");
            return Ok(());
        }
        self.check_and_download_forced(app).await
    }

    /// 不看开关的检查 + 下载（手动入口与自动入口共用）。
    async fn check_and_download_forced(&self, app: &tauri::AppHandle) -> Result<(), String> {
        let current = app.package_info().version.to_string();
        let prefs = Self::prefs();
        self.set(app, Status::Checking);

        // 1) 清单：挨个源拿，自己比版本 —— 理由见模块头。
        let fetched =
            net::get_first(&prefs, &[MANIFEST_URL], net::SOURCE_TIMEOUT, None, None).await?;
        let manifest: Manifest = serde_json::from_slice(&fetched.bytes).map_err(|e| {
            format!(
                "{} 返回的不是更新清单：{e}",
                net::source_label(&fetched.source)
            )
        })?;
        let target = tauri_plugin_updater::target().ok_or("这个平台不支持自更新")?;
        let newer = newer_than(&manifest.version, &current);
        tracing::info!(
            source = %fetched.source,
            latest = %manifest.version,
            current = %current,
            newer,
            "检查更新"
        );
        let Some(entry) = manifest.platforms.get(&target) else {
            // 发布里没有这个平台的包（比如 Linux 只用 deb）。不当成错误，只是没得更新。
            self.set(
                app,
                Status::Idle {
                    checked_at: Some(carter_app_core::protocol::now_ms()),
                    error: Some(format!("最新版本没有 {target} 的安装包")),
                },
            );
            return Ok(());
        };
        if !newer {
            // 已经是最新：把之前「就绪」但版本相同的记录清掉（重装过的机器）。
            self.set(
                app,
                Status::Idle {
                    checked_at: Some(carter_app_core::protocol::now_ms()),
                    error: None,
                },
            );
            return Ok(());
        }
        let version = manifest.version.trim_start_matches('v').to_string();

        // 2) 下载：同样挨个源。先报一个大致的进度，再按实际字节更新。
        self.set(
            app,
            Status::Downloading {
                version: version.clone(),
                downloaded: 0,
                total: None,
            },
        );
        let cancel = AtomicBool::new(false);
        let progress_max = Mutex::new(0u64);
        let app_for_progress = app.clone();
        let version_for_progress = version.clone();
        let me = self;
        let progress = move |done: u64, total: Option<u64>| {
            // 每 256 KB 才刷一次界面，否则一个几十 MB 的包会发上千条事件。
            let mut last = match progress_max.lock() {
                Ok(l) => l,
                Err(_) => return,
            };
            if done < *last + 256 * 1024 && Some(done) != total {
                return;
            }
            *last = done;
            me.set(
                &app_for_progress,
                Status::Downloading {
                    version: version_for_progress.clone(),
                    downloaded: done,
                    total,
                },
            );
        };
        let pkg = net::get_first(
            &prefs,
            &[entry.url.as_str()],
            DOWNLOAD_TIMEOUT,
            Some(&cancel),
            Some(&progress),
        )
        .await?;
        tracing::info!(
            source = %pkg.source,
            bytes = pkg.bytes.len(),
            failed = pkg.failed.len(),
            "更新包下载完成"
        );

        // 3) 验签：包内容 + 签名里绑定的版本号，两道都要。
        carter_app_core::net::verify_update(
            net::UPDATE_PUBKEY,
            &pkg.bytes,
            &entry.signature,
            &version,
        )
        .map_err(|e| format!("{e}（来自 {}）", net::source_label(&pkg.source)))?;

        // 4) 落盘 + 记下「就绪」，重启后还能直接用。
        let dir = Self::dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("建更新目录失败：{e}"))?;
        let package = dir.join(format!("carter-app-{version}-{target}"));
        let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut tmp, &pkg.bytes).map_err(|e| e.to_string())?;
        tmp.persist(&package)
            .map_err(|e| format!("保存更新包失败：{e}"))?;

        let cached = Cached {
            version: version.clone(),
            notes: manifest.notes.clone(),
            package,
            signature: entry.signature.clone(),
            raw_json: serde_json::to_value(&manifest).map_err(|e| e.to_string())?,
            size: pkg.bytes.len() as u64,
        };
        // 旧版本残留的包顺手清掉，别让 ~/.carter-app 越滚越大。
        if let Ok(raw) = std::fs::read_to_string(Self::cache_file())
            && let Some(old) = serde_json::from_str::<Cached>(&raw).ok()
            && old.version != version
        {
            let _ = std::fs::remove_file(&old.package);
        }
        std::fs::write(
            Self::cache_file(),
            serde_json::to_vec_pretty(&cached).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("记录更新状态失败：{e}"))?;
        if let Ok(mut c) = self.cached.lock() {
            *c = Some(cached.clone());
        }
        self.set(
            app,
            Status::Ready {
                version,
                notes: cached.notes,
                downloaded: cached.size,
            },
        );
        Ok(())
    }

    /// 用户点了「重启更新」。包在本地、签名在下载时已经验过，这里只让插件装。
    ///
    /// mac / Linux 装好之后调 `request_restart`（走 Tauri 官方的重启机制）；
    /// Windows 上插件拉起 NSIS 安装器后会自己退出进程，用户看到的是应用关闭再打开。
    pub async fn install(self: &Arc<Self>, app: tauri::AppHandle) -> Result<(), String> {
        let Some(cached) = self.cached.lock().ok().and_then(|c| c.clone()) else {
            return Err("没有已下载好的更新，先检查一次".into());
        };
        let bytes = std::fs::read(&cached.package)
            .map_err(|e| format!("读更新包失败（{}）：{e}", cached.package.display()))?;
        self.set(
            &app,
            Status::Installing {
                version: cached.version.clone(),
            },
        );

        use tauri_plugin_updater::UpdaterExt;
        let update = app
            .updater_builder()
            // 只给验签与装包用；网络请求这里一次都不发（清单与包都已经下好了）。
            .endpoints(vec![MANIFEST_URL.parse().map_err(|e| format!("{e}"))?])
            .map_err(|e| format!("{e}"))?
            .pubkey(net::UPDATE_PUBKEY)
            .build()
            .map_err(|e| format!("{e}"))?
            .restore_update(cached.raw_json.clone())
            .map_err(|e| format!("{e}"))?
            .ok_or("清单里的版本不比当前版本新")?;
        update
            .install(bytes)
            .map_err(|e| format!("安装失败：{e}"))?;

        // 装完把「就绪」记录清掉：万一重启失败，别让用户对着一个过期的按钮点。
        let _ = std::fs::remove_file(Self::cache_file());
        if let Ok(mut c) = self.cached.lock() {
            *c = None;
        }
        #[cfg(not(windows))]
        {
            app.request_restart();
        }
        Ok(())
    }

    /// 手动检查：绕过「自动更新」开关，但同样在后台跑。
    pub fn manual_check(self: &Arc<Self>, app: tauri::AppHandle) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = me.check_and_download_forced(&app).await;
            me.running.store(false, Ordering::SeqCst);
            if let Err(e) = result {
                tracing::warn!(error = %e, "检查更新没做成");
                let version = match me.status() {
                    Status::Downloading { version, .. } => Some(version),
                    _ => None,
                };
                me.set(&app, Status::Failed { version, error: e });
            }
        });
    }
}

/// `latest` 比 `current` 新吗？两边都容忍前导 `v`。
///
/// 预发布后缀按 semver 的规矩比较：`0.2.0-beta.1` < `0.2.0`，两个都有后缀就比
/// 后缀。不认的形态（`nightly`）一律当成「不是更新」—— 宁可漏一次，也别把
/// 一份看不懂的清单当成新版本推给用户。
fn newer_than(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<((u64, u64, u64), Option<String>)> {
        let core = v.trim().trim_start_matches('v');
        let (nums, pre) = match core.split_once('-') {
            Some((n, p)) => (n, Some(p.split('+').next().unwrap_or(p).to_string())),
            None => (core.split('+').next().unwrap_or(core), None),
        };
        let mut it = nums.split('.').map(|p| p.parse::<u64>().ok());
        Some(((it.next()??, it.next()??, it.next()??), pre))
    }
    match (parse(latest), parse(current)) {
        (Some((a, pre_a)), Some((b, pre_b))) => match (pre_a, pre_b) {
            (None, None) => a > b,
            // 正式版比同号的预发布版新。
            (None, Some(_)) => a >= b,
            (Some(_), None) => false,
            (Some(x), Some(y)) => (a, x) > (b, y),
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(newer_than("0.2.0", "0.1.9"));
        assert!(newer_than("v0.2.0", "0.1.0"));
        assert!(newer_than("0.2.0", "v0.2.0-beta.1"));
        assert!(newer_than("0.2.0-beta.2", "0.2.0-beta.1"));
        assert!(!newer_than("0.2.0-beta.1", "0.2.0"), "预发布不比正式版新");
        assert!(!newer_than("0.1.0", "0.2.0-beta.1"));
        assert!(!newer_than("0.1.0", "0.1.0"));
        assert!(!newer_than("v0.1.0", "0.2.0"));
        assert!(!newer_than("nightly", "0.1.0"), "看不懂的版本不当成新版本");
    }
}
