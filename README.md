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

`carter` 与 `llm_adapt` 是同级目录（`../carter`、`../llm_adapt`），通过 path 依赖引入。

## 跑起来

```bash
bun install
bun tauri dev
```

需要先在 `~/.carter/config.toml` 里配好至少一个渠道：

```toml
[providers.anthropic]
kind = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"
server_tools = ["web_search"]      # 可选：让模型自己联网搜索

  [providers.anthropic.models.sonnet]
  meta = "anthropic/claude-sonnet-4-5"
```

完整配置说明见 `../carter/config.example.toml`。会话、技能、记忆与 carter CLI 共用
`~/.carter/`；客户端独有的数据（原始报文捕获）落在 `~/.carter-app/`。

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

## 现状

- M0–M5 已完成（llm_adapt 三协议双向、carter 接入、客户端闭环、原生远程工具、报文检视）。
- 管理页是**只读**的：能看到渠道 / 技能 / MCP / 工具 / 会话，但还不能在界面里改配置。
- **三协议尚未对真实 API 联调过** —— 手上没有可用的 key。协议正确性目前由单元测试
  与「与重构前逐字节比对」保证。
