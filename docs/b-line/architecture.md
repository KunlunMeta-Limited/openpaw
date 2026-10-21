# B 线软件架构设计

## 1. 文档目的与范围

本文按 GB 风格的软件设计说明结构记录 OpenPaw B 线实现，但不声明任何外部标准
认证。范围包括 AgentLoop、实验性 engine-host、AI Office action gateway 和 Office
数据/动画消费者。OpenWork、A 线、managed spawn、OpenCode v2 和 1.0 公共 API
承诺不在本期范围。

## 2. 系统框图

```text
OpenCode v1 client
       | HTTP / SSE (loopback, optional Basic Auth)
       v
+------------------------- openpaw-engine-host --------------------------+
| http -> protocol -> AppState                                           |
|                         |                                               |
|             +-----------+------------+                                  |
|             v                        v                                  |
|   SessionHandle (one/session)   global OfficeDispatcher                 |
|   AgentLoop + ToolHost          ordered mpsc + role owners              |
|   approval lifecycle            500 ms POST timeout                     |
|   cancel/delete gate                    |                               |
|             |                            |                               |
|             v                            v                               |
|   OPENPAW_HOME JSONL            AI Office gateway :8765                 |
|             |                    | actions | /office/*                   |
+-------------+--------------------+---------+-----------------------------+
              |                              |
              v                              v
       session REST history          Pixi scene + dashboard panels
```

## 3. 模块分解与依赖方向

| 模块 | 输入 | 输出 | 职责 |
|---|---|---|---|
| `lib.rs` | Cargo feature | public API | 仅在 `engine-host` feature 下导出 host |
| `protocol.rs` | HTTP JSON | serde types | OpenCode v1 子集、HostConfig、时间字段 |
| `http.rs` | HTTP/SSE | runtime calls | 路由、鉴权、工作区优先级、错误码 |
| `runtime.rs` | prompt/session calls | Engine/Store 调用、SSE | session 所有权、协调锁、恢复策略、协议和 Office 组装 |
| Engine `scheduler/turn_execution` | 中立容量、AgentLoop、Session | 运行凭据、结果与检查点 | FIFO、完整会话快照、错误/取消/future drop 恢复 |
| Store `journal/transcript/catalog` | 独占 writer、事件或目录 | 已提交历史与写入结果 | 文件事务、尾行修复、Fork 发布、历史变换和只读查询 |
| `events.rs` | `AgentEvent` | `OpenCodeEvent` | message/part/status/todo 事件投影 |
| `approval.rs` | guardrail request/reply/abort | approval response + SSE | 单 session、单代际 pending permission |
| `office.rs` | turn/tool/approval event | ordered action POST | 名册映射、单 visit、跨 session 角色仲裁 |

依赖只允许 `http -> runtime/protocol`、`runtime -> engine/core/store/tools/llm`，以及
`runtime -> events/approval/office`。core 和 engine 不依赖 Axum 或 host 协议。
AgentLoop 新增的 per-turn provider/model 入口仍只接受 core trait，不导入 host 类型。

## 4. 运行时对象模型

每个 `SessionHandle` 独占以下状态：

- `Session`、可关闭的 `SessionWriter` 和已解析的绝对 workspace；
- 一个 `AgentLoop`、一个当前回合 cancellation token 和 active/deleting 状态；
- 一个 `HostApprovalManager` 和 session SSE broadcast channel；
- 当前 session 投影到 Office 的角色集合和每回合 visit tracker。

不同 session 不共享 AgentLoop，因此 budget、cancel 和 permission 不串扰。全局只共享
只读 HostConfig、global SSE channel 和 OfficeDispatcher。OfficeDispatcher 对角色维护
session owner 集合：一个 session 结束时，只有没有其他 owner 的角色才会发 `idle`，
避免并行 session 互相覆盖。

## 5. Session 生命周期状态机

```text
             reserve_turn
 idle --------------------------> active
  |                                 |  \
  | DELETE(begin_delete)            |   \ abort
  v                                 |    v
deleting -> writer close -> unlink  | cancelling
                                    |    |
                                    +----+
                                         v
                    persist assistant -> idle
```

`turn_gate` 线性化 reserve、abort、release 和 begin_delete。DELETE 先设置 deleting，
从 AppState map 摘除 handle，关闭 writer，再删除 JSONL。已取得旧 `Arc` 的请求在
`reserve_turn` 处看到 deleting 并失败。AppState 的 `session_lifecycle` 互斥 lazy load
和 DELETE，因此删除后的文件不会被并发装载并重新插入。

## 6. Provider 与模型

Session 创建时的 provider/model 写入首条 meta。每个 prompt 按请求
`model.providerID/modelID` 选择本回合 provider/model；providerID 缺失时使用 host
默认 provider，modelID 缺失时使用 provider 默认模型。AgentLoop 本身仍归 session
所有，provider/model 只作为本回合不可变输入，因此取消与审批仍作用于正确 session。

Anthropic/OpenAI base URL 在 provider 构造时校验为无 query/fragment 的 HTTP(S)
URL 并移除末尾 `/`。OpenAI base URL 应包含版本路径（默认 `/v1`），provider 再追加
`/chat/completions`；Anthropic provider 追加 `/v1/messages`。

## 7. 可靠性与故障边界

- user message 先写 JSONL，成功后才加入内存并返回 204/同步响应；
- assistant 持久化失败记录 error，不伪造写盘成功；REST 仍以当前内存状态服务；
- approval 的 pending、aborted 和 token 位于同一 mutex 生命周期中；
- abort 与新回合 approval 初始化位于同一 turn gate，abort 后不会迟发 asked；
- 审批返回 `Approved` 后再次检查 cancellation token，再进入 ToolHost；工具 future
  与 token 进行 biased select，取消后不再启动已批准工具；
- Office POST 使用有界队列，500 ms 超时、不重试，网络错误不改变回合结果；
- session 摘要在 blocking worker 中只读 JSONL，不创建 AgentLoop、ToolHost 或 writer。

## 8. 可移植性

路径通过 `PathBuf` 处理。创建时把最终 workspace 转为绝对路径并写入
`Metadata.cwd`；Windows、macOS、Linux 重启恢复时均读取该值。旧 JSONL 没有 cwd
时回退 CLI 默认 workspace。host 只允许 `127.0.0.1`，端口 `0` 由操作系统分配。

## 9. 修改影响

`openpaw-engine/src/agent_loop.rs` 的修改原因是 host 需要在 prompt 阶段选择模型并在
审批后继续支持可靠取消；provider/model 作为单回合输入，不写回 session 元数据。原有
`run_turn` 和 `run_turn_with_cancel_token` 保持兼容。
CLI 仅在显式 feature 下构造 HostConfig。关闭 feature 后默认命令、sidecar 行为和
依赖树不变。回滚 B 线只需停用 feature，既有 JSONL 仍由 openpaw-store 读取。

## CHG-2026-0023 共享能力边界

CLI 与 Host 都调用 openpaw-llm::config 的配置/工厂和生成设置、openpaw-store::transcript/catalog 的中立 codec/历史重放/目录快照、openpaw-engine::approval 的审批 gate/manual 策略。
Host 保留 REST/OpenCode 类型及字段转换，CLI 保留终端输入/提示。runtime 继续持有 session、锁、回合调度、恢复和 writer，只对选定共享能力组装和调用。
共享模块不包含 HTTP、Office 或终端依赖；Core trait、AgentLoop、Tools 策略及默认 feature 的依赖边界保持不变。
Anthropic endpoint 对服务根路径或已有 /v1 前缀规范化；OpenAI 保留自定义父路径。

## CHG-2026-0025 运行职责下沉

Engine 拥有独立的 FIFO scheduler 与 turn_execution；AgentLoop 的模型/工具算法未修改。
CLI 和 Host 都实际调用受保护执行：接收输入后捕获完整 Session，异常、取消或 future drop
恢复执行前状态；成功结果保留检查点，供调用方在持久化失败时恢复。恢复不撤销工具外部操作。

Store journal 接管独占 writer 的追加、批次回滚、finish 提交、尾行修复及 Fork 文件发布；
transcript 接管 Fork/Revert 的纯历史变换和持久类型，catalog 接管分支序号扫描。
Host 持有 writer 与协调锁并决定接收、终态和恢复策略；SSE/Office 发布保持在 Host。
CLI 保留终端输入、审批交互和 SQLite 索引刷新，完整持久化 User 角色承载的 ToolResult。
Engine/Store 不依赖 Host，Engine 不新增 Store 依赖；默认 CLI 不引入 Axum。

## 内联媒体与压缩边界（CHG-2026-0026）

Core InlineMedia 定义稳定编号、规范 MIME、Base64 原件和可选名称；Image 兼容旧字段，Audio 与
ToolResult.media 承载录音和富工具结果。CLI/Host 在接收前规范化并校验所选模型能力；LLM 负责协议转换，
Tools 负责有界读取与会话内查找。Engine 不依赖 Store、HTTP 或 Office。

CLI 与 Host 启用同一个自动压缩器。模型 Session 是可压缩视图，入口保存的有效完整历史是持久化/REST/分支事实。
TurnExecution 收集本回合完整真实产出，避免压缩后索引切片丢失助手与工具消息。失败、取消、panic/drop 和保存失败
恢复 checkpoint；工具已完成的外部副作用不回滚。附件临时缓存随活动回合清理，不能跨会话或失败续留。

新增依赖仅复用已有 Base64（Core）和测试用 serde_json（CLI）；默认 CLI 仍无 Axum，Office 动作语义保持既有合同。
