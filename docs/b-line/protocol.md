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
| `DELETE /session/:id` | 删除非活动 session；204 | 404, 409 |
| `GET /session/:id/message` | message 数组 | 404 |
| `POST /session/:id/prompt_async` | 204 | 400, 404, 409 |
| `POST /session/:id/abort` | `true` | 404 |
| `GET /session/status` | 以 session id 为键的对象；busy 时可带脱敏 `activity` | 401 |
| `GET /event` 及其别名 | SSE | 400, 404 |
| `GET /provider` | `{all,connected}` | 401 |
| `GET /permission` | pending permission 数组 | 401 |
| `POST /permission/:id/reply` | 204 | 400, 404 |

降级路由分别返回 `{}`（`/config`、`/mcp`）、`[]`（`/command`、`/project`、
`/agent`、`/find/file`）或 204（`/session/:id/command`、`/shell`、`/fork`、
`/revert`、`/summarize`）。v2 路由返回 404。

## 字段规则

- Session 的 `time.created` 和 `time.updated` 使用 Unix 毫秒。
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

旧 session 没有 `metadata.cwd` 时回退 host CLI workspace。列表摘要直接只读这些
字段和文件 mtime，不装载运行时对象。

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
| Basic Auth 缺失或错误 | 401 | 否 |
| session/permission 不存在，或 stale handle 正在删除 | 404 | 否 |
| session 已 active、DELETE active session | 409 | 否 |
| user JSONL append 失败 | 500 | 不加入内存，不启动 AgentLoop |
| Office gateway 超时/拒绝/断开 | 原请求不变 | 仅日志，回合继续 |
| v2 路由 | 404 | 否 |

## 认证与就绪标记

用户名和密码必须同时设置；两者均未设置时关闭鉴权。启用后 middleware 覆盖所有
路由，401 带 `WWW-Authenticate: Basic realm="openpaw"`。host 仍只绑定
`127.0.0.1`。按本项目中文日志约定，就绪行是
`OpenPaw 引擎主机正在监听 http://127.0.0.1:<port>`；客户端和 Kunlun 只解析其中
稳定的回环 URL，不依赖本地化前缀。该文案偏离 master-plan 的早期英文示例，兼容
决策记录于 ADR-002。

`provider.connected` 按 OpenCode v1 最小形状固定列四个协议适配器，含义是“可选
适配器”而不是“凭据已验证”。缺少外部 key 时实际 prompt 返回 400。环境变量配置
自定义模型时，`all[].models` 同时保留 master-plan 要求的默认模型并加入配置模型。
