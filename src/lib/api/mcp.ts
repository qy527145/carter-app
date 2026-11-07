import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

export type McpServerRow = {
  name: string;
  transport: string;
  /** stdio 给 command，http 给 url。 */
  target: string;
  enabled: boolean;
  args: string[];
  url: string;
  command: string;
  env: [string, string][];
  headers: [string, string][];
};

export type McpDraft = {
  name: string;
  enabled: boolean;
  /** `stdio` | `http`。carter 那边是必填，界面要挡住空值。 */
  transport: string;
  command: string | null;
  args: string[];
  env: [string, string][];
  url: string | null;
  headers: [string, string][];
};

export function listMcpServers(): Promise<McpServerRow[]> {
  return invoke<McpServerRow[]>("list_mcp_servers");
}

export function saveMcpServer(draft: McpDraft): Promise<void> {
  return invoke<void>("save_mcp_server", { draft });
}

export function deleteMcpServer(name: string): Promise<void> {
  return invoke<void>("delete_mcp_server", { name });
}
