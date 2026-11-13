# 授权与链路联调

面向用户的部署、升级和备份步骤在[服务器部署指南](../guide/server.zh.md)，改动部署方式时同步更新它和英文版。本文记录开发联调和实现细节。

此入口用于开发联调，直接调用 Rust 核心。已接入普通账号 OAuth、OUT 二次授权、IN/OUT 注册和配对、固定链路及多链路控制台。2026-11-08 用户完成真实账号配置并反馈手机端正常使用；iPad/iOS 基本硬切换已按用户确认通过（实测在 iPad，iOS 按相同逻辑纳入），设备故障场景仍待验收。不导入旧 PoC 凭据。

服务器已提供独立管理登录与受保护的 HTTP API，网页可以在线配置授权、配对和链路。服务器对每个租户只创建一个协调器，并共享账号 `AuthService`；所有配置和生命周期操作进入这一协调入口。服务器与 CLI 在整个运行期间持有同一数据库操作锁，不能同时使用一个状态库。未配置管理环境变量时仍只运行无凭据的工程状态服务。

网页账号标签来自本租户已保存的加密账号凭据中的 `https://api.openai.com/profile`：优先邮箱，名称作为辅助信息。无需重新授权，也不会因读取标签而联网续期。邮箱/名称仅作展示，不参与账号身份或租户授权；同邮箱多工作区会额外区分工作区。内部用户和工作区 ID 收在“账号详情”，上游未提供资料时仍可通过 ID 辨认。前端只接收这些展示字段，不接收令牌。

桌面与自托管服务器长期默认使用 SQLite，按单桥接进程管理多租户、多链路部署。PostgreSQL 为条件性扩展，依据见 [存储选择与边界](PLAN.md#存储选择与边界)。

## 一次完成配置与启动

在项目目录的真实终端运行：

```sh
bun run bridge:setup
```

向导自动初始化或读取 `local-state/bridge.sqlite` 和 `secrets/master.key`。首次创建的目录权限为 0700，主密钥文件为 0600；已有数据库或主密钥不会被覆盖。默认向导要求此本地状态只有一个租户，多租户管理员使用下方显式命令。

向导依次引导：登录 IN 账号（手机 A 的账号）、登录 OUT 账号（与 B 的账号一致）、OUT 二次授权、输入 B 配对码并自动保存唯一目标、创建 IN 并绑定、在手机 A 输入 IN 配对码。回调地址和 B 配对码在终端内输入，不回显；不要把回调发到聊天中，也不要把向导整体输出保存为诊断日志。

配置完成后自动持续运行。手机进入 IN，检查显示的对话来自 B，并验证能正常收发消息。该终端需要保持运行；按 Ctrl+C 会断开两侧、结束流并等待后台凭据操作完成，不会取消 B 已开始执行的任务，也不撤销配对或释放配置绑定。

已有一条本地绑定时，后续可直接启动，不重复登录和配对：

```sh
bun run bridge:start
```

启动日志中 `bridge.waiting_target` 表示 B 尚未确认在线，`bridge.connected` 表示两侧连接已建立；仍需用手机实际验证对话。会话续期不拆掉健康连接；已知 HTTP 401 刷新后最多重试一次。普通网络重连保留原链路映射、两侧队列和 cursor。冷启动的旧源流通过持久记录拒绝重放，需要客户端建立新流，必要时退出设备页面后重新进入。

## 初始化

Unix 服务器主密钥适配器适用于 macOS/Linux；Windows 和桌面系统凭据存储尚待接入。数据库目录和主密钥目录提前建立，主密钥不得在数据库目录或其子目录里；例如 `local-state/` 和 `secrets/`，两者均已被 Git 忽略。生产部署应使用独立 secret 挂载，不把这两个目录一起备份。

```sh
mkdir -p local-state secrets
chmod 700 local-state secrets
cargo run -p bridge-server --bin bridge-admin --locked -- \
  local-state/bridge.sqlite secrets/master.key init
```

输出新的 Tenant UUID。数据库、主密钥已存在时不覆盖；读取主密钥失败也不自动生成新密钥。主密钥是 32 字节随机值，文件必须为当前用户拥有、仅所有者可访问的普通文件，不接受符号链接。

数据库使用当前 schema 14，空库直接初始化，其他版本或非空无版本库明确拒绝打开并提示手工处理，不提供升级迁移。当前结构保存启停设置、切换意图、未完成注册与配对、唯一控制设备、管理用户/会话及加密代理配置。流记录不保存业务载荷或原始 client/stream ID。AES-256-GCM 每次写入使用随机 nonce，AAD 包含格式域、租户、对象与用途。身份种子采用原子创建或读取，重新授权不更换它；OUT 私钥按原固定身份规则派生。

## 普通账号登录

普通账号授权和 OUT 二次授权的链接以 `originator=codex_rc_bridge` 标识桥接；HTTP 与 WebSocket 的 User-Agent 使用 `codex-rc-bridge/<桥接项目版本>` 产品名，macOS 平台字段为 Mac OS X 加实际系统版本。OAuth client ID 保持现有值。2026-11-14 在独立新状态中完成普通授权、OUT 二次授权、两侧注册和配对、IN/OUT 注册刷新及 WebSocket，并由用户确认 iPad 到 Mac 的对话列表与消息收发正常。测试权限已撤销、独立实例已关闭；服务端设备显示和验收边界见 REFERENCE 当日记录。

将下面的 `<tenant>` 替换为初始化输出的 UUID：

```sh
cargo run -p bridge-server --bin bridge-admin --locked -- \
  local-state/bridge.sqlite secrets/master.key login '<tenant>'
```

命令输出一次性授权链接与事务 ID。用户在自己的浏览器登录。授权结束后 localhost 页面打不开也可以：复制地址栏中的完整回调地址，在本机终端运行下面命令并粘贴；输入不回显，也不通过命令行参数或日志传递回调。

```sh
cargo run -p bridge-server --bin bridge-admin --locked -- \
  local-state/bridge.sqlite secrets/master.key complete '<tenant>' '<transaction>'
```

成功后只显示账号的 workspace/user 标识。重复上述流程可以添加其他账号。授权事务限时 10 分钟、单次消费、限定租户；令牌兑换失败后重新发起登录。CLI 不访问回调 URL，只向固定官方 token endpoint 发送兑换请求。此入口使用手动提交，不监听 localhost 回调端口。

部署管理员在网页“设置 → 上游代理”选择直连或 HTTP 代理，点击“保存并应用”。首次 setup 或首次创建管理登录时也会询问网络方式；设置使用独立主密钥加密保存在数据库，原生二进制、Bun 启动和容器使用同一份设置，重启无需重复配置。HTTP 代理支持 Basic 认证，密码不回显；WebSocket 经 CONNECT 建立官方 TLS 连接。HTTPS 类型代理和 SOCKS 尚不支持。

设置覆盖 `auth.openai.com` 的授权/续期及 `chatgpt.com` 的控制 API 和两侧 WebSocket；不会让本地管理页或 localhost 回调走上游代理。“保存并应用”会等待旧操作结束、短暂断开全部运行中的桥接，并使用新配置恢复原先启用的链路；原本暂停的链路不启动，配对和绑定保留。手机可能需要重新进入设备页，不自动重发旧命令。保存成功不代表代理必然可达，实际连接状态以连接页为准。

旧 `BRIDGE_UPSTREAM_PROXY` 仅在没有保存配置时导入一次，之后不会覆盖网页设置；不读取通用 HTTP_PROXY/HTTPS_PROXY。部署管理员可在正常停服后执行 `bridge-admin <数据库> <主密钥> configure-proxy <tenant>` 修复网络配置，无需依赖源码目录或 Bun。

第一个管理用户具有部署管理员权限，后来创建的普通租户没有该权限。旧单用户实例升级时保留现有用户作为部署管理员；旧多用户实例不自动提权，由本机操作者停服后执行 `bridge-admin <数据库> <主密钥> grant-admin <tenant> <username>` 明确授权。普通租户无法读取或修改代理设置。

授权失败区分网络错误、上游拒绝、身份校验、回调和事务失效；令牌端点拒绝只记录 HTTP 状态，不记录响应正文、令牌或回调。

## OUT 二次授权

先完成 OUT 所属账号的普通登录，再使用上一步输出的 workspace/user 标识及固定身份名：

```sh
cargo run -p bridge-server --bin bridge-admin --locked -- \
  local-state/bridge.sqlite secrets/master.key authorize-out \
  '<tenant>' '<workspace>' '<user>' 'desk'
```

用同一 `complete` 命令提交此事务的回调。程序按 Desktop/PoC 流程进行 enroll/start、设备证明和 enroll/finish，验证账号、工作区、scope、近期密码验证、challenge、会话归属后，加密保存上游注册并将端点标记为 active。已经保存注册的 OUT 不重复注册。

此处 active 表示已经授权，**不表示已配对、目标在线或桥接运行**。二次授权与普通 OAuth 不混存；会话短期 token 不写入注册记录。HTTP 401 仅对内部已知控制请求合并刷新普通账号授权、重试一次；明确的注册失效要求重新授权，不当成普通 401 重试。

`check-account '<tenant>' '<workspace>' '<user>'` 使用与网页、App 相同的上游只读请求检查已保存账号，并在必要时续期；仍不证明 B 在线。

## 当前边界

- 已验证：加密持久化、迁移、身份稳定性、一次性回调、共享续期、设备证明、配对、原文转发、分片 ACK、独立 cursor、重连、缺口修复/退休、初始与明确离线边界、正常停止、目标租户隔离/独占以及拒绝指向本部署 IN 的环路。
- 72 项 Rust 回归包含独立内存 WebSocket 测试、本地 HTTP/WebSocket 假服务、协调器并发/恢复、撤权范围、配对响应校验、同名恢复、SSE 和 CLI 进程验证。假服务通过与管理 API 共用的租户协调入口跑通 OAuth → enroll → 配对 → 绑定 → 设备证明 → 消息往返 → 后台续期失败恢复 → B 离线，覆盖 401 单次重试、非正常 EOF 重连和注册失效分流。测试使用合成数据，不代替真实设备切换、撤权、离线和续期故障的验收；当前 72 项回归在 macOS 与 Linux arm64 容器通过，镜像已包含记录修复及实时状态更新；Linux amd64/Windows 未验证。
- CLI 的 `manage` 模式可运行多条链、硬切换并进行常规撤权删除；`start` / `run` 保留固定链路入口。停止/离线不释放绑定，已绑定 OUT 更换电脑通过重新配对，先停止链路并清空旧访问，成功后恢复原启停状态。配置管理服务后，网页已提供授权和链路管理；默认工程状态服务的 `remoteControl: false` 不代表另外运行的 CLI 进程状态。17 项 Playwright 测试使用合成服务验证网页流程，尚未通过网页操作真实账号。服务器已有原生包与 Compose；SQLite 已明确持久化配置并验证进程强杀和合成数据离线恢复；macOS/Linux 持久卷负载基线、原生服务器数据库/独立密钥文件恢复及 Compose 删卷归档恢复已验证；桌面 keychain/Tauri、目标服务器持续负载及异机灾备尚待完成。
- OUT 已新增加密注册检查点：enroll/start 成功后先保存所属 client/account/key 标识，再发送 enroll/finish；最后应答丢失或保存失败时，可以恢复原注册，或撤销这一注册后删除。缺少检查点的 OUT 可用原 client ID 经挑战核验后补回；没有原编号或无法核验的 OUT 仍需补回原记录；IN 沿用固定 installation ID 重试注册。此入口用于开发联调，尚不是生产部署入口。

## 多链路本地控制台

在没有其他桥接进程持有此数据库锁时运行 `bun run bridge:manage`，或使用显式路径命令 `manage <tenant>`。已经运行的固定链路不会自动迁移到新模式；不要为了验证新代码中断正在正常使用的会话。

控制台命令：

| 命令                       | 行为                                                                     |
| -------------------------- | ------------------------------------------------------------------------ |
| `list`                     | 显示当前租户各条绑定、启用设置和任务状态；“运行中”不等于 B 在线          |
| `start <IN>`               | 启用并启动一条已配置链路，独立等待 B 在线                                |
| `stop <IN>`                | 保存暂停设置，关闭并等待此链路退出；保留绑定和配对                       |
| `switch <IN> <OUT>`        | 预检并预留新 OUT，等待旧链路退出，再事务提交新绑定；原已启用则连接新目标 |
| `unbind <IN>`              | 先停用并等待退出，再释放绑定；不撤权或删除身份                           |
| `delete <IN或OUT>`         | 停用关联链路、撤权并清理此端点；待清理时用同一命令重试                   |
| `quit` / Ctrl+C / 输入结束 | 关闭并等待全部链路退出；保留各条启用设置，下次进入管理模式恢复           |

首次进入控制台，已有绑定默认暂停。启用设置只适用于 `manage` 模式；旧的 `start` / `run` 是显式单链启动，不读取这些设置。本地控制台之外的单步 CLI 须在管理进程退出、释放数据库锁之后使用；配置管理服务器后，可通过网页在线完成账号授权、配对和链路管理。

请求取消不会丢下半个切换：切换意图先落盘，旧任务全部退出后才提交绑定。进程中途退出，下次管理启动先完成保存的改绑，再恢复启用的链路。新目标连接失败时保留新绑定并报告失败，不自动回退。未启用的链路改绑后保持暂停。恢复失败时保留待恢复状态，不绕过独占约束启动旧链路。

硬切换会丢弃旧链路的队列、映射及分片状态，并保留旧流哈希以拒绝重放。iPad/iOS 基本 B → C → B 切换已按用户确认通过，App 内刷新即可；实测在 iPad 完成，iOS 按相同客户端逻辑纳入。审批归属及元数据刷新仍未专门验收。已核对 Desktop 普通 refresh 不携带名称或 app-server 版本，因此保留 IN 注册，不因改绑重新 enroll；不能把控制台提交成功视为手机初始化已经成功。依据见 [REFERENCE](REFERENCE.md#2026-11-08-补充核对元数据配对与删除)。

Desktop 推荐每个真实目标使用独立 IN/OUT 组合，同一个控制账号可配对多个 IN。Desktop 保存的远程项目绑定 IN 环境与文件夹路径；同 IN 切换 OUT 后，旧项目可能使用新目标上的同名目录，或因目录不存在而失败，刷新或重启不会清除这些条目。Desktop 同 IN 切换的具体报错、缓存清理和重连体验仍待实测；V1 如实记录该限制，不承诺已有项目自动切换。详见 [Desktop 项目依据](REFERENCE.md#2026-11-08-desktop-远程-project-与硬切换)。

### 被控端配对与重新配对

`POST /api/outputs/{id}/pair`（桌面 IPC 为 `/outputs/{id}/pair`）接受 `{code}`。先读取此 OUT 的全部可访问环境，只撤销此 OUT 在各环境的权限，不撤销其他控制端，也不删除 OUT 注册；确认列表已清空后配对，重新读取并确认恰好一个环境，原子保存目标、版本及可读名称。成功返回 `{"result":"paired_output","value":{"environment_id":"…","name":null}}`，`name` 可能为空，成功不代表电脑在线。

旧环境的撤权即使返回 404，也以此 OUT 的可访问环境列表确认是否已经消失；列表读取失败或仍有旧访问时不继续配对。HTTP 409 / 桌面错误的独立代码为 `out_pairing_cleanup_pending`、`out_pairing_no_environment`、`out_pairing_multiple_environments`，分别表示旧访问未清空、配对后无环境、配对后有多个环境。

运行中重新配对先停止并等待关联链路退出，成功后恢复原启停状态。失败时保留旧目标、绑定及启用意图，链路保持离线；重新提交配对码完成后恢复。其他链路继续运行。控制端可能需要退出远程设备后重新进入；这不会取消电脑上已经开始的任务。中途退出后的处理见 [配对状态恢复](STORAGE.md#配对状态与目标恢复)。

环境列表和自由选择目标的 HTTP / 桌面接口已移除。CLI 的 `targets` 仅用于只读诊断；`pair-out` 和配置向导自动保存唯一目标，`select-target` 已移除。单步 CLI 仍需独占数据库，不与运行中的服务或 App 并用。

### 控制端管理

每个控制端只配对一台设备（手机、平板或电脑上的 ChatGPT），在控制端页面的“设备”一节查看；换设备时用“重新配对设备”，会先移除当前设备再显示新配对码。启动/停止在远程控制页或控制端、被控端页面操作；停止保留配对和绑定，再次启动无需重新配对。

新建控制端或“重新配对设备”先完成 IN 注册，再清空该 IN 环境下的全部设备，确认清空后发码。清理失败返回 `in_pairing_cleanup_pending`，保留本地原记录和未完成标记，但不再把旧设备显示为当前设备。发码后在有效期内持续跟踪；恰好一台时保存名称、平台及型号，多台时全部撤销并通过快照 `pairing_error=in_pairing_multiple_devices` 报告。停止、删除或再次配对会结束旧跟踪。旧的设备列表与逐个撤权接口已删除；删除 IN 仍按环境撤销全部权限。正在电脑上执行的任务不会因此被取消。

### 删除、重试与同名恢复

`delete` 先持久停用端点，取消并等待关联运行任务，解绑后才发出远端撤权。IN 仅撤销自己 environment 下的配对控制端；OUT 仅撤销自己的 controller 注册，均不连带删除另一端点或其他链路。失败和请求取消不会把待清理端点恢复成可用状态。`list` 会显示待删除端点；再次 `delete` 重试，管理进程重启也会先尝试一次清理。每次最多撤销 32 个配对，较大列表分次继续。

IN 配对响应校验所属 server/environment 和到期时间后展示，有效期由上游管理。删除时撤销当前已配对的控制端，确认列表清空后完成本地清理。

注册记录缺失或撤权失败时，保留停用状态及必要记录供修复、重试。

完成删除会清除端点注册和目标，使对应未完成二次授权事务失效；保留租户种子、旧流哈希与独立的共享账号授权。之后同账号同名恢复得到相同设备 ID/OUT 公钥，随后重新注册并完成授权，旧回调不能给新对象写入授权。普通账号授权的单独删除属于后续管理功能。

## 单步配对与运行命令

以下命令均带前缀 `cargo run -p bridge-server --bin bridge-admin --locked -- <数据库> <主密钥文件>`：

| 命令                                                     | 用途                                          |
| -------------------------------------------------------- | --------------------------------------------- |
| `setup-chain <tenant>`                                   | 使用显式状态路径运行配置向导                  |
| `pair-out <tenant> <out>`                                | 隐藏输入 B 配对码，先清空此 OUT 的旧访问，确认唯一环境后自动保存目标               |
| `targets <tenant> <out>`                                 | 列出此 OUT 已配对的环境                       |
| `create-in <tenant> <workspace> <user> <固定名称> <out>` | 注册 IN，首次注册时参考目标的 app-server 版本 |
| `pair-in <tenant> <in>`                                  | 清空 IN 的旧设备后发码，并在有效期内跟踪唯一设备；Ctrl+C 停止跟踪 |
| `bind <tenant> <in> <out>`                               | 保存同租户独占绑定，不覆盖已有绑定            |
| `run <tenant> <in>`                                      | 运行此 IN 已保存的固定绑定，Ctrl+C 停止       |

## 管理服务器与网页

管理登录与 ChatGPT OAuth 是两套独立凭据。每个租户当前对应一个管理用户名，部署管理员在本机初始化；没有公开注册接口。用户名支持小写字母、数字、`_`、`-`、`.`，密码至少 12 个字符，终端隐藏输入并再次确认。账号或端点名称均不作为管理密码。

以下命令用于新部署，或既有桥接正常停止并释放数据库锁之后。本轮没有重启或迁移用户正在使用的固定链路。先按上文初始化数据库，记录 Tenant UUID，然后创建管理用户：

```sh
cargo run -p bridge-server --bin bridge-admin --locked -- \
  local-state/bridge.sqlite secrets/master.key add-user '<tenant>' 'my-user'
```

为朋友提供独立空间，先运行 `new-tenant`（不带 tenant 参数）取得新 UUID，再为这个 UUID 执行 `add-user`。不可把朋友的管理用户放入自己的租户。`reset-password <tenant> <username>` 在本机隐藏输入新密码，并使该用户的全部现有会话失效；这些命令执行时服务器必须先释放数据库锁。

开发环境的同源入口为 Vite 的 5173 端口，后端仍监听 8787：

```sh
BRIDGE_DATABASE=local-state/bridge.sqlite \
BRIDGE_MASTER_KEY=secrets/master.key \
BRIDGE_PUBLIC_ORIGIN=http://127.0.0.1:5173 \
cargo run -p bridge-server --locked
```

另一个终端运行 `bun run dev`，访问 http://127.0.0.1:5173。两个终端均需保持运行。不要将 `localhost` 与 `127.0.0.1` 混用，固定入口的 Host/Origin 校验会拒绝不同来源。未配置上述数据库/主密钥时只展示“管理服务尚未配置”。

部署时用 `bun run build` 生成 `ui/dist`，由 HTTPS 反向代理同时托管页面并将 `/api` 原路径转发至后端，保留浏览器的 Host；配置 `BRIDGE_PUBLIC_ORIGIN=https://你的域名`。仅回环开发入口允许 HTTP；不会根据客户端提供的 forwarded 头推断可信来源。`BRIDGE_BIND` 默认 `127.0.0.1:8787`。数据库与主密钥继续分别挂载和备份，已有原生发布包和下文部署示例，Linux arm64 镜像与 Compose 已完成隔离验证。

网页操作顺序：添加普通账号 → 创建出口并单独授权 → 配对 B 并自动保存唯一设备 → 创建入口并生成手机配对码 → 绑定入口与出口 → 启动桥接。IN 可以先独立创建；多个入口、出口可分别配置。停止桥接保留绑定，切换先断开旧链路，删除才执行撤权。界面只有在取得 B 在线证据并建立两侧连接后才显示“连接可用”，任务启动本身不代表可用。

管理会话有效期 12 小时，使用 HttpOnly / SameSite=Strict Cookie，HTTPS 入口另设 Secure；前端只持有 CSRF 值，不读取会话 Cookie。管理密码只保存 Argon2id 哈希，会话 Cookie 只存 SHA-256 哈希。每用户最多 10 个会话；错误密码触发账号限速，全局登录尝试和并发哈希也有限制。长期 OAuth 令牌、种子和私钥不返回前端；一次性授权链接、回调和配对码不写入浏览器持久存储。退出管理登录不会停止正在运行的桥接。

网页通过 SSE 接收状态变更通知，再读取最新配置与连接状态；请求超时只提示结果未知并刷新状态，不自动重试写操作。已接受的授权、配对、改绑或删除由协调器继续收尾。服务器先验证配置并成功监听端口，才开始恢复桥接/待删除工作；Ctrl+C 和 SIGTERM 会等待协调器及链路退出后释放数据库锁。

前端验证：

```sh
bun --bun ui/node_modules/@playwright/test/cli.js install chromium
bun run --cwd ui test:e2e
```

Playwright 使用独立 Chromium、5175 临时 Vite、合成 API 与独立回环 SSE 服务，结束后清理进程。`bun ui/tests/preview.ts` 仅供本地合成数据视觉检查（先构建，5176 端口，`demo / synthetic-password`），它不加载真实数据，也不是部署入口。真实服务器登录、租户 API、数据库锁和正常退出由 Rust 测试单独验证。

### 实时连接状态

连接页将保存的启停/独占关系与实际连接分开：

| 状态 | 含义与操作 |
| --- | --- |
| 正在确认电脑状态 | 尚无在线证据或原证据已过期；不会据此宣称电脑明确离线，手机入口暂不上线 |
| 目标电脑离线 | 上游明确报告 B 离线；在目标电脑开启远控后自动恢复 |
| 正在建立连接 / 正在重新连接 | 等待两侧连接或网络重试，绑定仍保留 |
| 连接可用 | B 在线证据有效，OUT 设备证明已发送且两侧 socket 已建立；不代表已验证每一条业务请求 |
| 需要重新登录账号 | 存量凭据缺少 refresh token、身份不符或上游拒绝刷新凭据（invalid_grant）；重新登录 IN/OUT 所属账号，匹配的已启用故障连接自动恢复 |
| 出口注册需要处理 / 服务拒绝访问 | 检查出口注册、授权或配对权限；不通过重连自动重新注册或改变设备身份 |
| 本地配置需要修复 / 连接校验失败 / 连接已停止 | 本地记录、协议校验或运行任务异常；检查配置和诊断后重试 |

正常后台续期的可重试失败不会把仍健康的连接显示为断线。对端没有发送 WebSocket Close 帧就关闭连接时按网络断开重试；其他畸形帧或证明校验失败仍终止连接。

`GET /api/events` 使用同源管理 Cookie 鉴权，仅订阅当前租户。事件只包含 `changed`、`heartbeat`、`session_expired` 或 `unavailable`，不包含账号标识、令牌、日志、消息载荷或历史快照。连接建立和变化通知触发 `/api/state` 读取；通知可合并，不重放旧目标状态。读取串行且抛弃已被新通知替代的快照。网页关闭只取消订阅，不停止桥接。

每 5 秒发送心跳，并在每次事件前重新验证管理会话。退出、会话到期和密码重置会关闭已有订阅；全服务最多 64 条、每租户最多 8 条订阅；达到任一上限时新增请求返回 503，已有订阅保留。停服取消订阅，不让 SSE 阻塞 SIGTERM。心跳与鉴权独立于授权/撤权操作队列。

浏览器在流出错或 15 秒未收到事件时关闭该流并尝试重连；不可用期间定时读取状态，快照失败也会重试。成功读取当前状态后可继续操作；无法读取时保留旧数据并明确标注、禁用变更按钮。旧管理会话中尚未返回的操作不会在重新登录后打开旧授权弹窗。反向代理必须允许 `text/event-stream` 持续返回且不缓存/缓冲整个响应；现有 Caddy 配置不需要新增业务路径。

本轮验证使用合成账号和本地服务；Linux arm64 镜像与 macOS arm64 原生包已包含本轮更新，真实 Caddy 后的 HTTP/HTTPS SSE 心跳、变更通知、登录失效与带订阅退出已通过。没有迁移或重启现有真实账号桥接。

### OUT 注册恢复

新注册先校验二次授权的账号、工作区、scope、有效期与近期密码验证，校验通过后才记录注册尝试并发出 enroll/start；本地校验失败可重新授权或删除 OUT。收到服务端分配且账号归属匹配的 client ID 后，立即加密保存 client ID、所属账号标识和原密钥标识，再校验 challenge、复核授权时效并生成证明。这样后续校验失败也保留清理依据；不保存二次授权 token、challenge 或签名证明。检查点写入失败时不发送 enroll/finish，也不会覆盖已有检查点。确认注册成功后，激活端点、保存最终注册、移除检查点及操作记录在同一事务内完成。

网页对有检查点的 draft OUT 显示“恢复注册”。恢复沿用普通账号授权和原 P-256 密钥，走既有 refresh/start → challenge 校验 → 原密钥证明 → refresh/finish；成功后恢复注册，不重新 enroll，不更换 ID/密钥。租户、账号、client 与公钥身份均须匹配；远端尚未完成注册或已撤销时不会强行激活。

若不再恢复，可删除这个 OUT：仅撤销检查点中这一 client；失败保留停用及检查点供重试，确认撤权后清理。已进入删除流程的 OUT 不允许再恢复成 active。其他 OUT、共享账号授权与租户种子不受影响。

本地控制台命令为 `recover <OUT>`；独占 CLI 为 `recover-out <tenant> <out>`，使用与其他命令相同的数据库和主密钥前缀。对于没有检查点的旧状态，不显示恢复按钮，也不通过扫描账号下其他设备来猜测归属。

### 缺失 OUT 注册记录的修复

未绑定、仍有未完成注册标记且没有注册/检查点的 draft 或 deleting OUT，现在可以“补回注册记录”。先从此出口的原备份取回 **上游 client ID**；它不是桥接设备 ID，也不是被控 B 的 environment ID。

管理员可将备份数据库与原密钥分别复制到独立目录，使用兼容 schema 的程序离线读取编号。下面的命令只读取指定 OUT 的注册或检查点，核对设备 ID/原公钥后输出 client ID；不会联网、续期、启动链路或自动继续删除，也不输出令牌、种子、私钥或整份注册。打开数据库可能执行 schema 迁移，因此操作备份副本，不直接修改唯一备份。

```sh
bridge-admin /备份副本/data/state.sqlite /备份副本/keys/master.key show-out-record <tenant> <out>
```

在运行中的管理网页打开该 OUT，选择“补回注册记录”，输入这个编号。服务只对指定编号发送已有的 `refresh/start`，核对返回的 client、账号归属、挑战用途/路径/期限及原设备公钥哈希；验证通过后加密保存检查点。此操作不发送 enroll、refresh/finish、配对或撤权，不扫描账号下其他设备，不覆盖已有记录。错误编号、其他出口的密钥、跨租户请求、已绑定或已激活端点、保存失败均不会借此启用出口或完成删除。

- draft OUT 补回后出现“恢复注册”，用户明确操作后才继续原 refresh 流程激活。
- deleting OUT 补回后仍停用，只允许重试原撤权清理；服务器重启也只继续该 client 的删除，不恢复为 active。
- 所属账号登录失效时，可直接选择“重新登录所属账号”，授权事务固定到原账号/工作区，再重试补回或清理。
- 原注册已撤销/未完成、没有原编号或挑战无法证明原公钥时，不能只凭客户端列表或人工确认放行；仍需完整可信备份或保持待处理。补回编号不等于恢复一份完整备份的证据。

本地控制台等价命令为 `restore-record <OUT> <原client_id>`；独占 CLI 为 `restore-out-record <tenant> <out> <原client_id>`，同样需要数据库和独立密钥前缀。CLI 与运行中的服务器互斥；不要为读取备份而启动第二个真实桥接。

IN 注册中断或标识缺失时沿用原 installation ID 重新尝试。已启动的桥接会对暂时故障自动退避重试；首次创建未完成时，在控制端卡片或控制端页面点击“重试注册”，成功后显示配对码。停止桥接会结束自动重试；删除会等待当前请求结束，撤销当前已保存环境的控制端权限，再清理本地状态。加密注册记录缺失时，优先使用本租户该 IN 保留的环境映射撤销控制端权限；重新注册也须先完成这一步，再保存新环境，之后重新配对控制端。撤权或新注册失败保留原映射，可继续重试。曾经 active 的 IN 若注册记录及环境映射均缺失，会报告本地记录错误并保留原对象。未完成首次注册的 draft 仍可重试或删除。

本轮通过合成上游验证原公钥/账号拒绝、落盘失败、不可覆盖、同账号其他 OUT、未解绑拒绝、重启后恢复与仅撤原 client；API 验证跨租户、伪造租户及缺失 CSRF，Playwright 覆盖 390px 手机修复与待删除端点不激活。尚未对真实账号执行这条补回流程；Linux arm64 镜像和 macOS arm64 发布包现已包含本轮功能，正在使用的桥接未重启。

## 原生服务器发布包

2026-11-11 起部署方式改变：容器里不再有 Caddy，`bridge-server` 通过 `BRIDGE_UI_DIR` 自己托管网页（实现见 `crates/bridge-server/src/web.rs`，只提供该目录内的普通文件并附带原 Caddy 的安全响应头）。Compose 只剩 `server` 和 `admin`，`server` 用普通 HTTP 发布到宿主机 `127.0.0.1:8787`，域名、TLS 和证书交给宿主机已有的反向代理整站转发；`deploy/Caddyfile.example` 与 `deploy/nginx.conf.example` 是宿主机反代示例，要求保留 Host、不缓冲 SSE。下文关于 `web` 容器、容器内证书、8080/8443 和 Caddy 只托管 `ui` 的描述已过时，以[服务器部署指南](../guide/server.zh.md)为准。

`bun run package:server` 运行前端构建和 Rust release 构建，将两个二进制、静态网页、部署示例，以及中英文 README 和服务器部署指南打包到 `dist/server/`（2026-11-11 起不再打包 `docs/agent/` 下的开发记录）。包名标明系统和架构，附每文件 manifest 与整包 SHA-256；采用固定文件清单，不包含数据库、主密钥、账号状态、日志或环境配置，拒绝静态资源中的符号链接。

本轮已经在 macOS arm64 实际构建、解包、核对全部文件哈希，并验证包内 CLI 初始化、管理登录、SSE 初始事件/心跳及保留订阅时的 SIGTERM 退出；文件清单现包含 STORAGE.md 及其引用的合成测量记录。这个包不能用在 Linux；Linux 包须在目标兼容的 Linux/glibc 环境运行同一命令构建。后续已在 OrbStack 的 Linux arm64 容器完成 Rust 回归、release 镜像和 Compose 验证；独立 Linux 主机的 systemd、公网证书签发、amd64 及 Windows 尚未实测。桌面应用的运行说明见 [DESKTOP.md](DESKTOP.md)。

运行已构建的包不需要 Rust/Bun，网页由反向代理托管。Linux 部署布局示例：

| 路径                              | 内容                                              |
| --------------------------------- | ------------------------------------------------- |
| `/opt/codex-rc-bridge/bin`        | 解包后的服务器和管理 CLI                          |
| `/opt/codex-rc-bridge/ui`         | 解包后的前端静态文件，不能指向整个项目目录        |
| `/var/lib/codex-rc-bridge`        | 数据库和进程锁，服务用户拥有，0700                |
| `/etc/codex-rc-bridge-secrets`    | 独立主密钥目录，服务用户拥有，0700；密钥文件 0600 |
| `/etc/codex-rc-bridge/server.env` | 由 `deploy/server.env.example` 修改的配置         |

部署管理员创建专用 `codex-bridge` 用户及上述目录，以该用户运行包内 `bridge-admin <数据库> <主密钥文件> init`，然后 `add-user <tenant> <username>`。示例不会自动创建管理员密码。主密钥必须由实际运行服务的用户拥有，不应保留 root 属主。已有数据库使用原主密钥，禁止再次初始化。

将 `deploy/codex-rc-bridge.service` 安装为 systemd 服务，将 `deploy/Caddyfile.example` 合并到反向代理配置，域名与 `BRIDGE_PUBLIC_ORIGIN` 保持一致。Caddy 仅托管 `ui`，保留 `/api` 路径和 Host 转发；不要改为会移除前缀的 `handle_path`。systemd 用专用用户和私有状态目录运行，SIGTERM 等待桥接及已接受的操作结束；不会因为固定短超时自动强杀清理中的进程。

升级或完整备份前先正常停止服务、确认释放数据库锁。备份完整数据库与独立密钥，分别保存；只有数据库或只有密钥都不能恢复设备身份。替换程序文件不覆盖这两个位置。旧版本不一定能读取新版数据，回滚应与升级前数据库备份配套，不能只换二进制。

配置依据：[Caddy 路径处理](https://caddyserver.com/docs/caddyfile/directives/handle)、[反向代理](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy)、[静态文件根目录](https://caddyserver.com/docs/caddyfile/directives/root)、[systemd 执行环境](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec)。

## Docker Compose 部署

`Dockerfile` 提供 `server`、`web`、`test` 三个目标：Rust release 后端和管理 CLI、Bun 构建后由 Caddy 托管的网页、Linux Rust 回归。运行镜像不带 Rust/Bun 工具链。`.dockerignore` 只放行构建输入，不传入本地数据库、密钥、环境配置或调查资料。

新部署在项目根目录执行（已有部署不要重复初始化）：

```sh
cp deploy/compose.env.example .env
# 编辑 .env 中的 BRIDGE_PUBLIC_ORIGIN 和 BRIDGE_SITE_ADDRESS
# 例如分别为 https://bridge.example.com 和 bridge.example.com
# 将域名指向服务器，允许外部访问 TCP 80/443

docker compose build server web
docker compose run --rm admin init
# 把上一条输出的租户 UUID 填入；密码在终端隐藏输入
docker compose run --rm admin add-user <tenant-uuid> <username>
docker compose up -d server web
```

浏览器访问配置的 HTTPS 地址。`server` 只在 Compose 网络内监听，不发布后端端口；`web` 保留原 Host 和 `/api` 路径转发。默认由 Caddy 申请公网证书，证书状态保存在独立卷中。两种镜像均以 UID/GID `10001:10001` 运行，根文件系统只读、移除额外 capabilities；Caddy 镜像额外移除了官方二进制的低端口绑定 capability，容器内使用 8080/8443。

数据库使用 `bridge-data` 卷，主密钥使用独立 `bridge-key` 卷；服务器将密钥卷只读挂载，初始化 CLI 才可写入。新卷的目录属主与服务用户一致；初始化生成 0600 密钥，重复初始化拒绝覆盖。镜像和 `.env` 不包含密钥内容。使用 bind mount 替代命名卷时，须自行保证相同的 UID、私有目录和密钥权限。Compose 默认部署名为 `codex-rc-bridge`；同一宿主部署多套时分别使用固定的 `-p <部署名>`，之后所有管理、升级和备份命令都要使用同一个名字。

如果需要上游代理，在 setup 或部署管理员网页设置中配置容器能访问的代理地址；`127.0.0.1` 指向容器自身。在 Docker Desktop/OrbStack 上可使用 `host.docker.internal` 访问宿主代理，普通 Linux 需配置实际可达地址。此设置只影响桥接对官方上游的访问。

停止、升级或离线管理：

```sh
# 等待服务器完成已接受的操作并释放数据库锁
docker compose stop --timeout -1 server
# 此时可分别备份 bridge-data 与 bridge-key，或执行 admin 管理命令
# 升级代码后重新构建并启动；不要重新 init
docker compose build server web
docker compose up -d server web
```

Compose 常规停止默认最多等待两分钟，超时会被 Docker 强制结束；计划内维护使用上面的无限等待方式，并检查服务器退出码。不要对正式部署执行 `docker compose down --volumes`，它会删除包括身份密钥在内的持久数据。恢复时数据库卷和原密钥卷必须成对，保持权限与部署名；不要用新生成的密钥代替遗失的原密钥。默认使用 SQLite，单部署只运行一个后端进程；数据库文件放在同机可靠持久卷，不通过网络共享文件来运行多个桥接副本。

容器回归入口：

```sh
docker build --target test -t codex-rc-bridge-test:local .
docker build --target server -t codex-rc-bridge-server:local .
docker build --target web -t codex-rc-bridge-web:local .
docker pull oven/bun:1.3.11
bun run test:container
```

Playwright 测试创建随机名称的 Compose 项目、合成管理用户和独立卷，只发布回环端口。测试结束销毁自己的容器、网络和卷，不读取现有账号状态；测试初始化脚本不会进入运行镜像。HTTPS 测试使用 Caddy 的本地证书，浏览器仅在该测试上下文忽略证书信任错误，不向宿主证书库安装 CA；它不等于公网域名与 ACME 签发验收。

构建与部署依据：[Docker 多阶段构建](https://docs.docker.com/build/building/multi-stage/)、[Bun 官方容器](https://bun.com/guides/ecosystem/docker)、[Caddy 自动 HTTPS](https://caddyserver.com/docs/automatic-https)。

2026-11-08 实测：OrbStack Linux arm64 上 72 项 Rust 回归通过；release 后端与网页镜像构建成功；1 项 Playwright 容器测试覆盖 HTTP/本地 HTTPS 登录退出、Host/Origin/CSRF 边界、Secure/HttpOnly/SameSite Cookie、Caddy 后的 SSE 初始事件/心跳/变更通知/退出失效、重启及分别归档/删除源卷/空卷恢复后的会话和密钥保持、数据库独占锁、带订阅的 SIGTERM 退出、密钥权限错误拒绝启动与重复初始化拒绝覆盖。另已实际验证 CLI 隐藏输入密码创建管理员。测试配置仅为自身进程清除宿主代理变量，避免回环请求经代理转发；这不会修改部署的上游代理策略。未执行真实账号授权或远控迁移，未验证 Linux amd64、公网 ACME 或独立主机 systemd。

### SQLite 持久化与恢复边界

新代码每次打开状态库时显式使用 `DELETE` 回滚日志、`EXTRA` 同步级别和外键约束，并请求 macOS 的 `fullfsync`；连接池继续只有一个连接。内存测试库仍由 SQLite 使用内存日志，不代表磁盘持久性。策略依据是 [SQLite 同步级别](https://www.sqlite.org/pragma.html#pragma_synchronous)：EXTRA 在删除回滚日志后额外同步目录；实际可靠性仍依赖文件系统与存储设备正确完成同步。

这次修改不改变 schema 或身份派生；新的连接配置在下次正常停止并使用新二进制启动时生效，没有对运行中的实例执行配置切换。已有 WAL 状态库切换日志模式需要独占访问，打开失败时先排查占用，不删除日志文件。本轮服务器与网页容器镜像以及 macOS arm64 原生包均已重建，并包含此改动。

已在 macOS 用合成数据验证：连接替换保持策略；持有未提交且已溢写磁盘的事务时强杀子进程，重开后已提交的租户与防重放记录保留，未提交写入回滚；关闭连接后复制数据库，用独立的原密钥材料恢复相同设备 ID/公钥、绑定、目标、启停设置、待删除状态与加密注册检查点。错误密钥、跨租户读取、重复绑定、旧流再次登记和待删除对象同名重建仍被拒绝。此测试没有执行真实账号操作，也不模拟断电或回滚官方服务状态。

另已在 macOS 和 Linux arm64 验证数据库与独立原密钥文件恢复到新目录后的原生服务器启动、管理账号与原会话、设备身份、停用绑定和未完成 OAuth 事务；缺失或错误密钥均拒绝启动。测量结果、可重复测试与完整操作顺序见 [SQLite 测量与恢复](STORAGE.md)。

备份前正常停止全部使用该库的进程。若发生异常退出，应先保留整个状态目录及日志，在独占条件下让 SQLite 完成恢复并正常关闭，再制作一致备份；不得删除热日志后只复制主文件。备份数据库和原主密钥文件时继续分别保管。完整部署恢复还需核对上游授权与待完成操作：旧快照中的 refresh token 或注册状态可能已变化，不能只凭数据库能打开就认定远控可用。
