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

## 3. 程序配置来源与优先级

OpenPaw 程序只读取当前进程环境，不自动寻找 `.env`、不读取 `OPENPAW_ENV_FILE`，也不通过密钥存在性自动切换 provider。
若启动脚本加载 `.env`，那属于脚本行为；直接运行 `openpaw` 时须先将变量导入进程。

chat 默认 `mock`；Host 使用显式 `--provider`，其次 `OPENPAW_ENGINE_PROVIDER`，最后 `mock`。
网络字段优先级是显式 chat 参数 > 对应 `OPENPAW_*` 进程环境 > 协议默认值。
Host session/per-turn 的 provider/model 覆盖保持原优先级，幂等指纹使用同一有效模型。

```powershell
$env:OPENAI_API_KEY = '<your-key>'
openpaw chat --provider openai --base-url https://example.invalid/v1 --model third-party-model --max-output-tokens 4096
```

OpenAI 地址是 `/chat/completions` 的父路径，自定义服务前缀保留；Anthropic 可给根地址或已带 `/v1` 的地址，尾斜杠会规范化，只生成一层 `/v1/messages`。
地址必须为 HTTP(S)，有 host 且不含内嵌凭据、query 或 fragment。Key/model 必须非空，token 上限须为正 u32。
配置错误在请求前失败，chat 退出 2；运行或持久化错误退出 1。错误只标明字段，密钥不进入诊断。
离线协议忽略网络字段，但仍校验通用生成上限。

`model list` 展示默认和有效配置模型；`config show`、`doctor` 使用相同解析规则，显示本地配置状态并隐藏 Key。
目录诊断不请求上游；doctor 的 WARN 仍退出 0。

chat 的 `--permissions auto|manual` 默认 `auto`。manual 下终端显示工具名，输入 `once` 批准本次，`session` 授权本 session 同工具的后续回合，`reject` 拒绝。
空白、无效输入和 EOF 都拒绝；非 TTY 直接拒绝需审批工具且不读取下一条聊天作为审批。授权不跨 session 或进程重启，不绕过工具自己的工作区或 bash 限制。
Host 的 manual 仍由 `OPENPAW_ENGINE_PERMISSIONS=manual` 开启，并通过既有 permission REST/SSE 交互。

CLI 与 Host 使用同一个 home 时，`session list/show/search --home <home>` 可直接发现 Host JSONL，无需先运行 chat 或启动 Host。
JSONL 是事实源；show/list 只读取快照，search 和 chat 按单会话原子刷新 SQLite 次级索引，遵守 finish commit 与回退可见性。
CLI 查询不追加 interrupted、不截断 JSONL，不删除整个 index.db；索引修复必须保留审计和其他数据。

## 4. 进程环境字段字典

| 字段 | 必填 | 默认值 | 消费者 | 说明 |
| --- | --- | --- | --- | --- |
| `OPENPAW_ENGINE_PROVIDER` | 否 | `mock` | Host 启动入口 | 显式 --provider 优先；chat 只使用自己的 --provider |
| `OPENAI_API_KEY` | OpenAI 必填 | 无 | OpenPaw、Gateway OCR | 只进入进程内存，不得进入日志/报告 |
| `OPENPAW_OPENAI_BASE_URL` | 兼容服务必填 | OpenAI 官方地址 | OpenPaw、OCR | 应包含服务版本路径，例如 `/v1`，不得带 query/fragment |
| `OPENPAW_OPENAI_MODEL` | 建议 | `gpt-4o` | OpenPaw、Gateway、页面诊断 | 必须是兼容服务实际支持的模型编号 |
| `ANTHROPIC_API_KEY` | Anthropic 必填 | 无 | OpenPaw | Anthropic 密钥 |
| `OPENPAW_ANTHROPIC_BASE_URL` | 否 | Anthropic 官方地址 | OpenPaw | 自定义兼容地址 |
| `OPENPAW_ANTHROPIC_MODEL` | 否 | `claude-sonnet-4` | OpenPaw | 默认模型 |
| `OPENPAW_ENGINE_PERMISSIONS` | 否 | 空 | OpenPaw | `manual` 开启人工工具审批 |
| `OPENPAW_SOURCE_ROOT` | 否 | Kunlun 相邻工程 | 启动脚本 | OpenPaw 源码绝对路径 |
| `AI_OFFICE_SOURCE_ROOT` | 否 | Kunlun 相邻工程 | 启动脚本 | AI Office 源码绝对路径 |
| `OPENPAW_ENV_FILE` | 否 | 由脚本确定 | 启动脚本 | OpenPaw 程序自身不消费 |
| `OPENPAW_MAX_OUTPUT_TOKENS` | 否 | `4096` | CLI chat、Host | 正 u32，共用生成配置 |

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
- chat 显式网络 provider 缺密钥时退出 2；默认 mock 不访问网络；Host 接收缺配置的模型请求返回 400。
- 基础地址解析失败时 Host 返回中文配置错误；不会悄悄把请求发往未知目标。
- `openpaw chat` 任一 JSONL 追加失败时立即返回非零，不再继续运行或显示“会话已保存”。
- 程序不自动加载 dotenv；入口配置不支持文件热切换。
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

## 图片与原生音频配置（CHG-2026-0026）

`OPENPAW_MODEL_CAPABILITIES` 是按 provider / model 键登记的 JSON。`input` 允许
`text`、`image`、`audio`，`context_window` 可选且必须为正整数；缺省预算 128000。
未声明的模型只接收文本。声明是本地配置，不能证明上游支持；Anthropic 的 audio 声明被拒绝，
mock/echo 只允许 text。CLI model list / config show、Host /provider 展示同一输入声明。

```powershell
$env:OPENPAW_MODEL_CAPABILITIES = '{"openai":{"vision-model":{"input":["text","image"],"context_window":128000},"audio-model":{"input":["text","audio"]}},"anthropic":{"claude-vision":{"input":["text","image"],"context_window":200000}}}'
openpaw chat --provider openai --model vision-model --attach "C:\images\screen.png"
openpaw chat --provider openai --model audio-model --attach "C:\recordings\meeting.wav"
```

沿用已有 Key、base URL 和输出上限配置；`--attach` 可重复。输入文字发送所有待发送附件，
`/attach <path>` 添加，`/send` 只发附件，`/clear-attachments` 清空，EOF 不发。
工具 `read_file` 读取工作区图片或录音并返回内联原件；文本仍使用行号与 offset/limit，媒体忽略行范围。
`read_attachment` 只接受当前有效历史中的 `attachmentID`，压缩后也可回看，不接受其他 session 或路径。
两类工具均沿用审批、工作区和取消规则。

Host `/session/:id/message` 或 `/prompt_async` 请求示例：

```json
{"messageID":"client-media-1","parts":[{"type":"text","text":"描述附件"},{"type":"file","mime":"image/png","filename":"screen.png","url":"data:image/png;base64,<原件的标准Base64>"}]}
```

只发 file 也合法。音频 MIME 为 audio/wav 或 audio/mpeg，兼容 audio/x-wav、audio/mp3 别名。
直接 text 字段优先于 text parts；file 保持输入顺序。远程 URL 不下载。
PNG/JPEG/WebP/GIF 图片 5 MiB（GIF 请求保留首帧，历史保存全部原件）、WAV/MP3 音频 20 MiB；每回合最多 4 个不同附件、原件总计20 MiB。
重复读取同一附件编号不重复计数；读取同一文件产生新编号。请求体上限32 MiB。错误媒体/能力返回400，
字节超限413，接收前拒绝不增加消息；同幂等键改变内容、位置、名称或模型返回409。

原件内联到 JSONL，摘要保留稳定编号；压缩不改变公共完整历史。
图片协议为 Chat image_url 或 Anthropic source，音频为 Chat input_audio 的 wav/mp3；回复仍为文字。
Chat 工具结果保留文本和调用 ID，随后媒体 user 输入只是协议适配，不形成新用户回合。
暂存回退保留 REST 原记录并提供 revert 游标，模型与分支只使用有效前缀。
本轮仅本地契约验收，实际模型理解及第三方接受行为尚待验证。

## AI PC 专属工具连接

适配源码基线为 `7c8262ee84eb77508a2da200714408e5f3aa5ebe`，独立分支 `feature/ai-pc-context-tools`（CHG-2026-0026）。AI PC 自动为自身创建的空闲会话登记 Windows 临时桥接端口，Docker Desktop 使用 `host.docker.internal`。Host 必须已启用 Basic 鉴权；该前提与模型运行在本地还是云端无关，权限由桌面的接收服务配置决定。

桥接凭据保存在 Host 会话登记表内存，不使用进程环境；重启后 AI PC 在下一次自己会话的问答前重新登记。旧 Host 无登记接口时 AI PC 明确提示工具不可用，已有文字问答仍可运行。

部署需在用户完成受保护接口审阅后单独批准。验证前记录现有镜像 identity、配置和卷映射；仅替换主 engine-host 镜像并保留配置/会话卷。先在独立测试 Host 验证 Basic、登记、默认会话隔离与容器到 Windows 桥接，再对批准后的主服务复验。回滚恢复原镜像，停止 AI PC 专属工具，保留会话数据。

多模态与 AI PC 工具的集成分支为 `merge/multimodal-ai-pc-context-tools`（CHG-2026-0028），起点多模态 `8d88e44`，合入 AI PC `a6b4703`；两份来源 CHG-0026 以完整标题区分。媒体能力声明与模型配置沿用多模态分支，桌面登记不会扩展模型能力或桌面外发权限。

Everypaw 实时列窗源码基于 `c105d80356f7217138d3b70bfab1d72d5a271f4d`，任务分支 `feature/everypaw-visual-06`，集成分支 `feature/everypaw-desktop-visual-integration`（CHG-0029）。构建仍显式启用 `engine-host`。登记 204 的 version/tools 响应头是实际能力协商，缺头是 legacy，不能按 Host 在线或普通文字成功推断实时工具可用。

桌面桥继续绑定 `0.0.0.0` 随机端口并返回 `http://host.docker.internal:PORT/ai-pc`，原生 IPC 仍只验证本机对端，不增加网络原生入口。回环受控测试仅证明协议/授权/证据逻辑；当前 Docker Desktop Linux pipe 不可用，真实容器可达验证待环境恢复。本轮只构建隔离源码与测试，不替换现有服务、镜像、数据卷或 DeepSeek 云端配置。

07 结构工具工作分支为 `feature/everypaw-visual-07`，基于已完成 06 的 `4acf3c8`；Everypaw 对应 `feature/desktop-structure-07`。Host 回执明确声明 `ai_pc_query_structure` 后，模型才能使用原生 generation/target_id 按需查询实时结构；其4096估算 token 投影限制独立于实际取得容量，部署和模型配置仍沿用上述独立门禁。

任务 10 的 `feature/everypaw-visual-10` 从同一引擎集成继续增加调用／点击批次。无需新环境变量或动作权限开关，沿用既有工具 auto/manual 设置与 Basic 登记。真实鼠标输入可能改变桌面焦点；停止是独立控制路径，不宣称能抢占正在同步执行的 UIA 调用。未部署此分支，所选 DeepSeek 云端配置保持原值。

09目标PNG复用所选provider/model的 `OPENPAW_MODEL_CAPABILITIES` image声明；未知或仅text在捕获前失败。配置声明只证明本地允许，实际第三方接受与理解需独立验收。现有DeepSeek配置不因新增工具自动获得image声明；没有改动其生产配置。原图按5 MiB/张等既有媒体边界，不自动缩放。

08正文分页分支为 `feature/everypaw-visual-08` / Everypaw `feature/desktop-text-08`，基于集成 `1b2312e` / `8be6fcb`并继续合入12。回执增加 `ai_pc_read_text`；UTF-16页长、原生有界probe、10240估算token模型副本分别见protocol/CHG0029。没有新服务端口、推理模型、OCR、配置开关或部署动作；当前DeepSeek服务保持用户既有选择，受控模型仅供隔离验收。
