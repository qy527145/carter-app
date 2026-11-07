use crate::prelude::*;

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(crate) struct McpServerRow {
    name: String,
    transport: String,
    /// stdio 给 command，http 给 url。
    target: String,
    enabled: bool,
    args: Vec<String>,
    url: String,
    command: String,
    env: Vec<(String, String)>,
    headers: Vec<(String, String)>,
}

#[tauri::command]
pub(crate) fn list_mcp_servers(shell: State<'_, Arc<AppShell>>) -> Vec<McpServerRow> {
    let config = shell.config.load();
    let mut rows: Vec<McpServerRow> = config
        .mcp
        .servers
        .iter()
        .map(|(name, s)| McpServerRow {
            name: name.clone(),
            transport: s.transport.clone(),
            target: s
                .command
                .clone()
                .or_else(|| s.url.clone())
                .unwrap_or_default(),
            enabled: s.enabled,
            args: s.args.clone(),
            url: s.url.clone().unwrap_or_default(),
            command: s.command.clone().unwrap_or_default(),
            env: sorted_pairs(&s.env),
            headers: sorted_pairs(&s.headers),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// HashMap 的遍历顺序不稳定，界面里得是稳定的。
pub(crate) fn sorted_pairs(m: &std::collections::HashMap<String, String>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = m.iter().map(|(k, val)| (k.clone(), val.clone())).collect();
    v.sort();
    v
}

#[tauri::command]
pub(crate) async fn save_mcp_server(
    shell: State<'_, Arc<AppShell>>,
    draft: McpDraft,
) -> Result<(), String> {
    config_edit::save_mcp_server_reload(shell.inner(), &draft)
}

#[tauri::command]
pub(crate) async fn delete_mcp_server(
    shell: State<'_, Arc<AppShell>>,
    name: String,
) -> Result<(), String> {
    config_edit::remove_mcp_server_reload(shell.inner(), &name)
}
