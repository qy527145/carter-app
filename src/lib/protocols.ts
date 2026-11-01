// 线协议与 kind 别名。
//
// 真正不同的**线协议只有三种**：`anthropic` / `openai_chat` / `openai_responses`。
// 协议下拉只列这三个 —— 服务商身份由「预设」按钮加 `base_url` 表达（见
// `templates.rs`），不再靠 `kind` 的取值区分。
//
// `KIND_ALIASES` 是 carter `config/mod.rs` 里 `kind_preset` 那张别名表的镜像：
// 老配置里可能写着 `kind = "deepseek"`，carter 会把它映射到 `openai_chat` 并补上
// 默认地址。这些别名**不再出现在下拉里**，只用来认回旧配置（显示、placeholder、
// 目录回退），否则用户会在编辑器里看到一个空的选择框。
//
// 为什么这张表可以镜像、而服务端工具那张不行：这里只是给下拉框提供候选项与
// placeholder，选错了也只是填错一个字符串，用户能看见也能改。而「某个协议认不认
// 某个服务端工具」是**会静默失效**的判断（不认就直接丢掉），那种必须问后端
// （`checkServerTools`）。

export type ProtocolPreset = {
  /** 写进配置的 `kind`。三种线协议各有一个同名 kind。 */
  kind: string;
  label: string;
  /** 映射到的线协议。与 kind 不同时说明走了别名。 */
  protocol: string;
  /** base_url 留空时 carter 用的默认地址（只用于 placeholder）。 */
  defaultBaseUrl: string;
  /** 常见服务端工具名，供 chip 快选。 */
  serverTools: string[];
  /**
   * models.dev 里对应的服务商 id（`meta` 的前半截）。有值说明这是个官方渠道：
   * 模型可以直接从目录里选，不用手写 meta。通用协议（openai_chat）为空。
   */
  metaProvider: string;
};

/** 三种线协议。协议下拉就列这三个，服务商由「预设」按钮负责。 */
export const PROTOCOLS: ProtocolPreset[] = [
  {
    kind: "anthropic",
    label: "Anthropic Messages",
    protocol: "anthropic",
    defaultBaseUrl: "https://api.anthropic.com",
    serverTools: ["web_search", "web_fetch", "code_execution", "computer_use"],
    metaProvider: "anthropic",
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
    metaProvider: "openai",
  },
  {
    kind: "openai_chat",
    label: "OpenAI Chat Completions",
    protocol: "openai_chat",
    // 兼容端点各家路径不同，没有可以默认的地址。
    defaultBaseUrl: "",
    serverTools: [],
    metaProvider: "",
  },
];

/**
 * carter 还认得的 `kind` 别名（`kind_preset` 那张表的镜像）。
 *
 * 只用于**认回旧配置**：老配置里 `kind = "deepseek"` 时编辑器要能显示「DeepSeek」、
 * 给出默认地址、并从对应目录预填模型。新建渠道时不会产生这些值 —— 模板写的都是
 * 三种线协议之一。
 */
export const KIND_ALIASES: ProtocolPreset[] = [
  {
    kind: "deepseek",
    label: "DeepSeek",
    protocol: "openai_chat",
    defaultBaseUrl: "https://api.deepseek.com",
    serverTools: [],
    metaProvider: "deepseek",
  },
  {
    kind: "openrouter",
    label: "OpenRouter",
    protocol: "openai_chat",
    defaultBaseUrl: "https://openrouter.ai/api/v1",
    serverTools: [],
    metaProvider: "openrouter",
  },
  {
    kind: "gemini",
    label: "Google Gemini（OpenAI 兼容端点）",
    protocol: "openai_chat",
    defaultBaseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    serverTools: [],
    metaProvider: "google",
  },
  {
    kind: "groq",
    label: "Groq",
    protocol: "openai_chat",
    defaultBaseUrl: "https://api.groq.com/openai/v1",
    serverTools: [],
    metaProvider: "groq",
  },
  {
    kind: "xai",
    label: "xAI / Grok",
    protocol: "openai_chat",
    defaultBaseUrl: "https://api.x.ai/v1",
    serverTools: [],
    metaProvider: "xai",
  },
  {
    kind: "ollama",
    label: "Ollama（本地）",
    protocol: "openai_chat",
    defaultBaseUrl: "http://localhost:11434/v1",
    serverTools: [],
    metaProvider: "",
  },
];

export function presetFor(kind: string): ProtocolPreset | undefined {
  return (
    PROTOCOLS.find((p) => p.kind === kind) ??
    KIND_ALIASES.find((p) => p.kind === kind)
  );
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
