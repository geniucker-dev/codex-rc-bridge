# 开发

[English](development.md) | 简体中文

这份指南讲从源码构建、本地运行和检查改动。安装和使用见 [README](../../README_zh.md)。用编程 Agent 开发时，Agent 还要遵守 [AGENTS.md](../../AGENTS.md)。

## 环境要求

- Rust 1.99.1，版本固定在 `rust-toolchain.toml`。
- Bun 1.3.11。依赖安装和所有前端脚本都用 Bun，仓库使用 `bun.lock`，不用其他包管理器。
- C/C++ 工具链和 CMake。构建 macOS 应用还需要 Xcode Command Line Tools。
- OpenSSL，用于独立的身份向量检查。

在仓库根目录安装依赖：

```sh
bun install --frozen-lockfile
```

## 本地运行网页版

管理接口需要一个数据库和一把主密钥。在本地状态目录里创建一次（这个目录不进版本库），再用 `init` 输出的租户 UUID 添加一个管理用户。第一次 `add-user` 会问上游代理，直接回车表示直连：

```sh
mkdir -p -m 700 local-state secrets
cargo run -p bridge-server --bin bridge-admin --locked -- local-state/bridge.sqlite secrets/master.key init
cargo run -p bridge-server --bin bridge-admin --locked -- local-state/bridge.sqlite secrets/master.key add-user TENANT_UUID dev
```

启动接口：

```sh
BRIDGE_DATABASE=local-state/bridge.sqlite \
BRIDGE_MASTER_KEY=secrets/master.key \
BRIDGE_PUBLIC_ORIGIN=http://127.0.0.1:5173 \
cargo run -p bridge-server --locked
```

在另一个终端启动前端：

```sh
bun run dev
```

打开 http://127.0.0.1:5173。前端会把 `/api` 转发到 `127.0.0.1:8787` 上的后端，所以两个终端都要开着。地址要和 `BRIDGE_PUBLIC_ORIGIN` 完全一致：配置的是 `127.0.0.1`，用 `localhost` 打开会被拒绝。没有数据库和密钥时，页面只会提示管理服务尚未配置。服务器运行时占着数据库锁，运行 `bridge-admin` 命令前要先停掉服务器。

## 本地运行桌面版

```sh
bun run desktop:dev
```

这会在 15174 端口启动桌面版前端，使用应用平常的本地存储，看到的数据和安装好的应用相同。

## 命令行流程

`bun run bridge:setup` 在终端里配置并运行一条连接，`bun run bridge:start` 再次启动它，`bun run bridge:manage` 打开管理多条连接的控制台。它们使用本地状态目录。`bun run acceptance` 会启动一个独立实例，用自己的状态和端口，供真实设备测试使用。

## 检查改动

按改动的范围选择检查项：

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
bun run build
bun tests/identity-vectors.ts
bun run --cwd ui test:e2e
bun test tests/package-server.test.ts
```

Rust 集成测试会绑定临时的回环端口。Playwright 测试在独立的浏览器环境里运行，用的是合成接口，不会碰到真实账号。第一次运行前安装一次 Chromium：

```sh
bun --bun ui/node_modules/@playwright/test/cli.js install chromium
```

真实的令牌、私钥、授权回调、配对码和聊天内容，都不要出现在测试数据、日志或提交里。

## 构建安装包

```sh
bun run desktop:build
bun run package:desktop
bun run package:server
```

macOS 应用输出到 `target/release/bundle/macos/Codex RC Bridge.app`。服务器安装包输出到 `dist/server/`，文件名带有构建时的系统和架构；Linux 包要在兼容的 Linux 环境里构建。

`package:desktop` 为当前 Mac 构建 DMG，检查应用签名，再把安装包和 SHA-256 文件写到 `dist/desktop/`。它使用 ad-hoc 签名，不需要 Apple 开发者账号，也不做公证。

Docker 镜像和容器测试这样运行：

```sh
docker build --target test -t codex-rc-bridge-test:local .
docker build --target server -t codex-rc-bridge-server:local .
docker pull oven/bun:1.3.11
bun run test:container
```

`compose.yaml` 默认使用发布的 `latest` 镜像，可以用 `BRIDGE_IMAGE` 换成别的，每个 Release 附带的副本固定为该版本；容器测试把它设为 `codex-rc-bridge-server:local`。容器测试会创建一个随机命名的 Compose 项目、一个合成的管理用户和自己的卷，只开放回环端口，结束后全部删除。

## CI 和发布

**Build and release** 在推送、PR 和手动运行时执行 Rust 测试、clippy、前端构建、身份测试向量和 Playwright 测试，然后在 Apple Silicon 和 Intel 原生 runner 上构建 macOS DMG。推送和手动运行还会构建 x64、arm64 的 Linux 服务器包。安装包可从该次运行的构建产物中下载，保存 14 天。工作流进入仓库默认分支后，GitHub 才会显示 **Run workflow** 手动运行按钮。

准备发布时，把 `Cargo.toml` 的 `workspace.package.version` 和 `crates/bridge-desktop/tauri.conf.json` 改成同一版本，并用 Cargo 更新 `Cargo.lock`。先提交这些修改，再创建例如 `v0.1.0` 的标签。标签必须和两个版本号一致；本地可运行 `bun run check:version` 检查文件。

推送标签后，所有构建成功才会把安装包连同 `compose.yaml`、`compose.env.example` 上传到 GitHub Release 草稿。检查草稿说明和附件，再手动正式发布。`v0.2.0-beta.1` 这样的标签生成预发布草稿。重跑标签构建会更新草稿附件，已正式发布的 Release 则拒绝覆盖。手动运行只上传构建产物，即使选中的是标签也不会创建 Release。不需要 Apple 凭据或额外的发布令牌；只有发布任务通过 GitHub 内置令牌获得写权限。

正式发布 GitHub Release 后，**Publish Docker Hub image** 开始运行。首次发布前，在仓库的 Actions Secrets 中添加 `DOCKERHUB_TOKEN`，填写 `geniucker` 的 Docker Hub token，授予它对 `geniucker/codex-rc-bridge` 的写权限。工作流在原生 runner 上分别测试、构建 `linux/amd64` 和 `linux/arm64`，再发布为一个多架构镜像。`v0.1.0` 这样的正式版生成 `geniucker/codex-rc-bridge:0.1.0` 和 `:latest` 标签；预发布版只有版本标签。工作流成功后镜像才可用，草稿不发布镜像；把已发布的预发布版改成正式版也不会更新 `latest`，要用新标签重新发布。发布失败时，可在 Actions 中重跑失败的任务。
