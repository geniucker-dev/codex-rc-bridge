# 实现依据

核对上游协议、Desktop 行为或技术选型时使用本文；产品设计与验收标准见 [PLAN](PLAN.md)。能对应到 [openai/codex](https://github.com/openai/codex) 开源实现的结论附源码链接；链接指向 main，内容随版本演进，依赖某个细节前先对照当前源码。

Desktop 相关条目是兼容性核对的结论。配对窗口、设备列表和远程 project 切换的界面行为还要实测；各条目写明了本地回归和真实上游验收各覆盖到哪里。

## Desktop 按环境撤权

控制端列表和撤权都按 IN 的环境进行：列表请求带 environment ID，撤权同时指定 environment ID 与 client ID。被控端撤销自身注册使用账号级 `/wham/remote/control/clients/{client_id}`；删除多个 IN 中的一个要用按环境的路径。按环境撤权的实际结果，以及同一控制端对其他 IN 的配对是否保留，需要在实现中验证。

[官方说明](https://learn.chatgpt.com/docs/remote-connections#remote-control-is-off-after-you-sign-back-in)明确指出退出登录会关闭远控，但保留已有配对。关闭连接、退出登录与远端撤销配对是不同操作。

### 2026-11-08 补充核对：元数据、配对与删除

公开的 [protocol.rs](https://github.com/openai/codex/blob/main/codex-rs/app-server-transport/src/transport/remote_control/protocol.rs) 中，首次 enroll 提交名称、系统、架构、app-server 版本及 installation ID；refresh 请求只包含 server ID 和 installation ID。因此 IN 注册按此保留，不给 refresh 添加未证实字段，也不为改绑擅自重新 enroll。不同版本目标切换后的手机初始化仍要实测，不能据此宣称已完成。

[clients.rs](https://github.com/openai/codex/blob/main/codex-rs/app-server-transport/src/transport/remote_control/clients.rs) 确认 IN 的列表路径为 `/wham/remote/control/environments/{environment_id}/clients`，撤权在其后追加 client ID 并使用 DELETE。OUT 自身注册撤销使用上述账号级路径，仅指定本 OUT 的上游 client ID。

[enroll.rs](https://github.com/openai/codex/blob/main/codex-rs/app-server-transport/src/transport/remote_control/enroll.rs) 校验配对响应中的 server/environment ID，并解析 RFC3339 到期时间。桥接在展示配对码前执行这些校验，有效期由上游管理。

Desktop 关闭配对窗口时只清空界面状态并刷新控制端列表，不发送取消请求；等待配对期间每秒查询控制端列表，发现新增控制端即结束等待。每次打开配对窗口都会重新请求配对码，临近到期前刷新。撤权成功后直接移除该条目。桥接据此由上游管理配对码、按需撤销当前控制端，不保存配对窗口；schema 8 删除了旧窗口表。

以上为协议核对及本地假服务回归，没有对真实账号执行撤权。

### 2026-11-08 OUT 注册恢复的本地改动

enroll/start 返回 client/account/challenge，原代码只在 enroll/finish 返回且验证成功后保存注册。新实现不修改这两步请求字段；将已验证的注册标识提前加密保存，再发送 finish。应答不确定时，使用已有 refresh 路径验证账号、client 与原公钥身份，不扫描或猜测其他注册 ID。已用本地假服务验证落盘失败不发 finish、完成应答丢失后磁盘重开恢复、错误公钥挑战拒绝，以及仅撤销该检查点 client。没有执行真实账号恢复或撤权。

公开客户端列表协议只明确 client ID 和展示元数据，没有提供可直接把任意丢失注册映射到本地派生身份的证明；现有配对状态查询仍要求已知配对码。缺少历史注册或配对记录的情况继续保持未知，不凭显示名或猜测超时解除隔离。

### 缺失 OUT 检查点的定向修复

复用现有 `refresh/start` 与 [设备挑战校验](../../crates/bridge-core/src/device.rs)，由用户提供原 client ID，不枚举或猜测注册。刷新挑战必须包含与本地派生公钥一致的 `device_identity_hash`，并通过账号、client、用途、路径和期限校验；验证后仅补回加密检查点，不发送 refresh/finish，也不改变生命周期。正常刷新仍要求保存的账号和密钥标识一致。deleting 状态的 OUT 补回后只能继续原撤权，不能重新激活。

依据是已有协议实现与新增本地假服务回归，没有真实账号补回或撤权实测。原 ID 不可得或挑战不可验证时仍保持未知。离线 `show-out-record` 只核对备份中的设备 ID 和公钥并输出注册编号，不把这个编号本身当成远端权限证明。

### 2026-11-08 Connections / Allow connections

Desktop 的“Devices that can control this Mac”区域包含刷新、Add 和控制端列表；一对一配对模式下显示“Allow connections”开关，按 host ID 设置启用状态。列表为每个 client 提供撤权，成功后移除条目并重新读取。关闭开关与逐个撤权是不同操作，不以删除主机代替撤权。

列表展示 client ID、display name、device type、platform、device model 和 last seen。桥接复用 environment 级 HTTP 路径，只向前端返回这些展示字段，不返回未知上游字段。桥接将启停统一放在连接页。连接暂停时仍可发起配对或重新配对，配对成功后保持暂停。一个 IN 只配对一台设备；换设备时使用“重新配对设备”，先撤销该环境下的全部设备并确认清空，再发新配对码。当前约定见 [PLAN](PLAN.md) 第 8 节。

### 2026-11-11 Desktop 设备列表的显示与刷新

- 列表数据：每次读取都按 environment ID 分页请求（每页 100），取 clientId、displayName、deviceType、platform、deviceModel、lastSeenAt。
- 缓存：列表只保存在内存中，数据新鲜期为 30 秒，停止使用后默认再保留 5 分钟。缓存还在时，30 秒内回到页面直接用缓存，超过 30 秒先显示旧结果、后台重新读取。重启后先显示加载状态，再从服务端读取。另有手动刷新按钮。
- 配对等待：等待配对时每秒重新读取，发现新的 clientId 后结束等待。
- 每行显示：按 deviceType 选图标（phone、laptop、其他）；标题依次取 displayName、deviceModel、platform、clientId；副标题为“Last connected {date}”，没有 lastSeenAt 时为“Authorized device”；右侧是撤权按钮。撤权成功后先移除该行，再重新读取列表。

### 2026-11-08 Desktop 远程 project 与硬切换

- Desktop 的远控 host ID 由 IN 的环境 ID 决定，不包含桥接内部 OUT 或真实 B/C 的身份。
- 远程 project 以 `{id, hostId, remotePath, label}` 持久保存，按 host ID 与规范化路径去重，打开时直接用保存的 `remotePath` 作为工作目录，不在每次重连时从被控端重新生成。最近会话元数据也按 host ID 保存。窗口刷新不等于删除这些记录。
- 正式版不保证提供 Reload Window 菜单或快捷键；设置里的 Restart connection 对应 SSH，不能当作远控的刷新入口。
- 移除远程 project 只删除客户端条目、排序和置顶记录，不删除 B/C 上的项目文件。不要混用本地项目或 ChatGPT 云项目的删除语义。

对桥接的推论：切换 OUT 保留 IN 环境，因此 Desktop 的旧远程 project 仍属于同一个 host。切到 C 后使用旧 project，会把旧路径用于 C；路径缺失可能失败，路径存在则可能落在 C 的同名目录。具体报错、会话缓存清理与重连体验未实测。这里讨论的是 Codex 远控添加的文件夹项目，不是 ChatGPT 云项目。

这类请求可以是切换后由 Desktop 新发出的合法请求，旧流隔离无法判断其路径原本指向 B。当前不能宣称“刷新即可安全迁移 Desktop 项目”。推荐每个真实目标使用独立 IN/OUT 组合，同一个控制账号配对多个 IN，使 B/C 的项目分别归属不同入口。按用户确认，V1 记录同 IN 切换的已知限制及上述推荐，不把项目自动切换列为新增阻塞项；Desktop 的切换行为仍未实测。不擅自改变 IN 身份或重写业务载荷。

## 协议兼容边界

ACK 是 relay 的发布确认，由本侧队列消费，不转发到另一侧。每个流只释放连续已确认的队首；某个分片消息的 ACK 尚未收齐时，保留该消息及其后的全部信封，重连按原顺序重放。实现和回归见 [delivery.rs](../../crates/bridge-core/src/delivery.rs) 与 [tests/delivery.rs](../../crates/bridge-core/tests/delivery.rs)。

OUT 使用普通会话的设备证明；可选的 `socket-refresh-v1` 路径桥接尚未实现。

初次上线需要 HTTP 确认在线，明确的 offline 立即撤下 IN。HTTP 在线确认超过 30 秒后，可以由已映射流最近 90 秒内新接受的完整数据或 active pong 维持在线；重复、无映射、ACK、unknown 和未完成的分片不算。外层信封没有可靠的生成时间，首次送达的历史积压仍可能被计为活动证据。实现见 [presence.rs](../../crates/bridge-core/src/presence.rs)。

Rust 实现之前的 Python 原型已停止维护，其中已验证的协议边界样例已改写为 Rust 回归测试。

## 技术参考

- [Tauri 进程模型](https://v2.tauri.app/concept/process-model/)
- [Tokio 任务关闭](https://tokio.rs/tokio/topics/shutdown)
- [serde_json RawValue](https://docs.rs/serde_json/latest/serde_json/value/struct.RawValue.html)
- [SQLx](https://docs.rs/sqlx/latest/sqlx/)
- [AWS-LC Rust](https://aws.github.io/aws-lc-rs/)
- [AWS-LC ECDSA 密钥生成与导入](https://docs.rs/aws-lc-rs/latest/aws_lc_rs/signature/struct.EcdsaKeyPair.html)
- [Tokio Tungstenite TLS 客户端与自定义连接](https://docs.rs/tokio-tungstenite/0.28.0/tokio_tungstenite/fn.client_async_tls_with_config.html)
- [HKDF 的上下文输入](https://www.rfc-editor.org/rfc/rfc5869.html#section-3.2)

管理登录与网页测试另参考 [RustCrypto Argon2 0.5.3](https://docs.rs/argon2/0.5.3/argon2/)、[OWASP 密码存储](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html)、[OWASP 会话管理](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html) 及 [Playwright 本地 Web Server](https://playwright.dev/docs/test-webserver)。这些资料用于自有管理服务的登录、会话和网页测试。

### 连接观察与管理状态订阅

2026-11-08：网页状态由 runtime 的在线证据、设备证明和 socket 建立、退出事件生成。SSE 属于自有管理服务，事件按管理会话的租户发送、只通知变更，页面读取当前快照；实现见 [核心状态](../../crates/bridge-core/src/runtime/status.rs)、[SSE 鉴权与关闭](../../crates/bridge-server/src/api/events.rs) 及 [页面状态同步](../../ui/src/state-sync.ts)。

本地 relay 回归复现了未发 Close 帧的 EOF 被误判为不可恢复错误。锁定依赖 Tungstenite 0.28 的 `read_message_frame` 将此情况映射为 `ProtocolError::ResetWithoutClosingHandshake`，该变体只表示没有完成关闭握手；[传输层](../../crates/bridge-core/src/transport.rs) 仅将此变体归为可重试网络错误，其他畸形帧保持终止。依据是依赖源码和真实 socket 合成回归。

### 2026-11-09 流废弃与管理会话切换

流出现序号缺口时，Desktop 尝试按 replay cursor 恢复；没有可用的 cursor，或重复恢复后仍有缺口，就关闭该流。关闭后同 ID 的晚到消息不再交付，后续初始化使用新的 stream ID，旧序号、cursor 和待发数据一并清空。

桥接据此修复首次收到中间分片时的遗漏：即使尚未建立 IN/OUT 映射，也先通过现有源流登记表保存废弃决定，再通知关闭。同一源流随后收到完整消息、或重启后再次到达，均不能新建映射；真正的新流不受影响。磁盘重开与提交失败回归已覆盖该边界，未新增身份或表结构。

Desktop 在 account 或 user 变化时清空路由发现并中止旧作用域，同一 principal 不触发。这支持账号切换应结束旧作用域的原则；网页管理会话与 CSRF 属于桥接自身实现。网页 `/api/state` 返回同一鉴权请求的身份与 CSRF，页面发现会话变化后清空旧状态、结束旧操作回调，再重新订阅和读取。Playwright 使用两个标签页共享真实 Cookie，覆盖跨租户换账号、同用户重新登录、SSE 断线回退及旧操作应答晚到；没有操作用户浏览器或真实账号。

### 存储依据

2026-11-08 按当前负载与部署模型确定 SQLite 为两种形态的默认存储，决策及后续条件集中记录于 [计划](PLAN.md#存储选择与边界)。代码依据是 [流首次登记](../../crates/bridge-core/src/routing.rs)、[内存转发与重放](../../crates/bridge-core/src/delivery.rs)、[存储连接](../../crates/bridge-core/src/storage.rs) 和 [数据库进程锁](../../crates/bridge-server/src/key_file.rs)；选型来自架构与代码核对；后续的 macOS/Linux 持久卷负载数据、原生与 Compose 恢复证据及适用边界见 [SQLite 测量与恢复](STORAGE.md)，不代表目标服务器容量上限。

- [SQLite 适用场景](https://www.sqlite.org/whentouse.html)：应用服务接受远程用户访问与多个进程直接共享数据库文件是不同情形；选型关注写入并发和数据位置。
- [SQLite WAL](https://www.sqlite.org/wal.html) 与 [同步级别](https://www.sqlite.org/pragma.html#pragma_synchronous)：读写并行仍受单写者约束；持久化保证需结合日志模式和同步配置验证。
- [SQLite 在线备份](https://www.sqlite.org/backup.html)：提供一致性备份接口；本项目目前验证离线数据库与独立原密钥文件恢复，没有实现在线备份接口。
- [PostgreSQL 行级安全](https://www.postgresql.org/docs/current/ddl-rowsecurity.html) 与 [时间点恢复](https://www.postgresql.org/docs/current/continuous-archiving.html)：作为明确隔离或灾备需求出现时的评估依据，不作为默认部署依赖。

## 协作说明的维护参考

以下指南用于维护 `AGENTS.md`、`SKILL.md` 等代理工作指令，按项目需要采用；这里不复制完整的通用提示词。README、使用和部署文档按读者需求组织。

- [Rethinking skills and prompts for GPT-6 Astra](https://developers.openai.com/blog/rethinking-skills-and-prompts-for-gpt-6-astra)
- [GPT-6 Astra prompting best practices](https://developers.openai.com/api/docs/guides/latest-model#gpt-6-astra-prompting-best-practices)

### 2026-11-09 Step-up、回调端口与 IN 失效恢复

Desktop 检查 step-up 新鲜度时使用秒级 iat 与毫秒级密码认证时间，只拒绝超过五分钟的情况；本地回调按 1455、1457 顺序监听，再用成功监听的端口构造 redirect URI。

公开 [websocket.rs](https://github.com/openai/codex/blob/main/codex-rs/app-server-transport/src/transport/remote_control/websocket.rs) 的 `prepare_remote_control_enrollment` / `ReplaceExisting` 路径在 server refresh 返回 HTTP 404 后替换旧 enrollment。桥接据此增加 IN 失效恢复，保留原设备身份，结束旧流后保存并使用新注册。Codex 对 WebSocket 握手结果的处理：401/403 刷新 server token；404 且 detail 为 `Remote app server not found` 时替换注册，无法识别的 404 保留注册并重试。握手携带 x-codex-installation-id；x-codex-host-device-kind 来自可选的硬件检测，桥接保留安装 ID，不模拟 Mac 硬件类型。

Desktop 把注册响应中的 online 字段当作可选；401/403 先作为授权错误处理，判定注册失效时只匹配 404 client not found 与 403 revoked。Step-up 校验要求当前账号的成员字段，global user 不能替代缺失的成员身份。

合成服务回归覆盖 IN 握手 401/403/两类 404、HTTP refresh 404、新 server ID 重连、并发恢复、应答丢失后的磁盘重开、自动重试及停止/删除边界；另覆盖 online 缺失、OUT 错误分类及成员身份。IN 采用 Codex 的固定 installation ID 重试方式：注册暂时失败继续退避，成功后保存注册与环境映射；旧版 IN 预写屏障在数据库打开时移除，OUT 检查点与授权恢复保持原规则。依据是公开 websocket.rs 的连接循环和 `enroll_and_persist_remote_control_server`；合成测试覆盖重试、身份保持、旧任务结束与隔离。没有操作真实账号。

### IN 缺失注册记录的本地恢复与删除

IN 的按环境撤权继续使用上述账号授权接口，不需要 server token。加密注册记录缺失时，从同租户、同 IN 的 `host_environments` 读取已保存的环境 ID，撤权并确认控制端列表为空后才完成删除或发送新 enroll；部分清理或注册失败保留旧映射。active IN 两份标识均缺失时保留对象并报告本地记录错误，区别于首次注册未完成的 draft。向导及 bridge-admin 复用核心恢复流程，已有注册的 refresh 404 也能恢复。

新增合成上游回归覆盖缺失注册后的限定环境撤权、分页及分批清理、失败重试与磁盘重开、重新注册前的撤权顺序、清理失败不 enroll、注册失败保留旧映射、active 两份标识缺失时拒绝误删或重注册，以及共享流程的 refresh 404 恢复。以上为本地合成回归；缺失注册记录时的真实上游恢复与删除尚未验收。

## User-Agent 与客户端标识

### 2026-11-12 服务端解析 User-Agent 的真实对照

按用户明确要求，使用可用的 IN 账号创建九个临时、未配对的 OUT 注册，逐个调用 enroll/start，再通过账号设备列表读取显示元数据；每个注册测试后立即撤销，并确认不再出现在列表。没有完成设备授权或配对，没有修改原有设备。以下是当次真实服务端结果，不是本地 UA 解析模拟：

| UA 特征 | display_name | platform / os_version |
| --- | --- | --- |
| 原格式 `Macintosh; arm64 Mac OS X`，无系统版本 | Unknown Mac | null / null |
| 原格式补上 `27_0_1` | macOS | macos / 27.0.1 |
| 原格式补上 `27.0.1` | macOS | macos / 27.0.1 |
| `Mac OS X 27.0.1; arm64` | macOS | macos / 27.0.1 |
| 更换应用产品名，保留上述 macOS 信息 | macOS | macos / 27.0.1 |
| 标准 Chrome macOS UA，系统版本 10_15_7 | macOS | macos / 10.15.7 |
| Windows NT 10.0; Win64; x64 | Windows 10 Computer | windows / 10 |
| X11; Linux x86_64 | Linux Computer | linux / null |
| 只有桥接产品名和版本 | Unknown Computer | null / null |

这组结果只说明这些 UA 在当次新注册中的解析结果，没有验证任意自定义名称，也没有验证已有注册的 refresh 或 WebSocket 是否会更新显示元数据。

### 2026-11-12 Codex CLI 格式的追加实测

公开的 Codex CLI [default_client.rs](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/default_client.rs) 使用 `<originator>/<version> (<os_type> <version>; <architecture>) <terminal>`，系统信息来自 os_info。这是 CLI 的通用 HTTP 客户端格式，不是 Desktop 控制侧的格式。

随后按用户明确要求，用 IN 账号执行同样的临时 enroll/start、读列表、撤销与确认消失流程，测试借用 CLI 系统字段构造的 UA：

| UA 的系统部分 | display_name | platform / os_version |
| --- | --- | --- |
| Mac OS 27.0.1; arm64 | Unknown Mac | null / null |
| Debian 13; aarch64 | Debian Computer | debian / null |
| Debian 12; x86_64 | Debian Computer | debian / null |
| Ubuntu 24.04; x86_64 | Ubuntu 24.04 Computer | ubuntu / 24.04 |
| Alpine Linux 3.22.1; aarch64 | Linux 3.22.1 Computer | linux / 3.22.1 |
| Linux Unknown; x86_64 | Linux Computer | linux / null |
| Windows 11; x86_64 | Windows Computer | windows / null |

补测完整 `codex_cli_rs/0.167.1 (Mac OS 27.0.1; arm64) unknown`，以及桥接产品名加相同终端后缀，仍返回 Unknown Mac；对照的 `Mac OS X 27.0.1; arm64` 返回 macOS、macos、27.0.1。所有临时注册都已撤销并确认从列表消失，原有六个设备仍在，未修改其配对、密钥或注册。

禁网 Debian trixie 容器中运行仅调用 os_info 的自有程序，实际返回 `Debian 13.0.0; aarch64`；这说明库会把数字版本规范成三段，不应把先前手写的 13 当作库的实际输出。尝试把同一程序放到 Ubuntu 22.04 时因 GLIBC_2.39 缺失而未能运行，没有将它计为 Ubuntu 系统检测通过。以上设备显示结果均来自新注册阶段，不代表已验证现有注册的 refresh 更新。

### 2026-11-12 Desktop 控制侧的 User-Agent

Desktop 控制侧的 UA 为 `Codex Desktop/<应用版本> (<平台>; <架构>)`。macOS 平台为 `Mac OS`，Linux 为 `X11; Linux`，Windows 为 `Windows NT 10.0`；架构值为 arm64、x64、ia32 等，不带系统版本或终端后缀。enroll/start、refresh/start、显式授权和 WebSocket 握手共用这个 UA。CLI 与 Desktop 使用不同的 UA 模板。

当时桥接按这一模板发送 UA，前缀为 Codex Desktop，版本使用桥接自身版本；2026-11-14 已改为桥接自己的产品名，见下文。

### 2026-11-12 Desktop 控制侧 UA 的服务端验收

按用户明确要求，仅通过服务端接口验证，没有操作被控电脑。只读桥接桌面版数据库中的账号凭据与代理设置，使用可用的 IN 账号依次创建临时未配对注册，读取设备列表中的元数据后逐个撤销。没有发送 enroll/finish、配对或账号续期请求，没有修改本地数据库。

| 完整 UA | display_name | platform | os_version | device_model | app_version |
| --- | --- | --- | --- | --- | --- |
| `Codex Desktop/0.1.0 (Mac OS; arm64)` | macOS | mac | OS | arm64 | 0.1.0 |
| `codex-rc-bridge/0.1.0 (Mac OS X 27.0.1; arm64)` | macOS | macos | 27.0.1 | Mac | null |

表中 OS 是服务端原样返回的字符串，未将其解释为真实系统版本。每个临时注册均已确认从列表消失，测试后设备数量仍为六，原有设备 ID 集合保持不变。

三个已有 OUT 所属账号的设备列表均返回 HTTP 401，因此没有发送这些 OUT 的 refresh/start 或 refresh/finish，也没有用 IN 账号替代它们的账号去刷新注册。旧注册在正常续期后能否更新元数据仍待该账号恢复登录后验证。

### 2026-11-14 桥接自有客户端标识验收

目标是授权链接、控制 API 与 WebSocket 都明确标识桥接。普通授权及 `reauth=remote_control` 链接的 originator 改为 `codex_rc_bridge`，OAuth client ID 不变。2026-11-12 的表格记录的是当时的实际请求，不改写旧证据。

本地回归覆盖两种授权目的及两个回调端口，并以合成 HTTP 和 WebSocket 服务检查 UA 和 originator。真实验收使用 acceptance 启动器的全新状态目录和独立端口，不复用曾迁入日常状态的 acceptance-v1 目录，不读取日常桥接的凭据、不修改日常账号、设备、配对和注册。用户在新实例完成普通账号授权后，使用该新凭据读取设备列表并依次创建临时未配对注册，没有发送 enroll/finish 或配对请求。

| 完整 UA | enroll/start 状态 | display_name | platform | os_version | device_model | app_version |
| --- | --- | --- | --- | --- | --- | --- |
| `codex-rc-bridge/0.2.0 (Mac OS; arm64)` | 200 | Unknown Mac | null | null | Mac | null |
| `Codex Desktop/0.2.0 (Mac OS; arm64)`（旧格式对照） | 200 | macOS | mac | OS | arm64 | 0.2.0 |
| `codex-rc-bridge/0.2.0 (Mac OS X 27.0.1; arm64)` | 200 | macOS | macos | 27.0.1 | Mac | null |
| `codex-rc-bridge/0.2.0 (X11; Linux; arm64)` | 200 | Linux Computer | linux | null | Computer | null |
| `codex-rc-bridge/0.2.0 (X11; Linux; x64)` | 200 | Linux Computer | linux | null | Computer | null |

27.0.1 来自本机 sw_vers 的实际输出。五个注册逐个立即撤销，并通过重新读取设备列表确认消失；原有六个设备的 ID 集合不变。新产品名被接受，Mac OS 短格式的识别不同，因此 macOS 改用 Mac OS X 加实际系统版本；Linux 保留原模板。服务端没有从新产品名中提取 app_version，该字段如实保留 null，不为补齐它恢复官方产品名。Windows 未做服务端实测。

本地验证：macOS 平台字段调整后，工作区测试 170 项通过、2 项按既有约定忽略，clippy 与格式检查通过。acceptance 启动器的 Playwright 合成验收通过；该配置补上 zh-CN，避免测试按中文按钮操作时因浏览器默认英文而超时。独立 agent-browser 检查会话已关闭，独立验收实例已加载调整后的二进制。

真实流程：A/B 的普通授权及换票、B 的 `reauth=remote_control` 二次授权、OUT 的 enroll/start 与 enroll/finish 均已成功。用户在新 OUT 完成电脑配对，快照确认唯一目标、电脑在线、没有未完成配对或错误。读取该新注册的设备元数据，实际返回 macOS / macos / 27.0.1 / Mac，app_version 为 null，与临时注册对照一致。随后以 A 创建测试 IN，绑定到本轮 OUT 并启用；运行时首次取得会话必须完成 OUT refresh/start、refresh/finish 和 IN server/refresh，已观察到 credentials.refreshed；两侧 WebSocket 握手成功并报告 connected。此处验证的是启动时真实注册刷新，不是等待自然到期后的定时续期。

用户随后完成控制端配对和实际对话列表、消息收发验证。后端快照确认唯一配对设备为 iPad（platform=ios）、电脑在线、连接仍为 connected，无账号登录标记、配对错误或故障归属；观察到一次 credentials.refreshed、一次 bridge.connected，没有 credentials.refresh_retry。真实 iPad → 桥接 → Mac 的基本链路验收通过，依据为用户的实际收发确认和后端状态，不读取或记录聊天内容。Linux 只验证上表的 UA 解析，未跑完整链路；Windows 未实测，未等待注册自然到期。

验收后清理：先停测试连接，再通过管理 API 删除本轮新建 IN 和 OUT。确认 IN 环境下设备权限列表为空，新 OUT 注册从全局设备列表消失；A 的全局设备 ID 集合不变，B 除该新 OUT 外的设备 ID 集合不变。独立实例中剩余端点和连接均为零，随后关闭该实例的前后端及临时后台任务。未操作日常桥接；账号状态保留在被忽略的私有验收目录中。
