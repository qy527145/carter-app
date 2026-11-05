# 发版

## 发版

```bash
bun run upgrade patch     # 改三处版本号 → 提交 → 打 tag（不 push）
git push origin main --tags
```

推 tag 后 `release-on-tag.yml` 转成一次 `workflow_dispatch` 交给 `release.yml`
（cargo-dist 生成）。四台构建机各自编出二进制，dist 打成 `tar.xz` / `zip` 并算
sha256，最后一起放进同一个 GitHub Release。

## 不要手改 release.yml

那个文件是 `dist generate --mode ci` 生成的，dist 在 CI 里会逐字比对，不一致就
在 plan 阶段直接失败（那一步的日志只有仓库管理员看得到）。要加构建步骤就改
`.github/build-setup.yml`（dist 会原样嵌进 `release.yml`），改完跑一次：

```bash
dist generate --mode ci
```

再提交。本地 `dist plan` 能提前发现「生成物与磁盘不一致」。

## 模型目录快照

内网 / 断网时「刷新元数据」会落到编进安装包的快照
（`crates/carter-app-core/data/models-snapshot.json`）。它由
`bun run models:snapshot` 从 models.dev 源仓库生成：官方 GitHub 优先，失败自动换
`gh-proxy.com` / `ghproxy.net` / `ghfast.top`；只收对话模型。`bun run upgrade` 之后
顺手跑一次再提交，发出去的包才带着当天的目录：

```bash
bun run models:snapshot
git add crates/carter-app-core/data/models-snapshot.json
git commit -m "chore: 更新模型目录快照"
```

## 常见问题

- **Release 在 plan 阶段失败**：几乎总是 `release.yml` 与 `dist generate` 的输出
  不一致（被手改过，或改了 `dist-workspace.toml` / `build-setup.yml` 之后没有重新
  生成）。本地跑 `dist plan` 能看到完整 diff。
