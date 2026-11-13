# 服务器部署

[English](server.md) | 简体中文

服务器版适合放在一台一直开着的主机上，用浏览器（包括手机浏览器）管理。多人共用同一套部署时，每个人有自己独立的空间（租户），彼此看不到对方的账号和连接。

桥接自己提供管理页面和接口，在主机的回环地址上用普通 HTTP 监听。域名和 HTTPS 交给主机上已有的反向代理（Caddy、nginx 等），由它把整个站点转发给桥接。安装方式有两种：Docker Compose，或者原生安装包配合 systemd。

## 反向代理

不管用哪种反向代理，都要做到三件事：

- 把所有路径都转发给桥接，默认地址是 `127.0.0.1:8787`；
- 原样传递 `Host` 请求头，桥接只响应 `BRIDGE_PUBLIC_ORIGIN` 里那个地址的请求；
- 不缓冲响应，页面上的实时状态（`/api/events` 的服务器推送事件）才能及时到达。

`deploy/Caddyfile.example` 和 `deploy/nginx.conf.example` 是现成的配置。Caddy 默认就会传递 `Host`、不缓冲，还会自己申请证书。nginx 要保留示例里的 `proxy_set_header Host $http_host` 和 `proxy_buffering off`，再填上你的证书路径。`$http_host` 会保留公网地址里的端口，例如 `https://bridge.example.com:8443` 中的 `8443`。主机上还没有反向代理的话，装一个 Caddy 用示例配置，是拿到 HTTPS 最快的办法。

桥接只在回环地址上接受普通 HTTP，所以 `BRIDGE_PUBLIC_ORIGIN` 必须填反向代理提供的 `https://` 地址。

## Docker Compose

需要准备：装好 Docker 和 Compose 的主机，以及按上面配好 HTTPS 的反向代理。

镜像发布在 Docker Hub 上，名为 `geniucker/codex-rc-bridge`，支持 x64 和 arm64。为这套部署建一个目录，从最新版本下载 Compose 文件和配置：

```sh
mkdir codex-rc-bridge && cd codex-rc-bridge
curl -fsSLO https://github.com/geniucker-dev/codex-rc-bridge/releases/latest/download/compose.yaml
curl -fsSL -o .env https://github.com/geniucker-dev/codex-rc-bridge/releases/latest/download/compose.env.example
```

之后的 `docker compose` 命令都在这个目录里运行。

填上反向代理提供的地址：

```dotenv
BRIDGE_PUBLIC_ORIGIN=https://bridge.example.com
```

桥接默认监听 `127.0.0.1:8787`。要换地址或端口，在 `.env` 里设置 `BRIDGE_HTTP_BIND` 和 `BRIDGE_HTTP_PORT`，反向代理也指向那里。

每个版本附带的 `compose.yaml` 写的是该版本的镜像，部署会一直停在这个版本，直到你升级。

拉取镜像并初始化存储。`init` 只在第一次安装时运行，已有部署再运行会被拒绝：

```sh
docker compose pull
docker compose run --rm admin init
```

`init` 会输出一个租户 UUID。用它替换下面的 `TENANT_UUID`，创建管理登录账号。第一次运行 `add-user` 时，会先问桥接怎么访问 OpenAI：直接回车表示直连，或者输入 HTTP 代理地址（见[上游代理](#上游代理)）。然后输入两次密码，至少 12 个字符：

```sh
docker compose run --rm admin add-user TENANT_UUID admin
docker compose up -d server
```

打开你的 HTTPS 域名，用 `admin` 和刚设的密码登录。这个管理账号只用来登录桥接的管理页面，和 ChatGPT 账号无关。第一个管理用户是部署管理员，可以在“设置”里配置上游代理。

数据库和加密主密钥分别存放在 `bridge-data` 和 `bridge-key` 两个卷里。两者缺一不可：只有数据库、或者只有密钥，都恢复不了原来的设备身份。

### 改用宿主机目录（Linux）

默认的卷不需要任何准备。如果你想把数据放在宿主机目录里，比如方便交给现有的备份工具，就在运行 `init` 之前，在 `compose.yaml` 旁边新建 `compose.override.yaml`，Compose 会自动读取它。数据库和密钥要放在两个不同的目录里，不要放在一起：

```yaml
services:
  server:
    volumes:
      - /srv/codex-rc-bridge/data:/var/lib/bridge:z
      - /srv/codex-rc-bridge/key:/run/bridge-secrets:ro,z
  admin:
    volumes:
      - /srv/codex-rc-bridge/data:/var/lib/bridge:z
      - /srv/codex-rc-bridge/key:/run/bridge-secrets:z
```

`z` 让容器在启用了 SELinux 的主机（例如 Fedora 或 RHEL）上也能访问这两个目录，做法是把目录重新标记为容器共享。没有启用 SELinux 的主机上，Docker 会忽略它。它会重新标记目录里的所有内容，所以只用在桥接专用的目录上。

容器以用户和组 `10001` 运行，自己改不了目录属主，所以要先为这个用户建好两个目录，并且不让其他人读取：

```sh
sudo install -d -m 700 -o 10001 -g 10001 /srv/codex-rc-bridge/data /srv/codex-rc-bridge/key
```

之后照上面的步骤运行 `init`。如果 `init` 报 `Permission denied`，说明目录原本不存在，被 Docker 以 root 身份自动创建了；再运行一次上面的 `install` 命令，然后重试。

备份时先停止服务器，再分别打包两个目录，生成的归档只有 root 能读：

```sh
docker compose stop --timeout -1 server
mkdir -p -m 700 ~/bridge-backup
for name in data key; do
  sudo sh -c "umask 077; tar cpf $HOME/bridge-backup/bridge-$name.tar -C /srv/codex-rc-bridge/$name ."
done
```

恢复时把两份归档分别解压到空目录里。以 root 运行的 `tar` 会还原属主和权限：

```sh
sudo mkdir -p /srv/codex-rc-bridge/data /srv/codex-rc-bridge/key
for name in data key; do
  sudo tar xpf ~/bridge-backup/bridge-$name.tar -C /srv/codex-rc-bridge/$name
done
```

已经在用卷的部署要换成宿主机目录，先按[停止、升级和备份](#停止升级和备份)备份两个卷，再用同样的方法解压这两份归档。

macOS 上的 Docker Desktop 和 OrbStack 访问宿主机目录要经过一层文件共享，SQLite 的同步和加锁在上面没那么可靠，那里请继续用卷。

### 上游代理

如果这台主机访问 OpenAI 需要代理，由部署管理员在“设置”里填写。地址要从容器里能访问到：`127.0.0.1` 指的是容器自己。在 Docker Desktop 或 OrbStack 上可以用 `host.docker.internal` 访问宿主机上的代理，普通 Linux 主机要填实际能访问到的地址。代理只影响桥接访问官方服务，不影响管理页面。

### 添加其他用户

每个人有自己独立的空间，登录、ChatGPT 账号和设备都是分开的，彼此看不到。

部署管理员在管理页面打开“用户”，点“添加用户”，填好用户名和至少 12 个字符的初始密码，再把两者告诉对方。对方登录后可以点页面顶部的“修改密码”自己改。整个过程不用停服务器。

有人忘了密码，就在对方那一行点“重置密码”。对方会在所有地方退出登录，但连接照常运行。密码输错太多次被锁定时，重置也能立即解锁。

“删除用户”会把这个人的空间从服务器上删掉，包括对方的 ChatGPT 登录、控制端、被控端和连接，正在运行的连接会停止。桥接不会去 ChatGPT 那边撤销授权，所以对方的 ChatGPT 里可能还留着旧的远程设备。这些设备已经用不了，对方可以自己在 ChatGPT 里移除。删除没法撤销。恢复删除之前的备份可以把这个人找回来，但整个部署的其他数据也会一起回到那个时间点。

页面上改不了管理员账号，你自己的也一样，这些要在停止服务器后用 `bridge-admin` 管理。忘了自己的密码时，用 `init` 输出的租户 UUID：

```sh
docker compose stop --timeout -1 server
docker compose run --rm admin reset-password TENANT_UUID admin
docker compose up -d server
```

停止服务器后，命令行也能添加用户：`new-tenant` 输出新的租户 UUID，再用 `add-user NEW_TENANT_UUID alice` 为它创建登录。

部署管理员掌管这台主机，也掌握保护所有人 ChatGPT 登录凭据的密钥，多人共用时，大家都要信任这个人。只添加各自连接自己账号的人。

### 停止、升级和备份

计划内的维护都先这样停止服务器。它会等已经开始的操作做完再退出，不会被 Docker 的默认超时强行结束：

```sh
docker compose stop --timeout -1 server
```

停下来后备份两个卷，分开保存，并记下是同一批备份。默认的部署名是 `codex-rc-bridge`，卷名因此是 `codex-rc-bridge_bridge-data` 和 `codex-rc-bridge_bridge-key`。密钥归档和密钥本身一样敏感，下面的命令把两份归档都写成只有你能读，并放进私有目录。命令用这套部署正在使用的镜像里的 `tar` 打包：

```sh
mkdir -p -m 700 ~/bridge-backup
image=$(docker compose config --images server)
for name in data key; do
  docker run --rm --network none --user 0:0 \
    --mount type=volume,source=codex-rc-bridge_bridge-$name,target=/state,readonly \
    -v ~/bridge-backup:/backup --entrypoint sh "$image" \
    -ec "umask 077; tar cpf /backup/bridge-$name.tar -C /state ."
done
```

如果用的是[宿主机目录](#改用宿主机目录linux)，改为打包那两个目录。

升级时下载新版本的 `compose.yaml`，再拉取镜像并启动。发布说明提到新配置项时，对照那个版本的 `compose.env.example` 检查 `.env`。不要再运行 `init`：

```sh
curl -fsSLO https://github.com/geniucker-dev/codex-rc-bridge/releases/latest/download/compose.yaml
docker compose pull
docker compose up -d server
```

旧版本不一定能读新版本写过的数据库。要回退，换回上一个版本的 `compose.yaml`，再恢复升级前的备份；只换镜像是不够的。

恢复时，用原来的部署名建好空卷，把同一批备份里的数据库和密钥解压进去，然后直接启动，不要运行 `init`：

```sh
docker compose create server
image=$(docker compose config --images server)
for name in data key; do
  docker run --rm --network none --user 0:0 \
    --mount type=volume,source=codex-rc-bridge_bridge-$name,target=/state \
    -v ~/bridge-backup:/backup:ro --entrypoint sh "$image" \
    -ec "tar xpf /backup/bridge-$name.tar -C /state"
done
docker compose up -d server
```

启动后检查账号、设备和连接：备份里的登录凭据或注册信息可能已经过期，需要按页面提示重新登录或重新授权。

`docker compose down --volumes` 会连同密钥一起删除全部数据，正式部署不要运行。同一台主机上部署多套时，每套用固定的 `-p 部署名` 和不同的 `BRIDGE_HTTP_PORT`，之后的所有管理、升级和备份命令都带上同一个名字；这时卷名的前缀也是这个名字，而不是 `codex-rc-bridge`。

### 自己构建镜像

想用自己构建的版本，在源码目录里构建镜像，再让 `BRIDGE_IMAGE` 指向它：

```sh
docker build --target server -t codex-rc-bridge-server:local .
```

然后在 `.env` 里设置 `BRIDGE_IMAGE=codex-rc-bridge-server:local`，跳过 `docker compose pull`。

## 原生安装包

从 [Releases](https://github.com/geniucker-dev/codex-rc-bridge/releases) 下载对应架构的 Linux `.tar.gz` 和 `.sha256`。在下载目录用 `shasum -a 256 -c 文件名.tar.gz.sha256` 核对后解压，运行不需要 Rust 或 Bun。包内有服务器、管理工具、网页和部署示例。

安装包在 Ubuntu 22.04 上构建，能在 glibc 2.35 及以上的 Linux 上运行，例如 Ubuntu 22.04、Debian 12 和更新的版本。更旧的系统请自行构建，见[开发指南](development.zh.md#构建安装包)。

Linux 上建议的目录布局：

| 路径                              | 内容                                               |
| --------------------------------- | -------------------------------------------------- |
| `/opt/codex-rc-bridge/bin`        | 服务器和管理工具 `bridge-admin`                    |
| `/opt/codex-rc-bridge/ui`         | 管理页面，由桥接提供                               |
| `/var/lib/codex-rc-bridge`        | 数据库，权限 0700，属于服务用户                    |
| `/etc/codex-rc-bridge-secrets`    | 主密钥目录，权限 0700，密钥文件 0600，属于服务用户 |
| `/etc/codex-rc-bridge/server.env` | 由 `deploy/server.env.example` 改出来的配置        |

先创建专用用户 `codex-bridge` 和上面的目录。以这个用户的身份初始化并创建管理账号；和 Docker 一样，第一次 `add-user` 也会先问上游代理：

```sh
bridge-admin /var/lib/codex-rc-bridge/bridge.sqlite /etc/codex-rc-bridge-secrets/master.key init
bridge-admin /var/lib/codex-rc-bridge/bridge.sqlite /etc/codex-rc-bridge-secrets/master.key add-user TENANT_UUID admin
```

主密钥要属于实际运行服务的用户，不能留给 root。已有数据库必须继续使用原来的密钥，不要再次初始化。

在 `server.env` 里把 `BRIDGE_PUBLIC_ORIGIN` 设成你的 HTTPS 地址；`BRIDGE_UI_DIR` 指向 `ui` 目录，`BRIDGE_BIND` 是桥接监听的地址。把 `deploy/codex-rc-bridge.service` 安装为 systemd 服务，再按上面的说明配置反向代理。

升级和备份前，先正常停止服务，确认它已经退出。数据库和密钥文件分开备份，替换程序文件时不要覆盖这两处。回退规则和 Docker 部署一样：程序和升级前的备份要一起换回去。

## 目前验证过的环境

已在 macOS arm64 上验证原生安装包，在 Linux arm64 容器里验证 Docker Compose 部署。独立 Linux 主机上的 systemd、amd64 架构和 Windows 还没有实测。
