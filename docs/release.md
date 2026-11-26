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

## 更新说明

Release 正文由流水线拼出来，分两段：

1. **更新说明** —— `scripts/release-notes.ts` 取「上一个 tag 到这一个 tag」之间的提交
   标题，`feat` → 新增、`fix` → 修复、`perf` → 优化，其余类型不收；
2. `<!-- installers -->` 之后是安装包文件表。

应用里的「更新内容」（首页提示条、设置 → 关于）展示的是第 1 段，所以：

- 这三类提交的标题就是给用户看的话，写成「用户能看懂的一句话」；
- 打 tag 之前跑 `bun run release:notes` 预览，不满意就在发版前改提交标题；
- **不要在 Release 页面上手改正文**：重跑同一个 tag 的流水线会把它盖掉；
- 分界标记两边各有一份常量（`release-notes.ts` 与 core 的 `update.rs`），改名要同步。
  正文里没有这个标记的旧版本，应用里不显示更新内容，只给发布页链接。

## Release 建不出来（403）

`gh release create` 报 `403 Resource not accessible by integration` 时，说明仓库
把 `GITHUB_TOKEN` 的默认权限限制成了只读。补一个带 `repo` 权限的 PAT，存成
仓库 Secrets 里的 `RELEASE_TOKEN`，流水线会自动改用它；不配则退回
`GITHUB_TOKEN`。

## 模型目录快照

内网 / 断网时「刷新元数据」会落到**编进二进制的 models.dev 快照**。这份快照在
`carter` 里（`assets/models.json.gz`，gzip 后约 380 KB），app 通过
`carter::registry::bundled` 直接复用 —— 与 CLI 用的是同一份，所以目录不会一边新
一边旧。

要更新它，在 carter 仓库跑：

```bash
python3 scripts/refresh-models.py
git add assets/models.json.gz && git commit -m "chore: 更新模型目录快照"
```

快照里的 `$generated_at` 会显示在「关于」页，用户据此判断该不该联网刷新一次。

## 常见问题

- **Release 在 plan 阶段失败**：几乎总是 `release.yml` 与 `dist generate` 的输出
  不一致（被手改过，或改了 `dist-workspace.toml` / `build-setup.yml` 之后没有重新
  生成）。本地跑 `dist plan` 能看到完整 diff。
