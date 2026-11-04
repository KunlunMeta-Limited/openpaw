# Office 修改申请集中台账

2026-10-28 最新运行升级入口：用户要求“全部都按照最新的版本运行”。Office来源已冻结b33e206c4decf5c111e78fc134506079e97dd765，26来源仅office-integration.md内容变化，无Schema/后端协议变化；按原CHG0007 §12.106切换8765并复用当前最新已验证Host。实际运行状态、PID、二进制/前端SHA、备份、回退及真实浏览器结论以 `/home/cheng/.openpaw/verification/office-latest-runtime-20261028/deployment.json` 和 `closure.json` 为准，不能仅凭目标版本认定部署成功。Office所有活跃Rx仍未实施，DeepSeek不新增付费验收；下方原时点版本记录保留历史语境。


更新：2026-10-28。归档方：OpenPaw。Office主目录已按用户指令原样快进`b33e206c4decf5c111e78fc134506079e97dd765`；OpenPaw协议冻结与8765实际Office部署仍为`ff9f6b7d6447665be459f077109c6224789e5f3c`，新版审核与隔离验证不等于已切换正式服务。未加入自定义Office修改，两份用户架构编辑保留。此前基线及结论保留原时点，最新复审见文末。活跃Rx仍暂缓，未批准、未实施；R16按用户最新明确指令归档，不纳入当前需求。

**当前决定：Office 修改暂缓集中审核，所有活跃申请均未批准，不得实施。**用户原话：“office的修改审核暂时都不允许通过，这个申请可以统一放在一个文档中保存，合适时机会统一审核”。批准 OpenPaw 文档、CHG、总计划、交付或 PR，均不能作为 Office 修改许可；旧版本讨论也不能覆盖本决定。

## 1. 用途与维护规则

这是唯一现行 Office 修改申请入口，集中保存缺陷修复、必要扩展、功能新增与工程调整的原因、方案和影响。**登记不等于申请已达到可批准程度，更不等于批准。**尚未确定准确文件/接口/迁移的候选保留待补材料，不能直接实施。

- Office 源码、文档、配置、测试、锁文件、生成文件和格式化均在暂缓范围内；不能用“只改测试/配置”绕过。
- 当前产品优先由 `openpaw-engine-host` 适配既有官方协议；能在 Host 内解决就不申请改 Office，通用缺陷修回所属 OpenPaw 模块。
- 本台账只保存在 OpenPaw，不向 Office 工作区写文件、不创建 Office 实施分支或 PR，不复制密钥、浏览器档案或用户配置。
- 一项申请保留独立 Rx、版本、状态和决定；集中审核不表示整份文档一次批准全部，也不能以某项获批扩大其他项范围。
- 历史 R1 保留在 [r1-chat.md](r1-chat.md) 供追溯；该附件不再作为并行现行审批单。R5 的撤回记录保留，不复用其编号。
- 后续只读研究可完善本台账；涉及扩大 OpenPaw 变更范围仍更新对应主 CHG。任何 Office 实施必须等用户恢复审核并明确批准准确单项版本与文件范围，再开始修改。

协议依据：[integration/README.md](https://github.com/KunlunMeta-Limited/ai-office-react/blob/d150b96ff0fff59ece869da7e3dc6067c1b066e3/integration/README.md) 及其四文档、公共 Schema、SDK/测试入口。现有接口细则和产品任务以 [master 第4—7节、第15节](master.md#office-function-coverage) 为准；不能将候选接口写成已存在的上游协议。

## 2. 总览

| 编号/版本 | 内容 | 当前状态 | 首期无修改方案/阶段 | 审核成熟度 |
|---|---|---|---|---|
| R1 / 历史设计2026-10-20 | 原长任务四 URL 扩展 | 被新版官方覆盖的部分撤回，不实施 | T4官方send/history/resume/cancel；T6恢复加固 | 历史附件，不恢复旧方案 |
| R2 / 候选2026-10-22 | ui.next领取ACK/租约/重投 | 暂缓集中审核，未批准，不实施 | T5单poller立即记录，说明领取丢失窗口 | 接口/租约状态/副作用幂等方案待补 |
| R3 / 范围记录2026-10-22 | 新侧栏、富文本与管理页 | 不在首期，未批准，不实施 | T5现有detail-v1、分页与白板 | 无当前必要性，不作为阻塞 |
| R4 / 候选2026-10-22 | Office原生测试启动器调整 | 暂缓集中审核，未批准，不实施 | OpenPaw外部runner启动原Node/浏览器 | 未证明必须改Office；默认不用 |
| R5 / 历史范围记录 | 旧多会话/会话列表扩展 | 上游已支持而撤回，不实施 | T4快照conversations[]及官方ChatWorkspace | 不复活旧接口 |
| R6 / 上游覆盖2026-10-23 | 服务器模式页面创建办公室 | d150b96已提供协议，原Office申请撤回 | T5按0006 r3适配office.create及预授权槽 | 不修改Office；Host已随T5 PR #11合入，故障组合仍T6 |
| R7 / 候选2026-10-22 | 旧单会话浏览器草稿迁移 | 暂缓集中审核，未批准，不实施 | 先保留旧档/备份，明确新旧入口 | 旧版本样本/存储键/迁移方法待核实 |
| R8 / 上游部分覆盖2026-10-23 | 断网后自动无感续接 | 180秒限制已由上游移除；自动恢复仍暂缓、未批准 | 现有“重新同步”→history/resume | 仅保留自动恢复候选，不再申请移除已取消的总时限 |
| R9 / 候选2026-10-22 | 完整账本累计统计/口径提示 | 暂缓集中审核，未批准，不实施 | 当前会话有界任务计数、详情完整历史 | 先确定统计对象，不预设改Schema |
| R10 / 候选2026-10-22 | 产物下载/附件体验 | 暂缓集中审核，未批准，不实施 | detail-v1授权文本预览，真实文件留工作区 | 先核Host-only替代，不预设必须改Office |
| R11 / 2026-10-23-r1-link | 白板无产物链接时不显示“查看任务拆解” | 暂缓集中审核，未批准，不实施 | Host可清空说明；模板默认链接仍存在，不能声称已隐藏 | 原因/最小文件/兼容和验收已列，可在恢复集中审核时评审 |
| R12 / 2026-10-23-r1-type | 白板墙面与阅读器字体/对比度 | 暂缓集中审核，未批准，不实施 | Host短目标/摘要/步骤；完整信息详情 | 有实图/最小候选文件；视觉参数待统一审核确认 |
| R13 / 2026-10-23-r1-menu | 真实接入人物详情与Demo原位菜单并存 | 暂缓集中审核，未批准，不实施 | Host先实现准确detail-v1成员详情；场景控制页保留 | 原位菜单呈现/行为需原型确认，不默认开放业务状态按钮 |
| R14 / 上游覆盖2026-10-23-fa6 | 原76d774a显式focus测试失败 | 撤回本地修改候选；原失败保留，不是本地修复 | fa6ae1f原测试863/863；现冻结/部署均fa6ae1f | 上游测试明确ready前idle、ready后typing；完整产品工作态共存仍须验收 |
| R15 / 2026-10-23-r1-doc | 聊天文档过期180秒总时限 | 暂缓集中审核，未批准，不实施 | OpenPaw明确实现/文档差异，不恢复旧R1 | 仅文档候选；参数依据已查，准确文字待审核 |
| R16 / 历史候选2026-10-23 | 动画到达与业务执行绑定的回执 | 按用户最新明确指令归档，不纳入当前需求 | 交接事实可靠展示；成员执行仍按后端队列，不以动画作为条件 | 旧分析保留历史，不再请求批准此执行绑定 |
| R17 / 候选2026-10-23-traffic | 默认空闲闲聊与业务交接通道互阻 | 暂缓集中审核，未批准，不实施 | 使用现有“关闭空闲闲聊”开关演示；保留默认开启失败证据 | 真实第五次visit为TRAFFIC_BLOCKED；不能保证全部动画完成 |
| R19 / 候选2026-10-24-completed | 首页任务流隐藏完成卡 | 暂缓集中审核，未批准，不实施 | Host保留真实completed及任务中心/成员详情 | 需产品决定首页保留范围/过滤器 |
| R20 / 候选2026-10-24-employee-panel | AI员工面板无详情点击 | 暂缓集中审核，未批准，不实施 | 现有侧栏/场景人物及Host detail-v1 | 复用employee.inspect，不新增私有协议 |
| R21 / 勘误2026-10-26 | 删除后标签保留 | 撤回Office申请：Host D91修复后真实UI通过 | 同一同步器的可信UI入口；Office未改 | 原始false回执与准确74条浏览器记录见本节 |
| R18 / 候选2026-10-24-mobile-notice | 手机尺寸决策侧边提示/倒计时离开视口 | 暂缓集中审核，未批准，不实施 | 桌面原用例通过；不以Host改快照或延长期限掩盖布局问题 | 原静态Demo一次失败、独立重跑两次同点失败；最小CSS/滚动方案仍待定位 |
| R22 / 候选2026-10-26-progress | 人物、白板与任务流统一业务进度 | 暂缓集中审核，未批准，不实施 | Host D95输出同源事实；现Office仍使用两种独立活动曲线 | 已定位准确文件，不能由Host伪造时间百分比规避 |
| R23 / 候选2026-10-26-task-controls | 按实际能力提供任务操作入口 | 暂缓集中审核，未批准，不实施 | 未实现暂停/改派/手工完成时明确拒绝，不篡改Run终态 | 先定义后端调度语义；前端能力按钮设计待审核 |
| R24 / 候选2026-10-27-busy-color | 左侧成员在线与忙碌颜色分离 | 暂缓集中审核，未批准，不实施 | online继续表示在线；Host提供真实任务归属 | 复用现有任务/状态，见文末准确最小文件 |
| R25 / 候选2026-10-27-task-selection | 历史失败/阻塞压过新完成任务的人物主卡 | 暂缓集中审核，未批准，不实施 | 保留真实历史失败，不伪报完成；新Run事实独立 | 原上游选择函数已复现；当前任务选择与历史入口需区分 |
| R26 / 候选2026-10-28-member-drawer | 新成员布局窄屏抽屉入口被聊天遮挡 | 暂缓集中审核，未批准，不实施 | 桌面成员入口继续可用；不改Host数据掩盖布局问题 | 390×844真实Chromium重复复现；准确证据、最小候选和待确认根因见文末 |
| R29 / 2026-10-29-r1-office-delete | 删除办公室入口、正式删除协议及目录清理 | 用户撤回，取消申请，不实施 | 改为仅扩本机Host预授权总容量8→32，不改Office | 原设计保留追溯；撤回决定见文末R29及原CHG0007 §12.118 |

“撤回”“不在首期”和“暂缓审核”含义不同，均不是已批准；不得为完成里程碑把这些项统一改成已完成。

## 3. 分项原因、方向与影响

### R1：原长任务四 URL 扩展（历史）

旧基线 b221a44 缺少稳定轮次/续接语义，曾提出独立Run创建、查询、订阅和取消四 URL。现固定02dbac4已提供同一聊天POST中的send/history/resume/cancel；继续实施旧设计会重复协议并迫使无必要的跨仓修改。

决定：被覆盖部分撤回，旧文档仅保留原始论证。T4实现官方四操作，T6验证180秒后重新同步、刷新和断线；自动无感恢复未由此证明，另见R8。无Office实施文件、无Office PR，也不因旧讨论中的“批准”恢复修改许可。

### R2：UI操作领取可靠性

- 原因：官方ui.next领取后不重投；HTTP响应丢失、领取后Host崩溃会丢失一次操作。不是违反当前契约的缺陷。
- 当前替代：单poller，收到立即持久记录，短查询有界并发；用户可再次发起读取。仍不能保证端到端恰好一次，副作用不能盲目重放。
- 候选方向：显式claim/ACK、租约与有限重投，同一逻辑interaction稳定关联，区分展示请求过期和业务执行状态。协议形式、租约/重启存储和过期竞争尚未定稿，当前不注册任何新操作。
- 影响：服务端队列、请求容量、60秒期限、重复动作处理和协议兼容；Host仍需幂等，不能让重投造成重复删除/写入。Linux/Windows共享协议测试。
- 候选定位：packages/protocol/src/office-integration.ts；server/src中现有UI队列/领取实现（准确文件待只读核定）；integration/docs/office-integration.md、message-relay.md及相应服务/集成测试。
- 审核前验证/回退：领取响应丢失、ACK丢失、租约到期、双消费者、服务重启/过期/重复副作用；保留旧客户端模式或明确版本升级策略，回退前处理未确认请求。
- 是否阻塞：不阻塞当前明确边界内的T5；若未来要求可靠交付每一次UI操作，再提交完整可审核版本。

### R3：侧栏、富文本与新管理页

原因：可能改善复杂内容浏览，但当前详情、分页、任务中心和白板已能承载首期产品。方向尚无经用户确认的需求；不能为页面美化扩大权限或新增HTML执行。

首期不实施；准确候选文件/原型/信息架构/安全及存档影响待有具体需求再写。当前无发布或回退变更，使用现有detail-v1。不是缺少该页面就阻塞T5。

### R4：Office原生测试启动器

原因：部分上游integration-e2e以Docker为默认入口，本项目要原生Linux/Windows。现已在OpenPaw使用外部runner构建并启动原Office Node服务，尚未证明必须改Office。

候选方向仅在外部runner确实无法覆盖正式行为时考虑；定位tests/integration-e2e的启动配置/文档及必要脚本，准确文件待问题复现后填写。不能更改断言或跳过原用例来取得绿灯。验证应是相同公开HTTP/浏览器旅程在双平台运行；回退使用原上游启动器。当前不阻塞、无Office改动。

### R5：旧多会话扩展（撤回）

新版已提供conversations[]、会话标签/搜索/草稿、各会话任务/成员/白板、history/resume和删除action。Host应适配，不新建另一套Office会话目录。

被覆盖部分撤回；T4接目录与聊天，T5实现删除后端，T6加固故障。旧单会话草稿迁移不能推定已完成，归R7，不复用R5批准或编号。

### R6：服务器模式页面创建办公室（上游已覆盖，撤回原申请）

**2026-10-23现行结论：**d150b96新增官方`office.create`空作用域动作及`ui.respond target=office`，原来“官方没有接口”的理由已过期。撤回Office工程修改申请，不实施旧候选。用户批准的0006 r3在OpenPaw通过预授权槽、真实Session、持久幂等与完整快照适配它；这不代表Office修改批准；Host现已随T5 PR #11交付合入，T6仍补故障/容量组合。以下保留2026-10-22申请原始理由。

- 原因：App中DemoOfficeDirectory会注入createTeam，而ConnectedOfficeWorkspace服务器入口未注入creator；官方七操作没有创建办公室请求。现有组件不证明远端创建已接通。
- 首期替代：用户在Host TOML配置授权工作区根、稳定teamId和名称，由offices.initialize建立目录。聊天只能引用允许范围，不能输入名称就创建任意磁盘目录。
- 候选方向：用户选择/申请已授权工作区→Host验证根/所有权/容量→可靠保存配置/绑定→初始化完整目录→页面确认。创建接口形式、目录选择方式、失败/重复/回收语义未定稿，不编造上游已存在接口。
- 影响：浏览器入口、目录协议、Host配置/持久化、权限和部署。多页面并发创建须幂等；Linux/Windows路径和文件所有权需验证，不要求WSL。
- 候选定位：apps/web/src/ui/ConnectedOfficeWorkspace.tsx、dashboard/officeDirectory.ts、ui/OfficeEmptyState.tsx及创建表单；若需新消息，还涉及packages/protocol与server。准确最小文件/字段待方案定稿，不能据目录名宽泛授权。
- 审核前验证/回退：无授权根拒绝、重名/重ID/容量、创建成功确认丢失、重启恢复、路径越界、其他页面目录更新；禁用入口后已有配置办公室仍可使用。
- 是否阻塞：不阻塞配置式首期产品；当前未批准，不能承诺页面创建可用。

### R7：旧单会话草稿迁移

- 原因：新版IndexedDB和CAS能保护当前档案，不等于自动读入旧版本单会话存档。尚未用真实旧存档证明具体丢失场景，不称已确诊的Office缺陷。
- 当前替代：保留原浏览器数据/备份，明确旧版本入口；不清空旧库、不用新档案覆盖旧记录。
- 候选方向：核对旧key/Schema及historyKey→只读旧档→原子写新格式→校验→标记幂等迁移。与服务端已接受turn/历史的合并策略须单独定义，不能把旧草稿自动提交执行。
- 候选定位：apps/web/src/chat/persistence.ts、ChatWorkspace.ts及对应存档Schema/测试；具体旧版本/键/迁移文件待只读核实。
- 影响与验证：多页CAS、容量、读取失败、部分迁移、重复升级、回退旧版、隐私；测试使用脱敏合成旧样本，不复制用户配置到docs。失败保留旧件，不静默丢消息。
- 是否阻塞：不阻塞新建当前格式数据的T4/T5；如需升级现有旧用户，先完成迁移提案审核。

### R8：自动无感长任务恢复（总时限部分已由上游覆盖）

**2026-10-23现行结论：**d150b96已移除send/resume的180秒接收总时限，并移除转发连接的自动流超时；history/cancel控制请求仍有15秒期限。原申请不再包含移除180秒总时限。网络断开后的自动无感恢复仍是未批准候选；Host继续提供官方history/resume，不替Office伪造自动恢复。以下是旧基线的历史理由。

- 原因：ChatSession.receive默认180000ms，超时中止当前接收并显示重新同步；服务器保留Run且支持resume，不等于前端会自动无限续接。
- 当前替代：页面点击“重新同步”触发history→resume；刷新接回原Run，显式停止才cancel。T6验证>180秒同Run、无重复文本/工具。
- 候选方向：沿官方四操作实现有界自动重新同步/退避，独立连接超时与Run预算；退出页面detach、点击停止cancel、失败给可操作提示。不是恢复R1旧四URL。
- 候选定位：apps/web/src/chat/ChatSession.ts、ChatWorkspace.ts及真实聊天组件/相关测试，是否需要sources.ts/存档字段待最小设计；不预设修改Fastify或公共Schema。
- 影响与验证：取消竞争、网络抖动、离线、慢消费者、多页、重试风暴、Provider失败/Host重启、旧存档；验收实际>180秒，不用心跳假装突破总接收时限。回退恢复手动重新同步，保留turn和历史。
- 是否阻塞：不阻塞明确手动恢复路径的基础范围；若用户要求无人值守连续展示，再审核完整版本。

### R9：累计统计及范围提示

- 原因：Office dashboardMetrics从当前会话投影任务数组统计“今日完成”等；Host仅保留最新Run和有界工具，主/子任务也不是请求数量。不能把投影计数当全部历史累计。
- 当前替代：T5明确当前可见业务任务口径和裁剪范围，完整历史走详情；system未采集则显示未提供，不制造监控值。
- 候选方向：先决定要统计Run、工具、业务任务还是人数，以及时区/完成时间/取消的口径；再选仅补范围提示还是新增后端汇总。禁止通过虚构任务撑累计值。
- 候选定位：apps/web/src/dashboard/selectors.ts、ui/OfficeShell.tsx；确需独立统计字段时才考虑packages/protocol/src/dashboard.ts和快照校验/文档/测试。准确字段与最小文件尚未定稿。
- 影响与验证：有界投影/分页、跨日/时区、重启不改完成时间、Run与工具去重、Schema兼容、旧数据无汇总时的降级；可选字段或纯提示需验证旧服务不受影响。
- 是否阻塞：全账本统计非首期。若发现当前展示连已定义口径都不符，先登记真实缺陷、说明影响，不能自行改Office。

### R10：下载与附件体验

- 原因：当前detail-v1是纯文本，文件预览不等于已提供下载/上传或模型附件支持。白板link只接合法HTTP(S)地址，本地绝对路径不是可用链接。
- 当前替代：授权根内≤256KiB普通文本详情预览，实际文件留工作区；无合法链接时留空，明确未支持附件，不静默丢用户输入。
- 候选方向：先检查可否仅在Host提供经授权的产物访问并复用现有合法链接/Markdown；能做到则撤出Office修改范围，按未来OpenPaw CHG处理。确需上传控件/受控下载行为，再形成单项Office提案。
- 候选定位：文件入口组件、ChatMarkdown/ChatSession与对应Schema仅为研究线索，不是允许修改集合；准确范围须区分“只下载”与“模型附件输入”，不捆绑授权。
- 影响与验证：授权作用域、凭证/URL过期、链接泄漏、文件内容类型/大小、符号链接/重解析点、文件变化、恶意内容、旧浏览器兼容；回退文本预览，不删除真实产物。
- 是否阻塞：不阻塞文本产物首期；不新增未经批准的Host下载接口或Office控件。

<a id="r11-empty-link"></a>

### R11：白板空链接不自动生成“查看任务拆解”

版本：2026-10-23-r1-link；状态：**未批准，暂缓集中审核，不实施**。这是用户明确的产品呈现要求，不把既有模板行为误称为协议缺陷；关联T5主CHG-2026-0006 r2第9.9/10.4节。

- 现状/证据：固定02dbac4的`packages/scene-office/src/taskBoard.ts::renderTaskBoard`在`data.link.url`为空时无条件输出`href="#whiteboard-steps"`和“查看任务拆解”；墙面和放大白板共用该函数。8765真实截图`artifacts/t5-product-review-20261023-a/8765-detail-timeout.png`与原快照均可核对。用户要求只显示任务目标、处理进度及必要子任务。
- Host无修改替代：清空description/note/技术tagline能去掉两段说明；目标/进度/子任务也可通过既有字段提供。但link.url为空仍触发模板回退。填假URL、无意义的#链接、空白字符、样式注入或改打包产物都不接受；首期保留默认链接并明确边界。
- 最小方案：只在存在合法非空link.url时显示产物链接；无URL时不渲染回退锚点。目标、步骤和进度仍直接显示，不新增按钮、接口、协议字段、状态或存储。合法HTTP(S)产物链接仍显示原label并保留noopener/noreferrer。
- 准确候选范围：`packages/scene-office/src/taskBoard.ts`修改上述link分支；`tests/whiteboardTaskBoard.test.mjs`扩空链接/真实链接及paper/wall共用输出断言；`integration/docs/office-integration.md`同步白板link展示说明。只允许这三文件，若真实审核发现其他必要路径，先修订本Rx，不自行扩展。
- 影响：旧后端省略link或发送空URL时不再看到“查看任务拆解”；原步骤仍可见，数据/存档/seq/鉴权/聊天/部署均不变。属于模板默认呈现变化，须人工认可，不能说零兼容影响。Linux/Windows使用同一前端构建；不适配macOS，不因本Rx要求当前补Windows原生环境。
- 验收：原Office全量测试、类型检查、构建；空URL/省略link时两种白板均无该锚点，合法产物链接仍能打开，未知/恶意URL依旧被原Schema拒绝；目标/步骤/进度及下载JSON不变。真实浏览器核对墙面、放大和导出。禁止通过CSS藏链接或删除旧测试放行。
- 回退：仅还原该模板分支、对应测试和说明；不迁移或删除任何用户数据。配套Host采用空link的官方合法格式，回退后只是重新出现原默认链接。
- 当前决定：没有用户批准版本/对象/批准人时间/Office PR；不得实施。R11不阻止OpenPaw独立修任务投影和统一服务，但未获批时不能宣称用户要求的“完全无该入口”已完成。


<a id="r12-whiteboard-type"></a>

### R12：白板可读性（新版真实截图与Demo均需核对）

版本2026-10-23-r1-type；状态未批准、暂缓集中审核，不实施。用户明确“字体看不清”；8765实图见0007产品差距审查。`taskBoard.ts`墙面使用26px基准再缩放到场景，说明文字为.85em/#778390，标题/步骤为nowrap省略；Reader虽20px，墙面原图仍较小。Host协议没有字体/颜色/CSS字段，不能在文本中注入样式。

- 无修改替代：Host提供短目标/业务标题、简短N/M进度，长路径和全文放详情；可以减少裁切，不能修字体/对比度。先按0007 P05做这部分，不能因此关闭R12。
- 最小候选范围：`packages/scene-office/src/taskBoard.ts`调整墙面字号/字重/对比度/换行及空间分配；`apps/web/src/components/WhiteboardReader.css`对应放大/窄屏布局；`tests/whiteboardTaskBoard.test.mjs`保留转义/链接/完整数据断言并增加显示结构覆盖。若像素核验需要现有端到端入口，仅在准确评审时补文件，不借此宽泛授权。
- 方案方向：优先保留固定模板/纯数据，目标/进度/3步骤在1440和常见笔记本尺寸可读、放大完整，360窄屏不溢出；颜色对比和正常/等待/失败不可仅靠颜色识别。具体像素/对比值需统一审核原型后冻结，不本轮擅定新产品规范。
- 影响/回退：只呈现，不变HTTP、seq、存档、权限或Host模型上下文。旧合法数据仍可渲染；截图基线会变。原完整Office测试/构建和真实墙面/Reader/窄屏截图通过；回退原模板/CSS，不改用户布局或白板数据。与R11默认链接是两项独立决定。

<a id="r13-character-menu"></a>

### R13：服务器模式人物点击保留Demo原位交互

版本2026-10-23-r1-menu；状态未批准、暂缓集中审核，不实施。最新Demo真实点击王明出现“互动…/开始工作/进入思考/暂时空闲/表情动作”；Connected的`useOfficeSceneController`在onInspect存在时立即调用并return，因此不建立menu。这是当前模式差异，不由Host补几个字段就能恢复。

- 无修改替代：OpenPaw用官方actor.inspect/employee.inspect返回该成员真实detail-v1，必要视觉控制在现“场景控制”页操作。可以修scope_denied/详情缺口，无需本申请；但不是Demo同位置菜单，不能说已达到完全一致。
- 候选方向：真实接入人物菜单保留身份与“业务详情”入口，视觉互动/表情复用原运行时。任务状态只从Host事实读取，不能把Demo“开始工作/暂时空闲”按钮当真实Agent启动/暂停，也不能覆盖任务状态或绕过审批。菜单和modal的层次/键盘焦点/关闭行为需先出原型供集中评审。
- 准确研究定位：`apps/web/src/controller/useOfficeSceneController.ts`点击分支；`apps/web/src/ui/OfficeSceneControls.tsx`原菜单及按钮；`apps/web/src/components/RuntimeWorkspace.tsx`onInspect装配；测试参考`tests/officeScene.test.mjs`及现浏览器入口。`OfficeUiPanel.tsx`是否必改尚不确定，保留条件而非笼统全UI授权。
- 影响/验收：默认Demo不变，真实模式明确本地视觉与真实业务边界；不新增Office公开HTTP。验证六人物、打开详情/返回、互动对象选择、工作中状态不被伪改、焦点与窄屏；完整原测试/构建。回退原onInspect详情分支，不删除Host成员数据。当前材料仍缺最终交互原型，不能直接批准实施。

<a id="r14-focus-regression"></a>

### R14：上游76d774a显式focus未维持工作态

**fa6ae1f复核决定（2026-10-23）：撤回本地Office修改候选。**最新原测试863/863、完整构建退出0；characterWork原两失败由上游测试等待phase.ready后再验证typing覆盖，并新增未ready时idle的断言。运行时本次未改，不能把这说成本轮修了生产工作状态，也不据此证明所有端到端动画；原失败和研究定位保留如下。新证据为`/home/cheng/.cache/t6-office-update-20261023-fa6ae1f/validation.json`及command-3.log。没有Office实施批准或本地修复提交。

版本2026-10-23-r1-focus；状态未批准、暂缓集中审核，不实施。干净最新版`npm run build:full`退出0；`npm test`792项790通过、2失败；再次单跑`characterWork.test.mjs`仍2/4，旧d150b96同机同Node为4/4。失败actual core.idle/expected office.type，对应“六人focus持续工作”和“复用render snapshot”。原日志位于0007审查证据根latest-test/latest-character-work-recheck/baseline-character-work。

- 无修改替代：暂不升级8765固定版本，Host继续原actor.presentation真实任务反馈，不新增focus来假修；新咖啡更新可读审查，但不称新Office全套通过。`remote working status`原用例在新版通过，不说所有工作动画均失效。
- 调查方向：`packages/runtime/src/SceneRuntime.ts::readActivePhases`把ready改成phaseStarted&&phaseReady，需核对没有动作阶段/重用快照时ready契约，再核对`packages/scene-office/src/pixi/index.ts`及实际工作投影入口。准确最小修复文件待证明，不能先回退全部咖啡提交。
- 验收/禁止：保留`tests/characterWork.test.mjs`原断言，并覆盖咖啡就绪/坐下/行走不打字；Office完整792项及真实focus/coffee/任务反馈共存通过。不将期望改core.idle、不跳过测试、不在Host伪造阶段。旧World/存档兼容；若runtime ready语义变化影响其他插件，要重新列准确兼容影响后评审。
- 回退：若修复后来获批但失败，仅回退准确修复提交/使用原已验证Office构建；不回滚Host会话或用户工作区。当前仍是可复现缺陷候选，不冒称已验证根因/修复。

<a id="r15-chat-timeout-doc"></a>

### R15：聊天接入文档的180秒旧约束

版本2026-10-23-r1-doc；状态未批准、暂缓集中审核，不实施。`integration/docs/chat-integration.md`仍写“一次流连接最长180秒”，而d150b96及最新`ChatSession.receive`和server转发已移除固定流总时限，history/cancel仍15秒控制期限。相同version=1.0文档与实现有差异，容易让集成人员实现错误的180秒重连。

候选只改上述文档，逐项列输入/回复/单行限制与控制超时，不误删Host自己的流空闲/总Run预算。无修改替代是本项目master和契约README明确源码差异；Office文档修订也须批准，不能用“只是文字”越权。审核时对照源码/190秒真实旅程，验证文档示例与Schema，原测试不变；回退原文档即可，无数据影响。与R8自动无感恢复分别记录，不靠本申请新增自动恢复。

<a id="r16-handoff-receipt"></a>

### R16：严格视觉交接确认（仅条件候选）

当前官方message-relay明确不提供动画执行结果/回执，snapshot自动task.assigned首次不重播、快完成会跳过visit。OpenPaw能保存真实派发、发稳定事件并录制已发生的拜访，不能承诺“每次人走到后才允许子Agent执行”。若用户只要求可观察演示，优先0007现协议方案，不需要修改Office；只有确认需要强顺序/可靠回执才继续此候选。

研究定位为浏览器SceneRuntime活动结果→受保护同源反馈→原Fastify关联→Host接收；准确路由/Schema/文件尚未确定，本轮不发明已有接口。必须先定义指定页面所有者、多页冲突、动画失败/离线/超时、重复ACK、取消、断网与业务执行一次性，再谈实现。回执只确认视觉事实，不能成为工具授权，也不能使已执行副作用回滚。未补齐准确协议/权限/范围/迁移及故障测试前，此项不成熟、不可实施；不是目前人物详情/多Agent业务可用的前置条件。

## 4. 不应误登记成 Office 必改的事项

| 事项 | 当前归属与安排 |
|---|---|
| 完整conversation.delete | 现官方协议已有；T5.3补Host共享删除、存储清理、墓碑/恢复和结果映射，不先提改Office |
| 审批后台通知、队列、会话待审数 | Office已有；T5.1适配并真实验收，不能重新造前端 |
| 任务重跑、暂停、改派、手工完成 | 当前缺引擎业务语义，先按后续OpenPaw CHG定义；T5按钮准确说明不支持，不伪改快照 |
| 地图、会议、家具、focus/emote、插件面板 | 已有原生能力，T8共存回归；不宣称多Agent或远程插件安装 |
| D37进程取消稳定性 | OpenPaw核心缺陷，开放shell前修原模块/回归；不转嫁Office |
| 窗口/浏览器/网络临时异常 | 先收集真实复现与协议证据，不能只凭一次画面异常认定Office缺陷 |

后续如真实验收发现Office违约，使用下一个空闲Rx编号追加到本文件，不另起散落申请；仅凭“必要”不能跳过准确方案和统一人工审核。

## 5. 集中审核时的单项清单

每个准备送审的Rx补齐以下内容；候选现未补齐的保持不可实施：

1. 准确Rx版本、Office冻结提交与必要性：真实复现或明确用户故事，期望/实际行为及证据。
2. 不改Office的替代、缺点及为何仍需修改；能Host-only解决就撤出Office范围。
3. 最小逐文件/符号清单：现状、原因、具体方法、复用/延展/修改/新增、允许/禁止路径。
4. 协议、状态/取消、数据/迁移、权限、部署、Linux/Windows和旧客户端影响。
5. 完整验证与回退方案、配套Host范围、是否阻塞哪项承诺。
6. 用户明确批准的单项版本/准确对象/文件集合、真实人和时间精度；实施授权与交付验收/平台PR审核分开。

收到“统一审核”指令后，先只读核对最新评审基线并补成熟提案；不得将本候选台账直接当允许修改清单。获批后按对应Tx/CHG实施，Office PR指向Office自己的评审基线，所有必要PR审核合入后才算该跨仓能力完成。

## 6. 决策履历

| 日期/精度 | 来源与决定 | 影响范围 | 批准记录 |
|---|---|---|---|
| 2026-10-22（日期） | 用户明确Office修改暂不允许通过、统一文档保存，合适时集中审核 | Office全部文件；现有/新申请均不得实施 | 没有任何当前Office修改批准；本轮只授权OpenPaw文档归档 |

| 2026-10-23（日期） | 用户“批准r3”，仅批准OpenPaw的0006 r3增量；上游d150b96覆盖创建与聊天总时限部分 | R6撤回，R8缩小候选范围，R11仍暂缓 | 没有新增Office修改批准 |

无Office批准人/批准时间/Office PR/合入提交可登记；不得为填满字段编造记录。将来批准时逐项追加，不覆盖本轮暂缓决定与旧撤回事实。


2026-10-23集中复检：新增R12—R16及上游76d774a观察记录；所有活跃项仍暂缓未批准。可先实现的Host角色/详情/委派按0007单独准确方案审核，不要求用户现在逐项批Office。R6 Host适配已随PR#11合入；R11模板在新旧基线均未变，不能撤回。R2/R7/R9/R10/R13/R16仍缺部分协议/样本/原型/口径材料，保留不成熟，不把“写了申请”视为方案完善或已解决。


### R17：默认空闲闲聊与业务交接通道互阻（候选，未批准）

- 实测对象：Office冻结d150b96＋OpenPaw准确947a7f8；真实Chromium先完成原单角色完整旅程/190秒长流，再运行一主五子。2026-10-23原始完整记录：/home/cheng/.cache/t6-agents-20261023-a/full/report.json；产品report.json、trace.zip、video及t6-616-audit-arrived.json位于同根product。不是最新76d774a的focus回归R14，也不是需要动画ACK的R16。
- 实际行为：前四个子角色有真实office.visit完成；第五个data-agent交接command为failed/TRAFFIC_BLOCKED。该时点office-lead停在(11,7)，app-agent位于(11,8)且motion等待(11,7)，空闲返岗/闲聊仍在产生排队和失败。五次任务派发真实存在，不能把forwarded或accepted当动画完成。此为已观察互阻事实，最小根因尚需隔离验证。
- 不改Office的替代：原产品界面已有“关闭空闲闲聊”按钮。只在独立多角色演示开始前通过真实界面关闭并记录该前置条件，不伪造运行时、不改源码/打包产物、不自动重播旧交接。后续关闭状态通过也只证明该配置，不关闭默认开启的失败；业务工具结果与动画结果保持独立。
- 候选调查/范围：packages/scene-office/src/core/OfficeIdleCoffee.ts的before/interrupt/return队列；packages/scene-office/src/core/idleCoffee.ts、office.visit及原交通规划/让行逻辑；原tests/idleCoffee.test.mjs与集成浏览器入口。当前仍是研究定位，未确定最小文件集合，不能直接实施。优先复用已有资源协调和导航，不绕开碰撞或永久禁用咖啡功能。
- 待冻结方案：业务活动到达时取消/收敛空闲返回不得和业务访客形成相向占格等待；需双向让行或安全重规划，具体算法必须先复现、定位和评审。不能将Host工具延迟绑定到未知动画回执，也不能用重发业务Run解决视觉失败。
- 影响/验收：六人物默认闲聊开启/关闭、连续五交接、多次空闲返回、布局变更/重载、活动取消与窄通道；保留原交通/碰撞/资源/超时断言和Office完整测试，真实浏览器导出每次command终态/截图录像与业务Run对照。故障不改变真实工具/审批结果；旧部署无迁移，回退准确Office提交。
- 状态：仅登记，用户统一审核前Office零改动。本项阻塞“默认闲聊开启仍保证五次可见交接”的承诺；不阻塞Host独立子Run、人物详情和在已说明配置下的验证。


## 7. fa6ae1f上游复审（2026-10-23，非Office修改授权）

Office已安全快进到fa6ae1f78efd5e9d11ea878e29c83b3ec5f1c518，用户两份原架构文档改动保留；上游追加章节与原用户格式patch合并证据见t6-office-update-20261023-fa6ae1f/report.json。冻结及8765部署仍d150b96。独立干净Office原863测试全通过，build:full退出0；26冻结资料10项发生变化。

- R11默认白板链接、R12字体/对比、R13原位人物菜单、R15过期180秒文档仍可由新源码确认未覆盖，继续暂缓。新版成员布局让场景面积变大，不等于修了墙面字体。
- R14旧focus失败由上游就绪测试覆盖，撤回本地修复候选；旧76d失败证据不删除。新上游没有赋予Host新的动画执行回执。
- R16严格到达确认仍无外部可靠协议；电商Demo用浏览器本地CommerceSceneQueue收到到达后推进，只证明本地模拟能力，不能拿来给Host工具授权或伪造远端ACK。
- R17默认闲聊与业务交接互阻本轮没有新完整复验，不关闭；R2领取无ACK、R7迁移、R8自动恢复、R9统计、R10附件等继续按各自成熟度保留。
- 成员布局、布局切换、employee.create都是上游已提供的功能，不新增“必须改Office”的Rx。优先OpenPaw复用快照和可信角色适配，准确增量在0007第12.3节；Office仍不改。


## 8. 2026-10-24 Demo再审与当前待审核事项

本轮远端main仍fa6ae1f，无新上游覆盖以下候选；没有实施Office修改。请注意本单仍是集中申请入口，不是已经批准清单。Host成员生命周期动态D61可以通过现有events完成，通用clarify/任务控制/空工位成员也应先做Host方案，不新增Office申请掩盖后端缺口。

| 项目 | 目前成熟度及仍需补齐什么 | 对本轮OpenPaw工作的影响 |
| --- | --- | --- |
| R11 / R12 | 原候选范围保留：默认空link、白板字体/对比。集中评审前按同一fa6ae1f补桌面/窄屏墙面与放大对照，确定最小字重/字号/颜色与基线 | 本轮只能提供短业务文案/真实进度，不能替Office改CSS或藏链接 |
| R13 | 源码再次确认classic连接模式onInspect直接return，不出Demo原位菜单；成员布局优先选中成员是另一入口。还需最终确认“成员资料/视觉互动”入口结构和触屏关闭/焦点/越会话影响 | 业务详情仍由Host detail-v1交付；原位菜单融合未实现，不以后端新增一个字段谎称可用 |
| R15 | chat-integration仍写180秒，真实实现/190秒验收已移除总时限；仅文档更正候选，不要重新增加超时接口 | 无需更改Host协议，保留差异记录 |
| R16 | 仅条件候选：仍缺远端到达/多页主执行者/过期离线协议，无法直接实施 | 本地CommerceSceneQueue可读运行时phase，远端Host不能读浏览器内存；不能复制这个机制假造ACK |
| R17 | 默认闲聊互阻原失败证据保留；需最小冲突优先级/取消与回工位设计及既有寻路占用回归 | 使用原公开关闭闲聊开关取得的5次visit只代表该配置，不声称默认已修 |
| R2 / R8 | UI领取无ACK及自动重连仍候选；需明确租约/重复投递与持久action幂等、重连/错误重试策略 | Host保持事实可靠、未知不授权、手动history/resume；不承诺端到端恰好一次/自动恢复 |
| R7 / R9 / R10 | 旧草稿真实样本、累计统计口径、附件/下载授权与Provider类型均未齐 | 优先Host-only评估；不能因Demo有数字/文件文字就扩大权限 |
| R1 / R5 / R6 / R14 | 上游已经覆盖的旧申请仍撤回，不复活 | 无Office修改批准，也无需本轮重复申请 |

首发前新草稿404暂未新增Rx：官方明确“加号未发送不建后端业务”，目前Office在UI入队前拒绝未知conversation；后续先确定产品是否接受首发后成员可查，再审核显式草稿生命周期。不能让history读接口产生Session或删scope校验。新增动态Agent也不能依赖绕过6工位容量。


### R18：手机尺寸决策侧边提示与倒计时不在视口（候选，未实施）

- 复现：准确Office fa6ae1f、原未修改的integration-e2e/office.spec.ts第551—579行、390×844。Demo进入第二决策后快进10秒，notice存在且倒计时继续变化，但第567行`toBeInViewport`失败（viewport ratio 0）。原9条8通过/1失败，独立重复两次仍失败；不能把它写成全Demo通过，也不据此推断桌面当前工具审批不可用。
- 原始证据：`/home/cheng/.cache/t6-demo-audit-20261024/{static-demo.log,results.json,test-results,repeat-mobile.log,repeat-results.json,repeat-test-results}`；三次失败截图/trace保留。快进时钟只验证期限UI，不代表真实等待60秒或模型执行。
- 影响：手机上后台团队仍有未决问题，但提示/剩余时间可能需滚动才能发现；这属于Office呈现/滚动问题，Host不能通过更改审批时间或假回答来修复。
- 候选方向：先对照`.team-decision-notice`的定位容器、页面自动滚动和窄屏布局，确认期望是固定可见提示还是页面内提示，再冻结准确最小文件/样式及原测试。当前仅有复现，不预先批准改变sticky/fixed或删测试断言；桌面和真实连接模式也须复验。
- 未来验收：同一原用例三次均通过、桌面/手机/缩放/键盘/到期/跨团队返回无覆盖聊天和焦点丢失，期限仍与问题一致；原Office全测试。回退准确上游文件，无Host数据迁移。本轮不改Office，不请求立即批准。


### R19：完成任务在首页被隐藏（候选，未实施）

真实Chromium审计准确Office fa6ae1f/Host7fa：原h同会话8张真实完成卡100%，首页0卡，任务中心8卡可全部展开。源码apps/web/src/ui/OfficeShell.tsx的flowTasks在overview明确filter(status!==completed)，因此不是Host完成任务丢失。证据/home/cheng/.cache/t6-ui-closure-20261024-a/before/{report.json,overview.png,section-任务中心.png,trace.zip}。候选最小改OfficeShell.tsx，提供“进行中/全部”或保留近期完成项的有界首页列表及对应UI回归；不改Schema/进度语义/业务状态。审核需先确定首页容量/排序及空态；验收完成/失败/等待/新追问/切换会话/刷新和窄屏都准确。回退仅前端过滤/样式，无数据迁移。未批准，不能用Host伪报running规避。

### R20：AI员工面板条目没有点击详情（候选，未实施）

相同真实审计中AI员工页6行.runtime-employee，点击王明未发/api/pawoffice/ui、无详情dialog；OfficeShell.tsx该行仅div，无onClick，而经典侧栏.runtime-person正常派发employee.inspect。候选仅在原OfficeShell.tsx复用现有inspection回调，以button/键盘可达方式打开同一官方详情；显示最近完成摘要需确认产品文案/范围，不能把空闲说成从未工作。验收六行/鼠标键盘/触屏/多会话/禁用/删除和错误状态与原侧栏一致，无新后端协议。Host本轮新增的任务详情导航可复用，不能据此声称AI员工页已可点。证据before/section-AI 员工.png及report.json/trace。未批准、不改Office；回退原UI，无持久影响。

### R21：删除后标签保留（Host D91修复后真实UI通过，撤回Office修改申请）

**2026-10-26证据勘误。**复查`/home/cheng/.cache/openpaw-d89-final-20261026/product/trace.zip`的准确`conversation.delete`响应：HTTP200封套中的业务结果实际为`success:false`、`projection_deferred：Office UI动作优先，延后投影扫描`。原文“conversation.delete已返回成功”不成立；仅快照不再包含会话不足以证明UI确认已成功。已确认根因是OpenPaw删除处理持有UI优先计数，又调用后台投影入口导致自身让行，归原CHG0007 §12.60 D91，仅修Host并重跑原真实删除旅程。此时Office保留标签符合其等待后端确认的约定。撤回本轮必须修改Office的结论；若Host真实成功回执后仍复现再补准确证据，不能提前认定前端仍有缺陷。Office所有文件保持未修改，下面原候选保留为历史依据。


**当前复验结论。**冻结Office不变、D91二进制`f80229f1…`的完整真实产品旅程已通过74条记录，`reviewFindings=[]`、零pageerror；原删除步骤同时核对标签消失、后端历史不可读、旧send/resume/cancel不能复活及工作区产物不删除。准确报告`/home/cheng/.cache/openpaw-d89-final-20261026/product-d91-corrected/report.json`，截图`27-t5-deleted-conversation-artifact-preserved.png`，trace同目录。撤回本R21的Office修改申请；问题是Host自身投影优先规则缺陷，不需要修改Office。以下原前端提案仅保留误判历史，不作为活跃待审项。

- **真实复现**：冻结 Office `fa6ae1f` 与当前 OpenPaw Host 产品套件中，`conversation.delete` 已返回成功，随后 `office.read` 已不再返回该 conversation；但 Office 浏览器仍保留被删除的当前 tab，15 秒内没有切换到剩余会话或隐藏该 tab，导致真实产品验收超时。原始报告与截图保留在 `/tmp/openpaw-product-d80-20261025-d/`。
- **必要性与影响**：用户会看到已删除的聊天标签，刷新/切换时可能继续进入旧本地归档；这破坏“删除聊天和草稿”的公开承诺，也会让任务/历史显示与 Host 事实不一致。Host 不应伪造快照、重复删除或通过隐藏后端数据解决页面状态。
- **候选最小范围**：只审查 `apps/web/src/chat/ChatWorkspace.ts` 的 `remove/sync` 状态转换及 `OfficeConversations.tsx` 的当前 tab 选择；在后端删除成功且本地归档删除完成后，原子移除 tab，并选择相邻会话或空状态。同步期间不能把短暂的远端旧目录重新插回当前 tab；删除失败必须保留原 tab并展示错误。复用现有 `deletedIds`、`selectedId`、`requestDelete` 和 IndexedDB 保存，不增协议和 Host 路由。
- **验收与回退**：真实 Host 删除成功/失败/运行中删除、单会话与多会话、刷新/断线/重复点击、键盘/窄屏均验证；确认后端历史返回404/非200、UI tab、selectedId、IndexedDB和剩余会话一致。回退只还原上述前端状态变更，无数据迁移。
- **状态与授权**：Office 工程、测试、配置和锁文件保持只读；用户已要求 Office 修改统一审核，本 Rx 未批准。OpenPaw D80 只修 CLI 配置根和测试夹具，不依赖本 Rx。


### R22：人物和白板的活动曲线被当成业务进度（候选，未实施）

**场景与原因。**本轮用户看到同一任务人物与墙面数值不同，步骤已经完成仍99%。源码冻结fa6ae1f：`apps/web/src/dashboard/actorTasks.ts`将所有人物任务强制设为`progressMode: activity`；`packages/renderer-pixi/src/ui/TaskBadge.ts`按12秒时间常数逼近99%，而`packages/scene-office/src/whiteboardProgress.ts`按90秒常数增长并在整板未completed时将单步100也截为99%。这不是三个后端进度源不同，不能通过Host加速刷快照解决。首页任务流还按`OfficeShell.tsx`过滤completed，属原R19；任务中心原组件能显示实际task.progress数值。

**最小候选方案与代码。**actorTasks对真实接入使用既有数值进度模式，未提供进度时使用无数字活动指示；TaskBadge保留Demo原活动模式兼容，数值模式只平滑插值真实报告值，不自行增长。WhiteboardProgress保留每步可信100，不因父任务运行把已完成步骤压到99；真实接入不使用自动增长，Demo可显式启用。OfficeShell若需要新增条形进度，可复用现百分比并标明未知；R19完成卡展示规则另审核，不能通过保持假running逃避过滤。候选测试在上述三个模块原测试和OfficeShell测试中验证0/33/66/100、未知、失败、等待、父运行子完成、会话切换和刷新。不改Schema、引擎预算或授权。

**影响与回退。**真实接入进度可能长时间不变，但表示已核验事实；活动文本另行更新。Office视觉变化需用户批准，回退还原前端选择/绘制策略，无数据迁移。当前Office完全未改，不宣称人物与墙面数字已一致。D95仅修后端事实来源、实际活动和动画队列错误影响业务终态。

### R23：任务操作应与真实执行能力匹配（候选，未实施）

**场景。**运行态任务卡提供“开始/阻塞/完成/改派”，但Host只有Run开始、取消及结果核验，没有安全暂停、恢复、改派或人工完成工作流。点击后明确拒绝是当前能力缺口，并非操作已实现。不得仅修改快照颜色伪造操作成功。

**方向与边界。**先在OpenPaw后续CHG定义哪些任务可人工完成（独立人工待办或真正Agent执行）、暂停安全点、资源清理、恢复/重放幂等、改派的上下文和权限，再明确接口。Office候选在`apps/web/src/ui/OfficeShell.tsx`和原任务详情/动作组件按已有或经审核新增能力声明隐藏或禁用不可用操作，说明实际可用的聊天停止。不把cancel当pause，不把已终态Run再标为执行中。需要改协议能力声明时，另列对应公共Schema和Fastify转发/测试准确范围后再审核；当前尚不能声称此提案可直接编码。读任务详情仍按已有task.inspect提供，不受影响。

**验收与回退。**实际支持的每个按钮均核对Run/工具/资源/审批事实，重复点击、取消竞争、跨会话、重启不得重复副作用；未支持按钮不可让用户等到超时。修改仅在人工批准准确方案后实施，当前暂缓；回退前端入口无持久迁移。


### 2026-10-26 上游观察：306698e（未部署，不构成本地修改批准）

只读fetch确认远端main已由fa6ae1f推进至306698ebeb6ce2638fbe09808facc38325b15c01，11文件变化；主Office工作区/运行仍fa6ae1f，原两份用户架构修改保留，未checkout/构建主工程，不改变26份冻结资料。此前“无新提交”为2026-10-24时点。d73fa14在ChatTranscript.tsx移除按expanded禁用虚拟列表，修复收起清空行后同范围不重绘；新增多宽度反复展开/草稿/切会话测试。882a7bb/fb68790在OfficeChat.tsx/CSS与OfficeDecisionPanel.css保留展开聊天时的审批面板并修菜单裁剪/手机层级。006a635补Demo等待chat.ready、恢复preview main与删除后停止，不引入真实后端任务操作。

“已有历史但重新展开空白”有此独立前端原因；现场不同Host home导致旧会话不存在是另一问题。当前旧版五次真实展开测试均显示2气泡/1336字，未重现漏绘，不能据源码差异断言用户现场已复现或修好。新上游可能覆盖R18部分手机样式，须新版原手机旅程实测后再定，不提前撤回申请。本次差异没有修改actorTasks/TaskBadge/whiteboardProgress或远端ACK/任务控制，R16/R17/R19/R22/R23均继续未批准、未实施。后续先审新基线、隔离副本完整验证与运行切换，再决定升级；不得夹带自定义Office修改。外部完整diff及研究记录位于`/home/cheng/.cache/openpaw-user-closure-20261026/office-upstream.diff`和`office-upstream-review.md`。

### 2026-10-27上游ff9f6b7复审（自定义Office修改仍未批准）

主目录已按用户要求原样快进上游，891原测试、构建与集成类型通过；聊天多页合并/恢复和展开修复已更新，R18相关审批样式需真实页面复验，不据源码提前关闭。新增白板summary/detail由Host适配，改善点击后阅读完整业务清单；不改变R22人物12秒/白板90秒活动曲线，也不解决R19首页过滤完成卡。R16部分基础已具备：2a3db3b让远端单条消息FIFO等待前一动作结束，避免后到远端工作态抢先；但DashboardSceneBridge直接提交working与visit不进入该远端FIFO，Host子Run也不等待浏览器，HTTP明确不含播放结果。严格“走到再执行”仍需ACK/页面所有者/取消/离线协议设计，不能撤销R16。

### R24：左侧成员在线色与忙碌色分离（候选，未实施）

2026-10-29 b33e206精确复核：用户要求包括人物卡、任务流及侧栏的工作黄色，不能只改左侧。`packages/renderer-pixi/src/ui/TaskBadge.ts`的COLORS.running=0x36b57e（绿）、blocked=0xe1a12d（黄）、idle=0x8b948f（灰），且activity进度条复用running；`apps/web/src/components/RuntimeWorkspace.css`的running为蓝色；`apps/web/src/ui/MemberOfficeShell.tsx`按employee.online显示绿/灰。Host正确传running也不能改变这些常量，原Schema拒绝任意color。将来最小范围需同时统一这些渲染/样式并验证工作、等待、空闲和终态，保留文字语义及可访问性；无需改Host真实状态或新增业务执行协议。当前仅统计、未批准、Office未改，不能通过伪报blocked/离线换色。

**场景与原因。**ff9f6b7的`apps/web/src/ui/OfficeShell.tsx:83`仍只用employee.online选runtime-presence.online，`apps/web/src/components/RuntimeWorkspace.css:28`固定绿色；不表示空闲，Host不能把忙碌人员伪设离线。用户要求有任务时黄色。

**最小方案与位置。**复用dashboard员工/任务负责人关联，OfficeShell根据当前活动任务派生busy/waiting，并在RuntimeWorkspace.css增加黄色及可读说明；在线/离线保留原语义。优先真实running/blocked任务，旧失败和已完成不使人员永久忙碌；切换会话后的当前任务与人物卡同源。若需要跨会话总忙碌应另明确目录数据范围，不凭当前会话猜整个团队。无需新增后端字段或协议；测试扩原界面/选择器及真实浏览器，覆盖无任务、运行、审批、终态、多任务、切换、离线和色盲可访问说明。

**影响与回退。**只改左侧显示，不改Agent执行或工具权限；回退原两前端文件，无数据迁移。准确基线ff9f6b7；当前仅候选、未批准，OpenPaw D98不冒充此项已经实现。

2026-10-27真实网页补证：六成员侧栏在运行中102—221ms，完成后六Pixi/六侧栏均已点击并得到对应成员详情，无scope_denied。新白板详情实际鼠标打开，后端任务清单/已完成100%可读，R12阅读器部分由上游与Host适配改善；墙面字体不据此宣称修复。第一轮真实主/五子与保存最终收敛100%，但运行中截图明确人物/墙面/任务流存在不同数值，R22继续有效；成员有任务仍绿色，R24继续有效。默认空闲闲聊保留开启，本轮未再次出现TRAFFIC_BLOCKED不等于充分证明旧R17已修。新FIFO不能解决Dashboard先设working和无ACK的R16。准确六项复审、截图和完整命令/真实模型报告统一由 `/home/cheng/.cache/openpaw-office-upstream-20261027/closure.json` 引用，后续如有失败按原报告登记，不以此段预填长追问或全量通过。


### R25：旧轮失败覆盖新轮完成的人物主卡（候选，未实施）

2026-10-28 17:14真实新会话补证：bf2ded93-457c-4bb3-8505-c09e9a6579ca 中，陈书完整档案为009修订已核验100%，同屏侧栏却选007旧失败25%；17:16再查已完成011正文，仍被旧失败覆盖。刘市完整档案显示008正在修订57%，有最近实际读取/来源核验/编辑结果。截图、真实点击和trace见 `/home/cheng/.openpaw/verification/live-stall-r19-20261028/{陈书.png,刘市.png,browser.json,trace.zip}`。这次是新的在途主Run，不是上一会话stream_idle超时；不改旧失败事实、不修改Office。

**场景与原因。**同一会话先有失败/待继续任务，再成功完成下一轮；后端保留真实历史任务，不能抹掉失败。ff9f6b7原`apps/web/src/dashboard/actorTasks.ts:4`的priority为running>blocked>failed>queued>completed，`:6`先按该优先级、再时间排序，`:11`每人物仅取一张；因此旧失败必定压过新的completed，旧blocked也会一直占头顶。这是“新答复完成、人物仍旧失败”的另一独立前端原因，不等于后端应把旧失败改成功。与R19首页隐藏completed叠加时更容易被理解为整个新任务失败。

**准确证据。**本轮只读运行原上游`officeActorPrimaryTasks`，同一app-agent的旧失败20%（00:00）与新完成100%（01:00）输入，实际选中old-failed；原源码未变，输出`/home/cheng/.cache/openpaw-office-upstream-20261027/live/task-priority-probe.json`。该证据是准确原函数反例，不包装成真实网页已通过；真实新轮结束后的截图/任务事实另见本轮closure.json。旧轮真实provider_output_limit仍应保留，不以此申请隐藏业务失败。

**候选实现与范围。**最小复用actorTasks.ts的选择器：当前主卡优先最近真实任务更新，再在同一轮/相同时间内用状态排序；已结束的历史失败/blocked不永久遮住新轮交付。历史问题继续在任务中心/成员历史可读，清楚显示历史与当前。扩原actorTasks.test.ts覆盖旧失败→新完成、旧等待→新运行/完成、同人多项、不同角色、刷新和会话切换；与R19一起真实UI验收。若产品需要严格按Run分组，应另定义官方轮次范围，不能让Office解析Host私有任务ID来猜归属。时间优先策略对多任务同时进行的显示取舍须人审，当前不直接实施。

**影响、回退与状态。**仅影响人物主卡/展示排序，不改Run、审批、文件、模型或真实失败事实；回退原选择器/测试，无迁移。Office所有自定义变更仍未批准，R25为新增待统一审查申请；Host D100—D102不能替代此项。

### 2026-10-28上游b33e206复审与适配清单

用户本轮只要求更新仓库和审核适配。主Office已从ff9f6b7原样快进b33e206c4decf5c111e78fc134506079e97dd765；2提交、9文件。两份用户架构改动已备份，并逐字节验证三方合并预期；未加入任何Office自定义修改。公共Schema、server、chat、依赖锁均未改，不新增HTTP/DTO/持久语义。当前冻结和8765实际Office仍为ff9f6b7，不能拿前一版测试冒充新版；原上游及真实浏览器结果以本轮closure和各report为准。

| 更新与真实场景 | OpenPaw是否需要新能力 | 复用/待适配方向 | Office申请结论 |
|---|---|---|---|
| 六人物头像常驻；当前会话只有部分成员或没有绑定时仍能选灰点人物 | 不需要补造六个业务成员或新增Agent；Host已有六固定可信角色 | 使用真实sceneActorId绑定；未绑定完整档案由上游禁用，不能发送虚构employee ID。补稀疏/未绑定及会话切换兼容旅程 | R13/R20原请求不因六头像可选即全部完成；原位菜单/AI员工页本轮未改 |
| 顶部办公室名称、六工位、当前会话运行/完成数 | 无需新指标上报接口 | 继续workspace.name及当前conversations[].tasks；快照只是有界列表，不能声称全历史或今日累计 | R9边界仍保留；新版成员布局明确当前会话口径有改善 |
| 顶部“活跃成员”和绿点按同人物任意online:true去重 | 不需要新字段，但存在产品语义须确认 | Host当前online表示可信角色启用/可用，并非正在执行任务，因此空闲时6/6仍符合当前投递；不擅自改成假离线。若要忙碌人数/黄色，按实际running/blocked任务派生展示另审 | R24继续待审；新版只有绿/灰，不解决忙碌黄色需求 |
| 新顶部当前任务按running→blocked→queued→failed取首项 | 无法靠Host将旧失败改成功修正显示 | 保留真实历史；现有memberOverview/actorTasks与新顶部选择器都须区分当前和历史，按同一产品规则选择任务 | R25待审范围扩至MemberOfficeShell.tsx顶部选择器，及memberSelectors.ts/actorTasks.ts与原测试；先统一规则再动Office |
| 当前任务/白板/人物数值一致性 | Host已有真实progress与终态，不需要新增计时百分比 | 新成员顶部仍取task.progress，场景人物/墙面活动曲线对应文件本次未改；未知业务进度不能用时间估算冒充 | R22仍待审；R19经典布局过滤完成卡本次未改，不因新完成计数而撤回 |
| 窄屏抽屉、焦点返回和布局切换保留画布/草稿 | 无新增Host路由或权限 | 新版原生浏览器验证六头像、抽屉关闭焦点、真实成员详情、经典/成员切换；异常先区分上游与Host | R18需独立审批菜单窄屏复验；本次抽屉修复不代表旧决策菜单问题已关闭 |

**当前建议。**后端强制协议适配项为零；应补本轮新布局的兼容回归及冻结升级方案。当前先审核/隔离运行，不改Rust、sources.json或正式服务；需要启用新版时再将准确来源、固定文档和产品验收一起升级。现有六角色不等于六项任务同时运行，人物可见性也不授予动态Agent/文件/模型权限。未要求跨会话共享人物执行队列，不扩展这一语义。

**R16归档。**用户已明确“这个我提都没有提，你不要再说了，总是混在一起”。因此旧R16到达与业务执行绑定仅保留历史设计，不再作为活跃申请、验收前置或待批准需求。仍验证交接事实/队列可靠展示，但不以动画播放结果控制业务开工。R17默认闲聊与展示通道竞争是另一问题，不能混为此需求或自行判定修好。

### R26：新版成员布局窄屏抽屉入口遮挡（候选，未实施）

**原因与场景。**纯上游b33e206生产构建、原Node服务、真实Chromium，390×844，成员布局已有办公室和展开的聊天；正常鼠标点击“展开成员侧栏”重复超时，命中被聊天会话页签截获。画面仍可聊天，但无法通过此入口打开六人侧栏。初次点击中还观察到侧栏过渡层截获；不把强制点击或直接触发DOM事件当可用证明。此为新布局入口问题，与R18决策倒计时离屏分别登记，不能据此声称R18已复现或已解决。

**定位与最小候选。**MemberOfficeShell.tsx的member-scene-area中放置抽屉入口；MemberOfficeShell.css第128—150行将窄屏概况置于第二行、场景置于第三行，按钮绝对定位且z-index为8。真实截图/点击命中证实遮挡，但叠层和父容器裁剪的最终根因还需几何/计算样式定位，不在仅审核阶段虚构已修复原因。候选仅修改该CSS的窄屏入口布局/叠层及必要TSX容器，使入口在聊天展开/收起时均可直接点击；不简单无限提高z-index，避免穿透模态框。回归扩展原tests/integration-e2e/layouts.spec.ts，覆盖390×844、抽屉开关/焦点返回、聊天展开和收起、桌面与布局切换。若定位涉及其他文件，补准确范围后再审核。

**影响、验证与状态。**不修改HTTP、Schema、Host快照或Agent队列；无数据迁移。验证需要真实非force点击、命中元素、截图和焦点断言，不能只通过类型检查；回退对应CSS/TSX和测试即可。证据在office-upstream-audit-20261028下ui、ui-second及最终ui-final的report/截图/trace；中间脚本初始化错误与真实界面问题分别保留，最终结果由closure.json索引。本项未批准、未实施，等待用户统一审核Office申请。

**验证与证据。**原CHG0007 §12.105；`/home/cheng/.openpaw/verification/office-upstream-audit-20261028`保存upstream.diff、用户补丁/合并哈希、原npm命令退出码、专项ui/report.json及截图/trace、当前OpenPaw文档候选完整full/report.json与closure.json。浏览器的协议夹具、真实Host只读详情、正式DeepSeek分别标注；本轮不调用正式模型。活跃Rx仍全部暂缓，没有因仓库更新自动批准任何Office修改。


### R27：新交接遇已完成任务被跳过、提交失败后不重试（候选，未实施）

2026-10-29责任补充：原真实Chromium因果模拟已区分Host提前发布与Office独立过滤/遮挡消费。OpenPaw D123撤去固定展示定时器、改随实际接手发送稳定assignment；不能修复Office先seen后过滤终态、聊天遮挡时运行时仍推进且返回不补播。修后独立真实Host链路证据见`/home/cheng/.openpaw/verification/handoff-fix-r21-20261029`，以实际report/closure为准，不把事件投递或HTTP接受当动画播放。R27仍待统一审核，Office未改，不恢复已撤销的动画到达执行门禁。

**现场与原因。**b33e206原apps/web/src/dashboard/sceneBridge.ts同步task.assigned时先写seenEvents，再检查目标任务。新交接在快照到达前已经执行结束时，completed/failed分支直接continue；activity.start返回通道忙错误时也没有移除seen或保留待重试项。因此Host保存且投递assignment，不代表该浏览器实际播放交接。与R17闲聊占通道相关，但此项独立涵盖无闲聊、短任务已终态和失败不重试；与已归档R16无关，不以动画结果控制执行。2026-10-28正式新会话23条实际委派与23条assignment匹配、封套约46KB，不支持将本次缺动画归因64KB丢失。45秒真实网页观察到007配图交接成功，不能据一次成功认定全部交接可靠。

**候选方案与文件。**仅修改原sceneBridge.ts及原对应测试（具体测试文件先按仓库入口核对），并扩tests/integration-e2e原旅程：首次挂载/刷新/切换不补播历史；订阅期间新出现的真实assignment进入有界本地队列，按事件顺序逐项提交。任务已终态不应单独作为丢弃尚未播放交接的理由；可呈现“成果已交接”的动画文案，不改变completed/failed事实。同一事件使用稳定commandId，只有runtime接受后才标已处理，通道暂忙等待原运行时通知/有界退避，永久错误与超时明确显示并可查。不能无界重试、重放已接受动画，不能伪造后端接收/到达ACK，也不修改Agent开始执行时机。

**验证、影响和回退。**测短任务在首个新快照即completed、同人物FIFO、多个来源、暂忙后恢复、永久拒绝、重复快照、刷新/切换不补播。真实浏览器逐项对照assignment与实际visit接受/终态，不把HTTP200当动画播放。仅影响展示，既有协议/权限/账本不变，无迁移；回退Bridge及测试。当前只是集中候选，Office仍未批准、未实施。OpenPaw来源参数纠错只能减少错误主角色派发，不能代替此项。

**2026-11-03 多对话责任复核（D150；仍未批准、不实施）。**在未修改的5d21cf2生产构建中，用两个独立浏览器同时选A；分别通过带conversationId的Host式relay、正常task.assigned启动A动画，然后仅将第一个浏览器切到B。两种来源均复现：B任务标题和白板已正确切换，但A已接受的visit还在该页面运行。对该页面B作用域发送已知旧commandId的受控取消后，A残留立即停止、B工作展示恢复，另一个仍在A的浏览器保持原动画。两个浏览器的同一assignment生成不同的随机dashboard-* commandId，不能由Host凭同一事件推算；当前协议也没有浏览器选择通知或该页活动ID目录。不能据此要求Host取消同团队全部动画，否则会误伤另一个页面。

**责任和待审方向。**OfficeTeamSession.setConversation仅切数据源/dashboard/白板，未清理该页之前接受的旧作用域活动；OfficeIntegrationSession只过滤新接收命令，sceneBridge生成的活动缺少可用于切换清理的会话归属。现有业务快照不需要更改。候选须在原TeamSession/IntegrationSession/sceneBridge及对应测试中保存本页活动与会话的关联，选择变化时只取消本页旧会话仍在途/排队的业务动画；不能删除其他浏览器/会话的后台Run，不取消与会话无关的场景活动，不补播历史，不增加“动画结束才执行”门禁。此为R27范围补充统计，未授权Office修改。Host私有relay可减少部分残留，但现有接口无法完整覆盖浏览器随机创建的assignment活动，因此不能承诺OpenPaw单侧完整解决。此前447项委派未发现错scope只是已审范围结论，不证明Host所有路径均无缺陷。

**证据。**`/home/cheng/.openpaw/verification/conversation-scope-recheck-20261103/report.json`两种来源的六项因果断言均通过；relay/assignment子目录含两个浏览器前后导出快照、截图及trace，0模型发送、0页面异常，正式服务与Office工程均未改。详细复核见同目录《多对话责任与修复边界.md》。

### R28：人物头顶动作被旧任务和单行裁切遮住（候选，未实施）

2026-10-28补充区分后端缺口：新文章会话的公开快照接近64,000字节，Host旧compact_protected_details连running/queued/blocked的summary也删除，使详情显示“暂无说明”；原CHG0007 §12.110 D117仅修Host这项容量裁剪，保留有界真实动作，不能以此宣称本申请的旧卡选择或常态单行裁切已修。正式在途任务不强停，候选测试与正式部署必须分别核实。R25/R27/R28仍待统一人工审查，Office未改。

**现场与原因。**正式文章会话scene-live-before.png中，王明仍显示已归档003旧步骤，陈书旧blocked75优先占卡；这部分归R25与Host D115各自修复。李研当前任务的summary实际含“刚完成：保存article.md；正在组织下一步”等工具事实，但packages/renderer-pixi/src/ui/TaskBadge.ts非展开详情限16px单行，头顶标题仍为任务名称，动作目标被截断；compact模式直接隐藏detail。useOfficeSceneController.ts已将task.summary传入detail，不能声称后端根本没有动作字段，也不能用思考文本冒充工具进展。当前API没有真正执行工具时只能显示等待模型/组织下一步，不能伪造“正在搜索”。

**最小候选与范围。**先按R25统一当前任务选择规则；在原TaskBadge.ts改为常态可读的两行任务/动作展示，并区分当前正在执行、刚完成和等待，不让completed文案或百分比挤掉动作目标。保留合理字数限制和遮挡布局；compact时保留简短动作类别，点击/悬停读完整脱敏summary，不能通过显示原始参数泄露路径或凭据。必要联动ActorView.ts的避让/卡片高度；若actorTasks.ts需要拆分任务与动作选择则归同一候选范围，不加新Host DTO。扩原renderer/UI测试与真实画布旅程，核对长中文、文件名、域名、六人同时活动、缩放、遮挡、鼠标展开、读写结束和任务切换。

**影响、状态与证据。**仅调整前端阅读性和当前任务选择，无执行、授权或持久迁移；回退对应展示文件/测试。证据为`/home/cheng/.openpaw/verification/live-stall-r18-20261028/{scene-live-before.png,snapshot-observed.json,trace-observed.zip}`。源码路径/条件已核对，尚无Office修后验证，仍待用户统一审核，未实施。Host D115不冒称解决R25/R27/R28，真实动作已传输不等于当前头顶完整可读。


### 2026-10-29 R24/R25/R28收窄复核：OpenPaw单侧能力与剩余Office原因

用户明确批准《单侧适配复核与方案》的Host实现，Office仍全部暂缓。只读实验与原Office真实Chromium已证明：将实际工具短动作放入原task.title可显示头顶；将真实终态退出当前tasks后原页面自然待命。OpenPaw r22（原CHG0007 §12.113）增加可信activity_window开关，兼容旧配置，并复用原历史查询，不新增Office协议字段。因此R25“必须改Office才能空闲”、R28“必须改Office才能显示短实时动作”的笼统判断撤回，不能再要求先通过Rx才能实现这两项。Host候选验证与生产切换结果单独登记，不把旧协议实验冒充本轮实施通过。

仍需Office且本轮不实施的内容：

| 申请 | 真实场景与原因 | 将来需要改的范围/方向 | 当前状态 |
|---|---|---|---|
| R24及状态渲染 | 用户要求工作黄色、空闲绿色；原Schema拒绝employee/command.color，渲染常量写死；online不等于工作 | StatusLabel/TaskBadge状态颜色映射及MemberOfficeShell在线/忙碌语义，或先提供官方可配置映射；不能伪报blocked换色 | 暂缓，未改Office |
| R25剩余统计分离 | 终态退出当前窗口后，顶部完成数和成员关联数也减少，因为都统计同一tasks；如要人物空闲且保留准确累计数量，Host现有字段无法同时表达 | actorTasks/memberSelectors/MemberOfficeShell统一当前活动选择与历史统计口径；须官方契约与回归 | 暂缓，当前接受窗口计数取舍 |
| R28剩余展示增强 | 原Office已经支持title粗体+summary灰字；r25恢复该映射，撤回“双行目标/动作必须改Office”的旧判断。紧凑卡仍会隐藏灰字，普通卡只一行且按宽度截断，更多行/完整内容属于展示增强 | 仅如需要紧凑卡也显示说明或增加灰字行数，再改原TaskBadge布局并验证；不为现有双行新增协议。交接气泡重新读取稳定业务title | 增强仍暂缓；现有双行由OpenPaw修复，不改Office |
| R22 | Office部分视觉进度按活动曲线展示，不等于后端核验百分比；Host不应伪造业务数值迎合它 | 原人物/白板/任务视觉进度统一真实progress或明确分开视觉活动 | 暂缓，未声称本轮解决 |
| R30 / 候选2026-11-02 | Office展开/切换强刷及隐藏办公室同步放大历史请求 | 原OfficeChat/ChatSession/ChatWorkspace核对失效和可见性，详细候选见文末R30 | 暂缓，未批准，不实施；OpenPaw D149独立提速 |

R27相关澄清：用户已明确后台播放、错过不补播，这是期望行为，不能再把遮挡期间消费或不补播本身列成缺陷。Host执行态和Office交接动画是独立通道；本轮不新增“动画结束才开工”的约束。真正发送缺失、错误身份、实际拒绝和异常终态过滤仍须准确数据区分，不凭推测宣称Office过滤了本次动画。

2026-10-29 r25原会话调查补充：33条委派中19条未提供source_task_id；刘市009正文到李研009配图有明确来源，009压缩返修存在刘市到刘市，010写作结束时尚无后续委派。缺来源会回到主角色，同人不拜访自己，无后续任务不凭空制造交接。另捕获seq269在途、seq271已停止；停止后人物idle不能作为在途待命反例。源数据、实时HTTP/Chromium与专项结果见`/home/cheng/.openpaw/verification/ui-status-r25-20261029`，未复现的偶发状态不宣称修复。R27终态过滤/实际拒绝仍待审，Office不改。


### R29：删除办公室（2026-10-29-r1-office-delete，用户撤回，不实施）

**最终决定（2026-10-29）。**用户明确回复“不进行，把这个功能申请也取消掉吧，直接把空间的容量限制从8设置成32”。本申请取消，不再待审，不实现任何删除入口、协议或持久墓碑。替代操作仅增加OpenPaw本机预授权目录/槽，总容量32；现有办公室和对话全部保留，Office工程继续不改。对应原CHG0007 §12.118，以下是撤回前的历史设计，不是现行实施范围。

**需求与审核边界。**用户本轮要求“增加办公室删除操作”。本项对应原 CHG-2026-0007 §12.117 r26；只登记方案，不代表用户撤销“Office修改暂缓、逐项先审核”的既有决定。当前未修改Office任何文件，未删除用户办公室、会话或项目文件，未切换正式服务。

**实际依据与双方责任。**2026-10-29只读核对，Office本地HEAD与远端main均为`b33e206c4decf5c111e78fc134506079e97dd765`。`packages/protocol/src/office-integration.ts`仅有`office.create`、`conversation.delete`等动作，无`office.delete`；`integration/docs/office-integration.md`明确初始化保留请求未列出的办公室、不提供删除。`server/src/officeState.ts`只有list/get/apply，不能移除目录。因此仅删除Host会话、发空快照或省略初始化条目，都不能实现真正的办公室删除。已有`OfficeIntegrationSession.ts`接收`office.sync.complete`会移除目录外办公室、停止session、销毁runtime并更新缓存，此部分可直接复用，不新增删除SSE或动画指令。

**用户能看到的效果。**团队空间中每张办公室卡片增加独立的“删除办公室”操作，不嵌套button，不因点击删除而进入办公室。单办公室时也允许打开管理入口。二次确认明确显示办公室名称，并说明“停止该办公室的所有任务，删除其全部对话及运行记录；工作区里的文章、图片和其他文件保留；本操作不能撤销；预授权工作区槽不自动回收”。需输入办公室名称确认，取消不发送动作。提交后显示删除中、防重复点击；真实完成后卡片消失，当前办公室被删则选择剩余首个办公室；最后一个被删进入既有空状态，仍可新建到其他未使用预授权槽。失败/结果未知显示具体原因，不提前隐藏卡片，不把超时解释为已回滚。其他标签页通过同一目录同步删除，其他办公室不受影响。

**候选协议（尚非官方现有能力）。**不新增URL，不复用`conversation.delete`冒充办公室删除。沿现有UI/messages通道增加两个明确职责：

```json
{"id":"delete-operation-id","teamId":"office-id","conversationId":"","action":"office.delete"}
```

```json
{"type":"office.delete","teamId":"office-id","deletionId":"delete-operation-id"}
```

前者是浏览器确认后的团队级请求，经原`ui.next`交给Host；空conversationId表示整个办公室，不能要求至少保留一个对话。后者是Host已持久接受并完成实际运行清理后，使用原API Key调用messages进行服务端目录移除；不依赖浏览器在线或原UI请求仍有效。候选messages成功data为`{teamId,deletionId,status:"deleted"}`，纳入原严格请求/结果Schema、能力列表及文档；同删除重复提交幂等，同ID换目标返回冲突。浏览器请求未知办公室且没有已删记录时返回不存在；可信Host删除消息即使该实例尚未初始化该办公室，也须登记墓碑并幂等返回已移除，以支持Fastify重启恢复。普通浏览器不能直接调用带服务凭据的messages。

Office服务端删除缓存并登记本实例的删除墓碑、阻止旧快照重新初始化该ID，再广播原`office.sync.complete`完整目录。`ui.respond`仍复用`target:"result"`，仅在目录已删除且Host清理完成时成功；原交互已超时不撤销后台删除，重新同步仍能得到正确目录。与被删办公室有关的其他待处理UI/决策等待需明确结束，当前删除请求本身除外。不改变其他办公室的seq，不把删除转成全局清空。Office现有缓存不持久化，本实例墓碑不假称跨重启持久；Host保存权威墓碑，在Office重启后先补发删除、再同步存活目录，阻止旧配置复活办公室。

**OpenPaw必须同时完成的部分。**先检查Office具备候选删除消息能力，再登记持久删除意图并关闭该团队的新会话/新Run/新委派入口；枚举该团队真实Office会话及其owned子会话，复用原`delete_session`/`quiesce_session`/`purge_session`和审批失效机制，等待真实工具/进程清理，最后清除派生投影、发送删除消息并记录完成。不能只改`workspaces::active`隐藏页面而仍接受旧请求；不能取消其他团队或同物理根下独立CLI/SDK会话。错误和断线保留删除意图，重启只继续清理/同步，不重放模型或工具。准确Host文件与持久状态见原CHG。

**数据与容量边界。**不递归删除工作区目录，不删除原`office_created`事实、用户备份、TOML或.env。办公室删除会清除其对话/Run/委派等会话数据，保留最小审计和墓碑；操作不可撤销，失败中途也不能恢复成可运行。固定配置办公室同样受持久墓碑控制，不能重启重建。首次版本不复用旧teamId和物理根，避免旧请求误入新办公室或新办公室读取旧文件；因此删除不会自动解决`capacity_exceeded`，新建仍需尚未使用的预授权槽。安全回收容量需要另审“新身份与物理根代际隔离”方案，不默默扩大本项。完整备份只用于显式离线整体恢复，不能在其他任务运行时回灌旧数据库。

**准确Office候选文件及必要性。**以下只是审核范围，不代表已修改；同一职责沿原文件扩展，无额外Node Bridge或业务数据库。

| 文件 | 具体改动及必要性 |
|---|---|
| `packages/protocol/src/office-integration.ts` | 新增严格团队级动作、服务端删除请求及结果；删除作用域与会话动作分开，补结果Schema路由和能力列表 |
| `packages/protocol/src/office-ui.ts` | manifest登记候选`office.delete`，保持能力说明与Schema一致 |
| `server/src/officeState.ts` | 增加幂等目录移除及实例内墓碑，拒绝旧snapshot/initialize复活 |
| `server/src/app.ts` | messages删除鉴权/校验、目录移除/完整目录广播、删除UI不依赖conversation、成功回执的清理前置核对 |
| `server/src/officeUi.ts`、`server/src/officeRequests.ts` | 定向结束该团队其他等待交互/决策，保留删除操作自身及其他团队请求 |
| `apps/web/src/ui/team-switcher/model.ts` | 可选删除回调，不让离线Demo在未提供后端能力时假删除 |
| `apps/web/src/ui/team-switcher/TeamOverviewDialog.tsx` | 卡片独立删除入口、名称确认、busy及错误展示、键盘/取消处理 |
| `apps/web/src/ui/team-switcher/TeamOverview.css` | 删除入口/确认状态的可读与焦点样式，避免卡片点击冲突 |
| `apps/web/src/ui/TeamSwitcher.tsx` | 只有一个办公室时也能进入可删除的管理面板 |
| `apps/web/src/ui/ConnectedOfficeWorkspace.tsx` | 调用原requestOfficeUi，核对真实result，当前办公室删除后的选择回退 |
| `apps/web/src/application/OfficeIntegrationSession.ts`（条件） | 优先复用原sync.complete移除及持久缓存；仅若专项证明回执/目录竞态，增加按同一work队列等待目录收敛的方法，不加独立目录存储 |
| `tests/officeDeletion.test.mjs`（新增） | 删除Schema/服务端/墓碑/无浏览器/幂等/延迟快照/其他团队专项 |
| `tests/teamOverview.test.mjs`、`tests/officeIntegration.test.mjs`、`tests/officeContractSchema.test.mjs` | 扩原入口/单办公室管理/缓存与协议回归 |
| `tests/integration-e2e/run.mjs` | 真实Chromium创建隔离办公室、取消删除、删除当前/后台/最后办公室、多页刷新与重启验收 |
| `integration/docs/office-integration.md` | 正式定义删除动作、请求/结果、幂等/重试、文件保留及不自动回收容量，不再笼统声称不提供删除 |
| `integration/README.md`（条件） | 仅目录/消息摘要确需同步时更新链接和摘要，不改其他架构文档 |

**验证与兼容要求。**先用最新原版Office证实现有能力缺口，批准后补Schema和服务端红灯，再实现；运行Office原npm test、build:full、test:integration:types、test:integration及OpenPaw原27项完整验证。真实UI不得删用户现有数据：只创建专用临时办公室和文件哨兵，核对取消不删除、空办公室可删、多会话/父子Run/审批在途清理、其他办公室继续工作、文章/图片/用户备份仍在、单办公室入口、删除最后办公室、双标签、刷新/Host及Fastify重启、过期回执、重复点击、同ID异目标、旧快照/旧send/resume不能复活。网络故障可用本地HTTP注入，不能用echo对话代替真实删除链路，也不新增付费模型请求。Windows原生按用户既定暂缓如实登记。

兼容：旧客户端不发新动作时原协议不变；新Host连接旧Office应在持久接受删除前返回能力不支持；旧Host接新Office的删除动作应明确拒绝，不允许Office仅凭按钮本地删除。回退程序保留墓碑，不自动恢复办公室；跨进程同步为可恢复最终一致，不宣称数据库、HTTP、浏览器三者有分布式事务。混合旧版本未验证不能称兼容通过。

**审批对象。**R29 r1仅覆盖上述删除能力及关联原CHG0007 §12.117，不包含R22进度、R24颜色、R27动画等其他暂缓项。须用户明确批准R29及Host持久语义后再实施；实现后还须独立交付验收，不能把本方案或原27项绿灯写成删除功能已验证。


2026-10-29 D131—D133复核：已复现多Agent开启但主角色工具按类型转移负责人，以及持续主目标在一批结束时提前100%，两项均是OpenPaw投影缺陷，不能归为Office过滤；多行提示词标题与正常聊天节点也可单侧修复（CHG0007 §12.119）。原会话本次约17KB且applied，没有证据将其归因为超限。R22独立视觉进度、R24硬编码颜色、R25历史/当前统计、R27运行时交接拒绝仍保留，未获批准、未实施；本轮不增加Office变更范围，不将错过动画改为补播。准确正式原场景同时显示两实际在途角色working、其余四真实已结束角色idle，不据此声称所有偶发待命已关闭。

2026-10-29 D135队列链路专项（原R27证据补充，仍暂缓）：真实Host/原Office/Chromium五角色A在途/B排队→B接手的专项已记录`/home/cheng/.openpaw/verification/handoff-queue-r29-20261029/ui-before-d`。例如`draft-B-window-0.json`中，人物presentation/title已经是B、B实际read_file已返回；runtime仍active的office.visit计划对白却为“请接手A 资料核对。”，B的activity.start记录为queued。是已有动画资源队列落后实时任务，不是用最近拜访推断后台正在做A的依据，也不是Host把B标题误写成A；原生产003/004审查的文件及Run边界未发现串稿。只读复现不能证明用户先前未捕获帧的全部原因，不能把本次模拟冒充原帧。

候选仍归R27：Office `apps/web/src/dashboard/sceneBridge.ts`与`packages/runtime`的活动队列，在开始/恢复一个queued visit前核对其任务ID与最新业务版本；过时排队交接应明确丢弃，当前卡与交接对白能区分“当前任务”和“前项交接”。活动已开始后的保留/中止规则须另行审核，避免打断人物资源导致坐姿/路径残留。此候选不增加“动画结束才执行”的门禁，不补播错过的历史动画；后台当前执行事实保持优先。待准确逐文件方案及人工批准后才实施，当前Office指纹保持不变。


## 2026-10-30 Office 5d21cf2 上游复审（D138，Office自定义修改仍暂缓）

**准确版本和责任。**主Office原样快进b33e206→5d21cf2，134文件/12条提交（含合并）。本轮未修改Office文件，保留两份用户文档；正式是否部署只认本轮deployment.json。原R29删除办公室仍撤回，不重新申请。新版成果页/文件预览是现有官方能力，Host接入不需要新增Office Rx；完整版本仓库及访问协议是OpenPaw扩展，单列原CHG §12.124。

**R27新增上游行为，不能据此关闭待命/动画问题。**上游cb515df及5d21cf2的`packages/scene-office/src/adapters/office.ts`新增`officeHandoffReceivers`：所有queued/running的office.visit接收人，在接收回复阶段结束前都进入receiving集合；`pixi/index.ts`将这些人的task强制undefined、入座状态idle。真实business snapshot仍running并未改变。`tests/handoffPresentation.test.mjs`明确断言这两个值。排队拜访可以延后用户看到当前任务，且不区分“之前任务的排队交接”与“当前任务”，因此用户的后台在途但人物无卡/无工作动作，在新版已有明确代码路径。此结论解释新版门控，不倒推此前b33e206所有未捕获帧，也不代表Host一定没有别的漏卡。

**场景与候选修法（未实施）。**前项拜访未释放人物/桌面资源→新assignment排队→后台子Run已running→新版接收人的当前卡被整段隐藏。Office应将视觉接收等待与当前业务任务分开：保留真实任务/动作文本，独立提示交接中；按准确任务ID判别过时排队交接，不用任意office.visit阻断后续新任务；单人动作播放可串行，但不改后台状态、不增加动画后开工门禁，不补播错过历史。候选文件为原`adapters/office.ts`、`pixi/index.ts`、`apps/web/src/dashboard/sceneBridge.ts`及既有交接回归。须准确方案人工批准后才能改；本轮只运行原上游测试和真实UI。

**其他申请复审。**R22两套前端进度曲线、R24工作颜色、R25当前卡/累计历史仍不能因成果阅读器上线而关闭。新版结构化whiteboard steps新增state，可避免仅依百分比猜阶段；Host本轮传入真实done/active/pending，但不改变原Pixi进度计算，不能声称R22解决。R11默认链接等其他未验证条目维持原状态，需逐项复现后再关闭，不将上游功能名称当通过证据。


2026-11-02 R27原申请补证（仍暂缓，Office不改）：真实文章会话23e59be7-849d-4128-8552-89ddb5aa7b5c连续40帧已捕获business working与office.visit running同时存在。例如live-d/world-4.json，李研presentation为working/002配图退修，赵审→李研visit运行中，原版接收者隐藏规则仍适用。另有Host旧办公室全历史投影阻塞单写队列数秒的独立缺陷，原CHG D145单侧修复；不能用R27排除Host责任，也不能把Host性能修复宣称关闭R27。准确逐帧、帧间采样局限及新候选结果见 /home/cheng/.openpaw/verification/handoff-state-20261102。保持既有候选文件/权限范围与待审状态，不增加动画后才执行或错过补播要求。


### R30：聊天展开/切换重复同步和隐藏团队请求（候选2026-11-02，暂缓、未批准）

**原因与责任。**原版5d21cf2 OfficeChat.tsx:49每次展开调用synchronize(true)，ChatWorkspace.ts:125切换亦强刷；首次页面观测10个不同办公室history。OpenPaw history递归工具重建是独立主要耗时，按已批准D149先修；不能把后端缺陷推给Office，也不能把Host提速写成前端不再请求。

**候选效果与范围。**已有正文保留可读，当前会话按明确失效/有限新鲜度后台核对，只有可见办公室自动同步；无缓存首屏、显式重新同步、在途恢复、跨标签更新和停止仍可靠。候选原apps/web/src/components/OfficeChat.tsx、apps/web/src/chat/ChatSession.ts、apps/web/src/chat/ChatWorkspace.ts，原ChatTranscript.tsx仅实证存在布局问题时条件调整；OfficeIntegrationSession等激活调用点须进一步锁定，原聊天/工作区/浏览器测试扩失效及切换边界。不简单删除所有同步，不让缓存跳过Host输入冲突或权限检查，不改模型执行/动画。

**状态。**用户本轮批准的是OpenPaw第一阶段，第4节Office仅统计；本条保持候选待补材料、暂缓、未批准、未实施。尚未声称精确前端方案通过；后续需准确逐文件范围、回退和专项验收再人工决定。本轮不改变任何Office文件，不关闭其他Rx。
