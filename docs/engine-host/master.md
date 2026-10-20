# OpenPaw 接入 Office：协议约束、交互设计与 T0—T8 实施计划

状态：T0 实施中，尚未获得非作者批准并合入；T1—T8 未开始。修订日期：2026-10-20。

本文件同时服务人工评审和 coding agent：先明确真实协议、用户行为与系统边界，再定义实现方法、产物、测试和 PR 交接。下文均是待实施要求；不能将设计、fixture 通过或 feature 连线描述为功能已交付。用户提供的 master-plan 仅作为参考材料。

## 0. 授权、平台与阶段规则

1. 本期一名开发者，Linux 为主要开发环境，Windows 10/11 x64 原生适配；不安排 macOS 适配与验收，不要求 WSL。ARM、安装器和签名不在本期。
2. 初期按 T0→T8 顺序实施，一 Tx 一主 CHG；每个 Tx 必须有对应 PR，当前提交经人工审核批准并合入后，才能开始下一 Tx。CI 通过不等于人工批准。T6 如果实施 Office 扩展，两个仓库的关联 PR 都通过并合入才完成该扩展。
3. 上述阶段串行门禁仅用于初期建设。T0—T8 的约定范围及最终验收完成后，转为 CHG 持续开发，不再要求建立新 Tx 或等待“上一 Tx”；需求/设计/测试/文档同步、正常 PR 审核、Office Rx 审批继续有效。
4. **Office 工程中的任何修改，都必须先提交 Rx，说明原因、方案、影响和准确修改范围，经用户明确人工批准后才能实施。** 缺陷修复、必要扩展、功能新增均可提案，但均不自动获得修改授权。
5. Office 源码、文档、测试、配置、依赖、锁文件、脚本、格式化、生成文件及重命名均受上述限制。不得以“只改测试”“顺手修复”“为联调方便”为由绕过。Office 既有用户改动不得覆盖或顺手提交。
6. 批准本总计划、批准 OpenPaw PR、允许提交 Rx、发现缺陷，都不等于批准某项 Office 修改。Rx 必须记录获批版本、范围和证据；扩大范围或实质变更后重新提交审核。
7. 未批准的 Rx 不执行、不标为已实现。优先在 OpenPaw Host 适配现有 Office；确实需要 Office 修改时，先完成可评审提案，并如实说明无修改方案的能力边界。
8. 本轮按用户明确指令补充 T0 工程治理：AGENTS、需求/设计、CHG、增量注释检查、验证执行器和 Linux/Windows CI。具体实现与限制见 governance/README.md；PR 尚待当前提交验证与人工审核，不据文档宣称阶段完成。测试产物写入 OpenPaw 的隔离测试区或临时目录，不改变 Office 工作区。

## 1. 基线与资料：以什么协议接入

### 1.1 固定基线与事实来源

| 对象 | 固定基线 | 用途与状态 |
|---|---|---|
| OpenPaw 初始审核 | 60a531a51172d14fc3db1a8e7eb6762c2c8369d8 | 引擎、Provider、工具、存储可复用；Host 当时只有占位说明 |
| OpenPaw T0 已提交工作 | PR #4 当前 head | 已有构建/测试基线修正；未合入，不能开始 T1；当前工作区文档不等于已提交版本 |
| Office | b221a44ce22fe08956356b5002d17df59a6a4f50 | 文档、公共 Schema、示例和实现的冻结来源；不得浮动拉 main 自动适配 |
| OpenCode SDK | @opencode-ai/sdk 1.18.15 | 精确版本、integrity、lockfile 和真实 v1 类型 |
| 集成分支 | merge/ai-ofice-react-overagent | OpenPaw Tx PR 的目标分支，保留现有 ofice 拼写 |

官方阅读入口：[integration/README.md](https://github.com/KunlunMeta-Limited/ai-office-react/blob/main/integration/README.md)。main 链接用于发现资料，实施依据固定提交；升级先审核差异，再修改来源锁、需求、fixtures 和实现。

| 来源编号 | 固定版本资料                                                                                                                                                                              | 本项目必须落实的内容                                 |
| ---- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------ |
| S0   | [README](https://github.com/KunlunMeta-Limited/ai-office-react/blob/b221a44ce22fe08956356b5002d17df59a6a4f50/integration/README.md)                                                 | 外部业务后端身份、服务模式、完整阅读链                        |
| S1   | [office-integration.md](https://github.com/KunlunMeta-Limited/ai-office-react/blob/b221a44ce22fe08956356b5002d17df59a6a4f50/integration/docs/office-integration.md)                 | 七类业务操作、完整流程、快照版本、审批、UI、错误与恢复               |
| S2   | [dashboard-integration.md](https://github.com/KunlunMeta-Limited/ai-office-react/blob/b221a44ce22fe08956356b5002d17df59a6a4f50/integration/docs/dashboard-integration.md)           | 成员、任务、事件、人物绑定、进度、白板与展示边界                   |
| S3   | [message-relay.md](https://github.com/KunlunMeta-Limited/ai-office-react/blob/b221a44ce22fe08956356b5002d17df59a6a4f50/integration/docs/message-relay.md)                           | 部署、认证、浏览器 SSE、容量、服务重启和访问控制                 |
| S4   | [chat-integration.md](https://github.com/KunlunMeta-Limited/ai-office-react/blob/b221a44ce22fe08956356b5002d17df59a6a4f50/integration/docs/chat-integration.md)                     | 聊天请求/NDJSON、长度、历史、停止、180 秒限制、IndexedDB     |
| S5   | [office-integration.ts](https://github.com/KunlunMeta-Limited/ai-office-react/blob/b221a44ce22fe08956356b5002d17df59a6a4f50/packages/protocol/src/office-integration.ts) 及关联 Schema | 精确字段、默认值、strictObject、枚举、引用、响应类型           |
| S6   | [plugin-runtime.md](https://github.com/KunlunMeta-Limited/ai-office-react/blob/b221a44ce22fe08956356b5002d17df59a6a4f50/docs/plugin-runtime.md)                                     | 场景 v2、能力、参与者、命令 ID、活动与队列                   |
| S7   | integration/clients/client.mjs、ui-client.mjs                                                                                                                                        | 外部客户端调用方法；示例数据不作为真实业务实现复制                  |
| S8   | tests/README.md、tests/integration-e2e/README.md                                                                                                                                     | 真实 HTTP/浏览器验收；现有 Docker 入口不等于 Windows 原生验收 |

[contracts/sources.json](contracts/sources.json) 已固定 21 份来源的哈希和 SDK integrity；不再手工维护第二张哈希表。S5 关联文件包括 schema.ts、dashboard.ts、office-ui.ts、chat.ts、relay.ts、index.ts、map.ts、spatial.ts。服务端核对点是 server/src/app.ts、officeState.ts、officeRequests.ts、officeUi.ts；聊天核对点是 apps/web/src/chat/ChatSession.ts、sources.ts、persistence.ts；场景投影核对 apps/web/src/dashboard/sceneBridge.ts。

协议字段按冻结 Schema，运行语义同时核对文档、实现和真实测试。若三者不一致，登记证据、最小复现和影响，停止依赖歧义的实现，不凭猜测改 Office。不得把 S7 的固定演示文案当成功能完成。

### 1.2 运行环境与关联文档

Office package.json 的 Node 范围为 ^22.13.0 || >=24.0.0；本项目契约测试固定 Node 24.19.0。Office 的实际依赖解析以其锁文件为准；契约包自己的 TypeScript 5.9.3 不等于 Office 生产 TypeScript。Rust MSRV 为 1.88，stable 和新增依赖均需实际检查。

| 文档 | 唯一职责与本次衔接 |
|---|---|
| 本文件 | 人可读交互、强制边界、Tx 产物、验收与阶段流程 |
| [audit.md](audit.md) | 复用矩阵、D01—D24、能力缺口和审核证据 |
| [contracts/README.md](contracts/README.md) | 已冻结的协议细节、SDK 差异和 fixtures 入口 |
| [r1-chat.md](r1-chat.md) | R1 历史提案，未批准；其中“必做跨仓 T6”解释由本文件第 8、10 节修正 |
| [validation.md](validation.md) | 历史实际测试记录；旧 macOS 覆盖仅属历史，不是当前支持平台承诺 |
| [handoff.md](handoff.md) | 交接模板；T0 后续同步 CHG、Rx 审批和阶段后流程 |

audit/r1-chat/handoff 已同步为条件提案；达到扩展体验需要修改，不是授权或默认必做范围。Office 审批规则适用于所有关联文档和后续 CHG。

## 2. 系统怎样交互，数据归谁

### 2.1 两条 Office 链路与共享 Host

~~~mermaid
flowchart TB
    U[用户] --> B[Office 浏览器]
    B -->|配置的聊天地址：POST JSON| C[Host：Office 聊天适配器]
    C -->|NDJSON 文本与终态| B
    B -->|内部 reply 与 ui| F[Office Fastify]
    F -->|Office 原有 SSE| B
    A[Host：Office 业务适配器] -->|官方 messages API| F
    F -->|同一次 HTTP 请求的响应| A
    C --> H[共享 Session / Run / Approval / 查询 / 资源协调]
    A --> H
    W[固定 SDK / 未来 OpenWork] <-->|HTTP / SSE| O[Host：OpenCode v1 适配器]
    O --> H
    H --> E[现有 AgentLoop / Provider / ToolRegistry / Store]
~~~

- OpenPaw Host 充当 S1 的“外部业务后端”，不是另建 Office 服务。
- 聊天：浏览器主动调用 Host；Office Fastify 默认不提供聊天生成。Host 提供 /office/chat，这是本项目地址选择，非 Office 强制路径。
- 展示和操作：Host 调用 POST /api/pawoffice/messages；Office 自己管理 GET /api/pawoffice/events、POST /api/pawoffice/reply、POST /api/pawoffice/ui。Host 不伪装浏览器回报，不直接接管内部 SSE。
- Office 主入口由浏览器建立 workspace=1 订阅。仅 teams=… 的场景订阅不能接收业务快照恢复或菜单，不能当作在线业务页面。
- 文本输出不会自动更新任务/白板；Host 从真实运行事实生成投影，经 messages 同步。
- 兼容 DTO、路由、错误转换集中 openpaw-engine-host；Host 内部直接调用 Rust 服务，不绕自己的 HTTP 查询。
- 不新增 Node Bridge，不复制 AgentLoop、Session、工具注册表、审批系统、存储解析器或场景运行时。MCP sidecar 保留原用途。
- engine-host feature 默认关闭；默认 CLI、离线能力和 sidecar 保持兼容。不得引入文件/crate 行数上限；lines 仅统计，解析/读取失败仍报错。

### 2.2 数据与标识

| 数据/标识 | 所有者与约束 |
|---|---|
| teamId / workspace.id | Host 配置的稳定办公室 ID；两者相等，默认 4 个办公室，可配置 |
| conversationId | 浏览器生成；与 teamId 一起绑定 Host Session，新对话不新建办公室 |
| Session / Run / message / call ID | Host/引擎稳定标识；不拿人物 ID 代替，不用浏览器截断历史重建账本 |
| employee.id | 业务成员或功能 ID；不代表已实现多 Agent 编排 |
| sceneActorId | Office 场景人物 ID；从能力查询核实，多个成员可绑定同一人物 |
| Approval ID | Host 业务审批标识，绑定 Run/call/工具/最终输入摘要 |
| decision.id | 一次菜单展示尝试的 ID；重开菜单换 ID，仍关联原审批 |
| action.id | 本次领取操作的 ID，用作 ui.respond.interactionId |
| dialog.action.interactionId | 原详情上下文 ID，不能代替新 action.id 回应 |
| commandId / event.id | 场景命令/业务事件稳定标识；重试不生成新的历史交接 |
| 消息与工具结果 | 现有 JSONL 为事实，SQLite 索引可重建；完整工具配对与终态必须保留 |
| Host 元数据 | 绑定、Run、Approval、投影版本及恢复；不再保存一份权威消息历史 |
| Office Fastify | 单进程内存中的最新快照/等待请求；不是业务数据库 |
| 浏览器 IndexedDB | 当前浏览器聊天、草稿和临时展示；不替代 Host 持久化 |
| UI/动画回执 | 不作为工具已执行、任务成功或授权成立的事实 |

## 3. Office 通用协议与七种操作

### 3.1 通用规则

所有业务操作和场景命令发送至 POST <Office URL>/api/pawoffice/messages，携带 Authorization: Bearer <PAWOFFICE_API_KEY> 和 Content-Type: application/json。业务请求用 type 区分；场景请求是 {teamId,command}，不得混用旧 /actions gateway。

响应为 {code,msg,data}：code 与 HTTP 状态一致，data 必须为对象。Host 校验 envelope 和具体操作结果；错误读取 reasonCode/details/issues，不根据 msg 文案分支。错误结构不完整或响应类型错误视为协议故障，不能当空数据成功。

| 约束 | 必须落实 |
|---|---|
| JSON | 任意层级无 null，拒绝 __proto__、非有限数字及超深结构；最大深度 64；strict 字段不随意扩展 |
| 请求大小 | messages 请求完整 UTF-8 JSON 最大 65536 字节，含外层包装；初始化数组和菜单也受限制 |
| 服务内存 | 最多 100 办公室、最新快照总量 8 MiB；不是 Host 应主动用满的目标 |
| ID | 办公室/人物/命令/交互 ID 为 1—100 字符且符合字母数字及 _.:-；员工/任务 ID 按对应业务字符串 Schema |
| 时间 | 带时区 ISO 8601；不以跨机器时间相等作为审批成立条件 |
| 默认值 | 可选文本空串、列表空数组、online=false、未知指标/进度=-1；不能发送 null |
| 等待 | 初始化/快照/查询默认等页面 10 秒；菜单默认 60 秒；ui.next 默认 30 秒；分别设置 HTTP 超时 |
| 大小层次 | 请求 64 KiB、规范化快照/内部 SSE 上限、Host 聊天限制分别处理；server OfficeState 还检查补齐默认值后的快照大小，不把 64 KiB误当所有响应上限 |

Host 可采用更保守的展示预算，但不得截断必填 ID/关联字段或损坏 UTF-8。64 KiB 不适用于本项目独立聊天路由；聊天合法历史可能超过此值。

### 3.2 操作实施表

以下各请求体均通过同一 messages 地址。成功响应下表描述 data；外层仍包含 code/msg/data。

| 操作/来源 | 请求关键字段 | 返回与时机 | Host 实现与验收 |
|---|---|---|---|
| office.capabilities；S1/S5 | {type:"office.capabilities"} | 立即 200；scene/ui/requests/responses | 初始化前查询，核对协议 2.0、人物、所需能力、detail-v1/task-board-v1 和 Schema；缺必需能力明确不兼容 |
| offices.initialize；S1/S2 | {type:"offices.initialize",offices:[完整快照]} | 200：取得页面逻辑初始化结果；202：已缓存未确认；data 含 offices/actors/supportedMessages/sceneCommandProtocol/ui | 按 workspace.id 新增或更新，未列出办公室保留；无删除功能；runtimeReady 不等于素材全部加载 |
| office.snapshot；S1/S2 | {type:"office.snapshot",teamId,snapshot:完整快照} | 200 applied/ignored 或 202 cached，含 teamId/seq/status，部分 ignored 含 reasonCode | teamId=workspace.id；已初始化；同版内容固定；T4 真服务验证所有版本分支 |
| office.read；S1/S5 | {type:"office.read",teamId?} | 无/空 teamId 查目录；指定时返回 snapshot/scene/pendingDecisions；未初始化 404，无在线业务页面 409 | 用于连接恢复和展示核对，不能覆盖 Host 账本或复原授权；目录 200 不代表每间办公室在线 |
| decision.request；S1/S5 | {type:"decision.request",teamId,decision,timeoutMs?} | 同一个 POST 等选择后 200；到期 408；无在线页面 409 | 绑定共享审批、校验返回标识；无结果不批准；详见第 6 节 |
| ui.next；S1/S5 | {type:"ui.next",timeoutMs?} | 1000—30000ms，默认 30000；data.actions 最多 1 项，空数组是正常无操作 | 单个全局领取循环按 teamId 路由；可取消；记录领取后处理；不承诺重投 |
| ui.respond；S1/S5 | {type:"ui.respond",interactionId,presentation} | 200 {interactionId,delivered:true}；过期/已结束 410 | interactionId=新 action.id；dialog 返回 detail-v1，whiteboard 返回完整新快照；不能仅传局部白板 |

### 3.3 启动、初始化与服务重建

~~~mermaid
sequenceDiagram
    participant H as Host
    participant D as Host元数据/账本
    participant F as Office Fastify
    participant B as Office浏览器
    B->>F: Office自己建立 workspace=1 SSE
    H->>D: 恢复绑定、未终结Run、seq和原投影
    H->>F: office.capabilities
    F-->>H: 能力和Schema
    H->>H: 校验必需能力及配置
    H->>F: offices.initialize，完整快照
    alt 在线页面确认
        F->>B: SSE初始化
        B-->>F: 内部reply
        F-->>H: 200及runtimeReady
    else 未取得确认
        F-->>H: 202，已缓存
    end
    H->>F: office.read核对目录/办公室
    Note over H,F: 连接恢复或定期协调发现404时重新初始化
~~~

Host 的 Office 协调器在启动、连接失败恢复、404 和可配置的有界周期执行目录核对/重同步；请求间有退避，可取消，不持引擎全局锁。不能通过 capabilities 或 /health 成功推断服务端目录仍在；也不新增依赖浏览器内部 SSE 的恢复通道。

Office Fastify 重启丢失目录和快照；Host 使用自己保存的最新完整状态重新初始化。Office 浏览器根据其内部 stateId 自行清理旧目录，Host 不模拟该事件。Office 暂不可用时运行结果仍按真实引擎事实保存；展示待同步、网络失败与 Run 失败分开记录。

## 4. 聊天需求：v1 可实现什么、不能承诺什么

### 4.1 请求、会话绑定与历史

浏览器以配置的 VITE_PAWOFFICE_CHAT_URL 向 Host /office/chat 发送：

~~~json
{"version":"1.0","teamId":"office-1","conversationId":"conversation-id","messages":[{"role":"user","content":"处理当前任务"}]}
~~~

1. version/teamId/conversationId/messages 都必填；role 只接受 user/assistant。Host 同时执行真实 Schema 和本项目语义校验：最后一项为本次非空 user，历史为完整交替对话，拒绝未知办公室/未授权工作区。
2. teamId 必须来自配置的办公室绑定，不能凭请求新建任意工作区。同 Session 或同办公室已有 active Run 返回 409；容量满 503，不建立隐式无限队列。
3. 首次未知 conversationId 可导入浏览器提供的合法完整历史，标记为不可信的 imported 用户/助手上下文，只作为历史文本，不导入工具执行、审批或运行成功事实。新 Session、绑定、导入文本和本次输入在接受边界可靠保存；失败不启动工具。
4. 已绑定会话以 Host 账本为准；浏览器历史须与 Host 中已完成且可发送的 user/assistant 展示投影后缀核对，不用浏览器短历史覆盖完整账本。合法短后缀接受，不一致返回明确 409，不能静默覆盖或自动新建会话。失败/取消回复不当成完整历史。
5. 首次导入、后续后缀核对、压缩后的历史查询分别测试；不以消息数组长度推断新增账本。导入规则是 Host 策略，不是 Office 协议自带同步机制。
6. 新对话创建新的 conversationId/Session，原办公室保留；旧账本不删除。切换办公室保留原运行，关闭/刷新导致的 v1 请求断开按取消处理。
7. v1 无 turnId，不承诺提交恰好一次、刷新续接或完成后的请求重试幂等。不能用“最后一条文本相同”误判两个相同用户问题；不得自动重放有副作用的请求。

### 4.2 NDJSON、容量与终态

成功 HTTP 200，Content-Type: application/x-ndjson，每行一个 JSON 并换行；事件仅允许：

~~~jsonl
{"type":"status","phase":"thinking"}
{"type":"status","phase":"streaming"}
{"type":"delta","text":"正在处理。"}
{"type":"done"}
~~~

失败流发送 {type:"error",message:"非空错误说明"}，不再发 done。status 仅 thinking/streaming；不向 strict v1 添加 runId、cursor、工具、审批状态等字段。接流前参数错误使用明确 HTTP 错误；接流后用 error，不能已输出 200 后再改 HTTP 状态。

| 边界 | 实现和测试要求 |
|---|---|
| 当前输入 | 最多 4000 字符，纯空白拒绝 |
| 历史 | 最近最多 10 轮完整历史，历史部分最多 100000 字符；另加本次输入 |
| 回复 | 最多 50000 字符；错误说明≤1000，必须非空 |
| delta | text≤16000；实际序列化单行≤32000，包含JSON包装/转义，不只检查正文 |
| 长度计算 | 浏览器 .length 按 UTF-16；Rust 按兼容语义计数，UTF-8 字节单独检查；覆盖中文/emoji/引号/控制字符及跨片段字符边界 |
| 请求大小 | Host 聊天独立配置有界 UTF-8 body 上限，至少容纳上述合法历史及JSON最坏转义；不能直接沿用 messages 的64 KiB |
| 请求时间 | Office 默认 180000ms 总时限，心跳不会重置；Host 的空闲超时/Run预算另设，不能宣称绕过浏览器总时限 |
| 成功 | 有实际文本，必要账本与终态保存成功后发送 done；空文本/错误/写入失败/输出超限不发虚假 done |
| 重试 | 已输出后不自动从头重试拼接；有副作用后不通过重新运行伪恢复 |
| 断开与停止 | v1 HTTP 断开触发取消；先关审批门，取消上游/等待/工具，回收完成才结束 |

浏览器停止/刷新会保留已生成部分并标停止，不证明 Host 已清理完毕；v1 没有可靠远程运行状态查询界面。这个缺口必须向用户说明，不因后端增加接口就宣称现有页面已获得续接能力。

~~~mermaid
sequenceDiagram
    participant B as Office浏览器
    participant C as Host聊天适配器
    participant S as 共享Session/Run服务
    participant E as 现有AgentLoop
    B->>C: v1请求，teamId/conversationId/messages
    C->>S: 校验范围/历史/容量，持久化接受
    S->>E: 新建独立运行上下文
    E-->>C: 文本与运行结果
    C-->>B: 合法status/delta
    alt 正常完成且账本保存成功
        S->>S: 持久化终态
        C-->>B: done
    else 错误
        C-->>B: error，不发done
    else 浏览器断开/停止/180秒超时
        C->>S: 请求取消
        S->>E: 取消并等待实际清理
        S->>S: 持久化cancelled
    end
~~~

### 4.3 密钥与部署

模型密钥仅 Host 持有；Office API Key 仅 Host/Fastify 持有，不写入 VITE_*、事件、工具继承环境或浏览器。VITE_PAWOFFICE_CHAT_URL 是公开构建配置，生产须在构建前配置；不提供携带任意 secret 的浏览器 Bearer 方案。

优先同源代理到 Host；跨域时 Host 显式允许受信 Office origin 和 OPTIONS，不依赖跨域 Cookie，不使用通配源开放私密工作区。Host 负责来源/访问范围验证，teamId/runId 不是授权凭据。Office SSE 不内置完整登录或租户隔离，部署方通过可信本机范围或反向代理保护页面、SSE 和聊天入口。

Office 开发用 npm run dev，纯 npm run dev:vite 不接收外部业务。生产复用 npm run build:full 后 npm start；环境由 shell/PowerShell/部署工具注入，启动器不会自动加载 .env。Linux/Windows 启动说明使用同一配置含义，Windows 不要求 sh。不得为了配置连接地址修改 Office 源码或擅自提交配置文件。

## 5. 快照、白板与真实状态投影

### 5.1 字段与引用

| 结构 | 必填/关键约束 |
|---|---|
| 快照 | schemaVersion="1.0"，seq为正安全整数，workspace.id/name；workspace.name≤80 |
| employees | ≤200；id/name必填，name≤80；role≤80，sceneActorId默认空，online默认false；ID唯一 |
| tasks | ≤1000；id/title/status/updatedAt必填，title≤160，summary≤1000；assigneeId空或引用当前员工 |
| events | ≤500；id/kind/summary/occurredAt必填，summary≤300；kind仅 note/task.assigned/task.started/task.blocked/task.completed |
| system | 未测cpu/memory/network为-1；不能生成假监控值 |
| 时间/引用 | 时间带时区；事件引用指向当前投影中有效对象或按Schema省略/空串；Host额外维护事件ID唯一和稳定性 |

Office 固定六个人物：office-lead、code-agent、file-agent、app-agent、review-agent、data-agent；仍通过能力查询验证。多个成员可共用人物，未绑定的成员仍显示列表但不关联动画。人物主任务选择由 Office 已有规则负责：running>blocked>failed>queued>completed，同级按 updatedAt 最新，同时间按任务 ID 升序；Host 不复制该选择器，也不因人物只有六个就删除全部其他业务任务。

**本期投影选择：**Run 对应主业务任务，工具调用通过详情查询；工具不自动变成独立 Agent。员工表达真实运行角色/工具功能，online 不冒充网络探测。进度未知为 -1；只有可验证工作量才报告百分比。

| Host事实 | Office任务/白板状态 | 说明 |
|---|---|---|
| 接受且未执行 | queued | 不伪造执行中 |
| 模型/工具实际执行 | running | proposed/ToolUseStart不能单独证明工具已开始 |
| 等待审批/资源 | blocked | summary说明原因，不伪造暂停能力 |
| 正常完成 | completed | 与Host持久化结果一致 |
| failed/cancelled/interrupted | failed | summary/详情分别说明失败、取消、重启中断；不增加官方枚举 |
| cancelling | blocked | 说明正在清理；清理结束再进入最终映射 |

事件由真实状态变化产生并稳定保存：分配才 task.assigned，开始才 task.started，完成才 task.completed，其他不能硬塞新 kind，可用 note。events 新到旧排序；首次加载/重试不重播历史交接。同人物交接不安排额外拜访。

### 5.2 白板与有界完整投影

白板固定 task-board-v1；title≤160、tagline≤40、description≤300、note≤160；link.label≤40，url为空或HTTP(S)；steps≤3，每项id/title有效，progress为-1或0—100。title=""表示清空；省略whiteboard或传{}会补空白默认结构。保留白板必须每次完整携带。

办公室进度条/白板的平滑增长最多99%是视觉反馈，不作为Host真实进度、API查询或完成判定。仅 completed 才表示完成，不能按动画时间推算业务成功。

投影器按办公室串行生成完整状态，优先保留活动/待审批任务，再保留有界近期终态；稳定排序、明确展示范围。裁剪任务/事件时同步维护引用闭包、员工和白板，不截断ID，不丢弃Host事实。完整历史由详情查询。先执行规范化和精确UTF-8序列化，再核验整个请求大小；无论数组上限是否达到，都不能超64KiB。超限时缩小可展示内容或明确失败，不拆成多个相互覆盖的局部快照。初始化多个办公室可分合法批次，每个办公室仍是完整快照。

### 5.3 seq、持久化与恢复

每办公室一个版本分配/投递协调器，覆盖普通snapshot和ui.respond.whiteboard，避免两个入口各自加seq。新内容先生成规范化完整状态并持久化seq与内容，再发HTTP；相同版本重试使用原内容，不重新生成时间/事件ID。事务失败不消耗一个被错误复用的版本；已分配版本不倒退，溢出拒绝并诊断。

| Office收到的版本 | 结果 | Host动作 |
|---|---|---|
| 大于当前 | 缓存并同步完整快照 | 等applied或记录cached |
| 小于当前 | 200 ignored / STALE_SEQ | 核对本地版本与单写所有权；不能当成当前版本已应用 |
| 相等且规范化内容相同 | 重新同步，可200 applied或202 cached | 保留相同内容重试，不因DUPLICATE_SEQ就跳过协调 |
| 相等但内容不同 | 409 SEQ_CONFLICT | 一致性故障，停止该办公室投递并诊断，不能盲目seq+1掩盖 |

Host 序列、发送和确认状态分开；某个旧请求的晚到确认不能把新版本标成已应用。保存最新完整内容和必要待确认版本，有界回收历史。超时/断线/422等失败不证明Office回滚；只按具体原因重试。Office重启清空缓存时重新initialize，Host不重置seq。

~~~mermaid
sequenceDiagram
    participant H as Host投影协调器
    participant D as Host持久化
    participant F as Office Fastify
    participant B as Office浏览器
    H->>D: 原子保存seq与完整规范化内容
    H->>F: office.snapshot
    F->>F: 校验并缓存
    alt 页面成功应用
        F->>B: SSE快照
        B-->>F: 内部应用回报
        F-->>H: 200 applied
    else 未取得页面确认
        F-->>H: 202 cached
    else 请求失败或超时
        Note over H,F: 快照仍可能已经缓存
        H->>F: 同seq同内容重试
    end
    H->>D: 按办公室和版本保存同步结果
~~~

## 6. 人工审批、用户操作与详情

### 6.1 审批：业务授权不是菜单回执

decision.kind为confirmation或choice。公共字段id/title必填，title≤160，description≤2000，actorId可省略/空；confirmation有confirmLabel/rejectLabel；choice有2—8个唯一选项，每项id/label有效、description≤500。timeoutMs为1000—300000，默认60000。业务Run剩余预算、Office v1剩余连接期限与菜单时间共同约束等待；不足以安全等待时不发一个必然跨越Run期限的授权请求。

成功data必须校验teamId、decisionId、choiceId；confirmation只接受confirm/reject，choice只接受本次选项。label只作显示。Office多页面最先到达的有效选择完成请求，Host仍用自身CAS保证授权只消费一次；后续/过期结果不得执行。HTTP客户端与代理超时应大于实际菜单timeout并留传输余量，例如300秒菜单至少按310秒通道规划，但不能因此延长Office v1聊天总时限。

~~~mermaid
sequenceDiagram
    participant E as 引擎工具流水线
    participant A as Host共享审批
    participant F as Office Fastify
    participant B as Office浏览器
    E->>A: 最终输入校验/分类，绑定Run/call/摘要
    A->>A: 保存pending，未持工作区锁
    A->>F: decision.request，新展示ID
    F->>B: 展示菜单
    alt 有效用户选择
        B-->>F: 内部reply
        F-->>A: 原POST返回teamId/decisionId/choiceId
        A->>A: 校验关联，CAS批准或拒绝
        A-->>E: 本次授权结果
        E->>E: 可取消取锁，重验输入/路径/前置条件后执行
    else 超时/断线/Run取消
        A->>A: 先使业务授权失效
        A-->>E: 不批准，拒绝/取消原因分别保留
    end
~~~

Office无菜单重试去重/已结束结果查询。网络未知结果不等于拒绝或批准；Host不自动重发同菜单，不从pendingDecisions推导授权。重新展示换decision.id，关联同Approval；Run取消后晚到的允许无效。批准后输入变化须重新授权。Run取消先CAS关闭审批，再取消HTTP展示；UI关闭失败不影响业务门已关闭。审批前工具零副作用，等待审批不持资源锁。

### 6.2 ui.next / ui.respond

一个全局领取器按teamId路由，不为四个办公室各建竞争poller。ui.next空数组继续等待；503有界退避；关闭可取消。收到action后立即持久化处理记录，再查服务或执行已支持行为；查询处理不阻塞下一次领取，不让慢详情堵住全局poller。

用户操作最多128个、等待60秒，ui.next同时等待最多16个；Host只需要一个。领取即失去再次投递资格，无ACK/租约/requeue，响应丢失或收到前Host崩溃可能丢一次操作。记录后可减少后续重复/丢失，但不能宣称端到端恰好一次。R2若需改变该边界，必须先Rx批准。

| action | 本期明确行为 |
|---|---|
| actor.inspect | 查该办公室人物绑定成员；多成员给真实列表/分页，不假定人物=唯一Agent |
| employee.inspect | 查真实成员、任务与关联历史 |
| dialog.action | 校验原interaction上下文、optionId、办公室/会话/查询范围和有效期；处理分页/返回/详情 |
| task.start | 首期未实现“从展示任务重新启动业务”的命令，明确不支持；不自动重放旧Run |
| task.block | 未实现真实暂停/恢复，明确不支持；不改快照伪暂停 |
| task.complete | 不允许用户把运行中Run伪标完成，明确说明实际终态由Host决定 |
| task.assign | 未实现执行所有权迁移，明确不支持；不只改assigneeId |
| 未知操作 | 明确协议/能力错误，不空成功 |

对受支持查询或不支持业务操作，用合法detail-v1回复说明；UI已过期410则记录未送达，不以新ID伪装原请求。ui.respond用新action.id，即使它是dialog.action也不能用其原interactionId。收到delivered=true只说明展示结果已提交，不证明副作用执行或用户已阅读。

presentation.target=dialog时content必须detail-v1；target=whiteboard时snapshot必须该办公室完整新快照，并经过同一版本协调器。不能只发白板字段、跨办公室回应或绕过seq。原详情按钮产生新的action，Host为新action.id再次回应。

### 6.3 六类只读详情

| 类别 | 数据来源与边界 |
|---|---|
| 历史 | 现有SessionReader/查询服务，包含真实终态；不从Office缓存拼历史 |
| 工具结果 | 完整call ID关联，区分提出、审批、实际执行、结果；敏感内容脱敏/限量 |
| Skill | 原FsSkillStore真正只读API，不能触发生命周期写入；不宣称自动注入已接通 |
| Goal | 原ledger只读，无自动目标调度承诺 |
| Todo | T2修复后的结构化读取；损坏/权限错误不能当空列表 |
| 文件 | 授权根内普通文本，≤256KiB；校验真实路径、符号链接/重解析点、文件变化，拒绝越界/二进制 |

detail-v1标题≤160、description≤4000、fields≤30（label≤100/value≤2000）、actions≤8。每页默认5项，为上一页/下一页/返回预留按钮；正文纯文本，不能塞HTML/富文本。大结果说明裁剪范围并提供后续分页。动作ID保持协议合法字符和长度，使用Host有作用域的映射，不把绝对路径或secret直接写入按钮ID；旧作用域不能访问另一办公室/会话。

## 7. 场景命令、错误策略与可观测性

### 7.1 场景仅作反馈

~~~json
{"teamId":"office-1","command":{"protocolVersion":"2.0","sceneId":"office-1","commandId":"say-1","type":"activity.start","capability":"scene.say","participants":[{"entityId":"office-lead","role":"actor"}],"params":{"text":"开始处理任务。","durationMs":2000}}}
~~~

先通过capabilities/office.read核实能力、参与者角色和参数；teamId必须等于每条sceneId，不能硬编码未知能力。新命令唯一ID，重试同ID同规范化内容。batch为{mode:"sequence"或"parallel",commands:[…]}，≤32，非全有或全无事务。默认只用已验收的发言/必要反馈，不顺手开放map.edit或任意物品写入。

HTTP200含forwarded只是连接转发数，无开始/完成回执；场景runtime本地的同ID去重和执行结果不等于Host能通过HTTP查询。临时动画离线不积压、不补播。command.cancel/activity.stop是场景控制，不取消AgentLoop；Run取消由共享服务执行，场景停止是尽力反馈。持续工作状态优先用snapshot表达，不依赖动画成功。

task.assigned已触发跨人物拜访时不重复visit；同人物交接不拜访。白板只能snapshot，已初始化墙面白板用object.state.set会400 SNAPSHOT_REQUIRED。场景有限活动和队列的超时按S6语义，不拿浏览器动画时间计算Run预算。

### 7.2 错误处理矩阵

| HTTP/原因 | Host动作 | 禁止行为 |
|---|---|---|
| 200 | 校验对应data和版本，区分applied/ignored/forwarded/delivered | 统一解释为工具或动画完成 |
| 202 cached | 保存已缓存未确认；等待协调/重连 | 标记页面已应用 |
| 400/422 | 保存reasonCode/issues，修正数据或登记实现差异 | 盲目无限重试或吞成空数据 |
| 401/403 | 标记认证/访问配置故障，停止高速重试并提示 | 记录密钥或换演示密钥自动降级 |
| 404 OFFICE_NOT_INITIALIZED | 以Host完整状态重新initialize，再续同步 | 重置本地seq/账本 |
| 408/网络中断 | snapshot/initialize可同版同内容有界重试；菜单结果未知不授权 | 重放工具或把超时当默认批准 |
| 409 SEQ_CONFLICT | 停止该办公室投递，诊断所有权/内容差异 | seq+1掩盖一致性问题 |
| 409 NO_RECEIVER | 区分菜单/查询/动画不可达与Run真实结果 | 宣称业务执行失败或成功 |
| 410 | UI已结束/过期，记录未送达 | 新ID无限回应旧请求 |
| 413 | 审查完整请求和规范化投影预算，缩小展示或明确失败 | 局部快照轮流覆盖完整状态 |
| 500/503 | 有界指数退避、容量控制和可取消等待；保留诊断 | 无限队列/阻塞所有Session |

重试策略按操作制定，设置上限、抖动、取消和恢复条件；未改变配置/数据的永久错误不得循环。日志关联team/session/run/call/approval/seq，不写密钥、完整prompt或敏感文件。Office连接状态与Run状态分开；UI/动画失败不把已完成Run改失败，Run失败也不伪装Office网络断开。

## 8. Office Rx：任何修改先人工审核

| Rx | 分类与原因 | 当前处理 |
|---|---|---|
| R1 | 必要功能扩展候选：超过180秒、刷新续接、Run标识、detach/cancel；v1缺这些能力，不属于现有Office缺陷 | 可提交审核，尚未批准；本期默认按v1交付，不实现提案 |
| R2 | 协议扩展候选：UI领取ACK/租约/重投；现协议明确无此保证，不伪报缺陷 | 本期不实施，记录可靠性边界 |
| R3 | 产品新增：侧栏/富文本/管理页 | 本期不做，复用detail-v1 |
| R4 | 工程扩展候选：为原生验收改Office测试启动器 | 默认在OpenPaw写外部原生runner、复用公开场景；确需改Office测试才提交Rx，不默认修改 |

每个Rx提案必须包含：冻结基线和准确版本；缺陷复现或必要性；无Office修改的替代方案及不足；接口/字段/生命周期；最小文件/符号/依赖范围；用户行为、存档/数据、部署、安全和Linux/Windows影响；测试/回退；与Host的配套范围；是否阻塞已承诺能力；批准人、批准证据和有效范围。缺陷先证明违背已有契约，能力缺口不能只改名为bug。

批准后才创建/修改Office实施范围，Office PR指向Office评审基线，不指向OpenPaw分支；与Host关联PR互链。变更后先完整测试，再审核当前提交；不因提案批准跳过PR审核。用户未回复、仅同意研究、CI成功都不是批准。

R1若将来获批，可复用[r1-chat.md](r1-chat.md)深化：Host新增Run创建/快照/事件续接/显式cancel；浏览器先保存turnId再提交、保存runId/cursor、刷新detach、停止cancel、cancelling等待清理、旧IndexedDB兼容。它是双方新增的opt-in契约，不是Office当前官方v1能力。接口字段和事件必须先冻结再开发，不能把runId/cursor塞入v1 strict事件。未经批准不提前修改Office，也不单方面实现一套无人使用的扩展冒充交付。

## 9. OpenPaw共享运行、缺陷与复用不变量

### 9.1 必须复用和修复的位置

| 模块 | 判定与要求 |
|---|---|
| AgentLoop/Provider/压缩/预算 | 复用；每Run独立实例/token；模型选择和生命周期修回原crate |
| CLI/Host装配 | 抽取无协议公共能力，不维护两套Provider/模型规则 |
| JSONL/SessionReader/Writer/SQLite | 复用并修完整性，索引可重建；Host不另写历史解析器 |
| ToolRegistry/文件/搜索/执行工具 | 扩展环境与调用上下文，包装锁/授权/事件，不复制主体 |
| Environment/Linux sandbox/Windows Job Object | 复用并修失败/取消/输出，不能新写第二套执行后端 |
| OS共享/独占锁 | 复用用于数据目录所有权和跨进程协调；不是存在一个.lock文件就算锁 |
| Skill/Goal/Todo | 原模块只读接口，必要时延展；不重复解析格式 |
| Host共享服务 | 新增Run注册、审批CAS、恢复、投影与协调，属于当前缺失宿主职责 |
| Office/OpenCode适配器 | 新增Rust协议边界，内部共享服务，不带协议字段污染核心 |

### 9.2 缺陷台账

D01—D11已定位待对应阶段修复；每项独立逻辑提交、复现、根因、测试和关键不变量注释。D12—D15、D17—D24为T0已发生的基线修正，须追溯登记而非伪造事前审批；D16待查。新问题沿用audit编号，不混淆缺陷与能力缺口。

| ID | 必须修复/验证的内容 | 阶段 |
|---|---|---|
| D01 | CLI未传Provider默认模型，真实请求可能携带openpaw-default；显式模型优先 | T1 |
| D02 | 外部token被覆盖、超时未生效、多阶段不可取消；区分取消与正常完成 | T1 |
| D03 | 过滤User导致tool_result丢失、call关联不完整、吞写入失败 | T1 |
| D04 | 读取容忍尾损但append不修；先备份修尾，中间损坏报错 | T1 |
| D05 | 串并行校验不同、metadata未落实、Hook审批后改输入 | T2 |
| D06 | 并行全结束才发结果、耗时失真、panic丢call ID、取消清理 | T2 |
| D07 | bash内外审批脱节、固定LocalEnvironment，需调用级授权/环境注入 | T2 |
| D08 | 只杀直接子进程、Windows输出死锁风险、句柄/挂起进程清理 | T2 |
| D09 | 受限执行失败降普通执行、把环境提示当隔离；必须fail closed | T2 |
| D10 | cmd实际执行却可能按Bash分析；绑定shell与实际参数 | T2 |
| D11 | Todo无锁覆盖/吞损坏，需原模块锁、原子更新、结构化读取 | T2 |
| D16 | OS文件锁释放后立即重获间歇失败，未证明根因，不得重跑洗绿 | T3使用前调查 |

D12—D24具体来源、类型迁移和许可证影响继续见audit、dependency-baseline和validation；不得因改写计划丢失既有审核要求。

### 9.3 运行与资源状态

同Session最多一个active Run；同Office最多一个；不同Session可以并行，全局默认4，超限明确503。默认关闭Run内工具并行，但已有并行路径仍修复。Run预算、取消、消息上下文不共享。审批/查询/取消不被长运行的全局串行锁堵住。

工作区按规范化真实根识别，读共享、写文件/Todo/shell独占；审批不持锁，取锁可取消，获批后重验最终输入、权限、路径和必要文件前置条件；实际提交/进程回收完成后才释放。外部编辑器不遵守锁，不能承诺防止其改动。同根复用Store；固定home显式注入，不能每请求new存储或改环境绕过全局OnceLock。

~~~mermaid
stateDiagram-v2
    [*] --> accepted: 持久化接受
    accepted --> running: 开始执行
    running --> awaiting_approval: 最终输入已分类
    awaiting_approval --> waiting_resource: CAS批准
    awaiting_approval --> running: 拒绝作为工具结果
    running --> waiting_resource: 无需审批的工具
    waiting_resource --> running: 获锁重验后执行
    running --> completed: 真实完成并持久化
    running --> failed: 错误或总预算耗尽
    accepted --> cancelling: 取消
    running --> cancelling: 取消
    awaiting_approval --> cancelling: 先关闭审批门
    waiting_resource --> cancelling: 可取消等待
    cancelling --> cancelled: 实际清理后持久化
    accepted --> interrupted: Host崩溃恢复
    running --> interrupted: Host崩溃恢复
    awaiting_approval --> interrupted: Host崩溃恢复
    waiting_resource --> interrupted: Host崩溃恢复
    cancelling --> interrupted: Host崩溃恢复
~~~

~~~mermaid
stateDiagram-v2
    [*] --> pending: 保存业务审批与输入摘要
    pending --> approved: CAS首次允许
    pending --> rejected: CAS首次拒绝
    pending --> expired: 到期
    pending --> cancelled: Run取消
    pending --> invalidated: Host恢复
    approved --> consumed: 绑定call和输入执行一次
~~~

approved不是无条件执行通行证；取消、输入变化、权限变化或前置状态不符时仍拒绝。启动复用OS锁限制同home单写，恢复账本/索引/元数据后把未结束Run标interrupted、旧审批失效、恢复原seq/快照，再宣告ready。不自动重放可能有副作用的工具。关键接受/审批/工具边界/终态可等待保存；token可合并，不能逐token fsync；同步事件回调不阻塞HTTP/磁盘，也不因队列满丢关键事实。

## 10. T0—T8任务卡：方法、产物、完成与交接

所有任务卡共用第11节验证与第12节PR规则。需求标识在本节只作交互分组，T0建立正式REQ/DES时逐项关联，不能只生成编号不填写行为。

### T0：工程治理、源码/协议基线与验收门禁

**目标与范围：**冻结S0—S8及第2—9节可实施契约，补工程治理；不实现Host业务，不修改Office。已有T0基线修复纳入追溯CHG。当前PR未合入，补充后重新验证与审核。

**实施要求：**
- 建立AGENTS入口：项目背景、冻结前端资料、先审源码/先复用、授权范围、中文注释、文档同步、完整测试、Rx和阶段后CHG流程。
- docs/requirements/产品需求说明.md记录REQ行为/边界/平台/状态/验收；docs/design/软件设计说明.md记录DES模块/状态/数据/复用/失败处理；引用已有规格，避免复制权威Schema。
- docs/changes/一Tx一主CHG，关联REQ/DES/D/R和代码/测试；索引生成。docs/verification/保存当前提交证据；历史结果不可覆盖。
- governance接入参考framework的最小固定工具和profile，记录来源/哈希/许可/修补；不依赖本机绝对路径，不擅自修改参考工程。工具命令登记不等于测试执行，人工姓名字段不等于批准证明。
- 每次功能/bug有文档记录和功能/回归测试；所属crate tests/features、tests/regressions用显式入口挂载，私有不变量保留inline测试并关联；不扩大public只为测试。
- 公共新增/行为变化声明中文注释，函数/方法含功能、参数、返回、错误、状态、并发与取消、权限、关联；保留SAFETY/Stability机器标记。历史债务报告，不批量翻译。
- 复用现有xtask语法解析和CI；补注释/CHG/范围/链接检查、治理工具正反用例及stability/smoke实际调用；Linux/Windows，MSRV1.88、Node24.19.0；不新增macOS验收任务。
- 固定Office原始Schema测试快照和SDK1.18.15，校验来源哈希及真实类型；Node只在测试依赖图内。每种业务请求、响应、错误、聊天与场景正反fixture都可定位。
- lines保留统计取消门槛；合法REQ/DES/D编号与scrub冲突时精确处理并测试，不把源码全放行。修正贡献/兼容说明的过期内容，保留历史事实。
- 为每项Rx准备原因/方案/影响模板，R1仍待审；不得调用approve伪造人工授权。

**产物：**AGENTS/需求/设计/治理规范及模板；T0主CHG与追溯记录；复用/D/R台账；协议矩阵/图/fixtures；CI及本地完整记录；测试入口与交接模板；Rx提案但无Office修改。

**完成标准：**七操作、聊天、快照、UI、审批、场景、恢复各有需求→Tx→测试；真实Schema/SDK检查通过；干净Linux/Windows可执行治理与测试；缺陷有根因/归属；不使用占位成功；当前PR获批合入。治理工具尚未实现不能靠文档宣称已生效。

**交给T1：**固定契约、D01—D04复现、完整测试基线、有效CHG与兼容边界。

### T1：引擎生命周期与会话持久化（D01—D04）

**目标：**修核心可信性，不混Office/OpenCode路由。复用AgentLoop、Provider、SessionReader/Writer、SQLite。

**实施要求：**抽取CLI/Host共用模型装配，显式模型优先；RunContext/RunOutcome兼容旧入口；外部token保留，覆盖建连/流/退避/压缩/等待；区分流空闲、工具超时与Run总预算；已输出后不从头拼重复文本。保存user/assistant/tool_use/tool_result/usage/终态和call ID，不按role或数组长度过滤；错误传播，副作用后不隐式重试。append前备份修损尾，中间损坏诊断停止；JSONL索引可重建。

**产物：**D01—D04各自独立逻辑提交与回归；公共装配/运行接口；工具配对/压缩/恢复/磁盘故障/损坏数据样例；关键不变量注释和CHG。

**完成标准：**捕获实际Provider HTTP模型字段正确；各等待阶段取消不报正常完成；恢复不丢不重工具链；持久化失败不报成功；旧CLI/API兼容；双平台完整验证，PR批准合入。

**交给T2：**可取消运行上下文、明确定义的结果与完整账本。

### T2：工具授权、环境、进程与Todo（D05—D11）

**目标：**修核心工具执行与跨平台清理，复用现有Environment/sandbox/Job Object/OS锁，不另写后端。

**实施要求：**Hook完成最终输入→校验→分类→审批→资源→执行；授权绑定Run/call/工具/摘要，改输入重审。bash使用本次调用授权，不永久放开危险判定；ToolRegistry注入Environment。落实metadata，结果完成即发、任务内计时、panic保留call ID、取消join清理。Linux进程组/bwrap复用；Windows挂起创建→Job→恢复，句柄即时RAII，Job前失败清理挂起进程。stdout/stderr并发持续排空，展示截断后继续丢弃超限输出；spawn_blocking不冒充可取消。不能实施限制就拒绝，不降普通执行；cmd/sh实际参数与分析策略一致，无Git Bash要求。Todo原模块结构化读、锁、原子写，不吞损坏。

**产物：**D05—D11逻辑提交/回归；调用授权/托管进程/环境注入API；普通/受限平台能力矩阵；大输出/子树/失败/审批/Todo证据；路径说明。

**完成标准：**审批前零副作用，拒绝/过期/取消不执行；孙进程和双管道大输出无残留/死锁；实际清理后放锁；普通Windows用户可本地执行，受限能力如实拒绝；中文/空格/盘符/UNC/大小写/链接重解析点/文件占用验证；Todo不丢更新；双平台完整验证，PR批准合入。

**交给T3：**统一工具上下文、真实Environment注入、进程树回收保证。

### T3：共享Host运行、查询、审批与恢复

**前置：**T2合入；采用锁前调查D16并补跨进程证据。Host默认feature关闭。

**实施要求：**建立Session/Run/Approval/资源/查询/配置服务；每Run独立AgentLoop；同Session/Office冲突、默认4容量。复用JSONL/索引/Store，Host只管元数据；固定home并复用同根Store。生命周期观察可等待，文本合并、关键事实可靠。CAS审批与可取消锁，恢复interrupted/失效审批，不重放。只读Skill/Goal/Todo不触发写入。CLI启动/loopback/port=0/发现/配置校验/优雅关闭，不泄露模型secret。预留Office投影查询，不在服务里引入Office字段或内部HTTP。

**产物：**共享服务API；CLI与配置；Run/Approval/锁/持久化边界/崩溃恢复测试；确定性Provider/工具场景；数据版本和索引恢复说明。

**完成标准：**两Session预算/取消/消息隔离；同Run多视图只执行一次；查询/取消不被长运行堵住；恢复不批准不重放；默认无axum，Host/sidecar共存；Linux/Windows启动/关闭/路径/锁/恢复真实通过；PR批准合入。

**交给T4：**可直接调用的共享服务、测试场景与稳定ID。

### T4：官方v1聊天、初始化、查询与快照闭环

**范围：**不改Office；实现第3节四个操作capabilities/initialize/snapshot/read及第4—5节。

**实施要求：**Rust Office客户端和v1聊天适配器；配置与访问范围、两条链路分开；实现导入/绑定/历史后缀核对，真实账本执行；NDJSON strict事件、UTF-16/序列化行/独立body预算、错误/断开取消。能力核验→初始化→投影协调→read恢复；按办公室单写seq+完整内容，处理applied/cached/ignored/冲突/重启，禁止局部快照。员工/任务/白板映射遵守第5节，先只开放已验证只读工具。

**产物：**客户端/适配器；请求与响应错误类型；绑定/历史策略；投影与重试持久化；连接/代理/密钥样例；外部HTTP和真实浏览器Linux/Windows runner，产物不写Office工作区。

**完成标准：**
- 页面真实对话入账；切换办公室不串线，新对话不删除旧账本；首次历史导入和不一致冲突有测试。
- 4000/50000/100000/32000/16000与中文emoji/转义边界、非NDJSON/提前断流/空文本/写入失败通过；不发假done。
- 新seq、旧seq、同seq同内容重试/异内容409、离线202、在线容量满202、服务重启404→初始化均真实验证。
- 停止触发实际清理；只有场景teams订阅时不能当业务页面在线。
- Linux/Windows原生Node与浏览器验收，不用Docker/交叉编译代替；完整测试与PR批准合入。

**交给T5：**可信对话、办公室绑定、完整投影与错误/恢复接口。

### T5：审批、详情、UI操作与场景反馈

**范围：**不改Office；补齐decision/ui.next/ui.respond及第6—7节。

**实施要求：**一个领取循环与有界并发处理，按team路由、收到即记录；实现action支持矩阵，未支持按钮返回真实说明。共享Approval映射一次菜单，校验ID和deadline，重开不重复执行；v1剩余时间不足时不延后授权；取消先关闭业务门。六类只读详情/作用域分页、文件授权/大小/路径；稳定真实状态和task.assigned事件，不重复动画；场景仅作反馈。通过审批闭环后才按清单开放写工具，不默认开放占位plan/clarify。

**产物：**审批/操作路由；菜单与动作关联；六类详情/分页；Run→任务/白板/人物/事件映射表；场景命令映射/去重；写工具清单；UI投递边界说明。

**完成标准：**
- 允许/拒绝/超时/断线/重复/旧菜单晚到/取消竞争/重开/Host重启，只产生正确一次授权或不执行。
- 审批等待不占锁，其他Session可用资源；获批重验后执行。
- actor/employee/dialog准确查询，所有task按钮明确实际支持程度；新action.id回应和410过期真实验证。
- 详情来自原存储，只读不改文件；多页按钮不越限，文件越界/重解析点/二进制拒绝。
- 动画转发失败不改Run，重连不重播历史；白板绕过更新返回SNAPSHOT_REQUIRED。
- Linux/Windows浏览器与完整测试通过，PR批准合入。

**交给T6：**Office现协议下完整常规交互、可靠性边界与待审Rx。

### T6：Host恢复、重试与协议边界加固；按批准范围实施Rx

**默认目标：**不依赖修改Office即可完成恢复/故障加固，明确v1无法续接的边界；不再把未批准R1作为T7前置。

**必做方法：**跨聊天、快照、审批、UI的故障注入；刷新/停止取消与进程清理；snapshot晚到响应、缓存未知结果、Office重启/断网、Host崩溃后的账本/seq/审批一致性；验证HTTP与代理超时分层，排除心跳绕过180秒的虚假方案；UI领取无重投如实记录。修复Host发现的问题，核心缺陷仍回所属crate。深化R1/R2/R4提案的必要性和替代方案；没有批准只提案不开发。

**条件分支：**只有用户明确批准具体Rx后，才将其准确范围加入T6主CHG，更新设计/接口/测试/风险与两个仓库PR。R1若获批，实施Run创建/查询/游标续接/显式取消及Office opt-in聊天源、存档、生命周期；快照+游标一致，幂等turnId、游标过期取快照，detach不取消、cancel回收后结束，v1默认不变。不得把这一分支提前当已批准。

**产物：**恢复/重试故障矩阵与确定性用例；Host修复和证据；v1用户限制/运维说明；每项Rx原因/方案/影响与人工决定。已批准扩展另附两仓产物、迁移/回退和完整测试。

**完成标准：**必做恢复场景双平台通过，v1超过180秒按现协议停止/失败而非伪续接；不重复副作用、不复活旧审批。Rx未批准时记录“提案待审/本期未实施”，T6仅标基础范围完成，不能宣称R1验收；若已批准纳入本期，则扩展测试（含>180秒、刷新同Run、无重复文本/工具）和两仓PR均通过才完成T6。OpenPaw基础PR经批准合入后交接，不拿未批准提案无限阻塞SDK。

**交给T7：**可靠共享服务、v1限制、Rx准确决议与当前可用接口。

### T7：OpenCode v1 / OpenWork固定SDK预兼容

**范围：**复用共享服务；仅Host+SDK1.18.15真实请求验收，指定OpenWork桌面源码取得后的联调另列，不阻塞Office。T6基础交付即可进入，不依赖未批准的Office扩展。

| 旧方案易错点 | 固定SDK要求 |
|---|---|
| permission.asked | permission.updated |
| 通用permission reply/requestID | POST /session/{id}/permissions/{permissionID}，body.response；成功boolean；回复事件permissionID/response |
| message.part.delta | message.part.updated，part及可选delta |
| Session事件平铺 | properties.info |
| global SSE直接Event | {directory,payload:Event} |
| 简化provider/message | 满足真实Provider模型元数据/default/connected及user/assistant/tool状态必填字段 |

**实施要求：**Host完成Session create/list/get/messages/todo/status/prompt_async/abort、provider、permission、/event与/global/event；可保留/api/event同服务别名。prompt_async持久化接受后204，SSE断开只退订，不取消/新建Run；查询与待审批重同步。稳定Session/message/part/call ID，实际执行才running。Basic用户名密码均无仅loopback无认证、只配一个启动失败、两者都有保护全部OpenCode路由。目录header>query>默认，只解码实际SDK编码一次，必须授权根，Session固定工作区。connected仅真实可用Provider；always按once说明限制；不支持写操作501、非文本part400，不假204、不丢用户内容。只读降级与额外路由详见contracts，不做managed spawn/v2/插件/MCP管理/目标调度。

保留发现标记：

~~~text
opencode server listening on http://127.0.0.1:<port>
~~~

**产物：**OpenCode适配器、修正契约、精确锁定SDK客户端、完整类型fixtures、真实HTTP/SSE测试、未来attached配置和桌面追加清单。

**完成标准：**真实SDK create→promptAsync→事件迭代→工具→审批→abort→查询恢复；类型检查不用as unknown as掩盖；认证/目录越界/容量/同Session冲突/不支持操作正确；SDK退订与Office取消不串线，共享工作区锁和授权；Linux/Windows完整验证，PR批准合入。只声明固定SDK预兼容，不声明真实OpenWork桌面通过。

**交给T8：**两个适配器的真实证据、已批准能力与剩余限制。

### T8：双平台联合验收、发布与交接

**实施要求：**Linux/Windows原生Host+Office+SDK同组旅程；同工作区两Session读写等待/取消/审批；Host崩溃、Office重启、刷新/断线/磁盘故障/损坏会话；同Run跨视图消息/工具/审批/终态一致。Linux脚本和PowerShell共用配置，默认本机与同源代理，不泄secret、不要求Docker/WSL。保留CLI/sidecar/default/v1回归。至少一轮配置好的真实Provider，自动化核心使用确定性Provider。构建启用Host的发布候选、版本/校验；携带THIRD-PARTY-NOTICES和third-party-licenses原文，遵守D21/D22迁移/许可审核。不为最终验收扩新功能。

**产物：**双平台命令/提交/退出码/报告/证据；可重跑入口；启动/关闭/备份/恢复/回退；需求能力矩阵、D/R台账；发布候选/校验值；集成分支到main最终PR。

**完成标准：**所有必需测试实际执行，未以skip/mock/交叉编译代替；Linux/Windows原生通过；真实Provider结果；关键缺陷关闭；Office任何修改均有有效Rx及PR；未批准扩展如实未实施，不将v1限制包装成已支持。最终用户验收和main PR批准后才可合main；初期结束再转CHG流程，未完成的本期必需项不能借此绕过门禁。

## 11. 测试与需求追踪：每次交付完整验证

### 11.1 测试分组与判定

| 组 | 必测场景 | 阶段 |
|---|---|---|
| OF-BOOT | 能力缺失、初始化在线/离线、目录、仅动画订阅、服务重启重建 | T0 fixtures，T4真服务 |
| OF-CHAT | 请求/历史/绑定、NDJSON、长度/转义、空/错/断流、取消和预算 | T0/T1/T3/T4 |
| OF-SNAPSHOT | 默认值清空、引用、大小、seq四分支、202/未知缓存、旧回执、恢复 | T0/T4/T6 |
| OF-APPROVAL | 输入摘要、菜单ID、允许/拒绝/过期/断线/竞争/重开/重启 | T2/T3/T5/T6 |
| OF-UI | action矩阵、作用域、新id回应、分页、过期、领取丢失窗口 | T0/T5/T6 |
| OF-SCENE | v2、参与者、team一致、命令ID、转发非完成、白板拒绕过 | T0/T5 |
| HOST-RECOVERY | 同Session冲突、四容量、可取消锁、进程树、磁盘/崩溃 | T1/T2/T3/T6/T8 |
| SDK-COMPAT | 固定SDK类型、真实HTTP/SSE、Basic/目录/审批/abort/查询 | T0/T7/T8 |
| LEGACY | 默认CLI/离线/sidecar、旧数据、默认feature无axum | 每个交付 |
| GOVERNANCE | CHG缺失/越界/风险降级、异常表格、中文路径、未挂载测试 | T0后每个交付 |

Schema通过只证明形状；202、队列、浏览器应用、动画和断线语义必须真服务/真浏览器验证。Office已有integration-e2e当前依赖Docker，不能据此宣称Windows原生通过。Host外部runner在隔离副本运行未修改的Office生产服务，通过公开协议和浏览器测试；确需改Office runner按R4先审。

Cargo不递归发现tests/features或regressions子目录，必须顶层features.rs/regressions.rs或明确test target；根是virtual workspace，根tests下随意新增.rs不会自动运行。优先扩展现有测试，私有不变量inline，追踪表指到实际执行目标，不为了专用目录暴露实现。

每个可交付变更（包括文档）运行当前阶段所有已实现套件和门禁；开发中定向测试不代替交付完整测试。失败修复后重新完整验证最终版本。尚未实现阶段明确待验，不拿fixture冒充其成功。不能机械要求每次按键运行全套，但提交审核的版本必须有完整证据。

### 11.2 基础验证命令与平台

以下均在OpenPaw根执行，工具链与依赖安装也需记录：

~~~text
cargo fmt --all -- --check
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo build --locked -p openpaw-cli --features engine-host,sidecar
cargo +1.88.0 check --locked --workspace --all-targets
cargo +1.88.0 check --locked --workspace --all-targets --all-features
cargo tree --locked -p openpaw-cli --edges normal --prefix none
cargo run --locked -p xtask -- lines
cargo run --locked -p xtask -- scrub --check
cargo run --locked -p xtask -- snapshot --check
cargo run --locked -p xtask -- golden
cargo run --locked -p xtask -- telemetry
cargo run --locked -p xtask -- stability
cargo run --locked -p xtask -- smoke-offline
cargo deny --workspace check
~~~

默认CLI tree必须无axum；依赖公告/许可检查不关闭或忽略。契约目录tests/engine-host-contracts使用Node24.19.0：

~~~text
npm ci --ignore-scripts --no-audit --no-fund
npm run prepare:office
npm run check
npm test
~~~

全部21来源核验使用prepare-office.mjs --office-source <Office checkout>读取固定git对象；测试快照输出在OpenPaw隔离副本，不写Office。治理命令、真实argv和证据入口见 governance/README.md、governance/commands.json 和 docs/verification/CHG-2026-0001/验证记录.md；登记不等于执行通过。

Linux本地完整验证，Windows原生CI和阶段所需真实浏览器/进程/锁验证；本机无Windows时明确未运行，不能复用历史CI证明当前提交。既有macOS历史不作当前门禁或验收目标；T0已从ci/nightly/release移除macOS矩阵；历史平台记录保留，不承担当前适配承诺。

Office若经Rx批准修改，除Host全量外在隔离工作区执行其npm ci、npm test、npm run lint、npm run build:full、packages:check/smoke、test:integration:types、test:integration、在线test:service及新增用例；保留既有生产验收，另补原生Node入口。未批准时不在Office工作区安装/生成/改测试以便“通过”。

证据记录提交/脏工作区说明、平台、工具版本、命令、起止/时长、退出码、测试数/失败/ignored、CI与日志位置。已存在ignored说明来源，新必需测试不能skip。CI artifact保存大日志，仓库摘要脱敏。当前head的CI是当前审核证据，历史绿灯不覆盖新提交；不不断提交“已通过”文字制造自指循环。

## 12. CHG、PR与单人交接

### 12.1 初期阶段流程

OpenPaw分支建议docs/engine-host-t0、fix/engine-host-t1、fix/engine-host-t2、feat/engine-host-t3…t7、test/engine-host-t8，均从最新已合入的merge/ai-ofice-react-overagent创建，不从未批准前序分支堆叠，不强推集成分支。

~~~mermaid
flowchart LR
    A[前一Tx当前提交批准并合入] --> B[本Tx主CHG与范围]
    B --> C[复用审核/实现/文档同步]
    C --> D[完整Linux与Windows验证]
    D --> E[对应PR Ready]
    E --> F{非作者人工审核}
    F -->|修订后重测重审| C
    F -->|批准并合入| G[下一Tx]
~~~

一Tx一主CHG，内部D编号独立逻辑提交/测试/注释；新功能关联REQ/DES，不为每个辅助函数新开CHG。开发可Draft，等待只修本PR，不开始下一Tx。DCO Signed-off-by按现有要求。作者不自批、不自动合入；CI成功不代表批准。合入采用既有约定squash，保留Tx/CHG/D/R/PR关联。

### 12.2 每个PR的交接包

1. 阶段目标、实际产物、未完成项；计划/已实现/已验收分开。
2. REQ/DES/CHG、D/R及契约关联，新增模块必要性和源码审核依据。
3. 复用/延展/修改/新增实际清单；必改/条件允许/禁止路径与实际diff。
4. 数据/公共接口/依赖/平台兼容影响，文档影响及同步章节；不涉及给理由。
5. Linux/Windows完整命令、退出码、提交/工具链、证据和未执行原因。
6. 可复现配置/请求/步骤，错误/取消/恢复及回退方式。
7. 下一Tx可直接调用的接口、fixtures、已知边界。
8. 人工审核来源、最终批准提交和合入记录；Office另含获批Rx版本/范围和关联PR。

公共批准信息、CHG状态、索引应一致；不能自填“用户批准”或使用占位负责人伪造门禁。相关源代码、测试和文档实质变化后重测并请求当前版本审核。

### 12.3 初期结束后

T8与最终约定范围验收完成后，登记初期结束基线。后续按CHG驱动需求/设计/实施/测试/正常PR，不再强制T编号或上一Tx串行门禁；仍遵循受保护范围、人审、DCO、完整测试和文档同步。Office任何修改仍先Rx审核，项目进入维护期不解除该限制。未批准扩展保持独立提案，不转成默认授权。

## 13. 完成口径与不能宣称的能力

- 单机、单用户、配置授权工作区；不扩成多租户平台。
- 核心执行/会话/工具最大复用；通用缺陷独立修复，不用Host复制补丁版本。
- 默认四办公室/四Run是本项目配置，不是Office协议人物数、办公室上限或多Agent承诺。
- 技能/目标/Todo首期真实查询及已有能力，不新增调度/技能写管理/多Agent编排。
- Office v1保留180秒、刷新停止、无严格turn幂等/续接、UI领取无重投、动画无HTTP完成回执的边界。
- R1/R2/R4只有明确人工批准后才可实施；批准前不修改Office任何文件；R3本期不做。
- v1限制不伪报Office缺陷；真实缺陷也先Rx，不能先改后补审批。
- 固定SDK通过不等于OpenWork桌面通过；T0/feature构建通过不等于Host运行或Windows进程验收。
- 未完成必需项不能标完成；未批准可选扩展标未实施。所有交付结论以当前版本的实际证据和人工审核为准。
