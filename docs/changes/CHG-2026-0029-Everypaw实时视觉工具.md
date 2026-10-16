# CHG-2026-0029：Everypaw 实时视觉工具

状态：approved / 06 列窗与09 PNG源码、Host受控模型及Everypaw大图成对验收、09本地治理完成；既有平台与外部门禁保留，后续视觉票依任务图进行。日期：2026-10-16。

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

### 11 实施前登记（2026-10-17）

正式11在既有批准的 ai_pc.rs、ai_pc_visual.rs 与本表文档范围内扩展同一 `ai_pc_action_batch`，不增加工具、依赖、审批或预算。模型步骤增加 `set_text:{target_id,text}`（text显式必填、允许空字符串）、`shortcut:{target_id,key,modifiers?}`（VK名称及明确修饰键白名单，未知值整批拒绝）与 `wait:{delay_ms}`（非负 signed32、允许0）。所有步骤严格解析后才可分发任何效果；wait不接受target，输入动作仍绑定原context/native generation/窗口witness。

沿用10的桌面会话FIFO、原调用证据、独立取消与未知不重放。仅动作批次的工程等待期限按已校验wait总毫秒加既有native/桌面10秒、Host20秒计算，使用u64安全求和；不把短HTTP收包期限当作wait产品上限，不扩大其他工具期限。已有悬浮问答180秒总体期限仍保留，其取消不证明同步输入已经结束。实际Windows通过自建隔离EDIT验证ValuePattern整值替换、只读/不支持、目标焦点与真实快捷键；部分SendInput可能已经生效，保留接受数量并报告unknown，不补发。

### 10 Host 局部验证（2026-10-16）

动作票在 `feature/everypaw-visual-10` 复用 07 的私有 `call(name, ...)` 接缝（本分支提交 `b5428a4`），只修改已登记的七个文件：ai_pc.rs、ai_pc_visual.rs、http.rs、本记录及三份 b-line 文档。`validate` 严格验证 generation、1–64 步 invoke/click 与 target_id，拒绝额外字段；`specs` 声明 Write/NeverParallel，`DesktopTools::call/validate_input` 接通已授权路由。`ai_pc_visual::call` 将实际 error/unknown/cancelled 投影标为工具错误，保留一次请求、精确 ACK、原身份和无重试合同；登记回执只增加已实现动作工具。

新行为测试 `action_batches_use_existing_write_approval_and_return_actual_partial_result_once` 首先真实红（工具尚未声明），实现后 1/1 绿。此后 `cargo test -p openpaw-engine-host --features engine-host -- --test-threads=1` 退出 0（59+16+36=111 项），显式 Host 全目标严格 Clippy 退出 0，fmt 退出 0，`cargo run -p xtask -- lines` 退出 0（Host business6505/7000、Host外部tests3943/4000、aux11497/11500）。未修改预算、计数或依赖；新增测试位于原批准模块的自然内联边界。切换共享 target 后曾出现 0 tests 的旧分支缓存，刷新本分支源码时间并确认真实编译路径后重新完成上述红绿；旧缓存不计为通过。

上述是早期单票局部结果。后续已完成独立原生取消控制确认、真实 pipe 断连取消、丢失回执保留 unknown 与原窗口遮挡检查；这些桌面行为由 Everypaw 应用边界和自建真实 Win32 控件验证，不由 Host HTTP 成功推断原生已停止。

### 10 组合后验收（2026-10-16）

实现提交 `027d3aa`，合引擎主线 `d568346`（含09）得到 `846c2e3`；Everypaw 合主线 `843d326`，组合代码 `53c9a5d`。冲突按已实现能力合并：list/PNG/actions 均保留、登记头三工具、工具总数27、PNG媒体保持、动作 error/unknown/cancelled 仍是工具错误。只修改相对主线已登记七个文件，没有依赖、预算、Core／AgentLoop／审批算法或生产配置变化。Engine 实现及合并提交带 Signed-off-by，未解决既有来源 DCO／CODEOWNER／CI 门禁。

| 命令／验收 | 当前实际结果 |
| --- | --- |
| Host `cargo test -p openpaw-engine-host --features engine-host -- --test-threads=1` | 组合后退出0，59+16+36=111；动作原始调用与部分结果、09六场景Router、取消／代次和精确ACK均通过。 |
| `cargo test --workspace` | 合09前退出0；最终相关变化由组合后Host111、两种workspace strict Clippy和parity复核，不声称最终全workspace重跑。 |
| `cargo clippy --workspace --all-targets -- -D warnings` 与显式 `--features engine-host` | 组合后均退出0，数组splat保留`--`，无警告放行。 |
| `cargo test -p openpaw-cli --features engine-host --test session_host_parity` | 组合后3/3绿。 |
| fmt、lines、scrub、golden、telemetry | 组合后均退出0；实际Host6573/7000、外部tests3943/4000、aux11497/11500；scrub356，telemetry284 dependencies／229 symbols。 |
| 默认 `cargo tree -p openpaw-cli --no-default-features` | 退出0，axum匹配0。 |
| `cargo run -p xtask -- snapshot --check` | 退出1，既有Windows CRLF/LF快照偏差；未修改受保护快照／检查器。 |
| `cargo deny --version` | 退出101，未安装cargo-deny；不安装新工具，依赖审计仍需CI。 |
| `cargo build -p openpaw-cli --features engine-host` | 组合后退出0；仅保留单文件 `openpaw-actions10.exe`，SHA256 `1392AAD06DA65707DCD38F5144CD5ED206C30412D6F4F01570D7EA700F67B437`，未部署。 |
| Everypaw组合后 `paired_ --ignored --test-threads=1` | 3/3绿（10.94秒）：真实官方Host隔离进程、受控SSE模型、HTTP桥、验证pipe原生替身及隔离Recorder。保留原列窗、09同轮四图窗口／子区PNG；独立动作场景list→invoke有效/click失效/invoke后续→answer，真实ToolResult等于保存投影，completed/error/not_started、执行1次、无强制观察／重放、无token。 |
| Everypaw Desktop与真实控件 | 组合后主测试87通过/8ignored、通知2通过/1ignored；真实隔离Win32 Invoke/click1/1（0.31秒），拒跨context及遮挡窗口，失败后不点击后续目标。 |

共享xtask的 `env!(CARGO_MANIFEST_DIR)` 会保留编译来源路径；首次lines误返回旧09的6529。刷新本分支xtask源码时间、确认真实编译路径后，五项xtask均重新执行，上表6573为正确组合值；未改算法或预算。一次PowerShell嵌套数组错误未实际执行检查，纠正数组后才取得上表结果，不将参数错误当业务失败。

桌面实际执行位仅在原生返回／进程确认结束后释放；停止、排队取消和10秒超时四场景以及控制断连／回执丢失已从真实HTTP/IPC与保存证据核对。完整红绿及Recorder分阶段验证见Everypaw `ticket-10-verification.md`。三共享target的构建／执行已全部结束并明确交07继续结构组合验证；本票只剩本地文档提交。实际Docker、管理员／多屏DPI、DeepSeek真实调用／模态、CI和部署未验证，不把回环／替身作为通过。生产DeepSeek、服务、数据卷和配置均未改变，不push／部署。


### 09 实施前的具体范围补充

既有规格 D15/D16 与正式09要求 PNG 经实际主模型媒体路径交付。沿用本 CHG 已登记授权：在 ai_pc.rs/ai_pc_visual.rs 实施严格 `{target_id}` 的 `ai_pc_capture_target`，受信调用 `/ai-pc/capture`，只接受单张合法原始 PNG，runtime 的实际 image=false/unknown 在原生分发前失败。媒体置于 ToolCallOutcome.media，不复制到文字投影；最大响应 7 MiB 仅用于一张合法5 MiB原件的Base64及有界元数据，沿用原件限制，不缩放。

实测现有 Everypaw SSE 每行1 MiB、REST全历史2 MiB会被合法PNG附件撑破。允许 http.rs 在既有 messages 路由增加可选 `turnID` 与 `textOnly=true` 窄读取；默认路由内容和其他消费者保持原语义。窄读取只返回指定回合的 assistant text，既有 runtime 快照不改变持久化或公共投影。Everypaw SSE仅消费文字且有界丢弃超大非正文event，最终文字以该窄REST恢复。实际>1 MiB完整PNG经真实受控provider、SSE与REST验收，不以提高客户端总容量代替。

此扩展沿用既有 GET messages 路径和稳定编号 `API-HOST-SESSION-004`，是同一路径的可选窄读取合同。新增私有 `MessageQuery`（turn_id/text_only）和扩充 `messages` 过滤逻辑：需有效ASCII turnID，保留指定回合assistant的info/error与text，缺省不变。没有新增Core/runtime接口或事件。Host当前仍先复制wire_messages历史快照再过滤，不能宣称历史内存或CPU开销恒定/全程流式。窄REST传输≤2MiB、SSE保留行≤1MiB/累计wire≤128MiB/180秒；同轮四张原件走四次工具结果，受控paired验证其实际主模型请求和最终文字恢复。超过任一工程边界明确失败，不删媒体以伪装成功。

09新增行为测试优先扩充已有 Router 用例及 Everypaw 成对验收。当前 Host tests 3943/4000 仅余57行，若实测超过先记录必要用例和最小预算文件范围，不以搬迁测试避计数。

### 09 实际实现与本地验收（2026-10-16）

隔离分支 `feature/everypaw-visual-09` 从集成 `4acf3c8` 实施。红测试提交266e078，借用07中性名称分发接缝54a676b（源4fe7b50），PNG实现8cf2283；Engine提交均带DCO。仅修改本表已批准文件；没有Core/AgentLoop/provider、Cargo、xtask或生产配置变化。新增私有字段不作为公共API；变动的pub(crate)工具声明说明与pub(super)调用函数保留中文八栏目。

`ai_pc_capture_target`为ReadOnly/ParallelSafe，严格单个target_id；`ai_pc_visual::call`在取得前检查runtime实际image，capture最多7MiB并要求恰一张PNG，InlineMedia原件验证在精确ACK前完成，媒体仅走现有ToolCallOutcome.media。MessageQuery对turn_id显式命名turnID；该命名错误由真实Router窄REST断言先红后修绿，不靠客户端猜字段。

| 已执行命令/场景 | 实际结果 |
| --- | --- |
| `registered_visual_tool_reaches_model_with_trusted_turn_and_exact_parsed_ack` | 最终退出0；6场景list、PNG支持、image未知、仅text、错误MIME、非法原件；不支持时native0、非法媒体不ACK，实际第二provider请求包含相同PNG。窄REST正确过滤text/保留原info，默认REST仍含原件；缺参数/单turnID为400。 |
| `cargo test -p openpaw-engine-host --features engine-host` | 最终退出0，58+16+36=110；包含窄REST参数和turnID修复后的全部测试。 |
| `cargo test --workspace` | 退出0；跨工作树切换先刷新09 Host/CLI/xtask源mtime并确认实际编译路径，使用现有共享target、jobs=1、debug=0及隔离临时目录执行提权。 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 最终退出0。 |
| `cargo clippy --workspace --all-targets --features engine-host -- -D warnings` | 最终退出0；PowerShell吞`--`的首次参数错误保留，后用cmd保持分隔符实际严格执行。 |
| `cargo test -p openpaw-cli --features engine-host --test session_host_parity` | 退出0，3项通过。 |
| `cargo fmt --all -- --check` 与 `git diff --check` | 最终退出0。 |
| `cargo run -p xtask -- lines` | 最终退出0；Host6529/7000、tests3943/4000、aux11497/11500，无预算或计数变更。 |
| `cargo run -p xtask -- scrub --check` | 退出0，扫描356文件。 |
| `cargo run -p xtask -- snapshot --check` | 退出1；与06/07及未实施基线相同的Windows CRLF/LF快照偏差，未修改Core、快照或检查器掩盖。 |
| `cargo run -p xtask -- golden` | 退出0，全部golden通过。 |
| `cargo run -p xtask -- telemetry` | 退出0，284 dependencies、229 symbols clean。 |
| `cargo tree -p openpaw-cli --no-default-features` | 退出0，532行依赖树、axum匹配数0。 |
| `cargo deny --version` | 退出1，未安装cargo-deny；不安装新工具，不冒称依赖审计通过，仍需CI门禁。 |
| `cargo build -p openpaw-cli --features engine-host` | 退出0；独立09官方制品openpaw-png09.exe，SHA256 AA73B0709453596B631921E1C87C28E67F36EF001CA49D5F7BE0885B29C86467，未部署。 |
| Everypaw同轮四张窗口＋子区PNG paired | 退出0，1/1（10.98秒）。真实官方Host进程/受控SSE provider/桌面HTTP/验证pipe/真实隔离Recorder。1100×1100原件4,841,533B×4=19,366,132B≈18.47MiB，同回合四个不同附件；子区768×768原件2,360,312B。完整解码核对RGBA，逐字节核对保存和实际provider data URL，最终SSE/窄REST回答成功。原列窗paired另1/1绿。 |
| Everypaw真实WGC像素 | 自建320×240黑色窗口、80×90白EDIT、遮挡/自有picker marker、负坐标、最小化明确失败；最终1/1绿，07共享window缓存修正后再1/1绿。仅捕获自己的隔离测试窗口。 |
| Everypaw大SSE文字/错误恢复 | 退出0；1.1MB文本行丢弃后窄REST原文恢复，2.1MB正文超过2MiB明确失败，超大error事件后权威failed明确失败。 |

四图fixture首次1160² Fast噪声RGBA超过既有5MiB，在服务前失败；改1100²进入合法范围后绿，没有放宽产品上限。原生fixture曾因STATIC系统颜色、NOACTIVATE选窗规则及UIA数值类型50004误判失败，修正独立fixture后真实像素绿。完整失败及命令记录在Everypaw ticket-09-verification.md。

09最终本地治理命令已按共享缓存序列完成，结果如上；Engine集成仍为基线4acf3c8，无新产品tip待合，Everypaw交付前已合最新纯文档13e059f。Desktop/Recorder和Engine共享构建/执行位均已释放给10。Docker、真实混合DPI/多屏/管理员、HDR及DeepSeek模态接受/理解未验证，不把受控模型或回环宣称这些通过；未改变DeepSeek image配置。CODEOWNER、CI、来源DCO缺口、部署与既有平台门禁独立保留。07后续结构工具合并需保留本票六场景Router与大图paired，并重新核对合并制品，不拿09独立SHA256代替。
### 07 实施前登记（2026-10-16）

基于 `4acf3c8`，继续已批准路径内的实时结构工具：`ai_pc_query_structure`。模型仅传 native generation、target_id、逗号分隔 directions、offset、limit、boxes；可信会话/Host turn/执行 generation/invocation 继续由运行时安装。方向集合对齐固定 Everywhere 的 all/parent/child(children)/previous(prev)/next/sibling(siblings)/none，默认 all/0/128/false、limit 最大 1024，非法输入拒绝。工具为 ReadOnly/ParallelSafe；复用现有一次调用、取消及精确 ACK 合同，只按工具选择 `/ai-pc/windows` 或 `/ai-pc/structure`，不提前声明其他未完成工具。模型投影 Balanced4096 估算 tokens 和实际取得/信息权限由桌面负责。Host 新测试写在既有批准模块的自然 inline test seam，不移动已有测试、改变计数或预算；06 测得 Host business6454/7000、外部tests3943/4000、aux11497/11500，若真实增量超限先登记最小治理变更再继续。

### 10 预实施登记

2026-10-16：正式任务 10 沿已批准 D16–D19 和既有允许文件表实施 `ai_pc_action_batch`。模型参数严格为 `{generation, steps:[{action:"invoke"|"click",target_id}]}`，最多 64 步；会话、原回合和 invocation 继续由可信 Host 封装。声明采用既有 Write/NeverParallel 工具分类与权限机制，不增加独立开关或逐步骤审批。此阶段不声明 set-text、shortcut、wait。

复用 `ai_pc_visual::call` 的唯一调用、取消、精确 ACK 与无重试路径，按工具选择 `/ai-pc/actions`；仅返回实际步骤状态，不强制观察或重放。桌面主路径将 Stop、Host 取消和等待超时传入独立 IPC Cancel，能力端在每步开始前检查取消，整批串行并保留实际完成槽位。实现仅改允许表中的 ai_pc.rs、ai_pc_visual.rs、http.rs 和对应文档；不改 Core/Engine/Store、依赖、审批算法或预算。真实隔离 Windows 控件与受控 Host/HTTP/IPC 验收分别记录。


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
