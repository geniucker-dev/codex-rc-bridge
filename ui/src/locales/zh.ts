export const zh: Record<string, string> = {
  "Checking…": "正在检查…",
  Connections: "连接",
  "Input IN": "入口 IN",
  "Output OUT": "出口 OUT",
  Accounts: "账号",
  Settings: "设置",
  "Your management session has expired. Sign in again.":
    "管理登录已失效，请重新登录。",
  Quit: "退出",
  "Stop bridging and quit": "停止桥接并退出",
  "Authorization saved.": "授权已保存。",
  "This authorization has expired or was canceled. Start again.":
    "这次授权已过期或已取消，请重新发起授权。",
  "Cancel authorization": "取消授权",
  "Codex RC Bridge": "Codex RC Bridge",
  Desktop: "桌面版",
  Web: "网页版",
  "Closing the window keeps the bridge running in the menu bar.":
    "关闭窗口后，桥接会继续在菜单栏运行。",
  "Sign out": "退出登录",
  "Connect across accounts": "跨账号连接远程设备",
  "Bridge remote control between different accounts.":
    "在不同账号之间建立远程控制连接。",
  "Connecting to the service": "正在连接服务",
  "Management is not configured": "管理服务尚未配置",
  "Cannot connect to the service": "暂时无法连接服务",
  "Configure management sign-in to add accounts, inputs and outputs.":
    "当前服务只提供工程状态。管理登录配置完成后，可在这里添加账号、入口和出口。",
  "Checking management access.": "正在检查管理入口。",
  "Check again": "重新检查",
  "Sign in to management": "登录管理台",
  "Use the credentials provided by your deployment administrator.":
    "使用部署管理员为你创建的登录信息。",
  Username: "用户名",
  Password: "密码",
  "Sign in": "登录",
  "Management navigation": "管理导航",
  "Set the network proxy used by this application.":
    "设置此应用连接服务时使用的网络代理。",
  "Network settings apply to the whole deployment and require administrator access.":
    "配置整个部署的网络连接，仅部署管理员可操作。",
  "Refresh status": "刷新状态",
  "Status is up to date": "状态实时更新",
  "Connecting to status updates…": "正在连接实时状态…",
  "Status updates stopped. The information below may be outdated. Refresh before acting.":
    "状态更新中断，下面可能是旧数据。请刷新状态后再操作。",
  "Loading your configuration…": "正在读取你的配置…",
  "Service status is unavailable.": "服务状态暂不可用。",
  "Retry registration": "重试注册",
  "Generate pairing code": "生成配对码",
  "Pairing code generated.": "配对码已生成。",
  "Add controller": "添加控制端",
  "Status subscription closed.": "状态订阅已关闭。",
  "Only deployment administrators can view or change this.":
    "只有部署管理员可以查看或修改这项内容。",
  "Invalid proxy address. Use http://host:port without a path, query or fragment.":
    "代理地址无效，请使用 http://主机:端口，不要包含路径、查询参数或片段。",
  "Cannot reach auth.openai.com. Check the server's network and proxy, then start a new authorization with a new callback.":
    "无法完成与上游授权服务器的通信。请检查后端到 auth.openai.com 的网络和代理；若在提交回调时失败，请重新发起授权，不要重复提交旧回调。",
  "The authorization service rejected the request. Check the server's proxy and authorize again.":
    "上游授权服务器拒绝了请求。可能是授权码已失效、凭据被撤销或网络出口受限。请检查后端到 auth.openai.com 的代理，再重新发起授权。",
  "Credentials are missing, expired or belong to another user or workspace. Check the account and authorize again.":
    "账号凭据缺失、已失效，或返回的用户/工作区身份不匹配。请确认账号与工作区后重新授权。",
  "This authorization has expired, was used or no longer exists. Start again with a new callback.":
    "这次授权已过期、已使用或不存在，请重新发起授权并使用新的回调。",
  "The callback does not match this authorization. Paste the complete callback URL from this attempt.":
    "回调地址与本次授权不匹配或格式不正确，请粘贴本次授权产生的完整回调地址。",
  "Authorization was not approved. Start again and complete the confirmation.":
    "授权未获同意，请重新发起授权并完成确认。",
  "The authorization response could not be read. Try again; contact your administrator if this continues.":
    "上游返回的授权数据无法识别，请重新发起授权；若再次失败，请联系管理员排查。",
  "The registration could not be verified as belonging to this output. Check its original client ID.":
    "未能确认该注册属于此出口的原身份。请核对原 client ID，不能用其他出口或被控端的编号替代。",
  "Your session expired or the username or password is incorrect. Sign in again.":
    "登录已失效或用户名、密码不正确，请重新登录。",
  "Too many sign-in attempts. Try again in a minute.":
    "登录尝试过于频繁，请一分钟后重试。",
  "Some information is missing or invalid. Check it and try again.":
    "提交内容不完整或格式不正确，请检查后重试。",
  "Request verification failed. Refresh the page; if this continues, check the management URL.":
    "请求校验失败，请刷新页面后重试；若仍失败，请检查管理入口地址。",
  "Management is temporarily unavailable. Refresh status shortly.":
    "管理服务暂不可用，请稍后刷新状态。",
  "The operation did not complete. Refresh status and check account authorization, device status and bindings.":
    "操作未完成。请检查账号授权、端点状态和绑定是否已变化，再刷新状态。",
  "The service could not complete the request. Refresh status and check again.":
    "服务未能完成请求，请刷新状态后检查。",
  "Cannot read service status. Check your connection and refresh.":
    "无法读取服务状态，请检查连接后刷新。",
  "No result was received. The service may still be processing. Refresh status before submitting again.":
    "未收到操作结果。服务可能仍在处理，请先刷新状态，避免重复提交。",
  "Reload network settings": "重新读取网络设置",
  "Loading network settings…": "正在读取网络设置…",
  "Network proxy": "网络代理",
  "Shared by all accounts in this application.": "此应用的所有账号共用。",
  "Shared by the whole deployment. Only administrators can change it.":
    "整个部署共用，仅部署管理员可以修改。",
  "Used for account authorization, renewal and remote-control connections.":
    "覆盖账号授权、续期和远程控制连接。",
  "Currently active: ": "当前生效：",
  "Direct connection": "直连",
  " (proxy authentication configured)": "（已配置代理认证）",
  "Choose a network connection before adding ChatGPT accounts.":
    "首次使用请先选择网络方式，再添加 ChatGPT 账号。",
  "Save and apply proxy": "保存并应用代理",
  "Network settings applied. Some connections could not recover. Check their authorization and status on Connections.":
    "网络设置已保存并生效，部分链路未能恢复，请到连接页检查授权和状态。",
  "Network settings applied. Previously enabled connections will reconnect; stopped connections remain stopped.":
    "网络设置已保存并生效，原先启用的链路已安排重新连接，暂停的链路保持暂停。",
  "Connection method": "连接方式",
  "HTTP proxy": "HTTP 代理",
  "Proxy address": "代理地址",
  "Use http://host:port, optionally with a username and password. Credentials are not displayed after saving; enter the full address when changing an authenticated proxy. SOCKS and HTTPS proxies are not supported.":
    "填写 http://主机:端口，可包含用户名和密码。保存后认证信息不会回显；修改含认证的代理时，请重新输入完整地址。目前不支持 SOCKS 或 HTTPS 类型代理。",
  "Applying settings briefly disconnects all active bridges in ":
    "应用设置会短暂断开",
  "this application": "此应用",
  "this deployment": "本部署",
  "Save and apply": "保存并应用",
  "Checking controlled device": "正在确认被控端状态",
  "Checking whether the controlled device is online. The bridge will connect automatically once confirmed.":
    "正在确认被控端是否在线，确认后会自动连接。",
  "Controlled device offline": "被控端离线",
  "Open Codex on the controlled device and enable remote control. The bridge will reconnect when it is online.":
    "请在被控端打开 Codex 并启用远程控制；恢复在线后会自动连接。",
  Connecting: "正在建立连接",
  "The controlled device is online. Connecting both sides.":
    "已确认电脑在线，正在连接两侧服务。",
  Connected: "连接可用",
  "The connection is ready. Open the remote device in ChatGPT on your controller.":
    "连接已就绪，可在控制端的 ChatGPT 中进入远程设备。",
  Reconnecting: "正在重新连接",
  Stopped: "已暂停",
  "Access denied": "服务拒绝访问",
  "Configuration needs attention": "本地配置需要修复",
  "Connection verification failed": "连接校验失败",
  "Unexpected protocol data or device proof stopped the connection. Retry; check diagnostic logs if this continues.":
    "收到的协议数据或设备证明不符合预期，连接已停止。可重试；持续失败时请检查诊断日志。",
  "Connection stopped": "连接已停止",
  Switching: "正在切换",
  "Prepare authorization": "准备授权",
  "Authorization link ready.": "授权链接已准备好。",
  "Authorization required": "待授权",
  "Add account": "添加账号",
  "Retry connection": "重试连接",
  "Check ": "检查",
  output: "出口",
  "Stop bridging": "停止桥接",
  " ": "个",
  input: "入口",
  "Add ": "添加",
  controller: "控制端",
  "controlled device": "被控端",
  "Controlled device: ": "被控端：",
  "Restore registration record": "补回注册记录",
  "Sign in to associated account": "重新登录所属账号",
  "Retry cleanup": "重试清理",
  Delete: "删除",
  " accounts": "个账号",
  "Connect your ChatGPT accounts": "先连接你的 ChatGPT 账号",
  "Add the accounts used by your controller and controlled device. They can be the same or different.":
    "添加控制端和被控端使用的账号，可以相同，也可以不同。",
  "No email provided for this account": "此账号暂未提供邮箱",
  "Account details": "账号详情",
  "User ID: ": "用户 ID：",
  "Workspace ID: ": "工作区 ID：",
  "Check account sign-in": "检查账号登录",
  "Account credentials are valid. This does not mean the controlled device is online.":
    "账号凭据可用。此检查不代表被控端在线。",
  "Check sign-in": "检查登录",
  "Sign in again": "重新登录",
  "Close dialog": "关闭对话框",
  Close: "关闭",
  Account: "所属账号",
  "Choose account": "选择账号",
  "Fixed name": "固定名称",
  "Complete ChatGPT authorization": "完成 ChatGPT 授权",
  "Submit authorization": "提交授权",
  "Sign in with the correct account in your system browser, then return here. Authorization is saved automatically. You can also submit the callback URL manually.":
    "在系统浏览器登录对应账号，完成后返回此窗口，授权会自动保存。也可以手动提交回调地址。",
  "The local callback port is busy. Finish authorization in your browser, then copy the complete callback URL below.":
    "本地回调端口被占用。请在浏览器完成授权，再复制完整回调地址提交到下方。",
  "Sign in with the correct account in a new window. Paste the complete callback URL from the address bar below, even if the localhost page cannot open.":
    "在新窗口登录对应账号。完成后，将地址栏中的完整回调地址粘贴到下方；localhost 页面打不开也可以。",
  "Open authorization page": "打开授权页面",
  "Copy authorization link": "复制授权链接",
  "Authorization link copied.": "授权链接已复制。",
  "Clipboard access was denied. Select the link below and copy it manually.":
    "浏览器未允许访问剪贴板，请选中下方链接手动复制。",
  "Authorization link (copy manually)": "授权链接（手动复制）",
  "This link expires at ": "此链接在",
  " and is only valid for this authorization.": "到期，仅用于本次授权。",
  "Complete callback URL": "完整回调地址",
  "Save authorization": "保存授权",
  ". The pairing code is not retained after closing this dialog.":
    "到期。关闭此窗口后不保留配对码。",
  Done: "完成",
  "Controlled device": "被控端",
  Output: "使用的出口",
  "Disconnect and switch": "断开并切换",
  "Verify original registration": "核验原注册",
  "Original registration client ID": "原注册 client ID",
  "Verify and restore record": "核验并补回记录",
  endpoint: "端点",
  "Stop and delete": "停止并删除",
  "{0}…": "正在{0}…",
  "{0} Status could not be refreshed. Check again.":
    "{0} 状态刷新失败，请重新检查。",
  "{0} completed.": "{0}完成。",
  "{0} · Workspace {1}": "{0} · 工作区 {1}",
  "Account {0} · Workspace {1}": "账号 {0} · 工作区 {1}",
  "Connection {0}": "连接 {0}",
  "Delete {0}": "删除 {0}",
  "Restore registration for {0}": "补回 {0} 的注册记录",
  Language: "语言",
  "Follow system": "跟随系统",
  General: "通用",
  "Accounts: {0}": "{0} 个账号",
  "Check account": "检查账号",
  "The application is exiting or the window is unavailable.":
    "应用正在退出或窗口不可用。",
  "The authorization link must use the official authorization service.":
    "授权链接必须指向官方授权服务。",
  "Invalid authorization link.": "授权链接无效。",
  "Cannot start the application. Check Keychain and local configuration.":
    "无法启动桌面应用，请检查钥匙串和本地配置。",
  "Cannot open the desktop database.": "无法打开桌面数据库。",
  "Cannot open the system browser. Copy the authorization link instead.":
    "无法打开系统浏览器。请复制授权链接。",
  "The desktop operation did not complete.": "桌面操作未完成。",
  "Another process is using the desktop configuration.":
    "桌面配置正在被另一个进程使用。",
  "Invalid proxy address. Use http://host:port.":
    "代理地址无效，请使用 http://主机:端口。",
  "Invalid proxy configuration.": "代理配置无效。",
  "The original device identity is missing. Restore the database and original Keychain key.":
    "原设备身份记录缺失。请恢复数据库及原钥匙串密钥。",
  "The application is exiting.": "应用正在退出。",
  "Authorization is being saved. Wait for the result.":
    "授权正在保存，请等待结果。",
  "The submitted information is incomplete or invalid.":
    "提交内容不完整或格式不正确。",
  "The operation did not complete.": "操作未完成。",
  "Cannot save proxy configuration.": "无法保存代理配置。",
  "Cannot create the local device identity. Check the disk and retry.":
    "无法创建本地设备身份，请检查磁盘后重试。",
  "Cannot read proxy configuration.": "无法读取代理配置。",
  "Cannot read desktop state.": "无法读取桌面状态。",
  Local: "本机",
  "The desktop state directory must contain exactly one local user.":
    "桌面状态目录应只有一个本地用户。",
  "The bridge could not shut down cleanly.": "桥接退出失败。",
  "The Keychain key cannot decrypt the device identity.":
    "钥匙串密钥无法解密设备身份。",
  "Cannot create the master key.": "无法创建主密钥。",
  "Cannot load the master key.": "无法加载主密钥。",
  "Cannot save the master key to Keychain.": "无法将主密钥保存到系统钥匙串。",
  "Cannot read the original master key from Keychain. Unlock Keychain or restore the original key and retry.":
    "无法读取系统钥匙串中的原主密钥。请解锁钥匙串或恢复原密钥后重试。",
  "This desktop build requires macOS Keychain.":
    "此桌面构建目前支持 macOS 系统钥匙串。",
  "Invalid Keychain master-key format.": "钥匙串主密钥格式无效。",
  "This link expires at {0} and is only valid for this authorization.":
    "此链接在 {0} 到期，仅用于本次授权。",
  "Applying settings briefly disconnects all active bridges in this application, then reconnects them. Tasks already running on the controlled device will continue.":
    "应用设置会短暂断开此应用的桥接，随后使用新设置重连。被控端已开始的任务会继续。",
  "Applying settings briefly disconnects all active bridges in this deployment, then reconnects them. Tasks already running on the controlled device will continue.":
    "应用设置会短暂断开本部署的桥接，随后使用新设置重连。被控端已开始的任务会继续。",
  Online: "在线",
  Offline: "离线",
  Checking: "确认中",
  "Unnamed device": "未命名设备",
  "Device ID": "设备 ID",
  "Status updates stopped": "状态更新已中断",
  "Checking status every few seconds": "每隔几秒检查一次状态",
  Cancel: "取消",
  "Connect and start": "连接并启动",
  Connection: "连接",
  Controllers: "控制端",
  Device: "设备",
  Input: "入口",
  Manage: "管理",
  More: "更多",
  "A controlled device reaches one computer running Codex. Choose the account signed in on that computer; it can differ from the controller's.":
    "被控端连到一台运行 Codex 的电脑。选择那台电脑上登录的账号，可以和控制端不同。",
  "Access is still being revoked. It stays stopped until cleanup finishes; its name can't be reused until then.":
    "权限仍在撤销中。清理完成前保持停用，名称也暂时不能复用。",
  "Access revoked and deleted.": "已撤销权限并删除。",
  "Account on the computer": "电脑上登录的账号",
  "Add a controlled device": "添加被控端",
  "Add a controller": "添加控制端",
  "Add and authorize": "添加并授权",
  "Add and show code": "添加并显示配对码",
  "Add controlled device": "添加被控端",
  "Add the ChatGPT account first.": "请先添加 ChatGPT 账号。",
  "Authorization needs confirmation": "授权待确认",
  "Authorization recovered using the original device identity and key.":
    "已用原设备身份和密钥恢复授权。",
  "Authorization saved. Pair the computer next.":
    "授权已保存。下一步配对电脑。",
  Authorize: "授权",
  "Back to overview": "返回远程控制",
  "Check controlled device": "检查被控端",
  "Check controller": "检查控制端",
  "Choose one from the list": "从列表中选择",
  Computer: "电脑",
  "Computer ID": "电脑 ID",
  "Computer pairing code": "电脑配对码",
  Connect: "连接",
  "Connect a controller": "连接控制端",
  "Connect a controller to start controlling this computer.":
    "连接一个控制端，开始控制这台电脑。",
  "Connect it to a controlled device to start controlling.":
    "连接到一个被控端，开始控制。",
  "Connect to a device": "连接被控端",
  "Controlled device added. Sign in with the computer's ChatGPT account.":
    "被控端已添加。请用电脑上的 ChatGPT 账号登录授权。",
  "Controlled device {0}": "被控端 {0}",
  "Controlled devices": "被控端",
  Controller: "控制端",
  "Controller {0}": "控制端 {0}",
  "Controller: ": "控制端：",
  "Delete controlled device": "删除被控端",
  "Delete controller": "删除控制端",
  Details: "详细信息",
  Disconnect: "断开连接",
  "Disconnected. Both sides are kept.": "已断开连接，两端都已保留。",
  "Disconnecting the old controlled device and connecting the new one. You may need to reopen the remote device in ChatGPT.":
    "正在断开原被控端并连接新的被控端。可能需要在 ChatGPT 中重新进入远程设备。",
  "Each controlled device drives exactly one computer. If the computer was reinstalled or replaced, pair it again.":
    "每个被控端只对应一台电脑。电脑重装或更换后，重新配对即可。",
  "Every controlled device is in use.": "所有被控端都已连接。",
  "Every controller is connected.": "所有控制端都已连接。",
  "If revocation fails, it stays stopped pending cleanup. Its name can't be reused until cleanup finishes.":
    "撤销失败时会保持停用并等待清理，清理完成前名称不能复用。",
  "Input IN ID": "入口 IN ID",
  "It was deleted. Return to the overview to see what's left.":
    "它已被删除。返回远程控制查看其余内容。",
  "More than one computer appeared after pairing. Pair again so only one computer remains.":
    "配对后出现了不止一台电脑。请重新配对，只保留一台。",
  Name: "名称",
  "New controlled device": "新建被控端",
  "New controller": "新建控制端",
  "No ChatGPT account has been added yet. Add one first.":
    "还没有添加 ChatGPT 账号，请先添加。",
  "No computer paired": "尚未配对电脑",
  "No controlled device is free. Add one, or disconnect another first; each serves one controller at a time.":
    "没有空闲的被控端。请新建一个，或先断开其他连接；每个被控端同时只服务一个控制端。",
  "No controller is free. Add one, or disconnect another first.":
    "没有空闲的控制端。请新建一个，或先断开其他连接。",
  "No other controlled device is free. Each serves one controller at a time.":
    "没有其他空闲的被控端。每个被控端同时只服务一个控制端。",
  "Not connected": "未连接",
  "On the computer, turn on remote control in Codex and show its pairing code. Enter it here.":
    "在电脑的 Codex 中开启远程控制并显示配对码，然后在这里输入。",
  "Original identity verified and cleanup record restored. The controlled device stays stopped. Retry cleanup.":
    "已核验原身份并补回清理记录。被控端保持停用，请重试清理。",
  "Original identity verified and registration restored. Recover authorization, or delete the controlled device to clean up.":
    "已核验原身份并补回注册记录。请恢复授权，或删除被控端完成清理。",
  "Output OUT ID": "出口 OUT ID",
  "Pair computer": "配对电脑",
  "Pair computer again": "重新配对电脑",
  "Paired with {0}.": "已与 {0} 配对。",
  "Recover authorization": "恢复授权",
  "Registration unfinished": "注册未完成",
  "Remote control": "远程控制",
  "Removal pending": "删除待完成",
  "Replace and pair": "替换并配对",
  "Restoring this record leaves the controlled device stopped. It does not change its identity or affect other devices.":
    "补回记录后被控端保持停用，不改变身份，也不影响其他设备。",
  Retry: "重试",
  "Retry. Check diagnostic logs if this continues.":
    "请重试。如果持续出现，请查看诊断日志。",
  "Sign in with the account used on the controlled computer to allow remote control.":
    "用被控电脑上的账号登录，允许远程控制。",
  Start: "启动",
  Stop: "停止",
  "Stop and disconnect": "停止并断开",
  "Stopped, but access revocation is incomplete. Retry cleanup on its page.":
    "已停止，但权限撤销未完成。请在它的页面重试清理。",
  "Switch device": "切换被控端",
  "Switch to": "切换到",
  "Switching disconnects the current computer before connecting the new one. Running tasks continue. You may need to reopen the device in ChatGPT. With ChatGPT Desktop, use a separate controller for each computer.":
    "切换会先断开当前电脑，再连接新电脑，正在运行的任务会继续。可能需要在 ChatGPT 中重新进入设备。使用 ChatGPT Desktop 时，建议每台电脑使用单独的控制端。",
  "The code was accepted, but no computer appeared. Check that remote control is on in Codex, then pair again with a new code.":
    "配对码已接受，但没有出现电脑。请确认 Codex 已开启远程控制，再用新配对码重新配对。",
  "The computer running Codex you want to control. Its ChatGPT account can differ from the controller's.":
    "你要控制的、运行 Codex 的电脑。它的 ChatGPT 账号可以和控制端不同。",
  "The connection failed temporarily. Retrying automatically; the connection is kept.":
    "连接暂时失败，正在自动重试，连接关系保持不变。",
  "The connection is kept. Start it to reconnect.":
    "连接关系已保留，启动即可重新连接。",
  "The controlled device's remote-control authorization is no longer valid. Authorize it again.":
    "被控端的远程控制授权已失效，请重新授权。",
  "The controller or controlled device setup is incomplete. Check both, then retry.":
    "控制端或被控端设置不完整。请检查两端后重试。",
  "The fixed name restores this controller's identity if it's deleted and created again with the same account.":
    "删除后用同一账号和名称重新创建时，会恢复这个控制端的身份。",
  "The fixed name restores this device's identity if it's deleted and created again with the same account.":
    "删除后用同一账号和名称重新创建时，会恢复这个被控端的身份。",
  "The last authorization didn't return a complete result. Recover it with the original identity.":
    "上次授权没有返回完整结果。请用原身份恢复。",
  "The name can't be changed later. Creating it again with the same account and name restores the same identity.":
    "名称之后不能修改。删除后用同一账号和名称重新创建，会恢复同一身份。",
  "The original registration record is missing. Restore it with the client ID from a backup, or create another controlled device with a different name.":
    "原注册记录缺失。请用备份中的客户端 ID 补回，或用其他名称新建被控端。",
  "The previous computer's access couldn't be fully removed yet. The current computer is kept. Try pairing again.":
    "原电脑的访问还没有完全撤销，当前电脑保持不变。请重新配对。",
  "This controlled device no longer exists": "这个被控端已不存在",
  "This controller no longer exists": "这个控制端已不存在",
  "This is the original service registration ID. Ask the administrator for a backup containing it if it is missing.":
    "这是原服务注册 ID。如果缺失，请向管理员索取包含它的备份。",
  "This replaces the current computer, {0}: its access is removed first, then the new code is paired. If the connection is running, it stops and resumes afterwards.":
    "这会替换当前电脑 {0}：先撤销它的访问，再配对新的配对码。如果连接正在运行，会先停止，完成后恢复。",
  "This stops its connection and revokes its access to the computer. The account sign-in and the controller are kept.":
    "这会停止它的连接并撤销它对电脑的访问。账号登录和控制端保留。",
  "Turn on remote control in Codex on the computer, then enter its pairing code.":
    "在电脑的 Codex 中开启远程控制，然后输入它的配对码。",
  "Unknown controlled device": "未知被控端",
  "Unknown controller": "未知控制端",
  "Unnamed computer": "未命名电脑",
  "Use the original client ID from this controlled device's backup. The account and public key must match before the record is saved.":
    "请使用这个被控端备份中的原客户端 ID。账号和公钥一致后才会保存记录。",
  "Work MacBook": "工作 MacBook",
  "the computer": "电脑",
  "{0} ({1})": "{0}（{1}）",
  "across ChatGPT accounts.": "跨 ChatGPT 账号相连。",
  "Account A": "账号 A",
  Bridge: "桥接器",
  "Connects the two accounts": "连接两个账号",
  "Your computer": "你的电脑",
  "Account B, runs Codex": "账号 B，运行 Codex",
  "Re-pairing unfinished": "重新配对未完成",
  "Pairing stopped partway, so the connection can't start. Pair the computer again with a new code.":
    "配对中途停止，连接暂时无法启动。请用新的配对码重新配对电脑。",
  "Registration didn't finish. Retry it, then enter the pairing code on the device you control from.":
    "注册未完成。请重试，然后在用来控制的设备上输入配对码。",
  "The device you control from: a phone, tablet or computer with ChatGPT. The controlled computer appears there as a remote device.":
    "你用来控制的设备，可以是装有 ChatGPT 的手机、平板或电脑。被控电脑会在其中显示为一台远程设备。",
  "Controller added. Enter the pairing code in ChatGPT on the device you control from.":
    "控制端已添加。请在用来控制的设备上打开 ChatGPT，输入配对码。",
  "A controller appears as a remote device in ChatGPT on the device you control from, whether a phone, tablet or computer. Choose the account signed in to ChatGPT there.":
    "控制端会在你用来控制的设备（手机、平板或电脑）的 ChatGPT 中显示为一台远程设备。选择那台设备上 ChatGPT 登录的账号。",
  "Account on the controlling device": "控制设备上登录的账号",
  "My devices": "我的设备",
  "Pair a device": "配对设备",
  "Connected. Open the remote device in ChatGPT on a paired device.":
    "已连接。请在已配对设备的 ChatGPT 中打开远程设备。",
  "Switched. Reopen the remote device in ChatGPT on a paired device.":
    "已切换。请在已配对设备的 ChatGPT 中重新进入远程设备。",
  "This stops the connection. The controller keeps its paired devices and the controlled device keeps its computer, so you can connect them again later. Running tasks continue.":
    "这会停止连接。控制端保留已配对的设备，被控端保留它的电脑，之后可以重新连接。正在运行的任务会继续。",
  "This stops its connection and revokes every paired device's access. The account sign-in and the controlled device are kept.":
    "这会停止它的连接并撤销所有已配对设备的访问。账号登录和被控端保留。",
  "Each controller lets its paired devices, through its ChatGPT account, control a computer that may use another account.":
    "每个控制端让已配对的设备经由它的 ChatGPT 账号，控制一台可能登录着其他账号的电脑。",
  "Sign in to the ChatGPT accounts used on the devices you control from and the computers you control. One account can serve several controllers and controlled devices.":
    "登录控制设备和被控电脑上使用的 ChatGPT 账号。一个账号可以用于多个控制端和被控端。",
  "One device controls another,": "一台设备控制另一台，",
  "Control a computer running Codex from a phone, tablet or another computer, even when each is signed in to a different ChatGPT account. Each user's accounts and devices stay separate.":
    "用手机、平板或另一台电脑控制运行 Codex 的电脑，即使两边登录的是不同的 ChatGPT 账号。每位用户的账号和设备彼此隔离。",
  "Your device": "你的设备",
  "Accounts: {0}, {1} need to sign in again":
    "账号：{0} 个，其中 {1} 个需要重新登录",
  "Authorize again": "重新授权",
  "Check all sign-ins": "检查全部登录",
  "Controller registration lost": "控制端注册失效",
  "Device authorization expired": "被控端授权失效",
  "Every account's sign-in is valid.": "所有账号的登录都有效。",
  "Not used by any controller or controlled device":
    "没有控制端或被控端使用这个账号",
  "Remote control on this device is no longer authorized. Authorize it again with the account on the computer; the connection resumes afterwards.":
    "这个被控端的远控授权已失效。用电脑上登录的账号重新授权，连接随后会自动恢复。",
  "Sign in again; connections using this account resume afterwards.":
    "重新登录后，使用这个账号的连接会自动恢复。",
  "Sign-in expired": "登录已失效",
  "The account on the marked side can no longer be renewed. Sign in to it again and the connection resumes.":
    "标出的一端账号无法续期。重新登录该账号后，连接会自动恢复。",
  "The controlled device's remote-control authorization is no longer valid. Authorize it again and the connection resumes.":
    "被控端的远控授权已失效。重新授权后，连接会自动恢复。",
  "The controller's registration could not be restored. Retry it, pair your devices again, and the connection resumes.":
    "控制端注册无法恢复。重试注册并重新配对设备后，连接会自动恢复。",
  "The marked side was refused access. Fix it there, then retry.":
    "标出的一端被拒绝访问。处理后再重试。",
  "This account's sign-in has expired. Sign in to it again.":
    "这个账号的登录已失效，请重新登录。",
  "{0} accounts failed the check. Any that need to sign in again are marked below.":
    "{0} 个账号检查未通过，需要重新登录的已在下方标出。",
  "{0} can no longer be renewed. Sign in to it again; connections using it resume afterwards.":
    "{0} 的登录无法续期。重新登录后，使用这个账号的连接会自动恢复。",
  "This side was refused access. Check its account and permissions, then retry the connection.":
    "这一端被拒绝访问。检查它的账号和权限后，重试连接。",
  "No device paired": "未配对设备",
  "This controller's registration could not be restored. Retry it, then enter the new pairing code on its device; the connection resumes afterwards.":
    "这个控制端的注册无法恢复。重试注册，再在它的设备上输入新的配对码；连接随后自动恢复。",
  "Pair the device you control from: open remote control in ChatGPT on it and enter the code.":
    "配对用来控制的设备：在它的 ChatGPT 中打开远程控制并输入配对码。",
  "Pair device": "配对设备",
  "Pair device again": "重新配对设备",
  "The current device's access couldn't be fully removed yet, so no new code was issued. Try pairing again.":
    "当前设备的访问还没能完全移除，所以没有生成新的配对码。请重试配对。",
  "More than one device used the code, so all of them were removed. Pair again and enter the code on one device only.":
    "有不止一台设备使用了配对码，已全部移除。请重新配对，并只在一台设备上输入。",
  "Controllers: {0}": "控制端：{0}",
  "Controlled devices: {0}": "被控端：{0}",
  "Open remote control in ChatGPT on it to use this controller.":
    "在它的 ChatGPT 中打开远程控制即可使用。",
  "In ChatGPT on the device you control from, open remote control and enter this code. Enter it on one device only.":
    "在用来控制的设备上打开 ChatGPT 的远程控制，输入这个配对码。只在一台设备上输入。",
  "Expires at {0}. This dialog updates once the device has paired; the code is not shown again after closing.":
    "{0} 过期。设备配对后这里会更新；关闭后不会再次显示这个配对码。",
  "This replaces the current device, {0}: its access is removed first, then a new code is shown. Use this when the device was changed, reset or lost.":
    "这会替换当前设备 {0}：先移除它的访问，再显示新的配对码。适用于换了设备、设备重置或丢失的情况。",
  "Remove and pair": "移除并配对",
  "Each controller pairs with exactly one device: a phone, tablet or computer with ChatGPT. To control another computer from the same device, create another controller.":
    "每个控制端只配对一台设备：装有 ChatGPT 的手机、平板或电脑。同一台设备要控制另一台电脑时，再新建一个控制端。",
  Platform: "平台",
  "More than one device can use this controller. Pair again to keep only one.":
    "有不止一台设备可以使用这个控制端。重新配对，只保留一台。",
  "More than one device": "多台设备",
  "Last connected": "上次连接",
  "Authorized device": "已授权设备",
  "This computer no longer allows this controlled device. Pair the computer again.":
    "这台电脑已不再允许这个被控端访问。请重新配对电脑。",
  "Another computer can also be reached through this controlled device. Pair again so only one computer remains.":
    "通过这个被控端还能访问另一台电脑。请重新配对，只保留一台。",
  "Computer unavailable": "电脑不可用",
  "More than one computer": "多台电脑",
  "Last online": "上次在线",
  Users: "用户",
  "Users: {0}": "用户：{0}",
  "Add user": "添加用户",
  "Reload users": "重新加载用户",
  "Loading users…": "正在加载用户…",
  You: "你",
  "Administrator. Manage on the server's command line.":
    "管理员，在服务器命令行中管理。",
  "Has their own accounts and devices.": "拥有自己的账号和设备。",
  "Sign-in is locked for a minute after too many wrong passwords. Resetting the password unlocks it.":
    "密码输错次数过多，登录已锁定一分钟。重置密码可立即解锁。",
  "Reset password": "重置密码",
  "Delete user": "删除用户",
  "The two new passwords don't match.": "两次输入的新密码不一致。",
  "Type it again": "再输入一次",
  "At least 12 characters.": "至少 12 个字符。",
  "Deleted {0} and all of their data.": "已删除 {0} 及其全部数据。",
  "This signs {0} out and deletes their ChatGPT sign-ins, controllers, controlled devices and connections from this server. Their connections stop. It can't be undone.":
    "这会让 {0} 退出登录，并从这台服务器删除其 ChatGPT 登录、控制端、被控端和连接。正在运行的连接会停止。此操作无法撤销。",
  "Nothing is revoked with ChatGPT, so their ChatGPT apps may still list the old remote devices. Those no longer work and can be removed there.":
    "删除时不会在 ChatGPT 那边撤销授权，所以对方的 ChatGPT 里可能还留着旧的远程设备。这些设备已经无法使用，可以在 ChatGPT 中移除。",
  "Type {0} to confirm": "输入 {0} 确认",
  "Added {0}. Give them the username and password to sign in.":
    "已添加 {0}。把用户名和密码交给对方即可登录。",
  "The new user gets a separate space with their own ChatGPT accounts and devices. They can change the password after signing in.":
    "新用户有独立的空间，使用自己的 ChatGPT 账号和设备。登录后可以自己修改密码。",
  "Lowercase letters, digits, underscores, dots and hyphens.":
    "可用小写字母、数字、下划线、点和连字符。",
  "Reset password for {0}": "重置 {0} 的密码",
  "Password reset. {0} has been signed out everywhere.":
    "密码已重置，{0} 已在所有浏览器上退出登录。",
  "{0} is signed out on every browser and needs the new password to sign in. Their connections keep running.":
    "{0} 会在所有浏览器上退出登录，需要用新密码重新登录。其连接会继续运行。",
  "New password": "新密码",
  "Change password": "修改密码",
  "Password changed. Other browsers signed in as you have been signed out.":
    "密码已修改，你在其他浏览器上的登录已退出。",
  "You stay signed in here. Other browsers signed in as you are signed out.":
    "当前浏览器保持登录，其他浏览器上的登录会退出。",
  "Current password": "当前密码",
  "Each user has a separate sign-in and their own accounts and devices. Only administrators can add or remove users.":
    "每个用户单独登录，各自管理自己的账号和设备。只有管理员可以添加或删除用户。",
  "Use up to 64 lowercase letters, digits, underscores, dots or hyphens for the username.":
    "用户名最多 64 个字符，只能用小写字母、数字、下划线、点和连字符。",
  "Use a password of at least 12 characters.": "密码至少需要 12 个字符。",
  "That username is already taken. Choose another.":
    "这个用户名已被使用，请换一个。",
  "This user no longer exists. The list has been refreshed.":
    "该用户已不存在，列表已刷新。",
  "Administrators can only be managed from the command line on the server.":
    "管理员只能在服务器命令行中管理。",
  "The current password is incorrect. Too many wrong attempts lock sign-in for a minute.":
    "当前密码不正确。输错次数过多会锁定登录一分钟。",
};
