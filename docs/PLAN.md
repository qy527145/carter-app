# carter-app 实施方案

> 目标：Tauri + React 桌面 agent 客户端。底层复用 carter 的 harness，
> wire 层收敛到 **llm_adapt**（升级为通用 LLM API 库），
> 原生支持 OpenAI Chat Completions / OpenAI Responses / Anthropic Messages 三种线协议，
> 支持模型原生远程工具，思考/工具/多模态统一抽象，
> 并能在对话中直查产生该内容的那次 API 请求原始报文。

---

## 一、现状结论

### 1.1 llm_adapt —— 通用库的底座

`../llm_adapt`，Rust workspace，`llm_adapt_core`(3520 行) + `llm_adapt_cli`(1072 行)，Phase 1 已完成。

**已有的、也是选择它当底座的理由：**

| 能力 | 位置 | 说明 |
|---|---|---|
| 对象安全的 handler 三分 trait | `handler/mod.rs:88-101` | `RequestHandler`(IR→HttpRequest) / `NonStreamResponseHandler`(bytes→IR) / `StreamResponseHandler`(bytes→chunks)，全部 `Arc<dyn>` |
| 协议注册表 | `handler/registry.rs` | `HandlerRegistry` 按 `api_format` 字符串索引，`RwLock`，可运行时注册自定义协议 |
| **prompt cache 断点写入** | `handlers/anthropic.rs:140,293` | `cache_control_value()` / `attach_cache()`，测试 `cache_marker_5m_emits_default_ephemeral`、`cache_marker_1h_emits_ttl` 守着 |
| **TTL 分档缓存核算** | `types/usage.rs:56` | `CacheWriteUsage{total, ephemeral_5m, ephemeral_1h}`，比 apilot 的单值细 |
| **多模态工具结果** | `types/tool_result.rs:31` | `ToolResultContent::Blocks(Vec<ToolResultBlock>)`，含 `Image`；Anthropic 原生、OpenAI 降级为文本并告警 |
| thinking 签名一等公民 | `types/conversation.rs:233`、`types/stream.rs:22` | `Thinking{text,signature}` + `ThinkingDelta{signature_delta}` |
| 角色感知编译期检查 | `types/conversation.rs` | `UserBlock` 只接受 text/image/tool-result，`AssistantBlock` 只接受 text/thinking/tool-call |
| **原始报文预览** | `client.rs:64`、`commands/preview.rs` | `preview()` 不发送、只渲染 `HttpRequest`；CLI 支持 `--format curl\|json` 且密钥脱敏 |
| 传输层 | `http/` | retry(指数退避) / proxy(HTTP+SOCKS5) / 三类超时 / `RequestHook` / `verify_ssl` |
| 错误分类 | `error.rs` | `LLMError` 11 个稳定变体 + `ErrorContext{request_id, http_status, retryable, vendor_code}` |
| 模型能力表 | `capability.rs` | `ModelCapabilities{context_window, max_output_tokens, supports_*}`，可变更表 |

**缺口（对照五条需求）：**

| # | 缺口 | 影响 | 参考来源 |
|---|---|---|---|
| 1 | 缺 OpenAI Responses handler（只有 `openai_compat`/`anthropic_v2`） | 需求 1 只满足 2/3 | apilot `oai_responses/`（2211 行） |
| 2 | 闭合枚举，无 `Unmodeled{raw}` 兜底 | 需求 2 的拦路虎；测试 `assistant_thinking_dropped_silently` 已体现静默丢数据 | apilot `ContentBlock::Unmodeled` + `extra: IndexMap` |
| 3 | `ToolDefinition` 只有函数形态（`types/tool.rs:12`） | 需求 2 无处声明 `web_search` | codex `ToolSpec = Function\|Namespace\|ToolSearch\|WebSearch\|Freeform` |
| 4 | 流式无原始帧捕获（`execute_stream` 不触发 hook，`http/mod.rs:110`） | 需求 4 只有非流式一半 | apilot 的双向 SSE 捕获 |
| 5 | `StreamChunk` 无块边界（无 index / BlockStart / BlockStop） | 同条消息内 text 与 tool_use 交错时无法保真装配 | apilot `UnifiedDelta` |
| 6 | 两份重复 SSE 解析（`find_event_boundary` 在 `openai.rs:508` 与 `anthropic.rs:505` 各一份） | 加第三个协议会变三份 | — |
| 7 | 只有出站方向，无 `decode_request`/`encode_response` | apilot 无法迁入 | apilot `Codec` 六方法 |
| 8 | `api_format: String` 无类型；`capability.rs` 无定价、无 models.dev 同步 | 元数据散落 | apilot `Protocol` 显式命名纪律 + carter `registry/`、`cost.rs` |
| 9 | `Turn` 严格 `User\|Assistant` 交替 | Responses 的 item 列表（reasoning / function_call / function_call_output 并列且顺序敏感）表达不了 | — |

### 1.2 carter —— harness 层

`../carter`，单 crate 62 个 .rs，约 6.3k 行非终端代码，220 个测试。

**提供**：`run_turn()` 多轮循环（`src/agent/turn.rs:67`，并发/串行工具分批、cancel、checkpoint、todo 复诵）、`Tool` trait + `ToolRegistry`（`src/tools/mod.rs:52,63`，8 内置工具）、子 agent（`agent/subagent.rs`）、技能（`skills/`，`SKILL.md` + frontmatter）、会话 JSONL（`session/`，resume/fork/list/gc）、上下文压缩（`agent/context.rs`）、MCP（`mcp/`，stdio + streamable-http）、生命周期钩子（`hooks/`）、模型注册表（`registry/`）、headless NDJSON 协议（`ndjson/` + `docs/ndjson.md`）。

**关键接缝**：`src/agent/ui.rs:62` 的 `UiEvent` / `:107` 的 `UiSink` trait——**该文件不依赖任何终端 crate**，是现成的 GUI 接缝；`ndjson/sink.rs:44-97` 已是一个把 `UiEvent` 桥到外部世界的完整范例。

**缺陷**：
1. 无 lib target（`main.rs:1-19` 全是私有 `mod`）
2. 消息模型太粗（`provider/mod.rs:33` 的 `Message` 是纯文本枚举，无 thinking 签名、无工具结果图片、无未建模块、无块顺序；图片靠 `[img:hash]` 文本 token 在 `genai_provider.rs:346` 临时展开）
3. 无 wire 掌控（底座 genai 的 `ChatRequest` 只有 6 个字段，无 `extra_body`；`llm_log.rs:1-9` 自述记录的是"归一化视图，非字节级 wire 捕获"）
4. 无权限系统；`init_tracing` 用全局 `init()`（已设 subscriber 会 panic）；`apply_env` 用 `unsafe set_var` 改整个进程环境

### 1.3 apilot —— wire 层的代码来源

`../apilot`，Tauri 2 + React 19。核心资产是 `src-tauri/src/protocol/`（7648 行）：

- `dto.rs`(575)：协议无关 IR。`Protocol`（三协议，**显式 `#[serde(rename)]`** 避开 `open_ai_chat` 陷阱）、`ContentBlock{Text,Image,ToolUse,ToolResult,Thinking{signature},RedactedThinking,Unmodeled{raw}}`、`UnifiedRequest`（顶层 `system` + `#[serde(flatten)] extra: IndexMap` 透传）、`UnifiedDelta{MessageStart,BlockStart,TextDelta,ThinkingDelta,ToolInputDelta,BlockStop,Usage,Finish,Error}`、`UnifiedUsage`（**`input_tokens` 不含缓存**的硬口径）、`FinishReason`
- `codec.rs`(324)：`Codec` 六方法 + `StreamDecoder`/`StreamEncoder` + `CodecRegistry`（同协议短路直通）
- `anthropic/`(935+622+250) `oai_chat/`(759+688+412) `oai_responses/`(1177+1034)：三协议双向编解码
- `inspect.rs`(475)：捕获字节 → IR，供 UI 三态渲染
- `strip_unportable_thinking`（`gateway/pipeline.rs:1766`）：跨协议时剥离签名不可保真的思考块
- `docs/PROTOCOL_MATRIX.md:170-284`：**转换损耗清单**，可直接当测试用例集

**已踩过的坑**（写进 llm_adapt 时避免）：`Protocol` 枚举别用 `rename_all`、`UnifiedDelta` 嵌套同 discriminant 会产生重复 `type` 键、空集合 `skip_serializing_if` 让前端崩、迁移版本戳漂移、API key 明文落库、reqwest 必须用 `rustls-tls-native-roots`（否则用户装的抓包 CA 不生效）。

### 1.4 codex / deepseek-harness —— 形态参考

- **codex `app-server`**：单二进制 `--listen stdio://`，typed method 枚举由一张方法表宏生成并自动导出 TS 类型；**server→client 反向请求**承载审批（`item/commandExecution/requestApproval`）；**item 生命周期通知** `item/started` → `*/delta` → `item/completed`。
- **codex `ToolSpec`**（`tools/src/tool_spec.rs:22`）：`Function | Namespace | ToolSearch | WebSearch | Freeform`——原生工具声明的形状参考。
- **deepseek-harness**：`SESSION_FORMAT_VERSION` + 相邻 `vN→vN+1` 迁移包（永不 rename 生成代）；provider-neutral canonical vocabulary + adapters。

### 1.5 三条硬约束

1. **想下发原生远程工具（需求 2），必须自己掌控 wire。**
2. **掌控了 wire，消息模型就必须是 IR**——否则 thinking 签名、服务端工具块、工具结果图片在回灌时全部丢失。
3. **1 和 2 是同一件事**，所以协议层与消息模型必须一起换，不能只换一半。

---

## 二、总体架构

### 2.1 llm_adapt 的五层结构

```
llm_adapt/crates/llm_adapt_core/src/
├─ http/            L1 传输：reqwest + retry + proxy + 超时 + hooks
│                   （已有；补 native-roots、per-request 覆盖、SSE 空闲超时、429 Retry-After）
├─ protocol/        L2 协议：三协议双向编解码（新，吸收 apilot protocol/）
│   ├─ ir.rs              ProtocolId / WireRequest / WireResponse / WireDelta / Block / ToolSpec / Usage
│   ├─ codec.rs           Codec 六方法 + StreamDecoder + StreamEncoder + ProtocolRegistry
│   ├─ anthropic/  oai_chat/  oai_responses/
│   └─ inspect.rs         捕获字节 → IR（供检视 UI）
├─ sse.rs           统一 SSE 帧解析（消掉两份 find_event_boundary）
├─ server_tools.rs  原生工具声明 + 跨协议降级策略表
├─ cache.rs         cache 断点标记 + 自动断点策略
├─ capture.rs       ExchangeRecord + 流式帧 tee
├─ catalog.rs       模型元数据（capability + models.dev 同步 + 别名解析 + 定价）
├─ pricing.rs       成本核算
├─ tokens.rs        tiktoken 真实计数 + 估算降级
├─ media.rs         内容寻址 blob（sha256 去重 + 1568px 降采样）+ data: URL 展开
├─ types/           L3 人机层：Conversation / UserBlock / AssistantBlock / ChatRequest / ChatResponse
│                   + lift/lower 与 IR 互转 + BlockAssembler
└─ client.rs        L4 门面：LLMClient（出站）+ ProtocolConverter（入站与 9 方向互转）
```

### 2.2 两层内容模型（D-A）

这是本次最核心的 API 形状决策。

- **扁平 IR 是唯一真源**（`protocol/ir.rs`）：`WireRequest{model, system, messages: Vec<WireMessage{role, content: Vec<Block>}>, tools, ..., extra: IndexMap}`。codec 只认 IR。`Block` 含 `Unmodeled{raw}` 兜底，顶层 `extra` 透传未建模参数。**保真、保序、可双向。**
- **角色感知层是上层便捷 API**（`types/conversation.rs`，大部分保留）：`Conversation` / `UserBlock` / `AssistantBlock` / `ToolResultContent`，编译期拒绝非法组合（`Thinking` 不能出现在 user 消息里等）。
- **两层无损互转**：`lift(&ChatRequest) -> WireRequest` 与 `lower(&WireResponse) -> ChatResponse`。方向性说明：`lift` 恒无损；`lower` 在遇到 IR 里角色感知层表达不了的东西（连续同角色、Responses 的并列 item）时进 `Unmodeled` 或降级为读视图，**文档中明确标注为 best-effort**。

客户端用户永远不必看见 IR；网关用户直接操作 IR。两边的公共 API 共享同一个 codec。

### 2.3 五个消费方与依赖方向

```
llm_adapt_cli ──┐
carter         ─┤
carter-app     ─┼──> llm_adapt_core
apilot(后置)    ─┘        ^
                          └── 无反向依赖
```

职责边界：

| 仓库 | 负责 | 不负责 |
|---|---|---|
| **llm_adapt** | 三协议双向编解码、IR、传输、原生工具声明、cache 断点、原始报文捕获、模型元数据/定价/成本、token 计数、媒体 blob | agent 循环、工具**执行**、会话、技能 |
| **carter** | agent 循环、工具**执行**与注册、权限、会话与压缩、技能、MCP、hooks、子 agent | 任何 wire 细节 |
| **carter-app** | UI、应用状态、捕获持久化（SQLite）、渠道配置 | wire、harness |
| **apilot**（后置） | 网关（HTTP server、路由、渠道、计费、客户端接管） | wire 细节（下沉给 llm_adapt） |

---

## 三、关键设计决策

| # | 决策 | 说明 |
|---|---|---|
| D1 | **llm_adapt 升级为通用 LLM API 库** | 已定。不是新建 llm-wire，而是把 wire 层做进 llm_adapt。 |
| D2 | **双向能力** | 已定。`Codec` 从第一天就定义六个方法（出站 encode_request/decode_response/new_stream_decoder + 入站 decode_request/encode_response/new_stream_encoder），入站方法默认返回 `Unsupported`，M8 逐协议补齐。apilot 未来可迁入。 |
| D3 | **两层内容模型** | 已定，见 2.2。 |
| D4 | **能力上提** | 已定。模型元数据 + 定价 + 成本核算、token 计数与估算、媒体 blob 与图片处理，全部从 carter 上提到 llm_adapt。 |
| D5 | **carter 就地 lib 化 + 消息模型 IR 化** | 加 `src/lib.rs`，`Message` 换成 llm_adapt 的 IR 类型；TUI 是另一个 `UiSink` 前端。 |
| D6 | **彻底移除 genai** | 连同 `genai_provider.rs` 与 `llm_log.rs`。非三协议渠道走 `openai_chat` + 自定义 base_url。 |
| D7 | **carter 的 NDJSON 协议升级为 App↔Core 的 IPC 契约** | 复用已有 `Command`/`Event` 设计，事件粒度升级到 codex 式 item 生命周期（`turn/started` → `item/started\|delta\|completed` → `turn/completed`）。走 Tauri IPC 而非 stdio。 |
| D8 | **数据分家** | `~/.carter/`（config/skills/sessions/models.json）与 CLI 共享；`~/.carter-app/app.db`（SQLite）只存 App 独有数据：原始报文捕获、渠道、UI 状态、会话索引。 |
| D9 | **不做代理层** | 客户端用 llm_adapt 的 `HttpExecutor` 直连上游。不内嵌 apilot 的网关、路由、计费。 |
| D10 | **首版不做工具审批与沙箱** | 但在 `turn.rs:302` 的 `execute_tool_calls` 留出拦截点（wrapper `Tool` 或 `PreToolUse` hook），后续接入不再动结构。 |
| D11 | **前端栈** | React 19 + Vite + TS + Tailwind v4 + shadcn/Radix + TanStack Query（apilot 全套）+ **zustand**（流式对话高频本地状态）+ react-markdown + shiki + @tanstack/react-virtual。 |
| D12 | **`Protocol` 命名纪律** | 内置协议用枚举 + 显式 `#[serde(rename)]`（`anthropic` / `openai_chat` / `openai_responses`），自定义协议用 `ProtocolId` newtype 包装 String。注册表以 `ProtocolId` 为键。 |

---

## 四、路线图

九个里程碑。M0–M1 造库，M2–M3 打通最小闭环（此时已可用），M4–M7 逐层加深，M8 可后置。

**进度**

| 里程碑 | 状态 | 备注 |
|---|---|---|
| M0 llm_adapt 地基重构 | ✅ 完成 2026-10-14 | 逐字节对比通过；见下方完成记录 |
| M1 补 OpenAI Responses | ✅ 完成 2026-10-14 | 三协议齐；见下方完成记录 |
| M2 carter 接入 | ✅ 完成 2026-10-14 | genai 移除；消息模型 IR 化；会话 v2 + 只读迁移；226 测试通过 |
| M3 carter-app 最小闭环 | ⬜ | |
| M4 原生远程工具 | ⬜ | |
| M5 原始报文检视 | ⬜ | |
| M6 能力上提 | ⬜ | |
| M7 harness 管理页 | ⬜ | |
| M8 双向能力 + apilot 迁入 | ⬜ 可后置 | |

### M0 — llm_adapt 地基重构

**目标**：引入 IR 与双向 `Codec`，把现有两个协议平移到新骨架上。**行为不变。**

改动：
1. 新建 `protocol/` 骨架：`ir.rs`（从 apilot `dto.rs` 移植 + 补 llm_adapt 的 `CacheMarker` 与 TTL 分档 usage）、`codec.rs`（`Codec` 六方法 trait + `StreamDecoder`/`StreamEncoder` + `ProtocolRegistry`）。
2. `handlers/openai.rs` 的编解码主体平移到 `protocol/oai_chat/`，`handlers/anthropic.rs` 平移到 `protocol/anthropic/`。**搬迁时不改语义。**
3. 抽出 `sse.rs`：统一 `SseEvent` 解析 + 复用 apilot 已验证的边界处理，删掉 `openai.rs:508` 与 `anthropic.rs:505` 两份 `find_event_boundary` 与两个手写 `Stream` impl。
4. `types/` 降为角色感知层，加 `lift`/`lower` 与 IR 互转；`LLMClient` 的 `chat`/`chat_stream`/`preview` 改为经过 IR。
5. `Codec` 的入站三方法先给默认实现返回 `LLMError::Unsupported`（M8 补齐）。

**验收**：`cargo test -p llm_adapt_core` 全绿（现有测试一条不改地过）；`llm-adapt preview` 与 `llm-adapt call` 的输出与重构前逐字节一致（加 golden 报文对拍测试）。

#### M0 完成记录（2026-10-14）

**结果**：49 个测试全绿（原 22 个断言全部保留，仅按新 API 调整调用形式；新增 27 个）。
用 `git worktree` 拉起重构前版本并排比对，**7 组 `preview` 输出逐字节一致**（含 openai 基础/带参数/带缓存标记、anthropic 基础/1h 缓存/带参数/curl 格式）。

**实际落地与计划的差异**（均为执行中的判断，记录备查）：

1. **没有引入独立的 `WireDelta` 流类型，改为把 `StreamChunk` 本身当作流式 IR。**
   计划里写了「`WireDelta` 加块边界」。实际做时发现：若解码器产出 `WireDelta`、再由 `lower()` 投影成 `StreamChunk`，这个 `lower` 必须逐条复刻旧实现的怪癖（例如 Anthropic 的 `content_block_start` 带空签名的思考块要产出「什么都没有」）才能保持逐字节一致——脆且难维护。改成单一 `StreamChunk` 之后，客户端与网关共用一套流式词汇，且少一层有损投影。块边界与 `index` 的补齐仍在 M1 做。
2. **协议名改用规范名 + 别名表。** 注册表键从 `anthropic_v2` / `openai_compat` 改为 `anthropic` / `openai_chat`（`openai_responses` 待 M1）。`ProtocolId::new()` 会把旧名归一，因此磁盘上已有的配置不用改就继续可用。只有 `handlers list` / `handlers capabilities` 的**显示**变了。
3. **`ToolResultContent` 三种形态整体移进 IR，不做归一。** 原计划把它压成 `Vec<Block>`。实测发现 Anthropic 的 `tool_result.content` 既接受裸字符串也接受块数组，压平会在 wire 上产生无谓差异、丢掉「上游给的是哪种形态」这条信息。移进 IR 后既无损又省一次改写。
4. **`FinishReason::Other` 从单元变体改为带 `value: String`。** 上游的 `pause_turn` 这类取值不再丢失。
5. **HTTP 层类型（`HttpMethod`/`HttpRequest`/`RawHttpResponse`/`ByteStream`）从 `handler/` 移到 `http/`**，`handler/` 与 `handlers/` 两个模块整体删除，由 `protocol/` 取代。
6. **`WireRequest.extra` / `WireResponse.extra` 采用「先铺 extra、再写建模字段」的顺序**，保证建模字段永远赢，避免一个笔误的 extra 键静默改掉请求语义。
7. **注释语言**：llm_adapt 原有注释是英文，本轮新增的 `sse.rs` / `protocol/` / `types/convert.rs` 用中文（与 carter、apilot 及本方案一致）。搁置一次性的全量统一，以免在这个已经很大的重构 diff 上再叠一层纯文本改动——**这是一个待你确认的选择**，若你希望全部统一成英文或中文，单独做一遍即可。

### M1 — 补 OpenAI Responses，三协议齐

**目标**：需求 1 达成。

改动：
1. 从 apilot 移植 `oai_responses/`（mod 1034 + stream 1177）到新 `Codec` 骨架。
2. 硬契约（`docs/PROTOCOL_MATRIX.md:128-166`）：Codex 只从 `output_item.done` 取工具调用；reasoning item 必须携带 `encrypted_content: null`。
3. IR 补保真要素：`Block::Unmodeled{raw}`、`WireRequest.extra: IndexMap` 顶层透传 —— **这两项已在 M0 完成**。M1 剩下的是给 `StreamChunk` 加块边界（`BlockStart{index, block}` / `BlockStop{index}` + 各 delta 带 `index`），并把两个已有协议的流式解码器改成产出带 index 的增量。
4. 加 `BlockAssembler`：把 `StreamChunk` 流装配成有序 `AssistantMessage`（同条消息内 text 与 tool_use 交错也保真）。这是 carter 的 turn 循环与 llm_adapt_cli 都要用的东西。

**验收**：三协议分别跑通带工具调用的流式对话；Responses 的 reasoning item 与 `function_call_output` 往返保真；构造一个含未知块类型的响应，断言它以 `Unmodeled` 存活而非被丢弃；同协议直通时字节不变。

#### M1 完成记录（2026-10-14）

**结果**：82 个测试全绿（M0 的 49 + 新增 33），零警告。`llm-adapt handlers list` 显示三个协议；`preview` 对同一段输入产出正确的三份报文：

| 协议 | URL | 差异 |
|---|---|---|
| `openai_chat` | `/v1/chat/completions` | system 落成 `messages[0]`；单段 user 文本压成裸字符串 |
| `anthropic` | `/v1/messages` | system 在顶层（无缓存标记时是裸字符串）；user content 恒为块数组 |
| `openai_responses` | `/v1/responses` | system 变 `instructions`；消息摊成扁平 `input` item 数组 |

**做了什么**：

1. **新增 `protocol/oai_responses/`**（`mod.rs` + `stream.rs`），把 apilot 的 Responses 实现移植到 llm_adapt 的 IR 与 `Codec` 骨架上。移植中修掉/补上了三处 apilot 的行为：
   - apilot 的解码遇到 `{"type":"web_search"}` 这类**没有 `name` 字段的服务端工具声明会整条丢掉**（`decode_tool` 里 `src.get("name")?` 直接返回 None）。llm_adapt 把它收成 `ToolKind::ServerTool{spec}` 原样下发。
   - apilot 解码响应时 `_ => {}` 静默丢弃未知 item 类型。llm_adapt 收成 `Block::Unmodeled`，服务端工具活动因此能被 UI 渲染。
   - apilot 编码请求时把所有 `function_call` 提到文本之前，丢掉了块间顺序。llm_adapt 按原顺序 flush。
2. **`StreamChunk` 升级为流式 IR**：新增 `MessageStart` / `BlockStart{index, block}` / `BlockStop{index}`，三个增量变体各带 `index`。Chat Completions 不标注块边界，由它的 mapper 按「首次出现」合成 index 并在 `finish_reason` 时补发 `BlockStop`。
3. **新增 `BlockAssembler`**：把增量流装配回有序 `Vec<Block>`，块间交错保真。未带 `BlockStart` 的增量会按类型自动建块（mapper 漏发不该让整条流报废）；后续分片带的合成占位 id 不会覆盖首片真实 id。
4. **`encrypted_content` ↔ `signature`**：Responses 的 reasoning item 带的不透明 `encrypted_content` 与 Anthropic 的 thinking `signature` 是同一概念（都必须原样回传），共用 `Block::Thinking::signature` 承载。
5. anthropic / oai_chat 两个 mapper 改为产出带 index 的增量；`message_start`（此前被忽略）现在产出 `MessageStart`。

**已知缺口（记录备查，非缺陷）**：

- **Responses 的思考内容默认不往返**。解码时若上游没给 `encrypted_content`，重新编码会跳过该 reasoning item —— 只有摘要、没有加密内容的 reasoning item 上游不接受。要让思考内容完整往返，调用方需在请求里带 `include: ["reasoning.encrypted_content"]`（通过 `extra` 透传即可，已有测试守着）。
  这里**没有**自动注入这个字段：codec 不该替调用方塞它没要求的请求参数。若 M2 里发现 agent 场景默认就该开，再在策略层加。
- **Responses 的结构化输出未建模**：`response_format` 非 `Text` 时明确报 `Unsupported`，而不是静默丢弃让调用方以为生效了。
- **`instructions` 的 system 分段用 `\n` 拼接**，而 anthropic / oai_chat 用空串拼接 —— 这是 M0「行为不变」约束留下的既有不一致。carter 的 system 是多段（人设 / 技能 / 记忆 / 环境），空串拼接会把它们粘成一坨，**必须在 M2 统一**。
- **三协议的流式对话只在单元测试里验证**（构造 SSE 字节序列断言装配结果），**没有对真实 API 跑过** —— 手上没有可用的 key。真实联调放在 M3（carter-app 最小闭环）里做。

### M2 — carter 接入

**目标**：carter 用上 llm_adapt，genai 退场，消息模型 IR 化。

改动：
1. `carter/src/lib.rs`（新增）：`main.rs:1-19` 的 19 个 `mod` 改 `pub mod`，`pub use error::{CarterError, Result}`；`main.rs` 瘦身为 thin bin。`Cargo.toml` 加 `[lib]` + 空 `[workspace]`（防止被 carter-app workspace 吸收）。
2. `init_tracing`（`main.rs:127`）改 `try_init()`；`apply_env`（`main.rs:157`）的 `unsafe set_var` 加显式开关；`build_tools`（`main.rs:509-550`）抽到 `agent/bootstrap.rs` 并 `pub`（与 `ndjson/runner.rs:190` 的重复实现合并）。
3. `WireProvider: LlmProvider`（carter 的 `provider/mod.rs:56` trait 已经是正确的接缝）桥接 llm_adapt 的 `LLMClient`。
4. **删除 genai**：移除依赖、`provider/genai_provider.rs`、`provider/llm_log.rs`。
5. `provider::Message` 换成 IR；`provider::Event` 扩展为 `BlockStart(ContentBlock)` / `ThinkingSignature` / `ToolInputDelta` / `BlockStop`。
6. `agent/turn.rs`：新增 `TurnAccumulator`，收尾时 append **一条** `WireMessage{role:Assistant, content:[Thinking{signature}, Text, ToolUse...]}`（取代现在的 `Assistant` + `ToolCalls` 两条，块顺序是 Anthropic 的硬要求）；工具结果用 `ContentBlock::ToolResult`，可含图片。
7. `session/`：`SessionMeta`（`session/mod.rs:52`）加 `format_version`；新写 v2；**v1→v2 只读迁移**（写新文件、原文件改名备份、缺省视为 v1）。
8. `media/` 改为转发 llm_adapt 的 `media.rs`。
9. config `[providers.*] kind` 收敛为三协议名，旧值别名映射（`openai`/`custom`/`openai_compat`→`openai_chat`；`gemini`/`ollama`/`groq`/`deepseek`/`xai`/`openrouter`→`openai_chat` + 各自默认 base_url 预设）。
10. **cache 自动断点**：最后一个 system 块、最后一个 tool 定义、最新一轮 user 消息。断点在**请求构造时**计算，不落进会话历史（否则断点位置一变历史就产生无谓 diff）。

**验收**：carter 220 个测试全绿 + TUI 手工冒烟；同一段带工具的对话在三协议下分别跑通；Anthropic 渠道下抓原始请求，断言 `cache_control` 出现在预期三处、且第二轮起 `cache_read_input_tokens > 0`；旧会话文件能正常打开。

#### M2 完成记录（2026-10-14）

**结果**：carter 226 个测试通过（原 220 + 新增 6），`cargo build` 零警告，CLI 冒烟（`--help` / `sessions`）正常。genai 依赖已从 `Cargo.toml` 移除。

**分四步落地**（每步一次提交）：
1. 拆出 lib 目标 + 修掉一个既有 bug（见下）。
2. `agent::bootstrap::build_tools` 提进库里，消除 NDJSON 的重复实现。
3. 协议层重建（属于 llm_adapt 那侧，见 M0/M1）。
4. provider 层接底座 + 消息模型 IR 化 + 会话 v2 迁移。

**顺带修掉的两个真 bug**：

- **hook 在 Windows 上被静默丢弃。** runner 原先在 Windows 一律用 `cmd /C`，而 hook 的文档与全部测试都按 POSIX 写，那些写法在 cmd 下报「不是内部命令」→ 退出码非 0 → 被当成 Continue。用户以为 hook 生效了，其实一次都没跑。现在优先探测 POSIX `sh`（Git for Windows / MSYS / WSL 都带），确实没有才退回 `cmd /C`。这个 bug 有多隐蔽：**未改动的 HEAD 上就是 214 通过 6 失败**，其中 `exit_zero_continues` 还是以错误理由通过的（命令找不到 → 退出码 1 → Continue，恰好等于断言期望）。修复后测试套件第一次在 Windows 上全绿。
- **llm_adapt 的 IR 无法序列化任何带工具结果的请求。** `ToolResultContent` 用了内部标签 + newtype 变体（`#[serde(tag="kind")]` + `Text(String)`），serde 不支持这种组合，会在运行时报 `cannot serialize tagged newtype variant`。编译期完全看不出来。已改成结构体变体，并补了「整请求 + 整响应往返回合」与「StreamChunk 全部 12 个变体往返回合」两条测试守它 —— 这类 bug 只有真去序列化才会炸。

**执行中的判断与偏差**：

1. **carter 的 `provider::Event` 直接换成底座的 `StreamChunk`**，没有另设中间层。底座的 `StreamChunk` 已经带块序号与块边界，正是 agent loop 需要的形状。
2. **工具结果合成一条 user 消息**（而不是每条结果一条）。这是 Anthropic 的规范形态（一条消息里放多个 `tool_result` 块），OpenAI Chat 需要拆成多条 `role:"tool"`，由它的 codec 在编码时拆 —— IR 保持更可表达的那一侧。
3. **图片仍是「引用 token 存在历史里 + 发送前展开」**，不是把 base64 存进历史。一张图几百 KB，直接进会话 JSONL 会把文件撑爆。展开放在 `provider/wire.rs`（媒体库属于 carter，底座不该认识它）。
4. **`Config::provider_for()` 保留「不写配置也能跑」**：`[providers.*]` 缺失时按名字合成一份，key 从 `<NAME>_API_KEY` 取。旧底座就是这个行为，突然要求人人写配置是没必要的破坏。但**无法辨认的 kind 且没配 base_url 时报错而不是猜** —— 猜错会把请求连同 API key 发到没打算用的域名上。
5. **`cargo fmt --all` 跑了一遍**，`tui/mod.rs`、`wizard.rs` 等文件的 diff 因此偏大（纯格式）。

**已知注意事项**：
- **默认并发跑测试会偶发卡住**（Windows 上大量并发 spawn 子进程）。用 `cargo test --lib -- --test-threads=4` 稳定通过。这不是 M2 引入的，但会让本地全量跑看起来像挂了。
- **三协议仍未对真实 API 联调过**（手上没有可用的 key）。这是 M3 的事。
- Anthropic 渠道下 `ReasoningEffort::Low/Medium/High` 现在映射到 `effort`，而 Anthropic 认的是 `budget_tokens`，llm_adapt 的 anthropic codec 会给一个默认预算（1024）。旧底座（genai）的映射行为未逐条对齐 —— 若实际效果有差异，在 M3 联调时调。`ReasoningEffort::Budget(n)` 则直接映射到预算。

### M3 — carter-app 最小闭环

**目标**：GUI 能完整跑一次对话。**到这里已经可用。**

改动：
1. 仓库改为 cargo workspace：`crates/carter-app-core` + `src-tauri` + `src`（React）。`carter` 与 `llm_adapt` 用 path 依赖。
2. `TauriSink: UiSink`：把 `UiEvent` 映射为 `app.emit("carter://...", payload)`；**delta 批量聚合（50ms / 20 条）**，生命周期事件绝不节流（apilot 的教训：丢一个 end 事件会留下永久 ghost）。
3. `SessionRunner`：每会话一个 tokio task，持有 `mpsc::Receiver<Command>` + carter 的 `CancelToken`；`send_prompt` / `cancel` 命令投递。
4. `carter-app-core` 的 `AppShell`：单一 `Arc` 状态根（参考 apilot `shell.rs:31-64`），`ArcSwap` 放热重载配置，`DashMap` 放注册表；**绝不跨 await 持锁**。
5. 前端：消息列表 + 流式文本 + thinking 折叠 + 工具调用卡片 + usage 底栏；markdown 渲染 + 代码高亮 + 虚拟列表。

**验收**：GUI 完成一次带工具调用的多轮对话；点取消立刻断流；重启后会话可续。

### M4 — 原生远程工具

**目标**：需求 2。

改动：
1. `protocol/ir.rs`：`ToolSpec::{Function{input_schema}, ServerTool(ServerToolKind), Namespace, Freeform}`（吸收 codex 的 `ToolSpec` 形状）。
2. `server_tools.rs`：声明与降级策略表——Anthropic `{"type":"web_search_20250305","name":"web_search","max_uses":N}`（同理 `web_fetch`/`code_execution`/`computer_use`）；Responses `{"type":"web_search"}`（含 `web_search_preview`/`code_interpreter`/`file_search`）；**Chat 无对应物 → 明确报不支持，而不是静默降级成看起来能用的本地工具**。
3. `agent/turn.rs`：收集 tool calls 时**跳过** `Block::Unmodeled`——服务端工具的活动（`server_tool_use`、`web_search_tool_result`、Responses 的 `web_search_call` item）本来就落在 `Unmodeled` 里，不能当本地工具去 dispatch；这些块作为 assistant 内容原样回灌。
4. UI：服务端工具块渲染成引用卡片（搜索词 + 命中 URL），区别于普通 tool_call。

**验收**：Anthropic 与 OpenAI 渠道下模型能自主发起 web_search，UI 显示引用来源；切到 Chat 渠道给明确能力缺失提示。

### M5 — 原始报文检视

**目标**：需求 4。

改动：
1. `capture.rs`：`ExchangeRecord{id, meta, protocol, request{url,method,headers,body}, response{status,headers,raw_sse_frames,body}, timings{ttfb,total}}`；`Authorization`/`x-api-key` 落库前脱敏。**补上 `execute_stream` 的捕获缺口**：用一个 tee 包装 `ByteStream`，逐帧投递。
2. `WireRequest.meta: RequestMeta{session_id, turn, round, purpose: MainTurn|Compaction|Subagent|Title}`——**这是"某条消息 ↔ 哪次请求"的关节**，没有它只能靠轮次猜。
3. `carter-app-core`：SQLite（sqlx + WAL，迁移数组按 `PRAGMA user_version`）；表 `exchanges` + `exchange_events`（SSE 帧，便于时间轴）。
4. Tauri 命令 `query_exchanges(filter)` / `get_exchange_detail(id)`。
5. 前端：消息气泡与工具卡片挂"原始报文"入口 → 三态视图（可视化 IR / 格式化 JSON / 原始字节含 SSE 帧）+ 流式时间轴。移植 apilot 的 `InspectViews.tsx` 与 `StreamTimeline.tsx`。

**验收**：点开任一条 assistant 消息，看到产生它的请求体（含完整 messages/system/tools）、响应 SSE 逐帧、TTFB 与总耗时；压缩触发的额外请求能被单独识别（靠 `purpose`）。

### M6 — 能力上提（carter → llm_adapt）

改动：
1. `catalog.rs`：合并 llm_adapt 的 `capability.rs` 与 carter 的 `registry/`（models.dev 拉取 + 别名解析）+ 定价表。
2. `pricing.rs`：carter `cost.rs` 上提。
3. `tokens.rs`：carter `tokens.rs` 上提（tiktoken 真实计数 + 估算降级）。
4. `media.rs`：carter `media/` 上提（sha256 内容寻址 + 1568px 降采样 + 磁盘存取）+ codec 层展开成 `data:` URL。
5. carter 这四处改为转发，删掉本地实现。

**验收**：llm_adapt 单独就能算出一次调用花了多少钱；carter 的 `Cargo.toml` 少了 `tiktoken-rs`/`image`/`sha2`/`base64`。

### M7 — harness 管理页

需求 5 的 UI 落点：

| 页面 | 数据来源 | 要点 |
|---|---|---|
| 渠道管理 | 客户端 SQLite | endpoint + 声明协议 + 模型映射 + `auth_style` + `extra_headers` + 连通性测试。**API key 用系统凭据库**（keyring），不重蹈 apilot 明文落库 |
| 模型注册表 | `catalog.rs` | models.dev 刷新、别名解析、定价、context window、能力开关 |
| 技能管理 | `carter::skills::discover` | 列表/编辑/启用/新建，展示 frontmatter 与 `allowed-tools` |
| MCP 管理 | `carter::mcp::McpManager` | server 增删改、连接状态、工具列表与调用测试 |
| 会话管理 | `~/.carter/projects/*/*.jsonl` + SQLite 索引 | 列表/续聊/fork/重命名/全文搜索/导出 |
| 工具面板 | `ToolRegistry` | 8 个内置工具开关 + 自定义工具注册（`ToolRegistry::push`，表单编辑 JSON Schema） |

### M8 — 双向能力 + apilot 迁入（可后置）

1. 逐协议补齐入站三方法：`decode_request`（wire→IR）、`encode_response`（IR→wire）、`new_stream_encoder`（IR deltas→SSE）。
2. `ProtocolConverter`：`decode(A) → IR → encode(B)` 覆盖 9 个方向；同协议短路直通（字节不变）。
3. 移植 `strip_unportable_thinking`。
4. apilot 的 `gateway/` 改用 llm_adapt 的 `Codec`，删掉本地 `protocol/`；`PROTOCOL_MATRIX.md` 的损耗清单转成测试用例集。

**验收**：`openai_chat 报文 → anthropic 报文 → 回 openai_chat`，工具调用/图片/缓存标记不丢；apilot 现有测试全绿。

---

## 五、风险与对策

| 风险 | 对策 |
|---|---|
| **M0 搬迁时被顺手改语义** | 搬迁与扩展拆成两个 commit 序列：先纯搬迁 + 让 llm_adapt 现有测试一条不改地过 + golden 报文对拍，再在新骨架上加东西。 |
| **carter 回归**（220 测试） | M2 拆细：先做可见性与全局副作用（不动语义），再动消息模型。每步跑全量测试 + TUI 手工冒烟。 |
| **会话格式迁移不可逆** | v1→v2 只读迁移：写新文件、原文件改名备份、`format_version` 缺失视为 v1。禁止原地改写。 |
| **usage 口径不一致导致成本算错** | 统一 apilot 的硬约定：`input_tokens` 不含缓存（Anthropic 语义），OpenAI 系 codec 负责减去 `cached_tokens`（llm_adapt `openai.rs:414` 已正确实现，保持）。写一条断言测试。 |
| **跨协议丢思考签名** | 同协议直通优先；必须转换时用 `strip_unportable_thinking` 显式剥离并在 UI 标注"本渠道无法保留思考签名"。 |
| **cache 断点打错位置反而更贵** | 缓存写入单价高于普通输入（Anthropic 5m 档约 1.25×、1h 档约 2×）。断点必须落在**稳定前缀**上（system 末尾、tools 末尾），不能落在每轮都变的尾部。验收断言：第二轮起 `cache_read_input_tokens > 0`。 |
| **Tauri 事件洪泛卡 UI** | delta 批量聚合（50ms/20 条）；生命周期事件不节流；长会话用虚拟列表。 |
| **跨目录 workspace 依赖被 Cargo 吸收** | 给 `carter` 与 `llm_adapt` 各自的 `Cargo.toml` 加空 `[workspace]` 段。 |
| **删 genai 后渠道覆盖缩水** | ollama/groq/deepseek/xai/openrouter 都讲 OpenAI Chat 兼容协议，改 base_url 即可；Gemini 用 Google 的 OpenAI 兼容端点（`generativelanguage.googleapis.com/v1beta/openai/`）。真正覆盖不了的只剩 Bedrock/Vertex 这类云厂商托管形态——那是新增第 4 个 handler，不是保留 genai 的理由。 |
| **llm_adapt 是别人也可能用的库，别塞进应用概念** | 分层评审：进 `protocol/`+`http/` 的必须是协议/传输概念；`capture`/`catalog`/`media` 是可选 feature gate。 |
| **`Protocol` 枚举命名在 IPC/DB/TS 三处不一致** | 抄 apilot 的显式 `#[serde(rename)]` + 守护测试（`dto.rs:537`），别用 `rename_all`。 |
| **空集合字段让前端崩** | 跨语言契约里空集合一律显式编码为 `[]`，不用 `skip_serializing_if`。 |

---

## 六、验收方式

每个里程碑结束跑：

1. `cd ../llm_adapt && cargo test --workspace`
2. `cd ../carter && cargo test`（220 测试全绿）
3. `cd carter-app && cargo test -p carter-app-core`
4. `cd carter-app && pnpm tauri dev` 手工走查该里程碑的验收清单
5. `cd ../carter && cargo run -- <冒烟对话>`（TUI 未被破坏）

端到端目标（全部完成后）：

- 同一段带工具的多轮对话，在三个协议下分别跑通，且第 3 轮的 thinking 签名在原始报文里原样回传。
- Anthropic 渠道下 web_search 由模型自行发起，UI 显示引用来源。
- 任取一条 assistant 消息，能点开看到产生它的 HTTP 请求原始报文（含 SSE 逐帧与时间轴）。
- 任一工具结果图片能被下一轮模型看到。
- 第二轮起 `cache_read_input_tokens > 0`（断点打在稳定前缀上）。
- 新建/续聊/fork 会话，重启 App 后历史与检查点完整。

---

## 七、已决议事项

| 项 | 决议 | 决策日 |
|---|---|---|
| wire 层归属 | **做进 llm_adapt**，升级为通用 LLM API 库（D1） | 2026-10-14 |
| 方向 | **双向**，`Codec` 六方法从第一天定义，入站 M8 补齐，apilot 未来可迁入（D2） | 2026-10-14 |
| 内容模型 | **两层**：角色感知 API + 扁平 IR 真源（D3） | 2026-10-14 |
| 能力上提 | 模型元数据 + 定价 + 成本核算、token 计数与估算、媒体 blob 与图片处理（D4） | 2026-10-14 |
| carter 复用方式 | 就地 lib 化 + 消息模型 IR 化（D5） | 2026-10-14 |
| genai | 彻底移除，不做 feature-gated 保留（D6） | 2026-10-14 |
| 不内嵌网关 | 客户端用 llm_adapt 直连上游（D9） | 2026-10-14 |
| 审批与沙箱 | 首版不做，但预留拦截点（D10） | 2026-10-14 |
| 数据分家 | `~/.carter` 与 CLI 共享 + `~/.carter-app` 存 App 独有数据（D8） | 2026-10-14 |

**已排除**：把 wire 层另建为独立的 `llm-wire` crate（改为直接做进 llm_adapt）。
