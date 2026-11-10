# 发版

## 发版

```bash
bun run upgrade patch     # 改三处版本号 → 提交 → 打 tag（不 push）
git push origin main --tags
```

推 `v*` tag 直接触发 [release.yml](../.github/workflows/release.yml)。三台构建机
（Windows / Apple Silicon macOS / Linux x86_64）各跑一次 `tauri build`，出
NSIS `.exe` / `.dmg` / `.deb`，直接传进 Release —— 不经过 cargo-dist，不产生
tar.xz / zip。

手动触发：Actions → **Release** → Run workflow，`tag` 填 `v0.1.2`。填 `dry-run`
只构建、不发 Release，适合先验证流水线。

流水线里的 `plan` 会校验 tag 的版本号与 `src-tauri/tauri.conf.json`、
`package.json` 一致 —— 不一致直接失败，免得发出版本号与文件名对不上的包。

## Release 建不出来（403）

`gh release create` 报 `403 Resource not accessible by integration` 时，说明仓库
把 `GITHUB_TOKEN` 的默认权限限制成了只读。补一个带 `repo` 权限的 PAT，存成
仓库 Secrets 里的 `RELEASE_TOKEN`，流水线会自动改用它；不配则退回
`GITHUB_TOKEN`。

## 模型目录快照

内网 / 断网时「刷新元数据」会落到编进安装包的快照
（`crates/carter-app-core/data/models-snapshot.json`）。它由
`bun run models:snapshot` 从 models.dev 源仓库生成：官方 GitHub 优先，失败自动换
`gh-proxy.com` / `ghproxy.net` / `ghfast.top`；只收对话模型。

**打包时会自动更新**：`tauri.conf.json` 的 `beforeBuildCommand` 先跑
`bun run models:snapshot --best-effort` 再构建前端。所以 `bun run build` 与 CI 的
`tauri build` 都会带上当天的目录；源站全挂时只打一行警告、沿用仓库里那份，不会
挡住发版。手动重跑（不带 `--best-effort`）失败即报错，用于确认快照到底是旧的还是
新的：

```bash
bun run models:snapshot
git add crates/carter-app-core/data/models-snapshot.json
git commit -m "chore: 更新模型目录快照"
```

## 常见问题

- **Release 在 plan 阶段失败**：几乎总是 `release.yml` 与 `dist generate` 的输出
  不一致（被手改过，或改了 `dist-workspace.toml` / `build-setup.yml` 之后没有重新
  生成）。本地跑 `dist plan` 能看到完整 diff。
