//! 用一份**已经解析好的**模型元数据装配 provider。
//!
//! carter 的 `provider::resolve_provider` 把「查元数据」和「建 provider」两步写在了
//! 一起，而查询那一步要求模型必须在 models.dev 目录里。目录里没有的模型（本地
//! Ollama 之类）在 [`crate::catalog::resolve_provider`] 里会被补上一份最小元数据，
//! 然后走这里建 provider。
//!
//! 这里刻意**不是**上一份 `resolve_provider` 的复制：查表的部分整个没有，剩下的
//! 只有「按渠道配置建线协议客户端 + 挂钩子」，那部分与 carter 保持逐行一致
//! （见 `carter::provider::resolve`），升级底座时只需要再看一眼这一处。

use std::sync::Arc;

use carter::config::Config;
use carter::error::Result;
use carter::provider::resolve::ResolvedProvider;
use carter::provider::wire::{ServerToolConfig, WireProvider};
use carter::registry::ModelInfo;

/// 按模型的 `provider` 查 `[providers.*]`，建出可直接发请求的 provider。
pub fn build(
    config: &Config,
    model: ModelInfo,
    extra_hooks: Vec<Arc<dyn llm_adapt_core::RequestHook>>,
) -> Result<ResolvedProvider> {
    let pcfg = config.provider_for(&model.provider);
    let protocol = pcfg.protocol();
    let client_config = pcfg.to_client_config()?;

    let hook: Option<Arc<dyn llm_adapt_core::RequestHook>> = config.debug.log_requests.then(|| {
        let dir = config
            .debug
            .llm_log_dir
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(carter::config::paths::llm_log_dir);
        Arc::new(carter::provider::llm_log::LlmLogHook::new(dir))
            as Arc<dyn llm_adapt_core::RequestHook>
    });

    let server_tools = ServerToolConfig {
        names: pcfg.server_tools.clone(),
        raw_specs: pcfg.server_tool_specs.clone(),
        max_uses: pcfg.server_tool_max_uses,
    };

    let provider = WireProvider::new(client_config, protocol, server_tools, hook, extra_hooks)?;
    let unsupported = provider.unsupported_server_tools().to_vec();
    Ok(ResolvedProvider {
        model,
        provider: Arc::new(provider),
        unsupported_server_tools: unsupported,
    })
}
