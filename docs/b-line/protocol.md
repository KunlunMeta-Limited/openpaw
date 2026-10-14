# B 线协议字典

host 实现 `master-plan.md` 第 3.2 节规定的 OpenCode v1 子集。所有 SSE frame
均为 `data: {"type":...,"properties":...}`，并且只发送连接建立之后发布的
事件。

| 路由 | 成功响应 | 错误响应 |
|---|---|---|
| `GET /global/health` | `{healthy:true}` | 启用鉴权时可能为 401 |
| `POST /session` | `id,title,slug,directory,time` | 400 |
| `GET /session` | session 摘要数组，最多 50 条 | 401 |
| `GET /session/:id` | session 对象 | 404 |
| `PATCH /session/:id` | 更新 title/archive 后的 session 对象 | 400, 404, 409 |
| `DELETE /session/:id` | 删除非活动 session；204 | 404, 409 |
| `GET /session/:id/message` | message 数组 | 404 |
| `POST /session/:id/fork` | 具有新 ID 的根 session 对象 | 400, 404, 409 |
| `POST /session/:id/revert` | 带 `revert.messageID` 的 session 对象 | 400, 404, 409 |
| `POST /session/:id/unrevert` | 清除暂存回退后的 session 对象 | 404 |
| `POST /session/:id/prompt_async` | 204 | 400, 404, 409 |
| `POST /session/:id/abort` | `true` | 404 |
| `GET /session/status` | 以 session id 为键的对象；busy 时可带脱敏 `activity` | 401 |
| `GET /event` 及其别名 | SSE | 400, 404 |
| `GET /provider` | `{all,connected}` | 401 |
| `GET /permission` | pending permission 数组 | 401 |
| `POST /permission/:id/reply` | 204 | 400, 404 |

降级路由分别返回 `{}`（`/config`、`/mcp`）、`[]`（`/command`、`/project`、
`/agent`、`/find/file`）或 204（`/session/:id/command`、`/session/:id/shell`、
`/session/:id/summarize`）。v2 路由返回 404。

## 字段规则

- Session 的 `time.created` 和 `time.updated` 使用 Unix 毫秒。
- 已归档 Session 额外返回正数 Unix 毫秒 `time.archived`；unarchive 请求写 `0`，响应省略该字段。
- staged revert 返回 `revert.messageID`；请求 user 边界时保持原游标，请求可由
  `TurnOutcome` 可靠关联的 `assistant` / `assistant_n` 边界时返回同回合 user 游标，
  从而隐藏完整回合及尾部。旧消息或完整分支没有有效回合关联时，已验证的真实 assistant
  按同会话顺序定位最近真实 user 输入，跳过非空且全部为 ToolResult 的 user 消息；没有此前输入时保持原游标。
  未知或已隐藏 ID 仍拒绝；失败展示消息仍须有已验证的回合关联。
  Branch 是新根会话，不返回 `parentID`。
- `GET /session/:id/message` 将可完整匹配此前 assistant tool part 的纯 ToolResult
  user 消息折叠到该 part 的 `state`；普通用户文本、混合内容和无法匹配的旧数据保持逐条投影。
  因此 REST 历史与实时 SSE 均以 assistant tool part 表示 `running/completed/error`。
- staged revert 期间 `GET /session/:id/message` 仍返回完整历史；`session.revert.messageID` 是客户端
  计算可见前缀的权威游标。这样 Unrevert 只清除游标即可恢复显示，与 OpenCode 行为一致。
- Text part 按输入顺序直接拼接，不插入分隔符；非文本 part 被忽略。
- `message.part.delta` 使用 `partID`、`sessionID`、`messageID`、`field` 和
  `delta`；tool state 为 `running`、`completed` 或 `error`。
- Permission 使用 `id`、`sessionID`、`permission`、`patterns`、`metadata` 和
  `always`。`once` 只批准当前请求；`always` 还会记住当前 session 的同名工具授权，
  不跨 session 或 Host 重启继承。
- Provider 为 `mock/mock-1`、`echo/echo-1`、`anthropic/claude-sonnet-4` 和
  `openai/gpt-4o`。

格式错误的 JSON 和不支持的 provider 返回 400；缺失或错误的 Basic Auth 返回
401；未知 session 或 permission 返回 404；同一 session 发起第二个活动回合返回
409。

## Prompt 字段与执行语义

| 字段 | 类型 | 必填 | 处理规则 |
|---|---|---|---|
| `model.providerID` | string | 否 | 缺失使用 host 默认值；只接受四个已登记 provider |
| `model.modelID` | string | 否 | 缺失使用所选 provider 默认模型；空字符串返回 400 |
| `parts[].type` | string | 否 | 只有 `text` 被处理，其他 part 忽略 |
| `parts[].text` | string | text part 是 | 按数组顺序直接拼接，不插入分隔符 |
| `text` | string | 否 | 兼容入口；存在时优先于 parts |

provider 和 model 是本回合选择，不替换 SessionHandle 或创建共享 AgentLoop。
Anthropic/OpenAI 缺少 API key 或 base URL 无效时，请求在预留 active turn 前返回
400。fake provider 集成测试同时断言最终请求路径和 JSON body 中的 model。

## Session 持久化字段

| JSONL 事件 | 字段 | 用途 |
|---|---|---|
| `meta` | `id/provider/model/created_at` | session 身份和创建配置 |
| `metadata` | `title` | 显式标题或首条用户文本前 80 字符 |
| `metadata` | `cwd` | 创建时解析出的绝对 workspace，供重启恢复 |
| `message` | core Message | user、assistant 和 tool result 历史 |
| `session_metadata_v1` | `title/archived` | PATCH 的原子元数据更新；零 archived 表示恢复 |
| `session_fork_origin_v1` | `sourceSessionID/ordinal` | 私有分支来源和同源序号，不投影为 `parentID` |
| `session_revert_staged_v1` / `session_revert_cleared_v1` | `messageID` 或无字段 | 暂存或取消会话级回退 |
| `session_revert_committed_v1` | `messageID/accept` | 与下一条 prompt 接收原子提交逻辑裁剪 |

旧 session 没有 `metadata.cwd` 时回退 host CLI workspace。列表摘要直接只读这些
字段和文件 mtime，不装载运行时对象。

所有会话变更继续使用追加写 JSONL。staged revert 只持久化服务端规范化后的权威游标，完整历史
继续保留在 REST message snapshot；客户端渲染、默认 Branch 和下一轮模型上下文按游标使用可见前缀。
unrevert 在下一条 prompt 被接收前可以恢复。下一条 prompt 将回退和 turn accept
写入同一条 committed 记录，之后 unrevert 为幂等 no-op。包含 committed 记录的数据不能直接交给
不识别 `session_revert_committed_v1` 的旧 Host，否则旧 reader 会重新显示逻辑删除的尾部。

`session.created` 和 `session.updated` 的 properties 固定为 `{sessionID,info}`，其中 `info`
符合 `contracts/host-session-v1.schema.json`。archive、unarchive、rename、revert、unrevert 和
revert commit 都发布完整的 `session.updated`。

## SSE 顺序与响应边界

## 实时活动摘要

`GET /session/status` 的活动会话可以返回以下可选字段：

```json
{"session-id":{"type":"busy","activity":{"kind":"tool","tool":"read_file","task":"正在读取文件"}}}
```

`activity` 只用于驱动客户端状态条和 Office 人物提示，不写入 JSONL，也不携带工具
参数、路径、文件内容或模型思考正文。`kind` 可为 `thinking`、`tool` 或
`responding`；回合正常结束、失败和取消都会清除该字段。旧客户端忽略该可选字段即可。

```text
message.updated(user)
message.part.updated(user text)  # 接收已持久化，先于后台执行
session.status(busy)
message.updated(assistant created)
message.part.updated(empty text/reasoning)
message.part.delta...
message.part.updated(tool running/completed)
message.updated(assistant completed)  # 每次 provider response
session.status(idle)
session.idle
```

非流式 provider 的 Final 内容由 AgentLoop 补成同样的 TextDelta/ThinkingDelta，host
因此不需要为最后一个 response 建立特殊路径。工具循环中的每一个 assistant
response 都在对应 `TurnUsage` 边界发送 completed 时间。

## 错误矩阵

| 条件 | HTTP | 是否改变 session |
|---|---:|---|
| JSON 无法解析、文本为空、provider/model/base URL 无效 | 400 | 否 |
| PATCH 空对象、空标题、负 archived，或 fork/revert 边界非法 | 400 | 否 |
| revert 提供 `partID` | 400 `unsupported_part_revert` | 否 |
| Basic Auth 缺失或错误 | 401 | 否 |
| session/permission 不存在，或 stale handle 正在删除 | 404 | 否 |
| session 已 active 时 archive、fork、revert、DELETE 或第二个 prompt | 409 | 否 |
| user JSONL append 失败 | 500 | 不加入内存，不启动 AgentLoop |
| Office gateway 超时/拒绝/断开 | 原请求不变 | 仅日志，回合继续 |
| v2 路由 | 404 | 否 |

### CHG-2026-0022 消息替换与执行错误

下一条 prompt 提交 staged revert 后，Host 使用同一公共消息投影计算删除 ID，
依次发出 `message.removed`，再发清除游标的 `session.updated`，最后发布新 user 消息和正文。
删除事件包含 `{sessionID,messageID}`，覆盖被裁剪的 user、assistant 与仅用于错误展示的消息。
幂等重放、冲突或持久化失败不重复发出删除或正文事件；staged REST 仍保留完整历史。

已接收回合执行失败仍保持 prompt 的 204，稳定 Gateway `TurnError/TurnOutcome` 不增加字段。
OpenCode 兼容层先发布归属失败回合的 assistant `message.updated`，再发
`session.error {sessionID,error}`，最后进入 idle。REST 对同一 assistant 返回 `info.error`。
零输出失败可生成 `parts=[]` 的展示消息，绝不写入模型 `Session.messages`；其 Revert 边界
可靠关联到同回合 user。fork 继续不复制 turn/admission/error 诊断状态。

安全错误形状为 `{name,data:{message,code,isRetryable,statusCode?,providerID?}}`，约束见
`contracts/host-message-events-v1.schema.json`。可靠 HTTP 状态保留；网络和流协议错误不猜测状态。
摘要有界，过滤凭据、URL、路径及输入回显；不公开 responseBody、responseHeaders 或原始错误。
取消沿用 cancelled，不发 API 失败。finish commit 的可选私有 `openCodeError` 与 terminal
原子保存；旧记录缺字段时使用稳定分类，回退提交同步裁剪 metadata。旧 reader 可忽略新增字段。

## 认证与就绪标记

用户名和密码必须同时设置；两者均未设置时关闭鉴权。启用后 middleware 覆盖所有
路由，401 带 `WWW-Authenticate: Basic realm="openpaw"`。host 仍只绑定
`127.0.0.1`。按本项目中文日志约定，就绪行是
`OpenPaw 引擎主机正在监听 http://127.0.0.1:<port>`；客户端和 Kunlun 只解析其中
稳定的回环 URL，不依赖本地化前缀。该文案偏离 master-plan 的早期英文示例，兼容
决策记录于 ADR-002。

`GET /provider` 保留 `all/connected/default` 字段：all 列出四个支持协议及默认/有效配置模型；connected 是无重复子集，表示本地配置满足运行条件。
mock/echo 需要有效通用生成配置；openai/anthropic 还需要非空 Key、合法 URL 和非空模型。
无效网络配置不影响离线项；无效通用 token 配置使各协议都不可用。此判断不验证上游认证或网络连通性。
default 保留显式 Host provider，即使不可用也不切换到 mock；实际模型请求明确返回配置错误或上游失败。
字段契约见 `contracts/host-provider-v1.schema.json`，共享配置由 openpaw-llm::config 提供。

## 图片与原生音频合同（CHG-2026-0026）

POST message / prompt_async 接受 `parts:[{type:"file",mime,url,filename?}]`；url 限内联 data URL。
纯附件请求有效，文本与附件顺序保留；直接 text 沿用优先规则。接收前校验 MIME/头/Base64、数量、大小和有效模型能力，
非法输入400、原件或body超限413；同 requestID / messageID 的规范内容相同重放，字节、顺序、名称或模型不同409。
最大4个/20 MiB原件、图片5 MiB、body32 MiB；具体 schema 见 contracts/host-prompt-v1.schema.json。

REST 和即时 message.part.updated 使用相同 file id/attachmentID/MIME/data URL；工具 parts 保留 callID，
completed 结果增加 attachments。GET /provider models 增加 capabilities.input 的 text/image/audio 布尔值，
connected 仍只表示本地配置。所有媒体只出现在授权消息接口，Office 活动、CLI摘要/FTS和诊断不输出原件。
Schemas 为 host-message-events-v1 和 host-provider-v1；旧 text 请求、tool 文本与 SSE 类型保持兼容。

OpenWork 已有图片 file part 形状在 OpenPaw 契约测试中覆盖；其音频入口和 AI Office 界面尚未实现，本轮不修改客户端仓库。

## 文本展示属性（CHG-2026-0027）

现有 message / prompt_async 的 text part 接收可选 `synthetic:boolean`、`metadata:object`。
带属性的文本保留独立内容块与输入顺序；直接 `text` 仍优先，忽略 part 的属性不迁移到正文。
REST 和即时 `message.part.updated` 共用投影，保留属性及稳定 part ID；缺属性的旧纯文本入口沿用原合并行为。
`synthetic=true` 只标记辅助正文，模型仍收到原 user 消息中的正文及附件，metadata 不进入模型内容。
OpenWork 已有过滤消费该标记；`metadata.openworkAttachments` 是客户端业务键，Host/Store 不解释或下载其中的 URL。

Store 的 `turn_accept_v1` 增加可选 `messagePresentation:{version:1,parts:[{blockIndex,auxiliary,metadata?}]}`。
注解与 user/outcome/admission 同一事务接收；replay 检查版本、User 角色、唯一块编号和 Text 类型。
Fork 在整体发布的分支文件中紧跟对应 Message 写 `message_presentation_v1:{messageID,presentation}`，并绑定新消息 ID；
staged Revert/Unrevert 保留完整历史，提交回退同步裁剪注解。压缩不改变完整历史的属性或内联媒体原件。
有展示属性的幂等指纹包含规范化块内容及属性，媒体使用 MIME/字节/名称，排除临时随机 attachmentID。

自动标题来自首条输入中的非辅助文本，最多 80 字符；无真实正文的媒体输入使用“图片附件”或“音频附件”。
已有显式标题与续聊标题保持优先。旧记录缺省无注解，已丢失属性的历史不按英文前缀猜测改写。
该后端兼容修复不改变 OpenWork 的发送前中文组合输入行为。

## AI PC 会话专属桌面桥接（CHG-2026-0026）

稳定接口编号：`API-HOST-AI-PC-REGISTER-001` 对应 PUT 登记；`API-HOST-AI-PC-UNREGISTER-001` 对应 DELETE 撤销。CHG-2026-0028 补齐编号，既有路径、字段与失败语义保持下述合同。

`PUT /session/{id}/ai-pc` 登记严格 JSON：`{"bridge_url":"http://host.docker.internal:PORT/ai-pc","token":"随机会话令牌"}`。仅接受 localhost、127.0.0.1、host.docker.internal 的 HTTP(S)，固定 `/ai-pc`，禁止用户信息、查询、片段与额外字段。请求限 8 KiB，令牌 32–256 个 ASCII 字母、数字、`-` 或 `_`。`DELETE` 同一路径撤销登记。

两入口均受 Host Basic 认证保护，并要求已配置非空用户名与密码。无配置 503、无效认证 401、未知会话 404、活动回合 409、非法请求 400、成功 204。登记只在内存，Host 重启需重新登记；fork 不继承，成功删除会话清除登记，删除失败保留原登记。

普通会话保留多模态注册表的 22 个工具（含 `read_attachment`）。登记会话动态增加 `ai_pc_current_context`（严格空对象）和 `ai_pc_history_search`（可选 keyword/application，最多 256 字；from_ms/to_ms 为非负 Unix 毫秒，起点不晚于终点），共 24 个。模型不能指定服务、会话、模式或凭据。包装器用可信 Host 会话 id 发 `POST /ai-pc/current` 或 `/history`，正文 `{"session_id":"真实身份","args":{...}}`，Bearer 使用登记令牌。不跟随重定向或系统代理，10 秒超时，响应最多 512 KiB，只接受 JSON 的 ok/empty/denied 状态。

AI PC 每次查询检查令牌、受管会话及最新授权，交付前登记来源；受限检索只在授权片段上匹配。AI PC 会话的全局 `session_search` 拒绝调用并要求改用授权历史工具；普通会话的内置工具行为保持原合同。令牌不得进入工具声明、模型输入、journal、事件或日志。

CHG-2026-0028 集成保留会话原生附件安装、回看与回合结束清理；AI PC 包装器透传媒体上下文给同一会话的内置工具。桌面当前/历史工具仍只返回授权文字 JSON、空媒体列表，不自动发送截图。
