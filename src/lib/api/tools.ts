import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// 工具
// ---------------------------------------------------------------------------

export type ToolRow = {
  name: string;
  description: string;
  /** 参数 JSON Schema。`task` 那一行是 null —— 它构造需要 provider，列不出来。 */
  parameters: unknown;
  enabled: boolean;
  /** `false` = 界面不提供开关。 */
  toggleable: boolean;
  note: string | null;
};

/**
 * 本地会执行的工具。
 *
 * 厂商在服务端执行的（web_search 等）不在这里 —— 它们是渠道能力，见
 * `listChannels`。MCP 工具也不在这里：名字要连上 server 才知道，见
 * `listMcpServers`。
 */
export function listTools(cwd?: string | null): Promise<ToolRow[]> {
  return invoke<ToolRow[]>("list_tools", { cwd: cwd ?? null });
}
