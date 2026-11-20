use crate::prelude::*;

// ---------------------------------------------------------------------------
// 渠道 / 模型 / 协议
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct ChannelRow {
    name: String,
    kind: String,
    /// 实际走哪种线协议（kind 可能是别名或兼容写法）。
    protocol: String,
    base_url: String,
    has_api_key: bool,
    /// 会被忽略的服务端工具 —— 该协议没有对应物或名字不认识。
    unsupported_server_tools: Vec<String>,
    /// 该协议**支持**的服务端工具（用来在界面上把支持与不支持并列展示）。
    supported_server_tools: Vec<String>,
    models: Vec<String>,
    is_default: bool,
}

#[tauri::command]
pub(crate) fn list_channels(shell: State<'_, Arc<AppShell>>) -> Vec<ChannelRow> {
    let config = shell.config.load();
    let mut rows: Vec<ChannelRow> = config
        .providers
        .iter()
        .map(|(name, p)| {
            // 不绕 `resolve_provider` —— 那条路要先解析出模型，而模型是从
            // models.dev 缓存里查的；这里只是想看渠道配置，不该依赖它。
            let unsupported = carter_app_core::unsupported_server_tools(p);
            // 「支持哪些」= 声明里没被丢掉的那些。
            let supported: Vec<String> = p
                .server_tools
                .iter()
                .filter(|t| !unsupported.contains(t))
                .cloned()
                .collect();
            let mut models: Vec<String> = p.models.keys().cloned().collect();
            models.sort();
            ChannelRow {
                name: name.clone(),
                kind: p.kind.clone(),
                protocol: p.protocol().to_string(),
                base_url: p.base_url.clone().unwrap_or_default(),
                has_api_key: p.api_key.is_some(),
                unsupported_server_tools: unsupported,
                supported_server_tools: supported,
                models,
                // 默认渠道 = 配置里 `agent.model` 指向的那个 provider。
                is_default: config.agent.model.starts_with(&format!("{name}/")),
            }
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// 编辑弹窗用的**原始**渠道配置（`${ENV}` 保持字面量，不回填明文密钥）。
#[tauri::command]
pub(crate) fn get_provider_detail(name: String) -> Result<ProviderDetail, String> {
    config_edit::provider_detail(&name)
}

/// 某个协议能吃下哪些服务端工具。返回 `(生效的, 不生效的)`。
///
/// 编辑弹窗勾选时就要把不生效的标出来，而不是等保存完再让渠道列表报警。
#[tauri::command]
pub(crate) fn check_server_tools(kind: String, tools: Vec<String>) -> (Vec<String>, Vec<String>) {
    carter_app_core::check_server_tools(&kind, &tools)
}

/// 写一份渠道草稿并热重载。返回写回后的详情。
///
/// **必须是 `async fn`。** 热重载会顺手在后台补模型目录（`tokio::spawn`），而同步
/// 命令跑在主线程上、没有 reactor —— 见 [`config_edit::apply`] 的说明。
#[tauri::command]
pub(crate) async fn save_provider(
    shell: State<'_, Arc<AppShell>>,
    draft: ProviderDraft,
) -> Result<ProviderDetail, String> {
    config_edit::save_provider_reload(shell.inner(), &draft)
}

#[tauri::command]
pub(crate) async fn delete_provider(
    shell: State<'_, Arc<AppShell>>,
    name: String,
) -> Result<(), String> {
    config_edit::remove_provider_reload(shell.inner(), &name)
}

#[tauri::command]
pub(crate) async fn save_model(
    shell: State<'_, Arc<AppShell>>,
    provider: String,
    model: carter_app_core::ModelDraft,
) -> Result<ProviderDetail, String> {
    let mut detail = config_edit::provider_detail(&provider)?;
    // 按名字 upsert，其余模型原样带走。
    match detail.models.iter_mut().find(|m| m.name == model.name) {
        Some(slot) => {
            slot.meta = model.meta.clone();
            slot.api_name = model.api_name.clone();
            // 草稿里是字符串；详情里存的是原样的 TOML 值。
            slot.default_reasoning = model
                .default_reasoning
                .clone()
                .map(serde_json::Value::String);
        }
        None => detail
            .models
            .push(carter_app_core::config_edit::ModelDetail {
                name: model.name.clone(),
                meta: model.meta.clone(),
                api_name: model.api_name.clone(),
                default_reasoning: model
                    .default_reasoning
                    .clone()
                    .map(serde_json::Value::String),
            }),
    }
    save_provider_from_detail(shell.inner(), &detail)
}

#[tauri::command]
pub(crate) async fn delete_model(
    shell: State<'_, Arc<AppShell>>,
    provider: String,
    model: String,
) -> Result<ProviderDetail, String> {
    let mut detail = config_edit::provider_detail(&provider)?;
    let before = detail.models.len();
    detail.models.retain(|m| m.name != model);
    if detail.models.len() == before {
        return Err(format!("渠道 `{provider}` 下没有模型 `{model}`"));
    }
    save_provider_from_detail(shell.inner(), &detail)
}

/// 把一份已读出的详情再写回去。
///
/// 注意 `api_key` 传 `None` —— 详情里那个字段是**原始文本**（可能含 `${VAR}`），
/// 但用户没改它就不该重写。`None` 的语义正是「不动」（见 config_edit 的三态约定）。
pub(crate) fn save_provider_from_detail(
    shell: &Arc<AppShell>,
    detail: &ProviderDetail,
) -> Result<ProviderDetail, String> {
    let draft = ProviderDraft {
        name: detail.name.clone(),
        kind: detail.kind.clone(),
        api_key: None,
        base_url: Some(detail.base_url.clone()),
        server_tools: detail.server_tools.clone(),
        server_tool_max_uses: detail.server_tool_max_uses,
        models: detail
            .models
            .iter()
            .map(|m| carter_app_core::ModelDraft {
                name: m.name.clone(),
                meta: m.meta.clone(),
                api_name: m.api_name.clone(),
                // 表形态（`{ budget = 1024 }`）在这里会落成 None = 不动那个键。
                default_reasoning: config_edit::reasoning_to_draft(&m.default_reasoning),
            })
            .collect(),
        rename_from: None,
    };
    config_edit::save_provider_reload(shell, &draft)
}

/// 全部模型引用 + 从 models.dev 解出来的元数据。单行解析失败只标在该行上。
///
/// `async` + `spawn_blocking`：每个模型都要重新解析一遍 models.dev 缓存，整张表
/// 要好几秒，而界面一启动就会调它。同步命令跑在主线程上，这几秒窗口完全没有响应。
#[tauri::command]
pub(crate) async fn list_models(
    shell: State<'_, Arc<AppShell>>,
) -> Result<Vec<carter_app_core::ModelRow>, String> {
    let config = shell.config.load_full();
    tokio::task::spawn_blocking(move || catalog::list(&config))
        .await
        .map_err(|e| format!("读取模型列表中断：{e}"))
}

/// 设主模型（`agent.model`）。
///
/// **必须是 `async fn`**：热重载里有 `tokio::spawn`（模型目录补全），同步命令所在的
/// 主线程没有 reactor，会 panic；release 是 `panic = "abort"`，用户看到的就是闪退。
#[tauri::command]
pub(crate) async fn set_default_model(
    shell: State<'_, Arc<AppShell>>,
    model_ref: String,
) -> Result<(), String> {
    config_edit::set_default_model_reload(shell.inner(), &model_ref)
}

/// 设副模型（`agent.fast_model`）：辅助场景默认指向的那个，见 `scene_models`。`None` = 清除，
/// 回落主模型 —— 所以界面上「清除副模型」和「设为副模型」共用一个命令。
///
/// **必须是 `async fn`**，理由同 [`set_default_model`]。
#[tauri::command]
pub(crate) async fn set_fast_model(
    shell: State<'_, Arc<AppShell>>,
    model_ref: Option<String>,
) -> Result<(), String> {
    config_edit::set_fast_model_reload(shell.inner(), model_ref.as_deref())
}

/// 场景模型总览：每个场景的绑定、回落链、此刻实际会用的模型。
#[tauri::command]
pub(crate) fn scene_models(
    shell: State<'_, Arc<AppShell>>,
) -> carter_app_core::scene_models::SceneModelsView {
    carter_app_core::scene_models::view(shell.inner())
}

/// 改一个场景的绑定。`None` = 清掉，走回落链。回传新的应用设置（前端 store 要对齐）。
///
/// **必须是 `async fn`**：改「普通对话」会写主模型并热重载，理由同 [`set_default_model`]。
#[tauri::command]
pub(crate) async fn set_scene_model(
    shell: State<'_, Arc<AppShell>>,
    scene: String,
    model_ref: Option<String>,
) -> Result<carter_app_core::AppSettings, String> {
    carter_app_core::scene_models::set_scene(shell.inner(), &scene, model_ref.as_deref())
}

/// 单次请求的最大输出 tokens（`agent.max_output_tokens`）。
///
/// `None` = 配置里没写，不上报这个字段、走渠道默认。界面显示它时需要说清楚
/// 「这是本地配的值，不是模型本身的输出上限」—— 后者见模型目录那份元数据。
#[tauri::command]
pub(crate) fn get_max_output_tokens(shell: State<'_, Arc<AppShell>>) -> Option<u32> {
    shell.config.load().agent.max_output_tokens
}

/// **必须是 `async fn`**：热重载里有 `tokio::spawn`，理由同 [`set_default_model`]。
#[tauri::command]
pub(crate) async fn set_max_output_tokens(
    shell: State<'_, Arc<AppShell>>,
    tokens: u32,
) -> Result<(), String> {
    config_edit::set_max_output_tokens_reload(shell.inner(), tokens)
}

/// 前台 bash 命令的超时策略（`agent.bash_timeout_secs` / `agent.bash_on_timeout`）。
#[derive(serde::Serialize)]
pub(crate) struct BashTimeoutSettings {
    secs: u64,
    on_timeout: carter::config::OnTimeout,
}

#[tauri::command]
pub(crate) fn get_bash_timeout(shell: State<'_, Arc<AppShell>>) -> BashTimeoutSettings {
    let cfg = shell.config.load();
    BashTimeoutSettings {
        secs: cfg.agent.bash_timeout_secs,
        on_timeout: cfg.agent.bash_on_timeout,
    }
}

/// **必须是 `async fn`**：热重载里有 `tokio::spawn`，理由同 [`set_default_model`]。
#[tauri::command]
pub(crate) async fn set_bash_timeout(
    shell: State<'_, Arc<AppShell>>,
    secs: u64,
    on_timeout: carter::config::OnTimeout,
) -> Result<(), String> {
    config_edit::set_bash_timeout_reload(shell.inner(), secs, on_timeout)
}

/// 自动压缩的当前设置（`agent.compact_enabled` / `agent.compact_threshold_ratio`）。
#[derive(serde::Serialize)]
pub(crate) struct CompactionSettings {
    enabled: bool,
    ratio: f64,
}

#[tauri::command]
pub(crate) fn get_compaction(shell: State<'_, Arc<AppShell>>) -> CompactionSettings {
    let cfg = shell.config.load();
    CompactionSettings {
        enabled: cfg.agent.compact_enabled,
        ratio: cfg.agent.compact_threshold_ratio,
    }
}

#[tauri::command]
pub(crate) async fn set_compaction(
    shell: State<'_, Arc<AppShell>>,
    enabled: bool,
    ratio: f64,
) -> Result<(), String> {
    config_edit::set_compaction_reload(shell.inner(), enabled, ratio)
}
