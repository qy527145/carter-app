# 发版与自更新

## 发版

```bash
bun run upgrade patch     # 改三处版本号 → 提交 → 打 tag（不 push）
git push origin main --tags
```

推 tag 后 `release-on-tag.yml` 转成一次 `workflow_dispatch` 交给 `release.yml`。
每个平台的构建机上会先跑 `.github/build-setup.yml`：用 `tauri build` 出原生安装包
（macOS 的 `.app` + dmg、Windows 的 NSIS、Linux 的 AppImage + deb）以及**自更新
包**（`.app.tar.gz` / `-setup.exe.zip` / `.AppImage.tar.gz`），每个都带一个 `.sig`。
发布 job 把这些自更新包收齐，用 `scripts/latest-json.ts` 生成 `latest.json`，
和 dist 的产物一起放进同一个 Release。

## 签名密钥（只需要做一次，但要保管好）

```bash
bunx tauri signer generate -w ~/.tauri/carter-app.key -p ""   # 密钥密码留空
```

得到两个文件：

- `~/.tauri/carter-app.key.pub` —— 公钥，已经写进 `src-tauri/tauri.conf.json` 的
  `plugins.updater.pubkey` **和** `crates/carter-app-core/src/net.rs` 的
  `UPDATE_PUBKEY`（两处必须一模一样）。换公钥 = 已发布的旧版本再也验不过新包，
  除非私钥泄露，不要换。
- `~/.tauri/carter-app.key` —— **私钥，只能放本地和 GitHub Secrets，永远不要提交。**

在仓库的 Settings → Secrets and variables → Actions 里加两个 secret：

| Secret 名 | 值 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | 私钥文件的全部内容（`cat carter-app.key` 的输出） |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 密钥密码；生成时留空就填一个空字符串，或干脆不建这个 secret |

CI 里 `tauri build` 用它给自更新包签名，签名里记录了版本号（`version:0.2.0`），
客户端装包前会核对「签名里的版本 == 清单里的版本」—— 防止有人拿旧版本的合法签名
配一份新版本号的清单，把用户「升级」回旧版本。

## 客户端怎么更新

`src-tauri/src/updater.rs`：

1. 依次试 `github.com → gh-proxy.com → ghproxy.net → ghfast.top` 上的
   `releases/latest/download/latest.json`，自己比较版本（不看插件的「第一个能解析
   就算数」）；
2. 从同一组加速源下载本平台的包，`carter-app-core` 的 `net::verify_update` 验签；
3. 包存进 `~/.carter-app/updates/`，状态记进 `ready.json`；界面（侧栏底部 / 托盘
   菜单 / 设置 → 关于）出现「重启更新」；
4. 用户点了之后交给 `tauri-plugin-updater` 的 `Update::install`：macOS 在
   `/Applications` 里原地换 `.app`，Windows 跑 NSIS 的 `/UPDATE` 并重启
   （`windows.installMode` 默认 `passive`，要完全无界面可在 `tauri.conf.json` 的
   `plugins.updater.windows.installMode` 改成 `quiet`），Linux 原地替换 AppImage。

## 验证一条完整链路（不用真发版）

1. `bun run upgrade patch` 之后**不要 push**，本地 `bun run build`（或 `build:mac`）
   出一个带 `TAURI_SIGNING_PRIVATE_KEY` 的包；
2. 在 `target/release/bundle/` 下 `bun run scripts/latest-json.ts <新版本> v<新版本> .`；
3. 把 `MANIFEST_URL`（`src-tauri/src/updater.rs`）临时指到一个本地静态服务，
   用**旧版本**的安装包跑起来，等 30 秒看日志（`bun run logs -f`）里的
   「检查更新 … source=…」；
4. 界面上应出现「重启更新」，点完之后版本号变成新的；把包改一个字节再试一次，
   应当报「签名校验失败」并且不安装。

验证完把 `MANIFEST_URL` 改回去，别提交临时改动。

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

- **Release 里没有 `latest.json`**：某个平台的构建没产出自更新包，或者没配签名
  secret（`tauri build` 会直接失败）。看构建日志里「Generate latest.json」之前
  那一步的 `find` 输出。
- **客户端一直说「所有源都失败」**：日志里会列出每个源各自的原因。机器上配了
  代理就在**设置 → 外观 → 网络 → 代理**里填上；没有代理就等加速站恢复，
  或者关掉自动更新手动下包。
