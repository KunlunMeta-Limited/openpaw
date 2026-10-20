# OpenPaw 接入 Office：T0—T8 工程实施计划

状态：T0 实施中，等待独立 PR 审核；T1—T8 尚未开始。2026-10-20。

本文件替换旧 master-plan 的三人分工、旧 actions gateway、侧栏、行数门槛和过期排期。用户提供的 master-plan 是参考材料，当前用户要求与冻结的真实协议优先。一个开发者按 T0→T8 顺序实施，每个 Tx 提 PR，获批并合入后才开始下一个 Tx；CI 通过不是审核批准。

## 1. 基线、架构与复用规则

| 对象 | 固定基线 | 说明 |
|---|---|---|
| OpenPaw | 60a531a51172d14fc3db1a8e7eb6762c2c8369d8 | Host 仅有占位说明，不能当作已实现 |
| Office | b221a44ce22fe08956356b5002d17df59a6a4f50 | integration/README.md、四份链接文档、Schema、客户端与测试入口 |
| OpenCode SDK | @opencode-ai/sdk 1.18.15，测试 lockfile 固定 integrity | 按实际 v1 类型和 SDK 调用，不照搬旧 master-plan |
| OpenPaw 集成分支 | merge/ai-ofice-react-overagent | 已存在，保留 ofice 拼写；T0 开始时与 OpenPaw 基线一致 |

Office 工作区两份既有架构文档修改不属于本计划，不覆盖、不顺手提交。OpenWork 本期边界经用户确认：Host + 固定版 SDK 真实请求验收；指定 OpenWork 桌面源码取得后的验收另列，不阻塞 Office 交付。

    Office 浏览器 ──聊天 HTTP/NDJSON──→ engine-host Office adapter
        ↑ Office 原有 SSE                       ↓
    Office Fastify ←──官方 messages API── 共享宿主服务
                                              ↑
    SDK / 未来 OpenWork ──HTTP/SSE──→ OpenCode v1 adapter
                                              ↓
                     现有 engine / llm / tools / store / memory / goal-ledger

- 所有客户端路由、DTO、错误和协议投影在 openpaw-engine-host；通用缺陷修回所属 crate。
- 不增加 Node Bridge，不复制 AgentLoop、Session、工具注册表或审批状态。
- Host 内部直接调用共享 Rust 服务，不建立内部 HTTP 查询层。
- Office 的 Fastify、SSE、场景队列、detail-v1 均复用；旧 /actions gateway 和新侧栏不采用。
- MCP sidecar 保持 MCP 用途，不作为运行后端；engine-host 默认 feature 关闭。
- 无文件或 crate 行数上限。xtask lines 保留 AST 统计和读/解析错误，不再因数值超限失败。
- 新代码必须在 PR 中说明现有能力不足之处；禁止为规避统计或协议隔离而搬运核心代码。
- [audit.md](audit.md) 记录复用矩阵、D01—D11、能力缺口及审核证据。
- [contracts/README.md](contracts/README.md) 冻结协议与 SDK 差异；[contracts/sources.json](contracts/sources.json) 记录原文哈希。
- [r1-chat.md](r1-chat.md) 是 Office 修改提案；设计待用户单独批准，不能据此改 Office。
- [validation.md](validation.md) 定义双平台命令和实际证据；[handoff.md](handoff.md) 是每个 Tx 的交接模板。

## 2. 共享运行与兼容约束

1. 一个 Session 最多一个 active Run；每个 Run 独立 AgentLoop、token、预算和工具上下文。不同 Session 并行，全局默认上限 4，可配置；超限明确返回容量错误，不建立隐式无限队列。
2. Office 默认四个固定办公室，可配置；(teamId, conversationId) 绑定 Session；同办公室最多一个 active Run。新对话复用办公室。
3. 工作区按规范化真实根识别；读取共享锁，写文件、Todo 修改和 shell 独占锁。审批等待不持锁；获批后取锁并重验最终输入和文件前置条件。等待可取消，实际操作/清理结束才释放锁。
4. 默认关闭 Run 内工具并行；修复现有并行路径的通用缺陷并测试，不为首版另写并行调度器。多 Session 并行必须真实可用。
5. JSONL 是消息事实来源，SQLite 索引可重建；Host 状态库只管理绑定、Run、Approval、快照版本和恢复元数据。不复制两份权威消息历史。
6. token 可以合并保存；接受运行、审批变更、工具副作用边界及终态必须可等待地持久化。同步回调不得阻塞 HTTP/磁盘，关键事件不能因队列满丢失。
7. Host 重启将未结束 Run 标为 interrupted，旧审批失效，绝不自动重放副作用工具。
8. Office v1 断开取消；R1 detach 不取消；OpenCode SSE 断开只解除订阅。cancelling 持续到实际回收后才能进入 cancelled。
9. 复用 OS 锁保护 home 单写进程和工作区所有权；协作锁不能约束外部编辑器。文件写入另校验前置状态，受限执行另校验实际隔离。
10. 按根复用 Store，显式注入固定 home，不在请求中切换环境绕过全局数据库 OnceLock。

~~~mermaid
stateDiagram-v2
    [*] --> accepted: 持久化接受
    accepted --> running: 取得容量
    running --> awaiting_approval: 最终输入已分类
    awaiting_approval --> waiting_resource: CAS批准
    waiting_resource --> running: 获锁并重验
    awaiting_approval --> running: 拒绝作为工具结果
    running --> completed: 持久化完成
    running --> failed: 错误或预算耗尽
    accepted --> cancelling: 明确取消
    running --> cancelling: 明确取消
    awaiting_approval --> cancelling: 先关闭审批门
    waiting_resource --> cancelling: 取消等待
    cancelling --> cancelled: 实际清理后持久化
    accepted --> interrupted: 崩溃恢复
    running --> interrupted: 崩溃恢复
    awaiting_approval --> interrupted: 崩溃恢复
    waiting_resource --> interrupted: 崩溃恢复
    cancelling --> interrupted: 崩溃恢复
~~~

Tool 超时可以形成失败工具结果；Run 总预算耗尽是 Run 失败且带 timeout 原因，不能混同。

~~~mermaid
stateDiagram-v2
    [*] --> pending: 持久化审批与输入摘要
    pending --> approved: CAS首次允许
    pending --> rejected: CAS首次拒绝
    pending --> expired: 超时
    pending --> cancelled: Run取消
    pending --> invalidated: Host恢复
    approved --> consumed: 同call和最终输入执行一次
~~~

展示失败不批准；展示尝试 ID 与业务审批 ID 分离。批准后 Run 已取消或输入变化仍不得执行。

资源与恢复的共享服务边界（两协议适配器调用同一实例）：

~~~mermaid
stateDiagram-v2
    [*] --> no_lock: 工具最终输入已分类
    no_lock --> awaiting_approval: 需人工批准
    awaiting_approval --> waiting_lock: CAS允许
    no_lock --> waiting_lock: 无需审批
    waiting_lock --> acquired: 读取共享或副作用独占
    waiting_lock --> cancelled: 可取消等待
    acquired --> executing: 重验路径/输入/前置条件
    acquired --> released: 重验失败
    executing --> cleaning: 完成或取消
    cleaning --> released: 提交/进程回收已结束
    released --> [*]
~~~

~~~mermaid
sequenceDiagram
    participant Host
    participant OS as OS所有权锁
    participant JSONL
    participant State as Host元数据与索引
    Host->>OS: 获取home单写/工作区所有权
    OS-->>Host: 失败则拒绝启动
    Host->>JSONL: 校验/按T1规则恢复消息事实
    Host->>State: 重建可派生索引与绑定
    Host->>State: 未终结Run→interrupted，pending审批→invalidated
    Host->>State: 恢复已持久化seq和原快照内容
    Note over Host,State: 不重放工具；完成恢复后才宣告ready
~~~

## 3. Tx 顺序实施卡

### T0：基线、契约、复用台账与门禁

**目标：**冻结源码事实和协议，不提前实现后续功能。D12—D15 是新发现的构建/测试/lint 基线阻塞，在 T0 各独立最小修复，不引入业务功能。

**实施方法：**
- 更新本总计划，建立 audit、contracts、R1、validation、handoff。
- Office 固定提交原始 Schema 经哈希校验后保存为只读测试快照，不复制业务实现，不使用浮动 main。
- SDK 固定精确版本、lockfile 与完整类型 fixtures，验证 SDK 自身构造的路由及事件形状。
- 扩展现有 CI 的 feature、Linux/Windows、MSRV、sidecar 共存和契约检查；CLI 仅增加 opt-in 依赖连线，不暴露未实现命令。
- lines 改统计，更新 PR 模板；准备 Rust stable、MSRV 1.88、Node 24。

**产物：**总计划与九卡；复用矩阵；D/R 台账；两协议请求/响应/流/错误 fixtures；运行/审批/锁/恢复状态；CI 和验证报告；待审 R1。

**完成标准：**首期能力有阶段；无假支持；真实 Schema/SDK 类型检查执行；默认构建/已有测试有证据，失败有编号归属；本地与远端结果如实记录；PR 获批合入。空 Host feature 构建不算 Host 功能测试。

**交给 T1：**固定基线、D01—D04 复现点、契约及兼容测试。

### T1：引擎生命周期与会话持久化缺陷

**依赖与范围：**T0 获批合入。仅 D01—D04，不混协议功能。

**实施方法：**
- 复用 AgentLoop/Provider/SessionReader/Writer，抽取 CLI 与 Host 共用的无协议装配。
- 选择显式模型或 Provider 默认模型，禁止真实请求携带意外占位模型。
- 增加兼容旧入口的 RunContext/RunOutcome，保留外部 token 关系；覆盖建连、流、退避、压缩和后续等待取消。
- 区分流空闲超时、Run 总预算、正常/失败/取消；已输出后不从头重试拼重复文本。
- 保存 user、assistant/tool_use、tool_result、usage、终态和 call ID；不按 Role 或压缩前数组长度推断新增历史。
- 写入错误传播，不上报假成功、不重放副作用；尾行先备份修复再追加，中间损坏报错。

**产物：**四个缺陷各自逻辑提交/回归；兼容运行接口；模型装配；完整工具会话、压缩恢复和损坏样例；原因和关键不变量注释。

**完成标准：**各等待阶段取消不误报正常完成；真实模型正确；工具配对不丢不重；压缩/磁盘错误/损坏有确定结果；原 CLI/API 兼容；Linux/Windows 通过；PR 获批合入。

**交给 T2：**可取消上下文、可信结果、完整会话保存能力。

### T2：工具、授权与跨平台执行缺陷

**依赖与范围：**T1 获批合入。D05—D11；复用工具、Environment、sandbox、Job Object、OS 锁。

**实施方法：**
- Hook 得到最终输入后统一校验→分类→审批→资源→执行；绑定 Run/call/工具/输入摘要，改输入重新审批。
- 调用级授权打通 bash 内外门；危险命令仍拒绝，不永久放开工具。
- 落实 concurrency metadata，完成即发结果，任务内计时，panic 保留 call ID，取消后 join 清理。
- ToolRegistry 可注入 Environment；Linux 进程组与 bwrap 复用；Windows 挂起创建→Job→恢复，普通和受限路径都管理进程树。
- 双输出管道并发排空，展示截断后继续读。句柄取得即 RAII，加入 Job 前失败也回收挂起进程。
- 不用 spawn_blocking 冒充可取消；限制无法落实拒绝，不降级环境提示；显式 cmd/sh 与实际参数，未知语法保守审批。
- Todo 原模块类型化读取、锁、原子更新，区分空/损坏/权限错误。

**产物：**七个缺陷提交/测试；授权和执行接口；平台能力矩阵；大输出/子树/异常/审批/Todo 证据；路径和 shell 文档。

**完成标准：**审批前零副作用；拒绝/超时/取消不执行；清理后放锁；大输出不阻塞泄漏；Windows 普通用户本地执行可用，受限不可用明确拒绝；空格/中文/盘符/UNC/大小写/链接/占用有测试；Todo 不丢更新；双平台通过；PR 获批合入。

**交给 T3：**工具上下文、可注入环境、跨平台回收保证。

### T3：共享 Host 服务

实施资源协调前必须调查 audit.md 的 D16 间歇文件锁失败，补跨进程回归并记录结果。

**依赖：**T2 获批合入。新增宿主责任集中 Host，不复制核心。

**实施方法：**
- Session/Run/Approval/Resource/Query/Config 服务；每 Run 独立 AgentLoop，同 Session 冲突，全局默认 4。
- 复用账本/索引，Host 元数据版本管理；关键边界可等待持久化，文本合并，不阻塞 sink。
- 审批 CAS，等待不占锁，获批重验；cancelling 直到实际回收。
- 恢复 interrupted，旧审批失效、不重放；OS 锁保护 home 和工作区。
- 按根复用 Store；Skill 使用真正只读 API，不触发生命周期变更。
- CLI、loopback、port=0、就绪标记、配置校验、优雅关闭；密钥不进日志/事件/工具环境。

**产物：**共享内部接口；CLI；状态机/锁/恢复测试；索引重建与数据版本；确定性 Provider/工具场景。

**完成标准：**Session 预算/取消隔离；多视图单套执行/审批；控制请求不被长运行堵住；恢复不重放不批准；Host/sidecar 共存，默认无 axum；双平台启停/锁/路径/恢复通过；PR 获批合入。

**交给 T4：**可直接调用的运行、审批、历史、查询服务。

### T4：Office v1 聊天与完整快照

**依赖：**T3 获批合入。不修改 Office 源码。

**实施方法：**
- POST /office/chat 输出 v1 NDJSON；办公室/会话绑定 Session，Host 账本为历史，浏览器截断不覆盖。
- 无 turn ID 不宣称严格幂等/续接；active 冲突，断开取消；UTF-16 字符和 UTF-8 字节分别校验，空输出/失败/超限不发假 done。
- 官方 messages 调 capabilities/initialize/snapshot/read，分开业务 ID 与演员 ID。
- 持久化每办公室 seq+完整规范化内容，同版重试不改时间；applied/cached/ignored 如实处理。
- Office 重启重新初始化，HTTP 失败可能已缓存；校验无 null、64KiB、数组/字段/时间。
- 不拆完整快照成覆盖性的局部快照；有界展示标注范围，完整数据走详情。仅开放已验证读取工具。

**产物：**HTTP 客户端与聊天 adapter；绑定/投影；快照持久化重试；密钥/部署配置；真实服务和浏览器脚本。

**完成标准：**真实对话入账、多办公室不串线；重试/冲突/旧版/离线/重启正确；停止最终回收；双平台原生 Node+浏览器可用，不依赖 Docker；PR 获批合入。

**交给 T5：**Office 对话、绑定与可信快照。

### T5：审批、详情、场景反馈

**依赖：**T4 获批合入。复用 detail-v1，不新增侧栏。

**实施方法：**
- 单个 ui.next 按 teamId 路由，不为每办公室竞争领取。
- decision.request 展示共享审批；业务审批与展示尝试分 ID；首次有效选择获胜，超时/断线不批准；取消先关业务门再终止 HTTP。
- action 收到即记录，但无 ACK/租约/重投，不能承诺领取响应丢失可恢复。
- 历史/工具/技能/目标/Todo/文件详情每页默认 5 项，字段/按钮有界，动作校验作用域；技能目标只读。
- 文件只读授权根内普通文本，最大 256KiB，校验链接/重解析点；场景用能力发现和 v2 命令，不用旧网关。
- 稳定 task.assigned 避免额外重复 visit；提议/待审批/实际执行分开，ToolUseStart 不等于执行。
- 未实现业务操作明确说明，不伪改快照；完成后才开放写工具。

**产物：**审批/action 路由；六类详情/分页/权限；任务/白板/演员映射；去重测试；写工具开放清单。

**完成标准：**允许/拒绝/重复/过期/取消/重开只执行一次；审批不占锁；详情真实只读；动画错误不改 Run，重连不重播；投递边界公开；双平台浏览器通过；PR 获批合入。

**交给 T6：**常规 Office 闭环和长任务限制。

### T6：R1 长任务与可靠停止（跨仓库）

**依赖：**T5 获批合入，用户单独批准 [R1 设计](r1-chat.md)。未经批准不改 Office、不标完成。

**实施方法：**
- opt-in 扩展，v1 默认不变；先持久化 turnId，再创建 Run，再订阅。
- (teamId, conversationId, turnId) 幂等，同键异内容 409；POST /office/chat/runs、GET /office/chat/runs/:runId、GET .../events?after=、POST .../cancel。
- 全部接口校验访问范围；一致快照+游标，缓冲失效重取快照，文本不重。
- 空闲检测替代 180 秒总时限，Run 总预算有效；刷新/卸载/断网 detach，停止 cancel。
- cancelling 等实际清理；IndexedDB 保存标识/游标，旧档迁移、错误保留原文；区分四类终态。

**产物：**Host 四接口；Office 聊天源/状态/存档；兼容/迁移/回退；慢运行/刷新/断线/重复/停止测试；两仓互链 PR。

**完成标准：**确定性运行超过 180 秒；刷新同 Run，无重复输入/文本/工具；重复提交单 Run；detach 继续、cancel 回收后结束；v1 测试通过；两仓 PR 全获批合入。

**交给 T7：**共享长运行和明确的客户端生命周期。

### T7：OpenCode v1 / OpenWork 预兼容

**依赖：**T6 完成。以真实 SDK 为准，不使用旧文档混版字段。

**实施方法：**
- Host 实现 session create/list/get/messages/todo/status/prompt_async/abort、provider、v1 审批回复、/event、/global/event。
- 接受持久化后 prompt_async 返回 204，后台共享 Run；断 SSE 不取消，新订阅同步待审批，查询恢复状态/消息。
- 稳定 ID，真实执行才 running；完整类型和 wrapper 按契约。
- Basic 两变量均无仅本地无认证，只配一个启动失败，两者都有保护全部路由。
- loopback port=0 和精确就绪标记；目录 header>query>默认，仅授权根，Session 不能被后续请求切换工作区。
- connected 仅可用 Provider；always 按 once 且说明限制；不支持写操作 501，不假 204；非文本 part 400，不静默丢弃。
- 必要只读空对象/列表降级；不做 managed spawn/v2/插件/MCP 管理/自动调度。

**产物：**adapter；修正契约；固定 SDK 客户端；完整 fixtures；真实 HTTP/SSE 测试；未来 attached 配置和桌面追加验收。

**完成标准：**SDK create→prompt→流→工具→审批→abort→恢复；实际事件迭代器通过；无强断言绕过；认证/越界/冲突/非法/不支持错误通过；断线不新建 Run；双客户端锁/审批不串线；双平台通过；PR 获批合入。只声明 SDK 预兼容，不声明真实 OpenWork 桌面通过。

**交给 T8：**两适配器与兼容证据。

### T8：双平台联合验收与交付

**依赖：**T7 获批合入。

**实施方法：**
- 双平台真实 Office、Host、固定 SDK 同旅程；并行读写/审批/取消。
- Host 崩溃、Office 重启、刷新/断线/磁盘错误/损坏；同 Run 多视角事实一致。
- Linux/PowerShell 启动脚本共用配置；本机/同源代理示例，密钥不进 VITE_*。
- 回归 CLI/sidecar/default/v1；新问题编号，不加新功能；至少一轮真实 Provider，自动化使用确定性替身。

**产物：**双平台报告与重跑入口；启动/关闭/备份/恢复/回退；最终能力与 D/R 台账；构建/版本/校验值；集成→main 最终 PR。

**完成标准：**产物/PR 可追溯；无 skip 代替必测；Windows 原生，不用交叉编译替代；关键缺陷关闭、限制如实记录；用户批准最终 PR 才合 main。

## 4. PR、审核与单人交接

OpenPaw 每个 Tx 从最新已合入集成分支建分支：docs/engine-host-t0、fix/engine-host-t1、fix/engine-host-t2、feat/engine-host-t3 至 t7、test/engine-host-t8。不从未批准分支堆叠。

1. 先确认上一 Tx 当前提交获批且合入，才开始下一 Tx。
2. 开发可 Draft；Dxx 各自逻辑提交/回归/关键注释；遵守 Signed-off-by。
3. Ready 前填交接包和实际双平台证据；未执行不算成功。
4. 用户/指定审核人明确批准；作者不自批、不自动合入、不强推。
5. 实质变更后重测并审核当前提交；Squash 保留 Tx/Dxx/PR 关联。
6. 等待只修当前 PR。T6 两仓 PR 全部批准合入才完成；Office PR 指向 Office 评审基线。
7. 最终 main PR 单独批准；实施请求不代表各 Tx PR 已获批。

PR 必填目标/前后行为、产物、复用/延展/修改/新增和必要性、D/R、接口/迁移、双平台命令/退出码、未测项、证据、下一阶段输入、审核提交号。使用 [handoff](handoff.md)。

## 5. 范围与完成口径

- 单机单用户、授权工作区，不扩展多租户。
- Windows 10/11 x64 原生，不要求 WSL；不含 ARM/安装器/签名。
- Skill/Goal/Todo 复用真实查询，工具按验证结果开放；不新增调度、技能管理、多 Agent 编排。
- R1 未批准只可交付受 v1 限制能力，不能声称续接完成。
- R2 投递增强本期不改；R3 新侧栏/富文本不做。
- 新缺陷登记归属，阻塞项不解决不能进入依赖阶段。
- T0 环境/contract/CI 不代表 Host 或 Windows 功能已实现，完成看对应阶段证据。
