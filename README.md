# carter-app

Tauri + React 桌面 agent 客户端。底层复用 **carter**（agent harness）与 **llm_adapt**（线协议底座）。

## 它是什么

一个能自己跑工具的对话客户端，同时把「模型看到的原始报文」摊开给你看。四件事：

1. **三种线协议原生支持** —— Anthropic Messages / OpenAI Chat Completions / OpenAI Responses。
   渠道按自己声明的协议说话，不做有损归一化。
2. **模型原生的远程工具** —— Anthropic 的 `web_search`、Responses 的 `web_search` 等由厂商自己
   执行。渠道不支持时**明确报出来**，不会静默丢掉让你以为开了。
3. **思考 / 工具 / 多模态统一抽象** —— 走 llm_adapt 的 IR：思考签名、工具结果里的图片、
   认不出的服务端工具块都保得住，块与块的顺序也保得住。
4. **原始报文检视** —— 对话里任意一轮，都能查到产生它的那次 HTTP 请求的原始字节
   （含流式响应的 SSE 逐帧与时序）。

## 结构

```
crates/carter-app-core/   应用核心：会话编排、事件桥、状态根、报文捕获
                          刻意不依赖 Tauri —— 能脱离 webview 测
src-tauri/                薄薄一层：Tauri 命令 + 事件转发
src/                      React 前端
```

依赖方向是单向的：

```
React ──▶ src-tauri ──▶ carter-app-core ──▶ carter ──▶ llm_adapt
```

`carter` 与 `llm_adapt` 直接从 crates.io 引入（`carter = "0.1.4"`、`llm_adapt_core = "0.1.0"`）。
两个 crate 同属这个作者，发布前先在各自仓库里改、发布后再升版本号。

## 跑起来

```bash
bun install
bun tauri dev
```

第一次跑需要至少一个渠道。可以直接在**设置 → 渠道**里新建（界面会写回
`~/.carter/config.toml`），也可以先在文件里配好：

```toml
[providers.anthropic]
kind = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"
server_tools = ["web_search"]      # 可选：让模型自己联网搜索

  [providers.anthropic.models.sonnet]
  meta = "anthropic/claude-sonnet-4-5"
```

完整配置说明见 carter 仓库的 `config.example.toml`。会话、技能、记忆与 carter CLI 共用
`~/.carter/`；客户端独有的数据（原始报文捕获、界面设置）落在 `~/.carter-app/`。

## 界面里能配什么

| 页 | 能做什么 |
|---|---|
| 渠道 | 增删改 `[providers.*]`：协议、Base URL、API Key、服务端工具、模型列表 |
| 模型 | 全部 `渠道/模型` 一览（上下文窗口、能力、定价来自 models.dev）、增删改、设默认 |
| 工具 | 内置工具的启用/禁用（**只对本应用生效**，见下） |
| 技能 | 查看 / 新建 / 编辑 / 启停 / 删除 `~/.carter/skills/*/SKILL.md` |
| MCP | 增删改 `[mcp.servers.*]`（stdio / http） |
| 会话 | 全部会话、全文搜索、重命名、fork、导出、删除 |
| 外观 | 浅色 / 深色 / 跟随系统、侧栏宽度 |
| 关于 | 路径速查与「哪些事还只能在文件里做」 |

改完即时热重载：正在跑的会话从**下一条消息**起用新配置。唯一的例外是 MCP ——
server 是会话建立时连的，改完要新开一个会话。

## 几个不显然的设计

**一个会话一个 tokio 任务。** `Thread` 是可变的、且一轮会连着跑很多次模型调用。
放进共享循环就得给每个会话加锁，而锁还要跨 `await` 持有 —— 一个会话卡在上游网络上，
别的会话全被堵住。一任务一会话天然隔离，并发量就是会话数。

**生成中只接受「取消」与「作答」两条命令**，其余排队等这一轮结束。否则一个还在等
上游网络的轮次会把取消按钮也堵住。

**事件按会话分包。** 多个会话可以同时跑，事件里带 `session_id`，前端按会话分桶渲染，
不会把两个对话的增量混进同一个气泡。

**text 与 thinking 是**有序的兄弟条目**，不合并成一个字段。** 模型会交错输出，
合并就还原不出顺序 —— 而回灌历史时顺序是硬要求（Anthropic 要求 thinking 块排在最前，
且签名原样回传）。

**报文捕获存 JSONL 而不是 SQLite。** 数据天然按会话分片，查询只有「按会话取全部」
和「按 id 取一条」两种，扫描一个文件与走一次索引没有可感差异；换来的是零额外依赖，
且文件能直接拿编辑器打开 —— 排障时最想做的事就是这个。

**图片不随历史存 base64。** 一张图几百 KB，直接进会话 JSONL 会把文件撑爆。
历史里存的是 `[img:<hash>.<ext>]` 引用 token，真发出去时才去媒体库取字节。

**改配置走 '定点编辑 TOML'，不是「读进来再全量写回」。** `carter::config::Config` 是
Deserialize-only，而且 `Config::load()` 会先对整份文本做 `${ENV_VAR}` 插值 —— 全量重
序列化会把用户写的 `api_key = "${ANTHROPIC_API_KEY}"` 换成**明文密钥**写进磁盘。
所以界面用 `toml_edit` 在原始文本上只动被改过的那几个键：注释、格式、未知字段与
`${VAR}` 字面量全部原样留下。写盘后再 `Config::load()` 验一次，读不回来就还原上一版。

**工具开关只对本应用生效，而且界面上写明了这一点。** carter 0.1.4 的 `ToolRegistry` 是
「全部常开」、`Config` 里也没有开关字段，所以本应用在 carter 装配完之后自己筛一遍
（`crates/carter-app-core/src/tools.rs`），名单存在 `~/.carter-app/settings.json`。
carter 命令行读不到它 —— 把这条藏在心里、「关了但只在 GUI 里关」是比不做更坏的结果。
子 agent 的工具池用的是**同一份筛过的快照**，所以关掉的工具对子 agent 也失效。

**单个技能的启停是把目录搬走。** carter 的 `parse_skill_md` 不认 frontmatter 里的
`enabled` 字段（会被当未知键静默忽略），所以启停落在文件系统上：目录在
`~/.carter/skills/` 与 `~/.carter/skills.disabled/` 之间搬，可逆且不丢内容。

## 现状

- M0–M5 已完成（llm_adapt 三协议双向、carter 接入、客户端闭环、原生远程工具、报文检视）。
- 管理页从只读升级为**可配置**（渠道 / 模型 / 工具 / 技能 / MCP / 会话），改完热重载。
- **三协议尚未对真实 API 联调过** —— 手上没有可用的 key。协议正确性目前由单元测试
  与「与重构前逐字节比对」保证。

### 还没做的

- **hooks / `[env]` / `system.md`** 只能在配置文件里改。
- **工具审批与沙箱** —— 方案 D10 首版不做，但 `turn.rs` 的拦截点留着。
- **API Key 进系统凭据库** —— 目前和 CLI 一样存在 `config.toml` 里，
  或者写成 `${环境变量}` 由环境提供。
- **单个 MCP 工具的开关** —— MCP 工具名要连上 server 才知道，所以只能在 server 一级启停。
- **自绘标题栏** —— 保留系统标题栏（拖动 / 缩放 / 贴边全交给系统）。
