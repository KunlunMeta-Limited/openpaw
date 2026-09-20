# OpenPaw 与 AI Office 连接和运行配置设计

> 文档级别：连接配置专题。端口图中的 `4096` 仅代表 Windows native 启动 profile；OpenPaw 通用 Host 默认端口为 `0`，启动后必须使用就绪 URL。当前候选的统一设计与验收结论以 `../../ai-office-react-main/docs/02-设计与实现.md` 和 `04-验收记录.md` 为准。

版本：2026-09-16
适用范围：OpenPaw engine-host、AI Office Gateway、AI Office 桌面页面
变更依据：`CHG-2026-0008`

## 1. 系统连接框图

```text
用户浏览器（React + Pixi）
  |  创建/发送/轮询                    ^  动作轮询与人物动画
  v                                  |
AI Office Gateway :8765  <----- GET /actions
  |  POST /session、/prompt_async      ^
  |  GET /status、/message             | POST /actions
  v                                  |
OpenPaw engine-host :4096 -> Session Runtime -> AgentLoop
                                              |
                                              | OpenAI Chat Completions
                                              v
                         OPENPAW_OPENAI_BASE_URL/chat/completions
```

三个进程只监听 `127.0.0.1`。浏览器不读取密钥，也不直接访问模型服务；Gateway 不执行
AgentLoop，只代理已登记的会话 API；OpenPaw 是 session、工具调用和模型回合的事实源。

## 2. 对话调用链

```text
1. 页面 POST /office/conversations {title}
2. Gateway POST OpenPaw /session {title, provider, model}
3. 页面 POST /office/conversations/:id/messages {text}
4. Gateway POST OpenPaw /session/:id/prompt_async
5. Host 根据 session provider 创建 LLM provider
6. OpenAiProvider POST <base URL>/chat/completions，Authorization: Bearer <key>
7. 页面轮询 Gateway status/messages，直到 idle 后展示助手正文
8. AgentEvent 同步投影为 POST /actions；页面轮询动作并驱动六名人物
```

工具输入、模型思考正文和 API key 不进入 `/actions`。人物只显示“正在搜索、读取文件、撰写文件”等
脱敏活动摘要。Office 动作失败不会改变 AgentLoop 的模型结果。

## 3. 配置加载与优先级

CLI 在解析 provider 前加载 dotenv。查找顺序如下：

1. `OPENPAW_ENV_FILE` 指定的文件；指定但不存在会启动失败。
2. 从当前工作目录向上寻找可识别的 OpenPaw 工程根，只读取该根的 `.env`。
3. 当前目录不在 OpenPaw 工程内时，从可执行文件目录向上寻找可识别的 OpenPaw 工程根。

工程根必须同时存在工作区 `Cargo.toml` 和 `crates/openpaw-cli/Cargo.toml`。程序不会读取任意
工作目录或任意祖先目录的 `.env`，防止在不可信目录启动时导入无关服务地址和凭据。

单个字段的优先级固定为：命令行 > 已存在进程环境变量 > `.env` > 自动推断 > 默认值。
dotenv 不覆盖进程中已有的同名变量。

provider 决策：

| 条件 | 结果 |
| --- | --- |
| 显式 `--provider`/`-Provider` | 使用显式值；真实 provider 缺密钥时中文报错 |
| `OPENPAW_PROVIDER` 有效且有对应密钥 | 使用配置值 |
| 未配置 provider，但存在 `OPENAI_API_KEY` | 自动选择 `openai` |
| 未配置 provider，但存在 `ANTHROPIC_API_KEY` | 自动选择 `anthropic` |
| 不存在可用模型密钥 | 中文提示并选择 `echo`，不会伪装为模型回答 |

## 4. `.env` 字段字典

| 字段 | 必填 | 默认值 | 消费者 | 说明 |
| --- | --- | --- | --- | --- |
| `OPENPAW_PROVIDER` | 建议 | 自动推断 | CLI、Host、Gateway | `openai/anthropic/echo/mock` |
| `OPENAI_API_KEY` | OpenAI 必填 | 无 | OpenPaw、Gateway OCR | 只进入进程内存，不得进入日志/报告 |
| `OPENPAW_OPENAI_BASE_URL` | 兼容服务必填 | OpenAI 官方地址 | OpenPaw、OCR | 应包含服务版本路径，例如 `/v1`，不得带 query/fragment |
| `OPENPAW_OPENAI_MODEL` | 建议 | `gpt-4o` | OpenPaw、Gateway、页面诊断 | 必须是兼容服务实际支持的模型编号 |
| `ANTHROPIC_API_KEY` | Anthropic 必填 | 无 | OpenPaw | Anthropic 密钥 |
| `OPENPAW_ANTHROPIC_BASE_URL` | 否 | Anthropic 官方地址 | OpenPaw | 自定义兼容地址 |
| `OPENPAW_ANTHROPIC_MODEL` | 否 | `claude-sonnet-4` | OpenPaw | 默认模型 |
| `OPENPAW_ENGINE_PERMISSIONS` | 否 | 空 | OpenPaw | `manual` 开启人工工具审批 |
| `OPENPAW_SOURCE_ROOT` | 否 | Kunlun 相邻工程 | 启动脚本 | OpenPaw 源码绝对路径 |
| `AI_OFFICE_SOURCE_ROOT` | 否 | Kunlun 相邻工程 | 启动脚本 | AI Office 源码绝对路径 |
| `OPENPAW_ENV_FILE` | 否 | `<OpenPaw根>/.env` | CLI、启动脚本 | 显式配置文件路径；应通过参数或进程环境传入，不能靠写入自身来定位 |

以下变量由启动脚本自动生成，一般不应写入 `.env`：`OPENPAW_HOME`、
`OPENPAW_OFFICE_WORKSPACE`、`OPENPAW_ENGINE_HOST_URL`、`OPENPAW_OFFICE_ACTIONS_URL`、
`VITE_OFFICE_HTTP_ACTIONS_URL`。它们包含运行目录和本次端口，脚本会保证三个进程一致。

## 5. 前后端配置传递

| 起点 | 传递字段 | 终点 | 用途 |
| --- | --- | --- | --- |
| `.env`/命令行 | provider、model、base URL、key | OpenPaw 子进程 | 构造真实 LLM provider |
| 启动脚本 | `OPENPAW_DEFAULT_PROVIDER`、model、key | Gateway | 创建 session 时补默认值、OCR 代理 |
| 启动脚本 | `VITE_OFFICE_DEFAULT_PROVIDER/MODEL` | Vite | 仅用于前端构建期诊断，不含 key |
| 启动脚本 | Host/Gateway URL | Gateway、Vite | 保证代理和页面请求指向同一端口 |
| 启动脚本 | `OFFICE_ALLOWED_ORIGINS` | Gateway | 自动加入当前 Web 端口的 `127.0.0.1` 与 `localhost` Origin |
| Host | `/actions` | Gateway | 人物状态和拜访动作 |

## 6. 安全与失败语义

- `.env` 已由 `.gitignore` 排除；`.env.example` 只能保留空密钥。
- `openpaw config show` 只显示“已配置（值已隐藏）”，不会输出密钥。
- 启动状态日志允许记录 provider、model、配置文件路径和密钥存在性，不记录密钥值。
- 显式真实 provider 缺密钥时退出码为 2；默认启动缺密钥时允许 `echo`，且必须显示中文提示。
- 基础地址解析失败时 Host 返回中文配置错误；不会悄悄把请求发往未知目标。
- `openpaw chat` 任一 JSONL 追加失败时立即返回非零，不再继续运行或显示“会话已保存”。
- dotenv 只允许在 CLI 创建线程前加载；运行期间不支持热切换配置文件。
- Gateway 和 Host 继续绑定回环地址，`.env` 不能改变该安全边界。

## 7. 兼容性和迁移

旧的 `KUNLUN_PROVIDER`、`KUNLUN_MODEL`、`KUNLUN_OPENAI_BASE_URL` 在脚本中仍作为兼容别名；
新部署应统一使用 `OPENPAW_*`。HTTP 路由、请求字段、响应字段、JSONL 和 Office 动作契约均未改变，
已有 session 不需要迁移。

Windows、macOS 和 Linux 都使用 UTF-8 dotenv。Windows 原生入口为 `start-demo-native.ps1`；
Ubuntu/Linux 为 `start-demo.sh`。macOS 可复用 Bash 脚本，但需自行安装 GNU 兼容的 `setsid`，当前没有
单独的 macOS 服务包装器，因此不能声明 macOS 一键启动已经验收。

自定义目录布局时，Windows 原生入口使用 `-OpenPawRoot/-OfficeRoot/-EnvFile`；WSL 包装器支持
同名参数并自动转换为 Linux 路径；Ubuntu 直接设置 `OPENPAW_SOURCE_ROOT`、
`AI_OFFICE_SOURCE_ROOT`、`OPENPAW_ENV_FILE`。两套验收脚本也读取相同的源码根变量，避免启动和
验收指向不同副本。
