# macOS 桌面版

[English](desktop.md) | 简体中文

桌面版是一个独立的 macOS 应用，适合和被控电脑放在同一台 Mac 上运行。不需要启动服务器，也没有管理密码。一个应用里可以添加多个账号、控制端和被控端。

## 安装

从 [Releases](https://github.com/geniucker-dev/codex-rc-bridge/releases) 下载与你的 Mac 对应的 `.dmg`：Apple Silicon 用 `macos-arm64`，Intel 用 `macos-x64`。打开磁盘映像，把应用拖到“应用程序”，再从那里打开。运行不需要 Rust、Bun 或终端。

应用使用 ad-hoc 签名，没有 Apple 开发者证书，也未经 Apple 公证。首次打开被 macOS 拦截时，打开“系统设置 → 隐私与安全”，选择“仍要打开”，再确认。详见 [Apple 的说明](https://support.apple.com/zh-cn/102445)。

每个安装包附有 `.sha256` 文件。在下载目录运行 `shasum -a 256 -c 文件名.dmg.sha256` 核对下载内容。

尚无正式版本时，可以登录 GitHub，在仓库的 **Actions → Build and release** 中选择一次成功的运行，下载 `release-desktop-arm64` 或 `release-desktop-x64` 构建产物，解压后打开其中的 `.dmg`。构建产物保存 14 天。自行构建见[开发指南](development.zh.md)。

第一次打开时，先在“设置”里选直连或填写 HTTP 代理，再按 [README](../../README_zh.md#连上第一台电脑) 的步骤添加控制端和被控端。代理只影响这个应用。

## 在后台运行

关闭窗口后，桥接继续在后台运行，Dock 图标也会隐藏。点菜单栏的双向箭头图标可以重新打开窗口，或者选择“退出并停止桥接”结束整个应用。退出时会等手头的操作做完。已启动的连接会记下来，下次打开应用时自动恢复。

## 授权回调

授权时，应用用系统浏览器打开登录页面，也可以复制链接到别的浏览器。登录完成后，浏览器会跳到 `http://localhost:1455/auth/callback`，应用会自动接收；1455 端口被占用时改用 1457。

如果这两个端口都被其他程序占用，页面会提示你把浏览器最后跳转到的完整地址复制回来手动提交。即使浏览器显示 `localhost` 打不开，地址本身也是有效的。

## 数据和备份

数据库位于：

```text
~/Library/Application Support/com.geniucker.codex-rc-bridge/bridge.sqlite
```

加密数据库的主密钥保存在登录钥匙串里，服务名是 `com.geniucker.codex-rc-bridge`，账号是 `desktop-database-key`。数据库要配合这把钥匙才能打开：只把数据库复制到另一台 Mac，是解不开的。

备份前先从菜单栏退出应用，再复制数据库文件。只要原来的钥匙串条目还在，把数据库放回原处就能恢复原来的设备身份。如果数据库还在，钥匙串里的密钥却读不出来，应用会拒绝启动，而不会另外生成一把新密钥。

目前还没有在两台 Mac 之间导出和导入密钥的功能。桌面版和服务器版的数据互不相通，不会自动导入对方的账号。

## 支持的系统

目前只支持 macOS。Windows 和 Linux 的桌面版还没有完成。
