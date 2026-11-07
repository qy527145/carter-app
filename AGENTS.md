# AGENTS.md

给 AI Agent 和人类协作者的工程约定。产品说明看 README，这里只写「怎么改这个仓库」。**改代码前先看 [CODE_MAP.md](CODE_MAP.md)**。

## 结构

| 路径                        | 职责                                                                 |
| --------------------------- | -------------------------------------------------------------------- |
| `crates/carter-app-core/`   | 应用核心：会话编排、事件桥、状态根。**不依赖 Tauri**，能脱离 webview 测 |
| `src-tauri/`                | 薄薄一层：`#[tauri::command]` + 事件转发 + 托盘                       |
| `src/`                      | React 19 + Vite 8 + Tailwind v4 前端                                 |
| `src/lib/api.ts` + `api/`   | **唯一**调用 `invoke` 的地方：按领域分文件，`api.ts` 只做再导出      |
| `src-tauri/capabilities/`   | 前端可调用的权限清单                                                 |
| `scripts/`                  | `bun run` 背后的辅助脚本（init / sandbox / build / logs / upgrade / clean） |

Cargo 是根目录的 workspace，产物在根 `target/`（不是 `src-tauri/target/`）。

## 命令

```bash
bun run dev            # tauri dev
bun run dev:sandbox    # 隔离数据目录跑，不碰 ~/.carter 与 ~/.carter-app
bun run check          # biome + tsc + fmt --check + clippy -D warnings
bun run fix            # 能自动修的都修掉
bun run test           # bun test + cargo test --workspace
bun run test:core      # 只测 carter-app-core
bun run logs -f        # 跟踪 ~/.carter-app/app.log
bun run upgrade patch  # 三处版本号一起改，并提交 + 打 tag
```

## 规则

1. **业务逻辑进 `carter-app-core`**，`src-tauri` 只做取参数、转调、把错误转成字符串。
2. **新增命令**：在 `src-tauri/src/commands/<领域>.rs` 写命令 → 在 `lib.rs` 的 `generate_handler!` 注册 →
   在 `src/lib/api/<领域>.ts` 补类型化封装。组件里不要直接 `invoke("...")`。
3. **跨语言类型手写并保持同步**：Rust 结构体字段与 `api.ts` 里的 interface 一一对应，改动时两边同改。
   事件名 `carter://event` 两边各有一份常量，改名必须同步。
4. **新增插件**：`Cargo.toml` + `package.json` 两边装 → `lib.rs` 注册 → `capabilities/default.json` 授权。
   只在桌面端可用的插件放进 `[target.'cfg(not(any(target_os = "android", target_os = "ios")))'.dependencies]`
   并在 `run()` 的 `#[cfg(desktop)]` 块里注册。
5. **环境变量必须有默认值**——发给用户的安装包没有 `.env`。新增变量同时更新 `.env.example` 与 README 表格。
6. **改环境变量的测试共用 `paths::ENV_LOCK`**，不要各自建锁。
7. 不引入第二个前端构建器，不依赖全局安装的 CLI（`tauri` 走 devDependency）。

## 完成定义

改完代码后至少跑通 `bun run check` 与 `bun run test`。涉及打包配置时再跑 `bun run build:debug`。
