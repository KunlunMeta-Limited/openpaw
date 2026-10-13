# CHG-2026-0023：CLI 与 Host 运行能力统一

**当前状态：已批准（approved），编码及功能验证完成；正式验收受阻。** 用户于本会话回复“OK开始干活吧”，批准本文件完整实施范围。
2026-10-13 新报告的历史/分支会话整轮回退缺陷见第 13 节；用户已回复“批准”，仅追加该节登记的实施、验证及可撤销部署/会话修复范围。
本次 bugfix 使用一份 CHG 统一模型配置、会话读取互通、工具审批与 Host 组装职责。修复回归通过，
行数预算超限及外部治理框架缺失仍阻断正式门禁，不宣称已通过合并、部署或发布验收。
用户要求将此前三份草案合并，原会话互通和审批临时草案不再独立作为审批入口。
人工批准对象为本文件的完整范围；实施批准不等于合并、部署或发布批准，AI 不执行治理工具 `approve`。

## 1. 变更审批

| 项目 | 内容 |
| --- | --- |
| 变更编号 | `CHG-2026-0023` |
| 变更标题 | CLI 与 Host 运行能力统一 |
| 当前状态 | 已批准（approved），编码及功能验证完成；正式验收受阻 |
| 变更类型 | 缺陷修复、入口能力补齐与复用收敛（refactor） |
| 风险等级 | 高（high）；涉及持久历史可见性、授权状态机、跨入口公共模块与 Host 接口语义 |
| 负责人 | CLI、LLM、Store、Engine 与 Host 负责人（待确认） |
| 必须审核 | 架构、安全、持久化、CLI、LLM、Engine、Host 接口/运行时、测试与消费者验收负责人 |
| 目标分支 | `bugfix/chat-runtime-provider-parity` |
| 基线 | `merge/openpaw-engine_host`，`d9b03ce1a8d7825e629ff337c87bdbe0f2e48dcf` |
| 设计基线 | `DES-OPENPAW-B-2026-09-15` |
| 关联已存在变更 | `CHG-2026-0007`、`CHG-2026-0008`、`CHG-2026-0013`、`CHG-2026-0015`、`CHG-2026-0016`、`CHG-2026-0019`、`CHG-2026-0020`、`CHG-2026-0021`、`CHG-2026-0022` |
| AI 参与 | 是；Codex |
| 批准人 / 批准时间 | 用户，本会话“OK开始干活吧”，2026-10-12 |

## 2. 事实、缺陷与目标

用户希望 CLI chat 的 OpenAI/Anthropic 格式支持自定义 Base URL、模型和 API Key，修复其他重复与互通问题，
并让 chat 也可使用工具审批，Host 对共享能力主要做组装和调用。
协议名称与服务商名称分离：第三方 OpenAI 兼容服务仍使用 `--provider openai`，再配置地址、模型和密钥。
`mock`、`echo` 保留为离线入口，仅存在网络密钥不会自动启用网络 provider。

实际复现使用基线源码构建的 Linux 程序、隔离 home/workspace、本地 HTTP/SSE fixture 和假密钥。
外部 HTTPS CONNECT 被 fixture 拒绝；生产 Compose 和真实密钥未参与。
脚本与证据保存在外层工作区 `../artifacts/chat-runtime-audit/probe.py`、`evidence.json` 和 `report.md`。

| 问题 / 候选 | 基线已核实事实与根因 | 本轮处理及证据边界 |
| --- | --- | --- |
| CLI 忽略第三方地址 | 设置 OPENPAW_*_BASE_URL 后，chat 仍尝试官方地址；同配置 Host 正确请求 fixture。chat 手写构造且未调用 with_base_url | 共用配置和工厂；原诊断回放及实际 CLI 本地请求回归通过 |
| CLI 模型接线错误 | chat 硬编码默认模型，也没有 AgentLoop.with_model；AgentLoop 默认 openpaw-default 被网络请求优先使用 | 有效模型同时用于 Provider、AgentLoop、JSONL；实际请求 body 和落盘模型断言通过 |
| Anthropic 版本路径重复 | Base URL 以 /v1 结尾时实际请求 /v1/v1/messages；根 URL 正常 | 根路径、/v1 和尾斜杠实际请求回归通过 |
| 输出 token 配置漂移 | Host fixture 收到 max_tokens=73；CLI 没读 OPENPAW_MAX_OUTPUT_TOKENS，沿用 4096 | 共用生成配置；实际 CLI/Host body 与显式覆盖断言通过 |
| 模型目录、诊断与执行漂移 | CLI model list 漏掉配置模型；config show 固定显示默认 mock，Host 单独生成 models | 共用配置；实际 model/config/doctor 一致性与零网络诊断测试通过 |
| 空密钥校验不同 | CLI 拒绝空串，Host 只检查变量存在，纯空白校验不一致 | 共用空白/Header 校验；配置错误退出码和脱敏回归通过 |
| provider 可用性失真 | /provider.connected 固定四项，无 Key 或无效 URL 也列出网络 provider | 本地配置矩阵、保留 unavailable default、目录 fixture 和 OpenWork attached 契约通过 |
| 会话发现与索引断开 | 两边都写 JSONL；SQLite 是 CLI 次级索引。Host 完成回复后，同 home 的 CLI list 仍提示无索引 | 真实 Host 写入、无预建索引的实际 CLI 读取与多进程刷新测试通过 |
| 会话历史解释不同 | CLI show 遍历原始 Message，忽略 turn_accept 内嵌 user、finish 事务和 revert；Host 有完整重放规则 | 共享投影；事务损坏/未提交矩阵、真实 staged/unrevert/committed 替换及活动回合查询通过 |
| chat 未接人工审批 | Core/Engine 已有审批接口和 Confirm 等待；chat 只接 ToolHost，沿用 PermissiveGuardrail + AutoApproveManager | 共享 gate 与终端 adapter；真实工具副作用、headless 输入隔离、100 次 reply/abort/reset 竞争及 Host 卡片清理回归通过 |
| 参数和组装重复 | cli.rs::Command::Chat 与 chat.rs::ChatArgs 重复声明参数；runtime 另外实现配置及通用审批策略 | 唯一 ChatArgs 与 Host 共享接线；帮助和默认依赖隔离检查通过 |

两入口已共用 LlmProvider、LLM HTTP/SSE 客户端、AgentLoop、工具注册表和基础 SessionReader/Writer。
runtime.rs 没有重写这些完整能力。本轮修复集中在配置/组装、持久历史解释、索引和审批交互缺口。
index.db 还保存 audit_log 等数据，不能通过删除整个数据库完成“重建会话索引”。

## 3. 分层、接口适配与 Host 职责

| 共享能力 / 入口 | 归属与职责 |
| --- | --- |
| LLM 配置 / 工厂 | openpaw-llm::config：元数据、默认值、环境/显式覆盖、校验、脱敏视图和 provider 构造 |
| 持久会话读取 | openpaw-store::transcript / catalog：版本化记录 codec、纯重放、可见历史、发现与索引同步 |
| 工具审批机制 | openpaw-engine::approval：pending、唯一回复、回合代际、取消、session 授权和 manual 策略 |
| CLI chat | 终端输入/输出、终端审批 adapter、调用 AgentLoop 与共享持久能力 |
| Host http.rs / protocol.rs | OpenWork 等客户端的 REST/SSE 接口、鉴权、请求/响应字段及错误码适配 |
| Host events.rs / office.rs | 引擎事件转 OpenCode 事件，以及向 AI Office 发送动作 POST |
| Host runtime.rs | session owner、回合/取消/调度/写盘时机及恢复协调；对上述通用能力做组装和调用 |

依赖方向为 CLI / Host → LLM、Store、Engine 的共享模块 → 既有 Core 接口、provider、JSONL / SQLite。
Engine 的审批模块使用已有 Core trait；Store 只提供持久化中立结果，Host 保留 OpenCode/Office 协议类型。
默认 CLI 不依赖 Host/axum；共享模块不包含 HTTP 路由、Office 动作或终端实现。

本轮移除 runtime 中所选通用逻辑的重复实现，保留现有并发与写入协调者。
runtime 的全部 session owner/调度/写入状态机迁移为新的跨入口服务，不包含在本草案中。
CLI 会话互通增强现有 list/show/search，工具审批增强 chat；不自动新增 archive/fork/revert 或 HTTP 控制命令。

## 4. 允许修改文件与符号

下表为批准后允许的完整文件集合，同一文件合并为一行。授权仅覆盖列出的符号与行为，
不得因为文件已登记而修改无关逻辑。草案阶段只整理 CHG 与索引；业务/测试源码等待批准。

| 仓库 | 操作 | 文件 | 符号或范围 | 必要性、影响与负责人 |
| --- | --- | --- | --- | --- |
| openpaw-main | 新增 | `docs/changes/CHG-2026-0023-CLI与Host运行能力统一.md` | 本范围、审批及验收记录 | 单一事实源；项目负责人 |
| openpaw-main | 修改 | `docs/changes/INDEX.md` | 本 CHG 索引行 | 只登记一个草案；项目负责人 |
| openpaw-main | 新增 | `crates/openpaw-llm/src/config.rs` | 目录、默认模型、环境 snapshot / override 解析、URL / Key / token 校验、工厂及内嵌测试 | 两入口和诊断共用实现；LLM / 安全负责人 |
| openpaw-main | 修改 | `crates/openpaw-llm/src/lib.rs` | config 模块注册和必要导出 | 开放既有依赖者使用；LLM 负责人 |
| openpaw-main | 修改 | `crates/openpaw-llm/src/anthropic.rs` | endpoint 组装及定向测试 | 消除 /v1/v1/messages；LLM 负责人 |
| openpaw-main | 新增 | `crates/openpaw-engine/src/approval.rs` | 中立 ApprovalGate、pending/代际/取消/session 授权、通知、manual guardrail、组装 helper 及内嵌测试 | 共用审批状态机，不改 AgentLoop；Engine / 安全负责人 |
| openpaw-main | 修改 | `crates/openpaw-engine/src/lib.rs` | approval 模块注册和必要导出 | 不含传输依赖；Engine 负责人 |
| openpaw-main | 新增 | `crates/openpaw-store/src/transcript.rs` | 中立持久类型、版本化 codec、纯重放、可见前缀、标题、稳定消息 ID 及内嵌测试 | 历史规则只有一个实现；Store / 安全负责人 |
| openpaw-main | 新增 | `crates/openpaw-store/src/catalog.rs` | 发现、路径/ID 校验、只读快照、逐会话索引同步及内嵌测试 | 两入口共用发现/读取；Store 负责人 |
| openpaw-main | 修改 | `crates/openpaw-store/src/lib.rs` | 两模块注册和必要导出 | 复用既有依赖，不加 crate；Store 负责人 |
| openpaw-main | 修改 | `crates/openpaw-store/src/sqlite.rs` | transcript 管理字段与消息/FTS 原子替换、失效处理及内嵌测试 | 表达逻辑裁剪，保留审计/其他字段，不改 schema；Store 负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/cli.rs` | 唯一 ChatArgs、Command::Chat、帮助和新增配置/permissions 参数 | 移除重复声明；CLI 负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/main.rs` | ChatArgs 直接转发 | 不再逐字段重新组装；CLI 负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/chat.rs` | 移除本地 ChatArgs；provider/AgentLoop/审批接线、单一输入路由、回合清理、best-effort 索引同步与错误 | 对共享能力做组装；CLI 负责人 |
| openpaw-main | 新增 | `crates/openpaw-cli/src/approval.rs` | 终端 adapter、答复解析、提示与非交互拒绝、单一输入所有者及内嵌测试 | 补人工审批，防止 stdin 争抢；CLI 负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/lib.rs` | 终端 approval 模块注册 | binary 与测试使用同一入口；CLI 负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/session_cmd.rs` | run_list/show/search、模块说明和读取/索引错误提示 | 无预建索引也能发现会话，遵守可见性；CLI 负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/model_cmd.rs` | provider/model 目录 | 消费共享配置，包含自定义模型；CLI 负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/config_cmd.rs` | run_show、key presence | 有效配置与脱敏展示一致；CLI / 安全负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/doctor.rs` | provider 配置检查 | 校验与执行一致，WARN 仍退出 0；CLI / 安全负责人 |
| openpaw-main | 修改 | `crates/openpaw-engine-host/src/runtime.rs` | build_provider/default_model/provider_base_url/模型常量/configured_agent/ManualGuardrail；restored_session_projection 及纯 helper、summaries、标题/前缀；审批接线及对应内嵌测试 | 委托共享能力，保留锁顺序、调度与原子写入时机；Host 运行时负责人 |
| openpaw-main | 修改 | `crates/openpaw-engine-host/src/http.rs` | provider/provider_models 和定向内嵌测试 | 目录与 connected 消费共享配置；其余路由逻辑不改；Host 接口负责人 |
| openpaw-main | 修改 | `crates/openpaw-engine-host/src/protocol.rs` | 持久 codec/共享中立结果转换、provider fixture 断言/Rustdoc 及相关内嵌测试 | 对外字段不变，不保留两套解析算法；Host 接口负责人 |
| openpaw-main | 修改 | `crates/openpaw-engine-host/src/session_mutation.rs` | 持久 codec/投影与通用 ID/前缀的转发或转换 | 回退请求归一化、失败边界和 fork 新 ID 生成仍在 Host；Host 运行时负责人 |
| openpaw-main | 修改 | `crates/openpaw-engine-host/src/approval.rs` | HostApprovalManager 委托 gate，PermissionInfo/SSE 转换，作用域 Rustdoc 和既有测试 | 成为协议 adapter，保留外部契约；Host / 安全负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/chat_e2e.rs` | 本地 OpenAI/Anthropic 实际请求及无效配置矩阵 | 锁定 URL/model/Key/token 与落盘模型；测试负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/help_snapshot.rs` | 新选项、clap 解析和 feature 隔离 | 唯一参数定义与默认入口兼容；测试负责人 |
| openpaw-main | 新增 | `crates/openpaw-cli/tests/provider_config_e2e.rs` | config/model/doctor 的配置一致性与脱敏 | 防止诊断再次漂移；测试负责人 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/cli_session_e2e.rs` | 实际 list/show/search、旧索引恢复、重复同步与回退 FTS | CLI 读取互通验收；测试负责人 |
| openpaw-main | 新增 | `crates/openpaw-cli/tests/session_host_parity.rs` | engine-host feature 下真实 Host 写入与实际 CLI 读取的跨进程回归 | 活动/重启/回退/归档/分支互通；测试负责人 |
| openpaw-main | 新增 | `crates/openpaw-cli/tests/chat_approval_e2e.rs` | 本地模型 fixture、工具副作用、终端 adapter 和实际 headless CLI | 批准/拒绝、EOF 与 prompt 输入隔离；测试负责人 |
| openpaw-main | 修改 | `crates/openpaw-engine-host/tests/http.rs` | provider、override/mock、恢复/回退/分支/归档/审批的定向回归 | 共享实现不破坏 Host 现有契约；测试负责人 |
| openpaw-main | 修改 | `crates/openpaw-engine-host/tests/fixtures/provider.json` | 无网络凭据目录 fixture | all 四项，connected 只含离线项；测试负责人 |
| openpaw-main | 新增 | `contracts/host-provider-v1.schema.json` | API-HOST-PROVIDER-001 的既有字段形状和新 connected 说明 | 明确唯一目录 HTTP 行为变化；Host 接口负责人 |
| openpaw-main | 修改 | `docs/b-line/configuration-and-connection.md` | 配置来源/path、chat permissions、同 home 读取与索引刷新 | 取代过时 dotenv 描述，说明当前入口；CLI / Host 负责人 |
| openpaw-main | 修改 | `docs/b-line/protocol.md` | provider.connected 与目录配置错误边界 | 固定四项改为本地可用，不承诺上游认证；Host 接口负责人 |
| openpaw-main | 修改 | `docs/b-line/architecture.md` | 共享 LLM/Store/Engine 模块、入口 adapter、Host 组装职责和 Anthropic path | 统一分层说明；架构负责人 |
| openpaw-main | 修改 | `docs/b-line/data-flow.md` | JSONL 事实源、投影与 SQLite 次级索引 | 读取/索引刷新不被误认为恢复写入；Store / Host 负责人 |
| openpaw-main | 修改 | `docs/b-line/testing.md` | 本 CHG 的统一验收矩阵 | 分阶段验证，一个整体验收入口；测试负责人 |
| openpaw-main | 修改 | `docs/b-line/traceability.md` | 缺陷/实现/测试/证据映射 | 区分静态风险、实际复现和修复验收；测试负责人 |
| openpaw-main | 修改 | `docs/governance/项目设计基线.md` | API-HOST-PROVIDER-001 的 all/connected；permission pending 内部所有者与 adapter 说明 | 修订原固定四项及专属 pending 所有者描述；架构 / 安全负责人 |

合并文档时原三份未提交、未批准临时草案从 docs/changes 移除，备份保存在仓库外诊断 artifacts；
它们不作为业务分支的独立 CHG 或额外允许文件。已批准历史 CHG 的事实记录不改写。

## 5. 明确禁止路径与行为

| 禁止范围 | 原因 |
| --- | --- |
| `crates/openpaw-core/**`、公共 trait、`crates/openpaw-engine/src/agent_loop.rs` | 既有模型/审批 seam 足够；不更改 L4 trait、请求字段、工具执行、重试、预算或取消算法 |
| Engine 中除本表登记 approval.rs/lib.rs 外的生产模块 | 本轮共享审批，不进行 Engine 全面改造 |
| Store SessionReader/Writer 算法、session.rs、global_db/frontmatter 及其他未登记模块 | 原始解析、fsync 和非会话数据保持现状；共享投影基于既有 events |
| Host events.rs/office.rs/message_projection.rs 的业务逻辑、审批 HTTP handler、协议字段、未登记路由 | 对外传输与消息/Office/权限契约保持现状；http/protocol 的允许修改仅限第 4 节 |
| runtime 中调度容量/策略、锁顺序、abort/delete/fork/revert 的写入流程与恢复决策 | 共享读取与审批接线不能丢失现有线性化和原子写入保证 |
| JSONL 格式变更、SQLite schema 迁移、物理重写/截断 JSONL、CLI 读取时写 interrupted | 本轮是读取互通；不能自动修复正在运行的 Host 或破坏持久事实 |
| 删除整个 index.db、跨 session/重启授权持久化、全局工具白名单 | DB 有审计等不可由 transcript 重建数据；授权只在当前 session 内存 |
| Tools/bash AST、sandbox、工作区与文件权限策略 | 工具人工批准不自动绕过其他执行限制 |
| Cargo.toml、Cargo.lock、CI、CODEOWNERS、xtask 行数预算 | 使用既有依赖和 feature，不新增 Host 默认依赖或通过抬预算掩盖超限 |
| OpenWork/AI Office/Gateway 产品、Compose、部署脚本 | 用户明确 Docker 更新另做；本轮是 OpenPaw 内部修复 |
| 未登记 TUI、session_search 工具、记忆/任务/审计业务 | 不宣称全面修改所有入口；原 SQLite API 和非会话数据保持兼容并评估受影响读取者 |
| .env 自动发现、配置文件自动加载、新平台协议、多 profile 系统、model default 持久化、自动网络探测、静默切换 mock | 保留 CHG-0016 的环境配置规则；显式配置失败须报告，目录查询不发网络请求 |
| 新 CLI archive/fork/revert/HTTP 服务控制命令、新跨入口运行时 crate | 会话互通与终端审批有明确范围，不扩张到完整 Host 能力迁移 |
| 凭据、完整 Prompt、敏感文件正文进入诊断/日志/审批展示/SSE/Office | Key 只进入内存与认证头；正文仅按既有会话持久化协议处理，不扩大泄露面 |

## 6. CLI、HTTP 与公共接口变化

### 6.1 模型配置与参数

chat 新增 `--base-url`、`--model`、`--max-output-tokens`，及第 6.3 节的 `--permissions`。
每个网络配置字段优先级为显式 chat 参数 > 对应进程环境 > 协议默认值。
变量为 OPENPAW_OPENAI_BASE_URL/MODEL、OPENPAW_ANTHROPIC_BASE_URL/MODEL 和 OPENPAW_MAX_OUTPUT_TOKENS；
Key 使用 OPENAI_API_KEY / ANTHROPIC_API_KEY，不引入携带密钥的命令行参数。
默认官方 endpoint、claude-sonnet-4 / gpt-4o 和 4096 token；chat 默认 mock，Host 原 provider 选择规则不变。
Host session/per-turn 显式 provider/model 优先，幂等指纹使用同一有效模型。

OpenAI Base URL 是 /chat/completions 的父路径，保留自定义版本/服务前缀；不盲目补 /v1。
Anthropic 支持服务根路径或已带 /v1 的前缀，只生成一层 /v1/messages，处理尾斜杠。
校验 http(s)、host、空值、query/fragment 与内嵌 URL 凭据；模型非空，Key 非空且非纯空白，token 为正 u32。
无效配置发送前失败，错误不回显 URL/Key；chat 配置错误退出 2，模型/运行/持久化失败退出 1。
离线 provider 不校验网络字段；相关通用生成配置仍需有效。
唯一 ChatArgs 放 cli.rs 库模块，Command::Chat 和执行入口共用；原命令/选项兼容，Rust 构造方式变化须记录。

```text
openpaw chat --provider openai --base-url https://example.invalid/v1 --model third-party-model
openpaw chat --provider anthropic --base-url https://example.invalid/v1 --model third-party-model --permissions manual
```

### 6.2 provider 目录与诊断

GET /provider 保留 all/connected/default 的原字段形状。all 列出四个已支持协议及默认/配置模型。
connected 是 all 的无重复子集，含义为“本地配置满足运行条件”：

| provider | 纳入 connected 的条件 |
| --- | --- |
| mock / echo | 内置离线实现及通用生成配置可用，不受无效网络 Key/URL/model 影响 |
| anthropic / openai | 对应 Key 非空且非纯空白，有效 URL/model/token 配置合法 |

该判断不验证网络或上游认证，不因短暂网络波动改变目录；实际 401/429/503 等由真实请求报告。
default 保留显式 Host provider，即使不可用也不静默切换 mock；model list 可展示尚未有 Key 的支持协议。
工厂、config show、doctor、model list 和 Host 目录消费同一配置 snapshot / 脱敏视图，诊断不要求建立连接。
密钥不通过默认 Debug 或序列化输出，doctor 的 WARN 不改为非零退出。

OpenWork provider-list-query.ts 的 getConnectedProviderItems 按 connected 成员筛选，utils/providers.ts 按 ID 过滤，
源码未要求长度四。模型可选范围收紧符合本地配置规则，仍需现有 attached 消费者验证；不修改消费者产品。
现有固定长度测试和 fixture 更新，不能以只读核实代替消费者完整验收。

### 6.3 工具审批

chat 新增 `--permissions manual|auto`，默认 auto 保持原行为；manual 对工具调用要求确认，与 Host manual 一致。
Host 沿用 OPENPAW_ENGINE_PERMISSIONS=manual 及原默认策略。终端提供“本次允许 / 本会话同工具允许 / 拒绝”。
空输入默认拒绝，无效答复不批准，EOF/输入失败拒绝并结束等待；提示走 stderr，stdout 保留模型回复用途。
manual 在非终端 stdin 下拒绝需审批工具，不把下一行聊天 prompt 当成 yes/always；无工具的 pipe chat 继续可用。
Host once/always/reject、GET/POST permission、PermissionInfo、asked/replied 的现有字段与状态码不变。
session 同工具授权可复用到后续回合，不跨 session/进程重启；仅纠正 Host 注释的跨回合文字矛盾。
人工批准不解除 Tools AST、sandbox 或工作区检查。

### 6.4 会话命令与共享 Rust 模块

现有 session list/show/search 可以读取同 home 的 Host / CLI 持久会话，无须先运行 chat 建索引或启动 Host。
标题、归档/暂存回退状态与可见历史一致；只读查询不提供实时 SSE 增量/调度状态，也不修改这些状态。
show 工具摘要必须关联可见消息的调用 ID，不直接遍历没有可见性规则的原始 ToolCallComplete。
安全 API 失败保留为诊断元数据，不伪造模型 Message 或写入全文索引。

LLM 配置/工厂、Store 持久投影/catalog、Engine approval 的新公共类型与函数需要架构审查及中文 Rustdoc 八栏目。
配置接受 snapshot + override，避免测试并发修改全局环境；Store 使用中立持久类型，不移入 Host 线协议类型。
Host 转换采用完整字段映射，不用默认值吞掉错误；共享 codec 同时服务 Host 持久写入与重放。
HTTP 唯一目录语义变化为 connected；损坏已登记持久变更的读取错误收紧见第 8 节。
有效 HTTP/SSE/session JSONL 字段和 SQLite schema 均保持兼容。

## 7. 实现顺序与共享模块设计

一份 CHG、一次范围审批、一个 bugfix 分支，内部按阶段提交/验证，最后整体验收。

1. 建立真实 CLI/Host 失败回归，固定本地 HTTP/SSE fixture、隔离 home/workspace 和副作用断言。
2. 在 LLM 共用配置/工厂/脱敏目录，接线有效 model/token；修 Anthropic path，统一 ChatArgs 和诊断/connected。
3. 在 Store 共用版本化 codec 与纯重放，Host summaries/装载和 CLI 查询消费同一结果；再接逐会话 SQLite 同步。
4. 在 Engine 共用审批 gate/manual 策略，HostApprovalManager 变协议 adapter；chat 接终端 adapter 和单一输入路由。
5. 整体验证默认 CLI、engine-host、旧会话、恢复/回退/审批、消费者兼容与治理；更新追踪和实际验收记录。

共享 transcript 一次重放返回已提交完整消息、当前可见前缀、标题/归档/分支来源、持久回合结果、幂等事实和安全错误。
该模块不创建 AgentLoop、ToolHost、writer、HTTP 或网络请求；catalog 提供发现/只读快照，Host 添加活动标记和 REST 形状。
Host 原 mock tools/delayed stream 保留为 Host 选择，普通网络 provider 与生成配置委托共享实现。

共享 approval gate 负责 pending、唯一回复、代际、begin_turn/abort_all、取消与 session 授权，使用已有 ApprovalManager。
入口提供中立通知 adapter 和 ID 生成：Host 用既有 UUID，CLI 用 session 内单调编号，不给 Engine 新增 UUID 依赖。
Host 把中立 pending/通知转换为 PermissionInfo/SSE；终端负责提示和答复，公共执行循环仍由 AgentLoop 承担。

## 8. 持久化、并发与权限不变量

### 8.1 持久历史重放

| 场景 / 记录 | 共用规则 |
| --- | --- |
| 旧 Message/Metadata | 保留独立消息；标题优先显式 metadata，其次首条 user，最后默认标题 |
| turn_accept_v1 | 验证 user UUID 与 outcome/admission 关联，展开已接收 user，不重复加入 |
| prepare + Message + finish commit | 仅合法匹配 commit 使批次可见；验证 turn/终态/ID 数量顺序与唯一性，保留既有 4096 消息上限 |
| 活动回合、残留 prepare、torn tail | 未提交批次不可见，继承原 Reader 尾部容忍，不截断或追加 interrupted |
| staged revert / unrevert | 隐藏边界及尾部；清游标后恢复，保留完整已提交历史供原 Host unrevert |
| committed revert | 验证原游标/边界，逻辑裁剪旧尾部并原子接收替换 user，同步裁剪 turn/admission/error |
| rename/archive/fork origin | 保留标题/归档/分支身份；归档不删除历史，分支独立 ID 不与源会话合并 |
| 安全 API 失败 / 重复消息 ID | 错误是诊断元数据；保持稳定兼容消息 ID 与去重，不把失败占位加入模型历史 |
| 已登记 v1 事务/变更损坏 | 返回投影错误，不降级遍历原始 Message 或以旧 FTS 暴露隐藏/未提交正文 |
| 无关未知事件 / 版本 | 保持前向兼容，不修改原记录，不把 Unknown 当成 Message |

旧 Host 可能忽略损坏的已登记 mutation，本轮改为读取错误，属于明确安全收紧。
有效旧会话兼容；损坏会话不由 CLI 自动修复。终端摘要与 REST parts 展示形式可以不同，持久可见消息必须一致。

### 8.2 发现与索引一致性

catalog 只发现指定 home/sessions 的 JSONL，校验合法 ID、文件名与 meta.id；不信任旧索引任意路径、不访问目录外链接。
空目录为空结果；权限/损坏要诊断，不能冒充空数据库或推导全库删除。
list 消费有效目录摘要，show 直接解析目标快照；search 同步当前可见消息后查 FTS。

同步按会话事务更新 transcript 管理字段并替换可见消息/FTS，使用持久更新时间，不把扫描当新活动。
保留非 transcript 字段、audit 和其他数据；不对原始 events 无条件 index_event，不改 schema 或新建 fingerprint 表。
同步先取得 SQLite 写事务再读目标快照/替换，串行化并发刷新；chat 原增量索引改共享同步，避免重复插入或旧快照覆盖。
保留 WAL 与 busy_timeout；Host 回复成功仅依赖原 JSONL 写入，不依赖 SQLite。

并发 Host 追加时 CLI 读取当次合法已提交前缀，下次查询刷新；不会接收生成半批次或改变运行中回合。
仅确认删除且位于 catalog 范围的索引项可失效，权限/扫描失败不删除整库。
search 只展示当次有效 catalog/投影允许的消息，出错会话不使用陈旧 FTS 作为历史替代。
索引失败给可操作错误，chat 维持 best-effort 索引，不建议删除 index.db、不输出原始事件正文。
本轮保持 JSONL 扫描方式，成本随历史量增长；大型历史缓存优化另定范围。

### 8.3 审批生命周期和终端输入

reply/reset/abort 与 pending 消费在线性化生命周期内完成，每个请求至多结束一次，清理后无迟到 asked。
中立通知只作非阻塞发送/排队，持锁时不能等待网络、终端输入或其他业务调用。
取消或 future 丢弃要清理等待；结束代际的答复不能批准新回合，取消不能被伪装成人工批准。

现 chat 的 stdin.lock().lines() 长期持锁，不能在 ApprovalManager 再读另一个 stdin。
改为一个输入所有者，prompt/审批顺序路由；等待不阻塞共享审批锁，回合结束丢弃旧答复。
可注入输入用于终端 adapter 测试；实际二进制 headless 单独验证，不伪装 TTY。
提示只含工具名/必要的安全原因，不打印完整工具输入、凭据、Prompt 或敏感正文，不扩大日志/JSONL/Office 内容。

## 9. 替代方案与保护路径影响

| 替代方案 | 不采用原因 |
| --- | --- |
| 只给 chat 加 with_base_url | 请求模型、token、目录和诊断继续漂移 |
| CLI 导入 Host 工厂/重放/审批管理器 | 默认入口耦合 Host feature/HTTP/SSE，违反依赖隔离 |
| CLI 遍历原始 JSONL 建索引 | 内嵌 user 遗漏，逻辑回退旧尾部和未提交批次可能暴露 |
| CLI 查询必须经过运行中的 Host HTTP | 离线持久查询依赖服务与鉴权，改变使用方式 |
| 复制 pending 状态机或只接 AutoApproveManager | 前者继续重复，后者没有人工交互 |
| 新建完整跨入口 runtime crate、改 Core trait | 需要额外依赖和生命周期迁移，超出所选共享能力 |

runtime/http/protocol/approval 属于 L3；新增跨入口 LLM/Store/Engine 接口须相应 CODEOWNER 审查。
Store 当前 2000 行等现有预算可能限制规模，必须报告真实基线和增量；不抬预算、不删可靠性测试。
治理工具缺失或历史门禁超限需真实记录，不能以“已批准实施”代替正式验收/发布裁决。

## 10. 测试与验收

测试编号统一属于 CHG0023。先建立失败回归，再迁移实现；Store 单测不代替真实 Host 写入/实际 CLI 读取。

| 测试编号 | 场景 / 命令 | 必须观察结果 |
| --- | --- | --- |
| TEST-CHG0023-CFG-001 | 配置 snapshot/override 表驱动测试 | 两协议默认/环境/参数、空白、URL、Key、token 校验一致；Debug/错误不含密钥 |
| TEST-CHG0023-CFG-002 | 实际 CLI → 本地 OpenAI HTTP/SSE fixture | 自定义 path、认证头、body.model/token、stdout 回复及 JSONL 模型正确 |
| TEST-CHG0023-CFG-003 | 实际 CLI → Anthropic fixture | 根 URL、/v1、尾斜杠仅一层 /v1/messages；认证头/模型正确 |
| TEST-CHG0023-CFG-004 | CLI 显式覆盖环境与无效配置矩阵 | 参数优先；无效 URL/Key/model/token 发请求前非零失败、三段式脱敏错误 |
| TEST-CHG0023-CFG-005 | config/model/doctor | 有效配置与执行一致，配置模型可见，只有 Key 存在性；零网络；doctor WARN 退出 0 |
| TEST-CHG0023-CFG-006 | connected 缺 Key/空白/无效 URL/model/token/合法配置 | all 四项；connected 仅本地可用，网络字段不影响离线；不请求上游或泄露凭据 |
| TEST-CHG0023-CFG-007 | 无效显式 default、目录 fixture 与现有消费者筛选 | default 不静默替换；prompt 明确配置错误；合法自定义模型可选 |
| TEST-CHG0023-CFG-008 | Host session/per-turn override、mock tools/stream、指纹 | 共享工厂保留既有覆盖和测试模式，审批/取消/错误行为兼容 |
| TEST-CHG0023-HIST-001 | 真实 Host 创建 echo 并回复，再实际 CLI list/show/search | 不运行 CLI chat/不预建索引仍可发现正确 user/assistant 和全文命中 |
| TEST-CHG0023-HIST-002 | 新旧 CLI/Host、原子 accept/finish 与旧三事件接收 | 无遗漏内嵌 user/重复消息，元数据和稳定消息 ID 兼容 |
| TEST-CHG0023-HIST-003 | Host staged revert/unrevert/committed 替换 | CLI show 与持久可见历史一致；隐藏消息不搜索命中，清游标恢复，commit 不复活旧尾部 |
| TEST-CHG0023-HIST-004 | rename/archive/fork 与重启 | 标题/归档、独立 ID 与分支消息正确，不增加 CLI 状态控制命令 |
| TEST-CHG0023-HIST-005 | 半批次、torn tail、commit ID/turn/终态错误、损坏 mutation | 未提交/隐藏正文不可见，错误不降级；CLI 查询前后 JSONL 字节/hash 不变 |
| TEST-CHG0023-HIST-006 | 活动 Host 并发 CLI 与 Host 重启 | CLI 不结束回合/不写 interrupted，现有恢复/幂等/取消/锁契约通过 |
| TEST-CHG0023-HIST-007 | 重复/多进程刷新、chat 同步、索引旧版/缺失 | 计数/FTS 不累加、不被旧快照覆盖，仅已提交当前可见内容 |
| TEST-CHG0023-HIST-008 | DB audit/非会话哨兵、回退/删除单一 transcript | 逐会话替换/失效，不丢审计/其他数据，schema 版本不变 |
| TEST-CHG0023-HIST-009 | 空目录/权限/损坏/非法 ID/meta 与文件名不符/目录外链接 | 错误隔离，不访问外部目标、不误删整库、不回显隐藏陈旧命中 |
| TEST-CHG0023-HIST-010 | API 失败、工具消息及回退工具 | 失败是诊断元数据；工具摘要关联可见消息，模型/索引无错误占位 |
| TEST-CHG0023-APR-001 | 共享 AgentLoop/gate + 终端 adapter 批准/拒绝 | 仅批准产生 fixture 文件副作用；拒绝不执行，批准不绕过 Tools 检查 |
| TEST-CHG0023-APR-002 | 实际 CLI manual/headless，下一聊天行含 yes | 需审批工具拒绝，后续 prompt 不被当成答复，无阻塞 |
| TEST-CHG0023-APR-003 | 可注入终端 once/session/reject/空白/无效/EOF | 作用域正确，无误批准/死锁/并发 stdin 读取 |
| TEST-CHG0023-APR-004 | reply 与 reset/abort 竞争、请求取消、迟到答复 | pending 至多消费一次，等待终止，旧请求不能执行或批准新回合 |
| TEST-CHG0023-APR-005 | Host GET/POST permission、once/always/reject、asked/replied | 原字段/状态码/Basic Auth/顺序兼容，失效卡片结束，无迟到 asked |
| TEST-CHG0023-APR-006 | 同 session 后续回合、不同 session/工具、重启 | 仅本 session 同工具授权复用，其他需确认，不扩大作用域 |
| TEST-CHG0023-APR-007 | 工具输入/原因凭据哨兵、auto/manual | 展示不泄露输入，默认模式和明确人工模式均符合约定 |
| TEST-CHG0023-CHECK-001 | mock/echo/hello 两轮、pipe chat、help/clap | 离线无密钥/外部网络；落盘 smoke、原选项、新选项与唯一参数定义兼容 |
| TEST-CHG0023-CHECK-002 | cargo test -p openpaw-llm --locked；cargo test -p openpaw-store --locked；cargo test -p openpaw-engine --locked | 共享能力与既有 provider/Engine 回归通过 |
| TEST-CHG0023-CHECK-003 | cargo test -p openpaw-cli --locked；同命令加 --features engine-host | 默认/Host 两构建的实际 CLI、配置、互通与审批回归通过 |
| TEST-CHG0023-CHECK-004 | cargo test -p openpaw-engine-host --features engine-host --locked | 现有 HTTP/SSE/审批/取消/恢复/关闭/Office 故障隔离回归通过 |
| TEST-CHG0023-CHECK-005 | fmt、定向 clippy、默认 dependency tree、xtask lines | 默认 CLI 无 axum/Host，不增加预算，记录基线与实际增量 |
| TEST-CHG0023-CHECK-006 | governance validate/contracts/comments/pr-check/comments-diff 与消费者既有验收 | 真正执行并报告退出码/限制；不冒充通过，不执行 approve |

Windows 缺 MSVC linker 时使用前轮已验证的隔离 Linux Rust 容器，生产 Compose 不更新。
修复完成需原诊断回放转绿、定向测试与相关旧契约通过、默认 feature 隔离成立，并真实记录治理/预算限制。
草案阶段上述修复回归均未执行；源码存在、历史绿测或模拟结果不能替代当前候选整体验收。

## 11. 回滚

不迁移 JSONL/SQLite schema，回滚代码与二进制；保留所有 home/session/index/audit 数据。
CLI 新参数与 manual 选项在旧版本失效，Host 恢复原 manager，内存 pending/授权随进程退出失效。
旧 CLI 不及时同步 Host 会话/回退，不能把回滚后的显示当回退正确性证据；索引仍只是缓存，不能删除全 DB。
connected 回到固定四项时需同步回滚目录文档/契约，并说明未配置网络 provider 会重新出现。
已写 committed revert 的会话只能回滚到仍识别该事件的 Host，继续遵循 CHG-0019 的旧 reader 限制。

## 12. 验证与审批记录

| 日期 | 执行者 | 行为与实际结果 |
| --- | --- | --- |
| 2026-10-12 | 用户 | 要求第三方模型配置、其他候选同 bugfix 修复、chat 工具审批和 Host 组装方向；随后明确要求三份草案合成一份。这是文档整理授权，尚未批准业务实施 |
| 2026-10-12 | Codex | 已读 AGENTS、设计/验收基线、相关配置/架构/协议/测试与 CHG-0007/0013/0016/0019/0022，按 ask-matt 路由检查复用能力 |
| 2026-10-12 | Codex | 前轮 Windows cargo build -p openpaw-cli --locked 退出 1：缺少 MSVC link.exe |
| 2026-10-12 | Codex | 前轮已有 Rust 1.88 容器、只读源码/缓存执行 cargo build --offline --locked -p openpaw-cli --features engine-host 退出 0；有既有 sandbox/goal-ledger 警告 |
| 2026-10-12 | Codex | 前轮 probe.py 退出 1（预期红灯）：CLI 忽略自定义 endpoint，Host Anthropic /v1 重复，Host 会话未被 CLI 索引发现，model list 漏配置模型。诊断容器已清理 |
| 2026-10-12 | Codex | 前轮 governance/check.py 的 validate、contracts、comments all/report-only、pr-check、comments-diff 均退出 2：外部治理框架缺失且 AI_GOVERNANCE_FRAMEWORK 未配置；不属于已通过验收 |
| 2026-10-12 | Codex | 只读核实 Core/Engine 审批、chat 默认接线、Host pending/SSE 与作用域、Store schema/Reader、Host 重放与 mutation；新会话/审批测试尚未执行 |
| 2026-10-12 | Codex | 合并模型、会话、审批三份未批准草案，文件范围去重、禁止范围统一、测试统一编号；原稿备份在 ../artifacts/chat-runtime-audit/merged-drafts-2026-10-12/，docs/changes 只保留本 CHG |
| 2026-10-12 | Codex | 合并稿结构与范围检查通过：12 个章节、41 个唯一允许文件、31 个统一测试编号；原三稿非 CHG 文件范围完整保留，无重复范围/编号、缺失的既有修改文件、冲突标记或尾空格 |
| 2026-10-12 | Codex | 本轮 governance/check.py index 的 Python 原始退出码 2：通用治理框架缺失。人工同步 INDEX 的唯一 CHG-0023 draft 行并标注来源，不冒充工具生成或验证成功 |
| 2026-10-12 | Codex | 本轮仅整理 CHG/索引，未修改业务/测试源码，未重新执行构建、修复回归或 validate/contracts/comments/pr-check/comments-diff；这些正式检查等待批准实施和治理框架恢复。既有失败记录保留 |
| 2026-10-12 | 用户 / Codex | 用户明确回复“OK开始干活吧”，批准本 CHG 完整范围；开始实施，AI 仅记录人工批准，不运行 approve |
| 2026-10-12 | Codex | 先执行红灯回归：CLI 不接受 --base-url；无索引的 Host 会话不能被 CLI 发现；丢弃审批 future 后 pending 卡片残留。完成共享配置、历史/目录/索引、审批 gate 与入口 adapter 的实现 |
| 2026-10-12 | Codex | 完整 Rust 回归退出 0：共享 LLM/Store/Engine 147 项、默认 CLI 19 项、带 engine-host 的 CLI 21 项、Host 97 项（50 unit + 14 HTTP + 33 reliability）；这是 284 次执行，CLI 两种构建包含重复测试，不等于 284 个不同用例 |
| 2026-10-12 | Codex | 完整回归后补充活动 Host 与 CLI 查询的确定性 latch 测试，并规范化 Host 显式模型空白。最终定向 CLI 互通/帮助共 7/7，Host unit 50/50，均退出 0；当前 session_host_parity 包含 3 项 |
| 2026-10-12 | Codex | 原 probe.py 经 replay_probe.py 回放退出 0；CLI/Host OpenAI、Anthropic 根路径与 /v1 共六项全部通过，地址/模型/token/认证匹配、Host 会话 CLI 可见、自定义模型目录检查通过。保留原 baseline-evidence.json，候选输出为 fixed-evidence.json/probe-fixed.log |
| 2026-10-12 | Codex | 最终 cargo fmt --all -- --check 和定向 clippy --all-targets 均退出 0；默认 cargo tree 退出 0 且无 axum/Host。Clippy 仍有 warning，包含既有依赖警告和迁入 Store 的 SessionMutationEvent large_enum_variant，未冒充零警告或执行 --fix |
| 2026-10-12 | Codex | 未修改 OpenWork 产品代码，执行 pnpm evals:pr specs/openpaw-engine-attach.test.ts 退出 0、7/7、0 skip；真实连接隔离候选 Host、本地 503 fixture 和假 Basic/API 凭据，验证目录、会话/回退、SSE/REST 错误、permission、鉴权及 v1/v2 边界。自有容器清理退出 0；没有运行浏览器 UI/E2E |
| 2026-10-12 | Codex | 最终 xtask lines 退出 1；四项超限和真实基线对照见下表。未改 budgets、CI 或压缩/删除可靠性测试来绕过门禁 |
| 2026-10-12 | Codex | validate/contracts/comments/pr-check/comments-diff 均实际退出 2：AI_GOVERNANCE_FRAMEWORK 未配置且固定版外部治理工具缺失。独立 JSON Schema validator 也不可用；Host fixture/shape 测试通过不能代替正式 contracts 门禁 |
| 2026-10-12 | Codex | 最终轻量范围自检退出 0：41 个批准路径中实际变更 40 个，无范围外文件，git diff --check 退出 0；新模块 42 个显式 public 函数八栏目齐全。未修改 cli_session_e2e.rs，既有测试与新增 parity 测试已覆盖会话行为；此自检不替代正式 comments/PR 门禁 |

### 12.1 当前候选执行证据

证据目录为仓库外 `../artifacts/chat-runtime-audit/`。`run_checks.py` 保存完整回归，
`final_checks.py` 保存最后一次定向检查，`consumer_check.py` 保存消费者 attached 执行。
具体命令、原始退出码和日志分别见 `test-results.json`、`final-check-results.json`、
`quality-results.json`、`governance-results.json`、`consumer-results.json`、`final-static-results.json`；
后者记录允许范围、工作树逐文件 SHA-256 和注释轻量检查，初始缺组件失败与成功重试同时保留。

Rust 测试使用 Linux Rust 1.88、`--offline --locked`、只读源码和 Cargo 缓存；完整回归容器为 `--network none`，
HTTP/SSE fixture 在容器内回环运行。Engine 原 parity 测试会写 fixture，因此只挂载一个额外可写 fixture volume，
不写仓库源码。镜像原有 `OPENPAW_ENGINE_PERMISSIONS=manual`，测试容器显式设为 auto，
避免既有自动工具测试等待审批；manual 场景由测试自身明确启用。
首次只读 fixture 导致的五项失败及默认 manual 导致的两项等待均属测试环境问题，修正后完整回归通过。
Rustfmt/Clippy 仅在临时检查容器中安装，成功重试没有修改项目依赖。
最终定向检查容器允许联网下载这两个组件；cargo 仍离线运行，模型请求均使用本地 fixture 和假密钥。

### 12.2 行数门禁的真实基线与增量

基线是 `d9b03ce1a8d7825e629ff337c87bdbe0f2e48dcf` 的 Git archive，
用同一未修改 xtask 工具测量，保存在仓库外 `baseline-d9b03ce-utf8/`；结果见 `lines-baseline.log`。
候选最终结果见 `lines-final.log`，未调整任何预算。

| 受限项目 | 基线行数 | 最终候选 | 变化 | 上限 | 候选超出 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Store 业务 | 1323 | 2576 | +1253 | 2000 | 576 |
| Host 业务 | 6917 | 6436 | -481 | 5700 | 736 |
| 辅助测试/xtask | 10008 | 10769 | +761 | 10100 | 669 |
| Host 集成测试 | 3140 | 3148 | +8 | 1900 | 1248 |

Host 业务与 Host 测试基线已超限；Store 和辅助总量为本次新增超限。
功能回归通过不能消除这些门禁失败。后续须在批准范围内进一步收敛，或由负责人通过独立变更裁决预算，
AI 不自动提高预算，也不据此宣称可发布。

本 CHG 的编码和功能回归已完成，当前交付为 bugfix 分支的未提交源码候选。未推送、合并或更新生产 Docker Compose。
正式治理工具和行数门禁仍受阻，浏览器 UI 验收未执行；实施批准不等于合并、部署或发布批准。

## 13. 已批准补充：历史与分支会话的整轮回退

**状态：已批准（approved），2026-10-13；用户明确回复“批准”，授权本补充的实施、验证、引擎更新及报告会话的可撤销修复。**
本节沿用同一个 `bugfix/chat-runtime-provider-parity` 分支和 CHG，仅追加下列边界；
第 4、5 节其余限制不变。此批准为用户的实施授权，不替代正式治理审批。

### 13.1 用户报告、复现和根因

用户在引擎回复上点击 Revert 后，用户输入仍显示，输出消失，并立即看到
`The task was accepted, but no result arrived`。未重新发送任何消息。

在当前部署 CHG-0023 引擎中只读复核旧会话：回退游标指向 assistant，
浏览器显示 1 条 user、0 条 assistant，并显示上述提示；该会话没有 TurnOutcome。
原消息内容、认证头及凭据未输出、未保存，原会话未被测试改写。

最小独立复现：创建会话 → echo 完成一轮 → fork 完整历史 → 对 fork 的 assistant 调用 Revert。
连续两次返回 assistant 游标、可见前缀只有 user，测试退出 1；无需真实模型或旧版本镜像。
对照：新建非分支会话完成两轮，在回复按钮上 Revert，正常返回 user 游标，
界面隐藏该轮输入和输出，保留前一轮，刷新后仍成立。

根因：`fork_session` 按合同只复制消息，不复制回合/admission/error 记录；
旧格式消息同样可能没有回合记录。`normalize_revert_boundary` 只通过稳定 assistant ID
和 TurnOutcome 关联 user；未命中时返回原 assistant 边界。OpenWork 如实隐藏该边界及尾部，
剩余 user 被其既有接收结果状态机判成 unresolved。不是重新调用模型造成的失败。

证据与可执行命令位于工作区 `../artifacts/openwork-revert/`：

```powershell
node artifacts/openwork-revert/fork-repro.mjs
node artifacts/openwork-revert/browser-repro.mjs assistant
node artifacts/openwork-revert/inspect-reverted.mjs
```

只读旧会话复现的完整命令保存在该目录 `report.md`；不在此公开原会话内容。

### 13.2 最小修复、替代方案和边界

1. 保留现有请求 ID 校验、可见消息前缀与稳定回合归属匹配。
2. 对已命中的真实 assistant，若缺少有效的回合关联，则在其前面的同会话消息中
   向前找最近的真实 `Role::User` 输入，跳过非空且全部为 `ContentBlock::ToolResult` 的工具结果消息，
   用同一 `wire_message_id` 规则生成权威回退游标；角色与内容块类型规则与现有公共消息投影一致。
   这会同时隐藏该用户输入、此轮所有 assistant/tool 消息及后续尾部。
3. 已验证的失败展示 assistant 继续使用原 TurnOutcome 映射；未知、被隐藏或伪造 ID
   不通过此补偿获得可见边界。assistant 前没有 user 时保留现有边界行为。
4. 不让 fork 复制 TurnOutcome；不根据模型正文推断归属；不修改 OpenWork 状态机或隐藏错误卡片。
   消息顺序和角色已经足以验证真实回复的最近用户输入，不需要新字段、协议或存储格式。
5. 不在加载时重新解释旧的 committed revert，不自动重写旧游标或 JSONL。
   对已处于旧 assistant 游标的会话，部署后通过既有 Restore/unrevert 恢复，再重新 Revert；
   这样追加的新游标直接是 user，避免破坏旧记录与 committed cursor 的严格一致性校验。
   可在用户授权该补充后，对本次报告的会话执行这一项可撤销修复；先确认无活动回合。
6. Revert 只回退会话历史，不撤销已经完成的文件、工具或外部副作用。

替代方案：复制分支回合记录会扩大数据流并违反现有 fork 合同；仅屏蔽 UI 提示仍会留下
错误模型上下文；自动迁移所有旧游标涉及恢复决策和提交校验，均不纳入本补充。

### 13.3 追加允许范围

| 仓库 | 文件 | 允许符号/范围 | 必要性与影响 |
| --- | --- | --- | --- |
| OpenPaw | `crates/openpaw-engine-host/src/session_mutation.rs` | `normalize_revert_boundary`、该 helper 的 Rustdoc 及相关内嵌测试 | 增加真实 assistant 的已验证顺序映射；Host 运行时负责人审查，不改写入流程 |
| OpenPaw | `crates/openpaw-engine-host/tests/http.rs` | 旧消息/完整 fork 的 assistant 回退、恢复/替换/重启回归；复用现有 fixture | 验证真实接口返回 user 游标，完整消息仍可 unrevert，替换不会保留旧 user |
| OpenWork | `evals/specs/openpaw-revert-resend.e2e.test.ts` | 既有 journey 追加通过回复按钮回退分支的场景 | 对真实 DOM 断言输入/输出均隐藏、前轮保留、无 unresolved 提示，刷新及替换一致 |
| OpenPaw | 本 CHG | 第 13 节状态、证据及验收记录 | 固定追加范围及批准记录，保留原批准内容 |
| OpenPaw | `docs/b-line/protocol.md`、`docs/b-line/testing.md`、`docs/b-line/traceability.md` | 仅整轮回退的已验证顺序映射与新增回归记录 | 使描述与本次可观察行为一致 |

禁止改动：`runtime` 调度/锁/写入与恢复决策、fork 的复制合同、Core/Engine/LLM/Store 业务代码、
公共 JSON 字段与 schema、依赖/锁文件/CI/预算、OpenWork `apps/**`/`packages/**` 产品源码、
Office 和 Gateway、原有 `.ua/`、模型配置和凭据。测试不改写用户会话。
工作区外层诊断证据、已有离线构建与 Compose 更新入口沿用现有部署授权，不是新增产品业务路径。

### 13.4 验证、部署和回滚

- 先在现有测试位置保留 red：无 TurnOutcome 的旧 UUID、fallback ID、多个 assistant/tool
  输出，以及真实 fork 后的 assistant 回退；未知 ID 保持拒绝。
- 回退后应返回 user 游标；完整 REST 历史供 unrevert；默认 fork 与替换上下文只含保留的前缀。
  覆盖 unrevert、提交替换及重启，避免只测试 helper 而漏掉实际调用点。
- 执行 Host 定向及现有 session/revert 回归、共享 Store/CLI 会话互通回归。
  OpenWork 运行 `pnpm evals:e2e openpaw-revert-resend`，记录 placement/verdict/skips。
- 经既有离线构建入口生成镜像，确认无活动会话后更新 engine-host；复测原报告会话及独立分支。
  保留镜像哈希、真实 DOM 断言和清理记录；不把既有治理/行数阻断算作通过。
- 回滚采用部署前镜像；不改变持久格式，旧 reader 仍接受新增的合法 user 游标。
  修复前的未提交回退可用 unrevert 撤销；已经提交的回退沿用原规则，不能靠 unrevert 恢复。

2026-10-13 诊断结论：已复现并最小化，用户批准后完成修复、回归、本机引擎更新及报告会话的可撤销游标修复。
本补充功能验收通过；原 CHG 的正式治理、预算及 CODEOWNER 门禁仍受阻，不以本机部署代替正式发布验收。

### 13.5 实施与验收记录（2026-10-13）

| 项目 | 实际结果 | 证据（仓库外 `../artifacts/openwork-revert/`） |
| --- | --- | --- |
| 用户批准 | 用户明确回复“批准”；先更新本节 approved，再补测试与修改业务代码 | 本会话批准消息、第 13 节 |
| 初始 HTTP red | 旧消息及完整 fork 两项均返回 assistant，退出 1；未改写原会话 | `http-red.log` |
| 修复与新增回归 | 使用同一 wire ID 转发规则，真实 assistant 缺回合记录时跳过纯 ToolResult 定位输入；UUID/兼容 ID、恢复、替换、重启、源隔离均通过 | `http-green.log`：2/2，退出 0 |
| Host 全包 / CLI 互通 | Host 50 unit + 14 conversation_reliability + 35 HTTP = 99/99；session_host_parity 3/3；均退出 0 | `host-tests.log`、`cli-parity.log` |
| OpenWork journey | `pnpm evals:e2e openpaw-revert-resend`；placement: local (daytona CLI missing or not authenticated)；2 passed、0 failed、0 skipped；verdict passed，退出 0 | `openwork-eval.log`、`eval-final-run.json`、`eval-passed/` |
| UI 断言 | 真实分支无 turn 记录；回复按钮 Revert 隐藏整轮，保留前轮；超过 1500ms grace 无提示；刷新一致；替换上下文无旧输入/输出且源会话保持原历史 | `eval-passed/fork/test-run.json` 与已运行 spec |
| UI 环境与测试定位失败 | 初始缺 lsof、未启用 init 留下 zombie；修正后确认旧验证镜像按钮操作区透明但可命中。最后使用公共 CDP locator 验证 hitTestOk，再真实鼠标点击按钮，不调用处理函数/修改 DOM；不声称验收按钮像素可见性 | `openwork-eval-initial.log`、`openwork-eval-locator.log`、`openwork-eval-hover.log`、`eval-hover-red/`；最终移除 DEBUG |
| 格式 / Clippy / 范围 | cargo fmt、Host all-targets clippy、diff 检查退出 0；允许范围无违规，保留既有警告 | `fmt.log`、`clippy.log`、`diff-check.log`、`scope-results.json` |
| 治理五项 | validate/contracts/comments/pr-check/comments-diff 均退出 2；固定版本外部框架缺失；未运行 approve | `static-results.json` 及各项日志 |
| 行数预算 | xtask lines 退出 1：Store 2576/2000、Host 6466/5700、aux 10769/10100、Host tests 3376/1900；不提高预算或宣称门禁通过 | `lines.log` |
| 构建与更新 | 既有离线入口退出 0，源码只读、Cargo offline/locked；切换前 activeCount=0；仅更新 engine-host，健康检查通过 | `../chg0023-deploy/build-results.json`、`engine-pre-deploy.json`、`engine-deployed.json` |
| 本机原始复现复测 | 独立 echo→完整 fork→回复 Revert 返回 user，visibleRoles=[]；测试源/分支已删除 | `fork-result.json`，passed=true |
| 报告会话修复 | 校验旧 cursor、两条原消息身份、零回合及无活动任务后，通过 unrevert/revert 追加新游标；完整 REST 历史保持一致 | `repair-result.json`，passed=true |
| 原会话真实浏览器复核 | userCount=0、assistantCount=0、outcomeUnknown=false；没有重发用户输入 | `browser-existing-result.json`，passed=true |
| 隔离与其他会话 | 另两个 staged cursor 保持不变；本机普通两轮回复 Revert/刷新回归通过；Compose 四个服务均 running/healthy | `other-sessions.json`、`browser-assistant-result.json` |

本机已运行镜像：`sha256:28e03a0e041349da4c0f0b7b4b39a2b681aa8c6803fce8fcceec3212e963d2f3`。
实际程序 SHA-256：`37ee0425ec3b26ab0e37e5d4de3fd074e1fa0ef4523f267e2d3b7e89bebaaf47`，与构建产物一致。
回滚镜像：`gitee-openpaw-engine-host:pre-chg0023-b0921a01fdeb`。
构建源码清单记录构建时文件状态；本节验收记录在构建之后补齐，不改动已验证的业务实现。
源码仍在 `bugfix/chat-runtime-provider-parity` 的未提交候选中，未执行推送、合并或正式治理批准。
最终清理：已移除本次隔离验证容器与本机浏览器 profile，测试会话均清理，淘汰未跳过工具结果的初始 patch。
`final-check.json` 验证修复业务文件/HTTP 测试与构建清单一致、消费者 spec 与实际运行文件哈希一致，
普通浏览器测试会话返回 404、无残留 fork 诊断会话；`engine-final.json` 健康且 activeCount=0。

## 14. PR 提交准备（2026-10-13）

用户在本会话要求向 merge 分支提交 PR，授权提交、通过既有 GitHub SSH remote 推送本 bugfix 分支，
并创建面向 `merge/openpaw-engine_host` 的 PR；不包含合并或正式治理批准。
已拉取 GitHub 最新 refs；目标分支仍为 `d9b03ce1a8d7825e629ff337c87bdbe0f2e48dcf`，与本 CHG 基线一致。

提交前核对构建源码清单中的 248 个 crates/contracts 文件，全部与本机已验证候选 SHA-256 一致；
本轮只补交付记录，不改已验证业务源码。提交身份沿用全局 `J R <2806429860@qq.com>`，提交附 DCO Signed-off-by。
未登记的 `.ua/` 分析产物留在工作区，不纳入本 PR；OpenWork 消费者的浏览器测试属于另一仓库，不混入本仓提交。

PR 按草案提交，保留第 12、13 节的真实功能证据及治理/行数门禁失败；不将定向 Clippy 成功写成
workspace `-D warnings` 通过，也不将消费者本机证据写成 GitHub CI 结果。多模态仅完成只读能力检查，未实现、不包含在本 PR。
PR 说明与本轮范围/治理检查输出保存在仓库外 `../artifacts/chg0023-pr/`。GitHub CLI API 尚未登录，
SSH 可以拉取和推送；远程 PR 创建结果以 GitHub 返回的 URL 为准。


## 15. PR 门禁修复接续（2026-10-13）

本功能已以 `e936575` 提交并推送，PR #1 已按用户要求关闭；原功能与历史失败证据保留。
后续修复及本地验证见已批准的 CHG-2026-0024：workspace 严格 Clippy、四项预算、外部治理依赖及新暴露的依赖门禁已处理；新 PR 继续面向 `merge/openpaw-engine_host`。没有合并或新增部署。
