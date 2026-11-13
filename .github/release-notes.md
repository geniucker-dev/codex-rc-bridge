## Downloads / 下载

- macOS: choose `macos-arm64.dmg` for Apple Silicon or `macos-x64.dmg` for Intel, then drag the app to Applications. / Apple Silicon 选 `macos-arm64.dmg`，Intel 选 `macos-x64.dmg`，打开后拖入“应用程序”。
- The macOS app uses ad-hoc signing and is not notarized by Apple. If macOS blocks it, open System Settings → Privacy & Security → Open Anyway after trying to open the app. / macOS 应用使用 ad-hoc 签名，未经 Apple 公证。尝试打开后如被拦截，在“系统设置 → 隐私与安全”中选择“仍要打开”。
- Linux server: choose the `.tar.gz` matching your architecture. Deployment instructions are inside the package at `docs/guide/server.md` and `docs/guide/server.zh.md`. / Linux 服务器选择对应架构的 `.tar.gz`，部署说明在包内的 `docs/guide/server.md` 和 `docs/guide/server.zh.md`。
- Docker: `geniucker/codex-rc-bridge:VERSION` for linux/amd64 and linux/arm64; download `compose.yaml` and `compose.env.example` from this release to deploy it. The image appears shortly after publishing. / Docker 镜像 `geniucker/codex-rc-bridge:版本号`，支持 linux/amd64 和 linux/arm64；部署用本页的 `compose.yaml` 和 `compose.env.example`。正式发布后稍等片刻镜像才会出现。
- Each download has a `.sha256` file. Verify it with `shasum -a 256 -c FILE.sha256` in the download directory. / 每个下载文件附有 `.sha256`，在下载目录用 `shasum -a 256 -c FILE.sha256` 核对。

Before upgrading, quit the desktop app or stop the server and back up the database together with its encryption key. / 升级前先退出桌面应用或停止服务器，备份数据库及其加密密钥。
