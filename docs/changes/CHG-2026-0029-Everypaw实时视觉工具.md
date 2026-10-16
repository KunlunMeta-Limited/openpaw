# CHG-2026-0029：Everypaw 实时视觉工具

状态：approved / 06 列窗源码、Host 本地门禁及 Everypaw 成对验收完成；后续视觉票继续进行。日期：2026-10-16。

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

### 07 实施前登记（2026-10-16）

基于 `4acf3c8`，继续已批准路径内的实时结构工具：`ai_pc_query_structure`。模型仅传 native generation、target_id、逗号分隔 directions、offset、limit、boxes；可信会话/Host turn/执行 generation/invocation 继续由运行时安装。方向集合对齐固定 Everywhere 的 all/parent/child(children)/previous(prev)/next/sibling(siblings)/none，默认 all/0/128/false、limit 最大 1024，非法输入拒绝。工具为 ReadOnly/ParallelSafe；复用现有一次调用、取消及精确 ACK 合同，只按工具选择 `/ai-pc/windows` 或 `/ai-pc/structure`，不提前声明其他未完成工具。模型投影 Balanced4096 估算 tokens 和实际取得/信息权限由桌面负责。Host 新测试写在既有批准模块的自然 inline test seam，不移动已有测试、改变计数或预算；06 测得 Host business6454/7000、外部tests3943/4000、aux11497/11500，若真实增量超限先登记最小治理变更再继续。

2026-10-16：04 完成后，在 `feature/everypaw-visual-06`（`.local/engine-visual-06`）实施 06，基于已批准集成提交 `64d601c`。本轮实际改动为表内 10 个文件（包含本记录和索引），不修改 Core、Engine、Cargo、xtask 或生产服务。新增/修改公共 Rust 项已人工核对中文说明，函数/方法保留八栏目；登记路径保持原稳定编号。

### 06 实际符号与工程选择

- `ai_pc::Registration.visual`、`specs`、`DesktopTools::{call,list_tools}` 和 `ToolHostValidator::validate_input`：只新增 `ai_pc_list_windows`，严格空对象，沿用 ReadOnly/ParallelSafe；保留 current/history、受管 session_search 限制和媒体行为。
- 私有 `ai_pc_visual::{VisualContext,ExecutionGuard,call}`：`runtime::run_turn` 在 durable admission 获准 running 后安装可信 request/Host turn/UUID generation/取消 token/实际模型 image 能力；guard 只清理自己的代次。调用创建独立 invocation，不猜配 provider callID。
- `http` 现有登记入口成功回执增加 `openpaw-desktop-protocol: 1` 与 `openpaw-desktop-tools: ai_pc_list_windows`。桌面端只有看到此组合才宣称实时工具可用；旧 204 仍可普通文字问答和使用旧工具。
- 每次工具只发一次，Host 列窗等待 20 秒、结果 1 MiB；完整解析后对精确 invocation/delivery ACK 等待 2 秒、严格接受不超过 128 字节的 `{status:"accepted"}`。独立 OnceLock 取消队列容量 64，Drop 使用 try_send、每条最多 2 秒且不重试；队满/运行时关闭不保证补充通知。桌面原回合 Stop 是主取消路径。
- 桌面仍绑定随机端口 `0.0.0.0` 并登记 `host.docker.internal`；原生入口仅受验证本机 IPC。桌面桥 HTTP 收包 3 秒与 native 执行等待分开，最多两个 detached acquisition，占位到真实完成。迟到实际取得内容留在原调用本地，不恢复生成/不 ACK；此行为不等于取消了同步原生操作。

### 06 已执行命令

所有 Cargo 命令均经相邻 Everypaw `scripts/native.ps1`、独立 `.local/engine-target`，dev/test debug=0、jobs=1；未发出真实模型请求。

| 命令 | 结果与证明范围 |
| --- | --- |
| `cargo test -p openpaw-engine-host --features engine-host` | 退出 0；58+16+36=110 项。包括真实受控 Router/模型 HTTP 的可信回合与精确 ACK、取消及旧代次不 ACK/不重放。 |
| `cargo build -p openpaw-cli --features engine-host` | 退出 0；生成真实官方 `debug/openpaw.exe`，供后续 Everypaw 成对进程验收。 |
| `cargo test --workspace` | 首次退出 101：既有 Windows `add_write_acl_on_temp_dir` 在沙箱中 SetNamedSecurityInfo 错误 5。随后对隔离临时目录以执行提权重跑完整 workspace，退出 0。未修改测试或 ACL 实现。 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 退出 0，默认依赖隔离保持。 |
| `cargo clippy --workspace --all-targets --features engine-host -- -D warnings` | 最终退出 0，包含本轮新 Host 测试。 |
| `cargo test -p openpaw-cli --features engine-host --test session_host_parity` | 退出 0，3 项通过。 |
| `cargo fmt --all -- --check` | 退出 0。 |
| `cargo run -p xtask -- lines` | 退出 0；Host business 6454/7000、Host tests 3943/4000、aux 11497/11500。未更改预算/计数。 |
| `cargo run -p xtask -- scrub --check` | 退出 0，扫描 356 文件。 |
| `cargo run -p xtask -- snapshot --check` | 退出 1：检测到既有快照在 Windows checkout 为 CRLF，而检查器逐字节比较 LF rendered；未改 Core/snapshot/检查器。对未实施 06 的 `engine-visual-integration`（64d601c）同命令也退出 1，记录既有平台门禁，未重写受保护快照来掩盖。 |
| `cargo run -p xtask -- golden` | 退出 0，全部 golden 通过。 |
| `cargo run -p xtask -- telemetry` | 退出 0，284 dependencies、229 symbols clean。 |
| `cargo tree -p openpaw-cli --no-default-features` | 退出 0，axum 匹配数 0。 |
| `cargo deny --version` | 退出 1，本机未安装 cargo-deny；依赖审计由现行 CI 门禁完成，不宣称本地审计通过。 |

Everypaw 已运行 `paired_official_host_model_desktop_http_ipc_and_store_roundtrip --ignored`（退出 0）：真实官方 `openpaw.exe` 隔离进程、本地受控 SSE 模型、真实桌面 HTTP 桥、受验证 named-pipe 原生替身、隔离 Recorder、精确 ACK 和第二轮模型请求贯通；ToolResult 与保存投影逐值一致、原回合关联正确、不含 token。另实际 Windows `metadata_window_list_retains_real_child_witness_without_initializing_uia --ignored` 退出 0，证明 EnumWindows 取得自建子进程窗口、应用身份和 -wv 引用、OS 销毁后失效，metadata 未初始化 UIA。这两个证明面分别记载于 Everypaw `ticket-06-verification.md`。

真实 Docker Desktop Linux engine 管道不可用，未做容器连通验收；回环/原生替身不作为 Docker 通过证据。L3 CODEOWNER、来源 DCO 缺口、CI 和部署仍待独立完成；本轮不 push、不部署、不切换现有 DeepSeek 服务。
