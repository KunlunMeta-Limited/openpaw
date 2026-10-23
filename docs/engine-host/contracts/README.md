# 冻结的接入契约

本目录与 tests/engine-host-contracts 同时审核。sources.json 固定 Office 提交、文档/Schema/示例/测试入口 SHA-256，以及 SDK 包版本/integrity。锁文件只用于测试，不把 Node 或 SDK 引入 Host 生产依赖。

Office 原仓不能匿名下载。upstream-office 是九个原始协议 Schema 与 LICENSE 的只读测试快照，非 Host 实现或手写替代 Schema。默认准备流程验证快照哈希、无网络；传 --office-source 时从固定 git commit 校验全部 21 份来源并刷新测试快照，不读取工作区未提交改动。升级先审源码/契约，再同步 commit、哈希和 fixtures，不手改快照。

## Office：02dbac4 官方服务（version仍为1.0）

Host → POST /api/pawoffice/messages，Bearer 仅服务端持有。外部消息包括 capabilities、initialize、snapshot、read、decision.request、ui.next/respond；场景走 teamId+command 现有 v2。Host 不向 Office 发送旧 actions gateway 格式。

| 规则 | 必须实现的语义 |
|---|---|
| JSON | 任意深度无 null，无 __proto__，最大嵌套 64；严格字段 |
| 容量 | messages 请求 65536 UTF-8 字节；每会话员工≤200、任务≤1000、事件≤500；每办公室≤100会话；缓存≤100办公室/8MiB |
| 快照 | schemaVersion 1.0，seq 1..9007199254740991，每办公室持久递增；conversations[]内独立成员/任务/事件/白板，id等于conversationId；整办公室完整替换，省略字段按默认清空 |
| 重试 | 同 seq 同规范化内容重同步，不改时间；同 seq 异内容 409；旧 seq ignored |
| 确认 | 200/applied 是浏览器逻辑确认；202/cached 仅缓存（离线或等待容量满）；请求失败可能已经缓存 |
| 恢复 | Office Fastify 状态内存存储，重启必须重新初始化；read 不是宿主账本 |
| 菜单 | 默认60秒，最多300秒；仅在线展示，超时/断开不批准；Host 审批 ID 与 display ID 分离 |
| UI领取 | 全局单 poller，最多等30秒；领取即删除投递资格，无 ACK/租约/requeue，可能丢失一次操作 |
| 详情 | detail-v1，标题160、说明4000、字段≤30（label100/value2000）、按钮≤8；纯文本、分页，不新建侧栏 |
| 场景 | 命令唯一 ID、batch≤32；HTTP 200不等于动画执行确认，离线临时动画不回放 |
| 交接 | task.assigned 稳定事件可自动访问，不再额外重复 visit |
| 状态 | queued/running/blocked/completed/failed；取消/中断映射 failed 并给原因；进度未知 -1，不伪测CPU/内存 |
| 白板 | task-board-v1 最多3步骤；前端平滑进度不是业务真实进度 |

聊天为浏览器同源POST /api/pawoffice/chat→Office Fastify→Host /office/chat。Host用offices.initialize.chat.url动态注册实际回环端口；不靠VITE变量，不要求Node无法转发的Origin/认证头；有Origin仍须白名单，Host只监听loopback并校验Host。

四操作均有version/teamId/conversationId：send加turnId/messages，history加cursor/limit，resume/cancel加turnId。send/resume是NDJSON，history/cancel是code/msg/data。新增snapshot{text,state,message}，resume第一条必须snapshot或error；terminal snapshot可直接结束。重复turn相同内容同Run，异内容冲突；history从Run原输入边界投影，工具结果不是新轮次，页内旧→新，游标稳定。断开仅detach，显式cancel清理完成后stopped:true。

请求≤512KiB；输入≤4000、回复≤50000，历史最多10轮且≤100000，行≤350000，delta≤16000；按JS UTF-16兼容限制，字节另计。history响应≤8MiB、每页1—50。单流连接180秒，非流15秒；Run预算独立。空输出/错误不发假done。完整契约与状态图见master第4节，T4实际能力及证据见0005，旧Office验证不能覆盖新版。


r6错误传输：仅send/resume接受阶段的history_conflict、request_conflict、session_busy、slot_busy、capacity_exceeded、not_found，以HTTP200/application/x-ndjson返回一条官方error{message}，没有done/成功snapshot/新Run；200只表示流可传输，客户端必须读事件判断业务结果。安全文案由适配器白名单决定，不回显原诊断，也不按Accept头切换语义。非法JSON/字段/来源/未授权办公室保留真实4xx，存储/未知故障保留HTTP失败。history/cancel仍是JSON；Office转发上游非2xx时按官方行为显示通用502，不保证其原细分状态透传。

r6展示文本：Host内部RunText同时处理live与JSONL投影，每条assistant消息内部原样连接；两个有文字的消息之间补足两换行，已有换行不重复补。可靠消息保存后才标记下一边界，空/Thinking/纯工具消息不造文字。实时4MiB预算计入分隔符；原JSONL、模型上下文和数据库版本不变。流、history、resume、重启和复制应一致，旧浏览器缓存按官方history重新同步，不清用户存档。


Fixtures 验证 Schema 的规范化、引用关系、数量/字段、场景绑定、无null/深度、响应形状。seq 冲突/缓存/浏览器应用等运行语义要在 T4 真服务测试，不能靠本阶段 fixtures 声称通过。

## OpenCode：SDK 1.18.15 真实 v1

从 @opencode-ai/sdk 根导入，不使用 ./v2。T0 用真实 SDK 构造请求并用真实迭代器解析 loopback fixture SSE；T7 换为真实 Host，不写平行协议服务冒充实现。

| 旧 master-plan 内容 | 冻结的真实 v1 |
|---|---|
| permission.asked | permission.updated，properties 为 Permission |
| POST /permission/:id/reply，reply 字段 | POST /session/{id}/permissions/{permissionID}，body.response once/always/reject，200 boolean |
| permission.replied 的 requestID | sessionID、permissionID、response |
| message.part.delta | message.part.updated，properties.part 与可选 delta |
| session.created/updated 平铺 | properties.info: Session |
| global SSE 与普通事件同体 | /global/event 返回 {directory,payload:Event}；/event 直接 Event |
| 简化 Session | id/projectID/directory/title/version/time，必填按 SDK |
| 简化 message | user 含 agent/model；assistant 含 parentID、model/provider、mode、path、cost、tokens |
| running tool 不带 time | 每种 tool state 按 SDK 必填 input/time/output/title/metadata |
| provider 只有 id/name | all/default/connected，模型能力、limit、options 等完整类型 |
| 未配置 Provider 也 connected | 仅实际可用项 connected |
| 未支持写接口假204 | 501 + 明确结构错误，SDK throwOnError 走失败 |
| 忽略非文本 part | 首期拒绝400，不能静默丢内容 |

| 方法/路径 | 成功结果 |
|---|---|
| GET /global/health | {healthy:true}，宿主发现端点，不宣称 SDK 有此方法 |
| POST /session | Session，接受 title |
| GET /session | Session[] |
| GET /session/:id | Session |
| GET /session/:id/message | {info:Message,parts:Part[]}[] |
| GET /session/:id/todo | Todo[]，来自真实 Todo 查询，空数据才 [] |
| GET /session/status | sessionID→busy/idle 状态，终态不继续 busy |
| POST /session/:id/prompt_async | 持久化接受后204 |
| POST /session/:id/abort | 200 boolean，取消幂等；状态仍需等清理 |
| POST /session/:id/permissions/:permissionID | 200 boolean |
| GET /provider | 完整 ProviderListResponse |
| GET /event | data: Event 加空行 |
| GET /global/event | data: GlobalEvent 加空行 |
| GET /api/event | 仅保留旧 attached 兼容别名，映射 /event，同一订阅服务 |
| GET /config、/mcp | {}，本期只读降级 |
| GET /command、/project、/agent、/find/file | []，明确能力不支持 |
| POST command/shell/fork/revert/summarize | 501，不执行、不造成功 |
| v2 路径 | 404 |

错误：400 非法/不支持输入或模型；401 Basic；403 工作区未授权；404资源不存在；409 active冲突；503运行容量；501不支持操作。错误字段按 SDK 可表达的 name/data 形状，T7 实测具体失败提示，不能依赖客户端对空204抛错。

Basic：OPENCODE_SERVER_USERNAME/PASSWORD 都无仅loopback关认证，只配一个启动失败，两者都有保护所有 OpenCode 路由。Office 协议鉴权独立，不把 Basic 密码放浏览器。

attached：OPENWORK_OPENCODE_BASE_URL、OPENWORK_OPENCODE_USERNAME/PASSWORD 供未来 OpenWork server 使用；不 managed spawn。监听127.0.0.1:0，精确stdout一条就绪标记：
    opencode server listening on http://127.0.0.1:<port>

目录：x-opencode-directory > query.directory > 默认，白名单根+Session固定归属。SDK GET 会将目录转 query，POST 目录 header 是 encodeURIComponent 结果；只按 SDK 实际编码解码一次再作路径校验，不接受双解码绕过。

SSE：session/message先声明再 part；permission.updated 可重同步同ID；成功回复后 permission.replied；终态 status idle/session.idle。未知成本/usage按实际未测口径表示，不伪造真实模型计费。全局事件必须用正确目录包装。

always 首期与 once 等价，只批准本次调用，不建立永久规则；这是明确的兼容限制。SSE断开不取消，重连查询快照/状态/待审批，不发明 v2 event。SDK v1 SSE 内部使用 global fetch 而非 config.fetch，T0 使用真实本地HTTP fixture 避免错误 transport mock 产生“通过”。

## R1/R2/R3 与升级

R1 见 r1-chat.md，单独批准；R2/R3 本期不改。协议升级必须先更新此文档、来源锁、正反例及评审，再修改 Host。不能让 Office 无null规则污染 OpenCode DTO，不能因 SDK 类型变化改核心协议。


## T5现有协议适配（CHG-2026-0006 r1实施中）

来源仍为固定02dbac4的office-integration第3/6/7节及office-ui Schema，未修改Office。Host所有业务请求走POST /api/pawoffice/messages；浏览器内部reply/ui不是Host端点。decision.request超时按业务剩余期限封顶300秒，HTTP加10秒；ui.next为30秒、HTTP35秒，普通请求15秒。

共享ApprovalView是唯一授权事实。展示前同步完整会话并读取服务端最新快照；display ID每次唯一，响应核对team/conversation/decision/choice/label，再委托Host::decide_approval。202缓存不等于网页应用，未知/过期/旧菜单/关闭均不授权；确认不代表已经执行，实际started由原工具观察器记录。

唯一UI领取器先登记action，再交有界队列128/处理16。无领取ACK，领取后登记前丢连接仍可能丢操作；接收时间不证明服务器还剩60秒。响应使用本次action.id，dialog.action载荷的旧interactionId只定位同范围、10分钟且最多64项的导航；数据摘要变化则要求重新打开。detail-v1每页5字段，UTF16字段界限和64KiB整包均保留，大文本逐段展示。

conversation.delete调用中性Host::delete_session：先墓碑/失效审批，等待目标Run清理，再移除JSONL、自动损尾备份、原索引及派生记录；保留工作区产物及用户备份。完成后同步更高seq完整快照并核对服务端移除，才回复success；410/断线如实未确认，不复活业务数据。v4墓碑禁止旧send/history/resume/cancel/approve重新创建会话。

任务按钮不模拟执行/暂停/完成/改派。task.assigned稳定事件由Office自动拜访，Host不另发visit。scene.say仅2秒公开摘要，forwarded仅为转发；动画和场景停止不改变Run。未完成的浏览器/故障组合验收见0006验证记录，不能据本节视为通过。


T5场景仍用官方commandSchema：expiresAt取本次观察后10秒，timeoutMs=3000，busyPolicy=reject；只接受forwarded回执，不虚构动画完成。交接事件以原tool_started为依据，审批拒绝与前置校验失败不触发工具执行交接。删除后ui.respond未确认仅留短期控制回执，不保留正文或恢复会话。
