// 渠道预设。
//
// 这是 carter `config/mod.rs` 里 `kind_preset` 那张别名表的**界面镜像**：
// `kind` 是配置里写的字符串，carter 据此映射到一种线协议与一个默认 base_url。
//
// 为什么这张表可以镜像、而服务端工具那张不行：这里只是给下拉框提供候选项与
// placeholder，选错了也只是填错一个字符串，用户能看见也能改。而「某个协议认不认
// 某个服务端工具」是**会静默失效**的判断（不认就直接丢掉），那种必须问后端
// （`checkServerTools`）。

export type ProtocolPreset = {
  /** 写进配置的 `kind`。 */
  kind: string;
  label: string;
  /** 映射到的线协议。与 kind 不同时说明走了别名。 */
  protocol: string;
  /** base_url 留空时 carter 用的默认地址（只用于 placeholder）。 */
  defaultBaseUrl: string;
  /** 常见服务端工具名，供 chip 快选。 */
  serverTools: string[];
};

export const PROTOCOLS: ProtocolPreset[] = [
  {
    kind: "anthropic",
    label: "Anthropic Messages",
    protocol: "anthropic",
    defaultBaseUrl: "https://api.anthropic.com",
    serverTools: ["web_search", "web_fetch", "code_execution", "computer_use"],
  },
  {
    kind: "openai_responses",
    label: "OpenAI Responses",
    protocol: "openai_responses",
    defaultBaseUrl: "https://api.openai.com",
    serverTools: [
      "web_search",
      "web_search_preview",
      "code_interpreter",
      "file_search",
    ],
  },
  {
    kind: "openai_chat",
    label: "OpenAI Chat Completions",
    protocol: "openai_chat",
    defaultBaseUrl: "",
    serverTools: [],
  },
  {
    kind: "deepseek",
    label: "DeepSeek（Chat Completions 兼容）",
    protocol: "openai_chat",
    defaultBaseUrl: "https://api.deepseek.com",
    serverTools: [],
  },
  {
    kind: "openrouter",
    label: "OpenRouter（Chat Completions 兼容）",
    protocol: "openai_chat",
    defaultBaseUrl: "https://openrouter.ai/api/v1",
    serverTools: [],
  },
  {
    kind: "gemini",
    label: "Google Gemini（OpenAI 兼容端点）",
    protocol: "openai_chat",
    defaultBaseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    serverTools: [],
  },
  {
    kind: "groq",
    label: "Groq（Chat Completions 兼容）",
    protocol: "openai_chat",
    defaultBaseUrl: "https://api.groq.com/openai/v1",
    serverTools: [],
  },
  {
    kind: "xai",
    label: "xAI / Grok（Chat Completions 兼容）",
    protocol: "openai_chat",
    defaultBaseUrl: "https://api.x.ai/v1",
    serverTools: [],
  },
  {
    kind: "ollama",
    label: "Ollama（本地）",
    protocol: "openai_chat",
    defaultBaseUrl: "http://localhost:11434/v1",
    serverTools: [],
  },
];

export function presetFor(kind: string): ProtocolPreset | undefined {
  return PROTOCOLS.find((p) => p.kind === kind);
}

/** 用户填的 kind 不认识时的兜底说明。 */
export const UNKNOWN_PROTOCOL_HINT =
  "carter 不认识这个 Kind，会按 OpenAI Chat Completions 处理，并且必须自己填 Base URL。";

/**
 * `default_reasoning` 在配置里可以是裸字符串（`"medium"`），也可以是
 * `{ budget = 1024 }` 这种表（carter 的 `ReasoningEffort::Budget(n)`）。
 * 后端把**原值**给过来，下面两个函数负责区分。
 */
export function reasoningText(v: unknown): string {
  return typeof v === "string" ? v : "";
}

/** 是不是界面表达不了的自定义取值 —— 那种只展示、不改写。 */
export function reasoningIsCustom(v: unknown): boolean {
  return v != null && typeof v !== "string";
}

export function reasoningDisplay(v: unknown): string {
  if (v == null) return "（未指定）";
  if (typeof v === "string") return v;
  return JSON.stringify(v);
}

/** 推理档位。carter 的 `ReasoningEffort` 认这几个字符串。 */
export const REASONING_EFFORTS = [
  { value: "", label: "（不指定）" },
  { value: "none", label: "none" },
  { value: "low", label: "low" },
  { value: "medium", label: "medium" },
  { value: "high", label: "high" },
  { value: "xhigh", label: "xhigh" },
  { value: "max", label: "max" },
];
