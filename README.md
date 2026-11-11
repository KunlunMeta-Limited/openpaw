# OpenPaw

OpenPaw 是用 Rust 实现的个人 AI Agent 运行框架。CLI 和可选 Engine Host 共用执行引擎、模型适配、工具与存储；Host 为独立的 AI Office 前端提供多 Agent 协作、审批、状态和成果适配。

支持 Linux 与 Windows 10/11 x64。macOS 不属于当前支持和验收范围。项目不包含遥测上报；模型请求、联网工具及用户显式装配的扩展仍会访问相应服务。

## 快速开始

先安装 Git、Rust stable（最低 1.89）和原生编译工具。Linux 需要 C/C++ 工具链及 pkg-config；Windows 使用 Rust MSVC 工具链及 Visual Studio Build Tools 的 C++ 工作负载和 Windows SDK。Office 另外需要 Node.js 24 LTS/npm，Linux 启动脚本需要 bash、curl 和 ss 或 lsof。

仅运行离线 CLI：

```sh
cargo build --locked -p openpaw-cli
cargo run --locked -p openpaw-cli -- hello
```

`hello` 是离线 Echo 演示，不是实际 AI 回答。真实模型使用 `chat` 和下面的模型配置。

### Office 全新安装

建议两个工程并列：

```text
workspace/
├── openpaw/          本仓库
└── ai-office-react/  独立上游 Office
```

在本仓库根目录执行。Linux：

```sh
scripts/openpaw-office.sh setup
scripts/openpaw-office.sh status
scripts/openpaw-office.sh stop
```

Windows PowerShell 5.1 或 7，无需 WSL：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\openpaw-office.ps1 setup
.\scripts\openpaw-office.ps1 status
.\scripts\openpaw-office.ps1 stop
```

setup 会提示协议、模型、基础 URL、密钥和权限选择，生成本机配置、独立 Office 服务密钥、空白会话目录及 32 个预授权工作目录。它不会复制作者聊天、文章或凭证。已有完整配置会保留，不会覆盖；不完整配置明确报错。

权限选择是“逐次审批写文件/修改/绘图”或“持久允许这些工具”；仅作用于授权工作目录，每次调用保留审计，不开放 Shell。交互补充信息可单独选择，回答问题不会批准工具。

Office 缺失时，setup 经确认克隆并检出[来源清单](docs/engine-host/contracts/sources.json)中的固定提交；已有目录不会自动更新。**下载 Office 需要相应仓库访问权限。** 合同测试携带许可允许的协议快照，不要求访问完整 Office 仓库。

下一步可选择只生成配置、安装依赖并编译，或编译并启动。setup 不发送付费模型请求，成功不代表密钥/余额已验证。模型与 Office 都需可用才有真实对话。

### 编译与运行

Linux：

```sh
scripts/openpaw-office.sh build
scripts/openpaw-office.sh start
scripts/openpaw-office.sh status
scripts/openpaw-office.sh stop
```

Windows：

```powershell
.\scripts\openpaw-office.ps1 build
.\scripts\openpaw-office.ps1 start
.\scripts\openpaw-office.ps1 status
.\scripts\openpaw-office.ps1 stop
```

`build` 编译启用 engine-host/sidecar 的 debug 程序及 Office；`start` 使用现有构建，不自动编译或替换运行中的服务。停止先等待清理；仅确认确需强制终止时使用 Linux `stop --force` 或 Windows `stop -Force`。

默认页面：`http://127.0.0.1:8765/office/`；单办公室入口：`http://127.0.0.1:8765/office/?conference=0`。总览和本地 mock 演示由上游 Office 提供；当前已验证上游的真实创建入口在单办公室菜单。它们不等于 Host 的真实空间，不能将本地演示回复当模型结果。

默认 Host 健康检查为 `http://127.0.0.1:32885/health`。端口覆盖应在 setup 和后续命令使用相同变量，并与已有 TOML 的 Office 地址一致。

## 配置和数据目录

setup 默认使用用户目录的 `.openpaw`，不是仓库或临时目录：

| 路径 | 用途 |
|---|---|
| `.env` | 模型协议、名称、URL 和密钥，仅本机使用 |
| `engine-host-office.toml` | Host 数据根、工具、审批、工作区及并发配置 |
| `.office-service-key` | Office 服务独立凭证，不能用模型密钥替代 |
| `office-host-home/` | Host 会话、任务、审批、账本、索引和成果版本 |
| `workspaces/office-1` 至 `office-32` | 文章、图片等用户工作文件 |

Linux PID/日志默认在 `$XDG_RUNTIME_DIR/openpaw-office-<uid>`，无此变量时为 `/tmp/openpaw-office-<uid>`；Windows 在 `%LOCALAPPDATA%\OpenPaw\office-runtime`。仅运行状态和日志可临时存放，会话及工作文件使用持久目录。

| 环境变量 | 覆盖内容 |
|---|---|
| `OPENPAW_SETUP_HOME` | setup 配置和数据根，默认 `~/.openpaw` |
| `OPENPAW_ENV_FILE`、`OPENPAW_HOST_CONFIG` | 模型文件、Host TOML |
| `OPENPAW_OFFICE_SOURCE` | Office 源码，默认相邻 `ai-office-react` |
| `OPENPAW_OFFICE_KEY_FILE` | Office 密钥文件 |
| `OPENPAW_HOST_BINARY` | Host 程序，默认 `target/debug/openpaw`，Windows 为 `.exe` |
| `OPENPAW_RUN_DIR` | PID 与日志目录 |
| `OPENPAW_HOST_PORT`、`OPENPAW_OFFICE_PORT` | 默认 32885、8765 |
| `OPENPAW_NODE` | Node 可执行文件 |

自定义变量要在 setup、build、start、stop、status 保持一致。普通 CLI 的 `OPENPAW_HOME` 是其数据/模型配置目录；`OPENPAW_SETUP_HOME` 属于 Office 脚本，二者用途不同。脚本会显式把模型文件和 TOML 传给 Host。

密钥不要写进 Git、浏览器 `VITE_*`、测试报告或工具环境。配置只在启动时读取，修改后需要安全重启；不要为重启中断未确认的在途任务。

### 独立 CLI 模型配置

首次把 `.env.example` 复制到 `~/.openpaw/.env`（Windows 为 `$HOME\.openpaw\.env`），已有文件直接编辑，不覆盖。Linux 建议目录权限 700、文件权限 600。设置 `OPENPAW_HOME` 时使用相应位置。

```dotenv
OPENPAW_PROVIDER=openai
OPENPAW_MODEL=deepseek-chat
OPENPAW_BASE_URL=https://api.deepseek.com/v1
OPENAI_API_KEY=replace_with_your_own_key
```

`openai` 表示 OpenAI 兼容协议；URL 填服务基础地址，不追加 `/chat/completions`。也支持 Anthropic。

```sh
cargo run --locked -p openpaw-cli -- config show
cargo run --locked -p openpaw-cli -- chat
```

优先级：明确 CLI/TOML 字段 → 系统环境 → 选定 `.env` → 默认。可用全局 `--env-file <path>` 显式选择；配置检查只显示密钥有无，不验证余额。未配置时 CLI 为 mock、直接启动的 Host 为 echo；Office 启动脚本拒绝 Echo，避免把演示当真实模型。

高级 Host 示例保留在 [host-config.example.toml](docs/engine-host/host-config.example.toml)，默认只读与离线，不能把它直接当成 setup 的完整真实配置。

## 架构与模块边界

```text
CLI                           Office / 其他可信适配器
 │                                      │
 │                     Engine Host：作用域、Run、审批、角色队列
 │                     Office 转换：chat/UI/快照/白板/成果
 └──────────────────────┬───────────────┘
                 Engine / AgentLoop
          模型流 → 工具请求 → 审批 → 工具结果 → 后续响应
                        │
       LLM / Tools / Store / Memory / Sandbox / MCP / Plugins
```

| crate | 职责 |
|---|---|
| `openpaw-cli` | 命令、模型配置加载、可选 Host 和 sidecar 启动 |
| `openpaw-engine`、`openpaw-core` | AgentLoop、消息、事件、工具及策略抽象 |
| `openpaw-engine-host` | 会话和 Run 生命周期、工作区授权、并发、角色队列、持久审批；Office 协议及展示转换 |
| `openpaw-llm` | OpenAI/Anthropic、流式读取、错误分类及有边界的重试 |
| `openpaw-tools` | 文件、命令、网络、任务及媒体工具与安全检查 |
| `openpaw-store`、`openpaw-memory` | 会话 JSONL、索引、记忆和技能 |
| `openpaw-goal-ledger`、`openpaw-reflect-lite` | 目标账本、反思与未核验知识的库能力 |
| `openpaw-sandbox` | 显式装配的受限执行；普通本机执行不等于 OS 隔离 |
| `openpaw-mcp`、`openpaw-plugins` | 可装配的 MCP 客户端和插件扩展 |
| `xtask` | 格式/源码/协议以外的工程质量检查 |

Host 调度角色、父子任务及队列，复用 Engine 执行模型与工具；Office 的 `office/` 模块负责把执行事实转成前端协议，不包含另一个 AgentLoop。工具结果、Run 终态、持久审批是事实来源，动画不是执行成功凭证。默认 CLI 不引入 Axum。

普通 `chat` 自动允许其本地工具请求，适用于可信本机；Host 才有配置根、工具白名单和持久审批，Host 权限不会自动套用到普通 CLI。Linux 受限执行需要 bubblewrap；无法落实的限制明确拒绝，不自动降级。

反思、插件钩子、MCP 客户端和策略库提供扩展接口，但默认 CLI/Host 不自动装配所有库能力。`embed` 是兼容 feature，不会启用向量嵌入；`goal resume` 只报告恢复决定，不自动执行；CLI 不提供可工作的 clarify/计划模式。`model default`、`completions` 未实现并明确失败。

## 前后端和扩展接口

| 类别 | 方向和内容 | 边界 |
|---|---|---|
| 健康 | 本机调用 Host `GET /health` | ready/unavailable，不是会话授权 |
| Office 聊天 | Office → Host `POST /office/chat` | 官方请求操作及 NDJSON 事件，含发送/恢复/取消；订阅断开不等于停止执行 |
| UI 命令 | Office 原 HTTP 中继 → Host 命令处理 | 人物/任务详情、会话、审批等；没有自定义的任意任务控制端点 |
| 场景投影 | Host → Office `/api/pawoffice/messages` | 快照/增量、成员、任务、进度、交接、白板、会话与成果元数据 |
| 成果读取 | 浏览器 → Host `/office/artifacts/{version}` 及 delivery/catalog | 真实工具成功后捕获版本，按会话/版本凭证限制，只读访问 |
| Host Rust API | 可信宿主 → Host/Store | scope、session、Run、事件订阅、审批和查询；不表示存在完整 OpenCode HTTP 服务 |
| MCP sidecar | 外部客户端 → `openpaw --sidecar` | 可选 stdio 或 loopback HTTP |
| 库扩展 | Rust Provider/Tool/Environment/Hook 等接口 | 显式装配，保留安全与取消契约 |

任务交接传递真实任务关系、发送/接收角色和事件标识；头顶动作与白板来自投影后的任务字段。完整历史保留账本，快照是有预算的当前视图，不能无限放入正文和历史。运行失败、取消、等待审批和成功分别表达，不把工具成功等同整个任务成功。

### 版本与已知限制

Office 合同唯一清单为 [sources.json](docs/engine-host/contracts/sources.json)：固定提交、文件哈希和 SDK 完整性。测试快照必须保留上游原字节及 LICENSE，不手动修改或浮动跟随 main。合同基线为 `5d21cf2e359d7ab2100e8344d08d58a09a20fe25`；本机另已验证上游运行提交 `a4c2a9b59ee60b8ed00c26f1b3aa6c030ab19fed`，两者不是同一概念，也不代表所有未来版本兼容。

上游 Office 仍有跨会话场景残留、同接收者交接展示串行，以及部分通知层遮挡问题；OpenPaw 适配不能据此承诺所有动画都可见。真实交接不要求模型等待动画完成。具体现场仍需结合浏览器事件与账本排查。

setup 默认全局运行容量 24、最多 5 个活动子任务、持续模式同时 4 个业务事项；这些不是“只能有一个办公室”。32 个工作槽用于预授权创建，不是运行并发数。实际值以本机 TOML 为准。

`render_visual` 生成 SVG 图表。`generate_image` 需要单独支持图片生成的服务、模型和密钥，普通 DeepSeek 聊天配置不能直接提供真实图片。持续任务依赖显式配置、模型服务和任务停止条件，不承诺永久无人值守。

## CLI、feature 与 sidecar

```sh
cargo build --release --locked -p openpaw-cli
cargo build --release --locked -p openpaw-cli --features engine-host,sidecar
```

默认构建是 CLI；`engine-host` 增加 Host，`sidecar` 增加 MCP 服务入口。同一 target 目录后续构建普通 CLI 会覆盖同名程序；运行 Office 前重新执行脚本 build，或指定单独保留且已核验的 Host 二进制。二进制在 `target/release/`；脚本默认 debug，使用 release 时显式设置 `OPENPAW_HOST_BINARY`。

常用命令包括 `hello`、`chat`、`session list/show/search`、`memory list/search/show/delete`、`goal new/status/resume`、`evolve status/reset`、`config show/doctor`、`doctor`、`tool list`、`init`；准确参数执行 `openpaw --help` 及子命令 `--help`。

sidecar 由编辑器或客户端启动并拥有进程：

```sh
openpaw --sidecar --sidecar-transport stdio
```

客户端配置将 command 指向该二进制，args 为 `["--sidecar", "--sidecar-transport", "stdio"]`。HTTP 模式使用 `--sidecar-transport http`，只监听本机并输出动态地址。它不是独立安装的后台服务；`sidecar status/stop` 为信息入口，不管理其他进程。

## 开发与验证

格式、测试和编译命令都在仓库根目录执行，使用锁定依赖：

```sh
cargo fmt --all -- --check
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo +1.89.0 check --locked --workspace --all-targets --all-features
python -m unittest discover -s scripts/tests -p 'test_*.py' -v
```

合同测试只需要 Node 24/npm，不需要完整 Office 或模型密钥：

```sh
cd tests/engine-host-contracts
npm ci --ignore-scripts --no-audit --no-fund
npm run prepare:office
npm run check
npm test
```

`fixtures/` 和 `tests/golden/` 是构造样例，不是作者聊天导出。`upstream-office/` 为 MIT 协议快照，其精确来源在清单和第三方声明中；如需只读核验完整 Office 源码，运行 `node prepare-office.mjs --office-source <existing-office>`。

完整技术执行器：

```sh
python scripts/verify.py --suite all --source worktree --base <review-base> --output <new-directory-outside-repo>
```

把占位符替换为准确 Git 基线和新目录。支持 `rust/quality/contracts/runner` 套件；含 Rustdoc 差量审核时必须传入 `--base`。`--source head --head <commit>` 要求当前检出匹配且干净；工作区模式会记录未提交源码。执行器保存实际命令、退出码、源码摘要与日志，拒绝覆盖旧证据和零子测试假成功。Python 3.11+ 为开发验证依赖，不是普通运行依赖。

### 真实 UI 验收

`office-product.mjs` 执行真实 Office、Host、Chromium、公开 HTTP 和真实点击，覆盖状态/历史/审批/停止/多 Agent/成果版本等。模型使用受控 HTTP 回执，因此通过不等于真实模型能力验收。

使用既有干净上游和可信构建回执时，设置：

- `OPENPAW_OFFICE_SOURCE`：已有 Office 路径。
- `OPENPAW_OFFICE_EXISTING_BUILD`：构建回执 JSON，包含 sourceCommit/sourceTree/contractCommit、buildRecord 的路径/哈希及完整 buildFiles。
- `OPENPAW_HOST_BINARY`：本轮候选二进制，不能在验收前被其他 feature 构建覆盖；不设置时产品脚本会重新编译 Host。
- `OPENPAW_OFFICE_PRODUCT_OUTPUT`：新的仓库外证据目录。

运行 `node tests/engine-host-contracts/office-product.mjs`。该模式核对干净源码、合同、成功构建记录及所有运行文件，不克隆或改写 Office。回执依赖调用方信任，给未知 dist 自行计算哈希不能证明构建来源。`--verify-existing-build-only` 只核对来源，不是 UI 验收。

不提供回执时，产品脚本会在证据目录准备固定 Office 副本并构建，需允许该操作及相应源码访问；本地开发可选择前述只读模式。`continuous-product.mjs` 是持续任务/图文/故障的可选专项，不纳入每次编译。历史一次性 100/200 轮验收不属于常规构建要求。

Linux、原生 Windows、受控模型、付费真实模型和浏览器分别记录。当前本地 Linux 结果不能替代本次 Windows 原生结果；未运行、跳过和外部网络失败不能标成通过。真实模型验收需自己的服务与余额，会产生相应请求费用。

## 文件格式与兼容性

普通 CLI 在数据目录保存 sessions/*.jsonl、memories/、skills/、goals/ 和可重建的 index.db。Host 另维护其会话/审批/任务及成果版本，不能只复制数据库的一部分来迁移运行数据。

记忆/技能采用 UTF-8 Markdown 与 YAML frontmatter，保留标识、类型、时间、标签和状态等字段；未核验反思成果保持 unverified，不能自动提升为已审核知识。固定样例在 tests/golden/，以解析器和往返测试为准。变更持久格式、公共接口或许可时说明兼容、迁移及回退；不以删除历史证据掩盖不兼容。

## 贡献与问题报告

PR 描述问题、实际行为变化、验证源码和命令，并说明公共接口/权限/数据影响。复用已有模块和夹具，保留有独立覆盖的回归；不要提交密钥、真实聊天、工作成果、target、node_modules 或本地备份。

使用 `git commit -s` 添加 DCO Signed-off-by，声明有权提交贡献。CI 不代替人工审核。缺陷可提交脱敏 GitHub Issue，附平台、版本、复现、预期/实际及最小日志；安全漏洞或凭据不得发公开 Issue，使用仓库提供的私密安全报告入口，若入口尚未开放先向维护者请求安全渠道，不公开漏洞细节。

## 许可证与发布范围

项目根许可为 Apache-2.0，第三方代码分别遵守其许可。保留 [NOTICE](NOTICE)、[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) 及相应许可文本，Office 协议快照另附 MIT LICENSE。

release workflow 当前构建 Linux/Windows 默认 CLI，附许可证、校验和及 Sigstore 签名；下载包不包含完整 Host/Office。签名和固定版本不表示已证明逐字节可复现。crates.io 步骤为 dry-run，内部依赖未发布可能延后，不等于包已正式发布。

本地内部方案、原始证据和治理材料不属于当前公开源码树。停止跟踪不会清除 Git 历史；完整历史公开前仍需单独核查旧会话、内部资料和授权不明的上游代码，不能只看当前树。
