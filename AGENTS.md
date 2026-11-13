# 开发约定

用户在对话里的最新明确要求优先于本文件、`docs/agent/` 中的约定和技能建议。两者冲突时按用户的要求做，并说明依据的是哪一条。本机若有 `docs/agent/private/AGENTS.md`，其中的约定同样适用。

## 产品模型

界面和文档只讲两个对象：控制端是一个 IN 加它唯一配对的一台设备（手机、平板或电脑上的 ChatGPT），被控端是一个 OUT 加它唯一的一台电脑。连接把一个控制端接到一个被控端。IN、OUT 是实现层的名字，只在代码、详情页的 ID 和 `docs/agent/` 里出现。设计变化时同步 `docs/agent/PLAN.md` 中对应的接口约定。

## 按任务查阅

- 构建、本地运行、检查命令和打包：[开发指南](docs/guide/development.md)。
- 改动产品行为、接口或界面约定：[计划](docs/agent/PLAN.md)，第 1–2 节是范围与对象，第 8 节是各阶段接口约定和实现记录。
- 改动设备身份派生：[身份契约](docs/agent/IDENTITY.md)。
- 核对上游协议或 Desktop 行为：[实现依据](docs/agent/REFERENCE.md)。
- 改动存储、备份恢复或做负载测量：[存储记录](docs/agent/STORAGE.md)。
- 授权、注册、配对和 CLI 联调的细节：[授权联调](docs/agent/AUTHORIZATION.md)；桌面版实现与验证记录：[桌面记录](docs/agent/DESKTOP.md)；真实设备验收：[验收记录](docs/agent/ACCEPTANCE.md)。

## 工具与环境

前端用 Bun 安装依赖、执行脚本和构建，使用 `bun.lock`，不用 Node、npm、pnpm 或 yarn 运行。Rust 用 Cargo。自动化浏览器测试用 Playwright。交互式浏览器操作用 agent-browser 的独立命名会话，不连接或操作用户自己的浏览器，用完或失败后关闭这个会话。

本地测试使用合成数据和临时端口，不接触真实账号或上游。可以直接运行 cargo test、clippy、Playwright 和本地合成测试，失败就修，修完重跑受影响的部分，不用逐步请示。连接真实账号、真实上游或用户正在使用的桥接，需要用户在对话里明确要求。

给出预览地址前，确认前后端都在运行，并说明怎么启动、要保持什么运行；临时工具进程不算常驻服务。

## 文档

文档分给人看和给 Agent 看两类，写法不同。

给人看的是 `README.md`、`README_zh.md` 和 `docs/guide/` 下的指南，中英双语，英文文件和 `.zh.md`（README 为 `README_zh.md`）内容保持一致。只链接给人看的文档，不链接 `docs/agent/`。写完用 /humanizer:humanizer 处理两种语言，命令、路径和界面按钮名保持原样。

给 Agent 看的是本文件和 `docs/agent/` 下的文档，只用中文。写法参照 GPT-6 Astra 的建议：先写目标和原因，再写约束、可以直接做的事和完成标准；用短段落和直接的陈述，只有并列或有先后顺序的内容才用列表。

## 安全与汇报

真实令牌、私钥、授权回调、配对码和聊天内容不写进代码、测试数据、日志、文档或提交。账号状态目录不纳入版本库。

完成后用简洁的中文说明结果、依据和还没做完的部分。
