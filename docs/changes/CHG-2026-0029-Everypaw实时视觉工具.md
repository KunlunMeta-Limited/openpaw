# CHG-2026-0029：Everypaw 实时视觉工具

状态：approved / 已有用户范围授权登记；逐票实施和本地验证待完成。日期：2026-10-16。

## 授权、基线与边界

用户已确认 Everypaw 的桌面能力迁移规格、17 项正式任务并明确调用 implement-spec。规格 D05/D13–D19/D22 和任务 06–11 明确要求修改官方 Host 的工具声明、可信会话调用、取消、真实结果、PNG 主模型路径及交付证据。按 AGENTS.md“已有明确用户授权覆盖该范围时记录授权后继续”，本记录登记该既有授权，不再要求用户重复批准相同范围。历史 CHG-0026/0028 不作为本次新增功能的授权。

权威需求：相邻 Everypaw 仓库 `.scratch/architecture-revision/spec.md`、`ticket-plan.md` 和正式 `issues/06` 至 `11`；不得以审核前 ticket-drafts 代替。源码基线为 `c105d80356f7217138d3b70bfab1d72d5a271f4d`，来自 `merge/multimodal-ai-pc-context-tools`；独立集成分支 `feature/everypaw-desktop-visual-integration`，工作树位于 Everypaw 忽略目录 `.local/engine-visual-integration/`。不修改用户现有源码分支、主服务、模型配置、会话或卷。

这是可审查实现的范围授权，不能代替 L3 受保护接口 CODEOWNER 审阅、来源 DCO 缺口处理、CI 或部署批准；这些门禁保留未完成。任务图的依赖仍有效：准备本记录不表示 06 已开工或完成。

## 允许修改文件

| 文件 | 必要修改、符号与影响 |
| --- | --- |
| `crates/openpaw-engine-host/src/ai_pc.rs` | 扩充现有私有会话工具包装器、登记能力回执、动态工具声明与分发；保留原工具和媒体透传。新增视觉工具的严格参数、受信封装、取消控制、解析后 ACK、PNG InlineMedia 与相同模块的边界测试。模型参数不能携带身份或凭据。 |
| `crates/openpaw-engine-host/src/ai_pc_visual.rs`（按需要新增） | 私有视觉协议/活动回合上下文、受信 invocation 标识、代次 RAII 及 HTTP 结果/媒体验证；为避免 ai_pc.rs 过大可拆出同一职责，不改变依赖方向或行数计数。不得暴露为 Core/全局工具 API。 |
| `crates/openpaw-engine-host/src/lib.rs` | 若新增上述私有模块，只增加 feature 隔离下的模块声明；不新增公开导出或默认依赖。 |
| `crates/openpaw-engine-host/src/runtime.rs` | SessionHandle/AppState 持有小型私有活动工具上下文；真实回合获准 running 后安装 requestID、Host turn、取消代次和所选模型实际能力，退出时仅清理自己的代次。复用原有会话生命周期、执行、持久化与取消；不绕过 runtime 写 Session，不改变 AgentLoop 算法。 |
| `crates/openpaw-engine-host/src/http.rs` | 在既有受 Basic 保护的登记入口返回向后兼容的视觉协议/已实现工具回执；保留旧请求与失败码。必要的 Router 测试放入既有内联测试模块，不改变一般 prompt、SSE 或 Office 语义。 |
| `docs/b-line/protocol.md` | 登记扩展回执、五类视觉工具参数、受信调用/取消/ACK、结果与媒体契约、期限、错误和兼容行为；沿用登记接口稳定编号。 |
| `docs/b-line/configuration-and-connection.md` | 记录实际源码和构建身份、旧 Host 降级、容器宿主机路径、实际模型输入能力和未部署边界。 |
| `docs/b-line/testing.md` | 记录工具/取消/ACK/媒体/身份的受控应用验收及真实 Docker、Windows、所选模型验证的不同证明范围。 |
| `docs/changes/INDEX.md` | 登记本次新增 CHG 的独立授权、状态、实际文件数和未完成发布门禁。 |
| 本 CHG | 更新实际符号、协议工程选择、逐项命令/退出码、审查结果、未执行原因与回滚说明。 |

只允许表内文件出现在本次引擎分支差异中。若需要其他文件、依赖、公共接口或预算变更，先具体更新本 CHG 并核对已有授权是否覆盖；不能借用本表泛化扩大范围。新增/修改公共 Rust 项使用中文 Rustdoc，函数与方法具备固定八栏目。

## 禁止路径及理由

- Core、Engine/AgentLoop、Store、默认 Tools 注册表、公共 trait 和 provider：现有媒体与执行路径已经具备所需能力；先采用 Host 私有上下文与包装，不把桌面专属身份植入中立层。
- Cargo.toml/lock、feature、CI、CODEOWNERS 和 xtask 行数/审计算法：本次不需要新依赖，不改变默认 CLI 无 axum，不通过移动测试或修改计数掩盖预算。若实测确需调整预算，必须在修改前单独记录测量、原因和最小范围，不能默默放松门禁。
- Office、OpenWork、CLI 行为、部署文件和环境密钥：保持既有消费者与服务；不新增 Shell/文件系统/通用远控工具，不更换模型，不访问生产桌面作外发验收。

## 行为和数据流

1. 保留 `PUT/DELETE /session/{id}/ai-pc` 的 Basic 身份、严格登记请求、内存生命周期和 fork/delete/restart 隔离。成功登记回执另明确视觉协议版本和实际已实现工具；只有旧 204 时 Everypaw 仍可普通文字问答和使用旧工具，不能伪报五类视觉工具可用。各票只声明已实现工具。
2. 新增职责为列窗口、实时结构、UTF-16 正文分页、目标 PNG 和顺序动作批次（invoke/click/set text/shortcut/wait）。具体名称/字段由对应实施票记录；本地取得预算由桌面端控制，模型参数不能扩大。默认广泛工具授权，复用既有工具审批，不新增独立读写开关、逐步骤权限弹窗或审批模型。
3. 真实调度回合进入 running 后安装私有活动上下文，包含已接收 requestID、Host turn、取消代次和实际所选模型能力；RAII 只能清理自己的代次。包装器从此上下文与可信登记生成唯一 invocation ID，携带真实 session/request/turn/invocation 与模型的工具参数，经现有 Docker 可达桥转到普通桌面后端和受验证本机 IPC。身份不从模型参数读取。
4. 原始 provider tool_use_id 并非现有 ToolExecutor::call 输入；不能按工具名/参数/顺序猜配并发取消的调用。原生 invocation ID 为真实独立主键；能通过真实 ToolResult 事件精确关联时可补 provider callID，无法取得时留空。
5. 不同工具采用明确工程等待期限/容量；HTTP 收包期限不等于 UIA 执行期限。工具执行只发一次，不内部重试。桌面 Stop 按原回合直接取消为主；Host future Drop 的显式取消为独立、有界的补充控制。取消先到时必须拒绝晚到工作，不能误执行动作。同步原生动作未结束不代表已取消，执行位由能力端持有到真实结束。
6. 每次结果保留唯一 delivery ID 和已投影的不可变内容。包装器完整读取、解析并验证媒体/活动代次后，向可信桌面桥发送精确调用与 delivery ID 的 ACK。只有此证据可标记 Host 已解析/接受该投影；不是 Engine/provider 接收或模型理解。ACK 失败如实保留 unknown，不能重执行；撤销后的后续交付和受管副本清理由原授权链处理。
7. PNG 使用既有 ToolCallOutcome.media、InlineMedia 和主模型协议；以实际所选模型声明判定 image，不按模型名推断。无支持时明确失败，不 OCR、第二 VLM、自动换模型或静默文字替代。保留既有每图 5 MiB、每回合 4 个不同附件/20 MiB、32 MiB 请求等引擎传输限制，超限明确失败，不自创缩放/最长边/图像 token 默认值。
8. 旧 current/history、已登记会话限制全局 session_search、媒体回看/清理、会话持久化、普通模型网络重试和现有 Office 语义继续有效。令牌、完整采集内容和 PNG 不进入诊断日志或 Office 动作。

## 替代方案

不只改 Everypaw 路由或提示词，因为模型必须实际可选择工具并收到结果。不新增通用 MCP/独立网络原生入口。不直接改 Core ToolExecutor 传入上下文：本期可信应用回合/原生 invocation 可以通过 Host 私有状态实现；若未来必须在执行前取得 provider callID，再按受保护接口治理。ACK 区分真实 Host 解析与仅 socket 写成功，不以界面推断代替交付事实。

## 验证计划

沿用已获用户确认的悬浮提交与 Host 工具桥 seam：真实受控 Host/HTTP、原生能力替身、隔离本地存储与受控时序，检查模型真实输入、调用次数、媒体字节、来源和取消/迟到。逐票红→绿，保留原工具和会话回归。错误参数/身份伪造、未登记、旧 Host、服务切换/撤销、取消早到、相同并发调用、超时不重发、ACK 失败和跨会话媒体均需覆盖。

最终按 AGENTS 执行 fmt、workspace strict Clippy/tests、lines/scrub/snapshot/golden/telemetry、显式 Host feature strict Clippy/tests、CLI/Host parity、默认 CLI tree 无 axum 与现行依赖检查，逐项登记真实退出码。预算不足和既有未通过项不得隐去。真实 Docker 可达、Windows 操作和实际所选 DeepSeek 模态验证另记，替身/回环不冒称通过；当前 Docker Desktop 管道不可用，未启动服务。

## 回滚

未部署，只回退本次独立源码分支/补丁；不重置任何用户来源分支，不删除 session/卷/授权/证据。应用发现旧 Host 时明确关闭新增视觉工具能力，保留普通问答及旧工具。未来部署仍需最终具体制品与单独授权；此 CHG 不执行服务切换。

## 执行记录

2026-10-16：已完成基线和规则只读核对、建立隔离集成工作树并登记已有用户范围授权。尚未修改引擎业务代码、运行构建、调用模型或部署；待 Everypaw 04 完成后认领 06，后续逐票补实际选择及验证。
