# OpenPaw 引擎宿主

- 状态：执行中。基准日 2026-09-09，收工日 **2026-09-30**
- 执行人员：3 人，分别负责 OpenPaw、OpenWork 和 AI Office。协议、排期和验收标准均以本文为准。
- 范围：`openpaw-engine-host` 实验，不进 1.0

`openpaw --engine-host` 将 `AgentEvent` 分别转换为 OpenCode v1 事件和办公室动作。A、B 两线没有代码依赖，但都依赖 host 提供相应的事件。

| 部分 | 对端 | 接口 | 本期交付 |
|---|---|---|---|
| 引擎 | — | `openpaw --engine-host`（本机 HTTP/SSE） | 本文 §3.2 定义的 OpenCode v1 子集 |
| A 线 | OpenWork | attached 模式，`baseUrl` 指向 host | 流式文本、工具状态、审批和终止 |
| B 线 | ai-office-react | `POST /actions`；`GET /office/*` | 六人动画；会话、技能、文件和任务侧栏 |

## 1. 架构

### 1.1 三方

| 系统 | 仓库 | 角色 | 关键位置 |
|---|---|---|---|
| OpenPaw | openpaw | 运行 `AgentLoop`，保存 JSONL 会话，并通过 `--engine-host` 提供 HTTP/SSE | `crates/openpaw-engine-host/**`、`crates/openpaw-cli/src/cli.rs` |
| OpenWork | openwork | 桌面请求先到 `/workspace/:id/opencode/*`，再由 `openwork-server` 使用 Basic Authentication 转发到 host | `apps/server/src/server.ts`、`opencode-connection.ts` |
| AI Office | ai-office-react | 8765 网关接收动画动作并只读 `OPENPAW_HOME`，Pixi 场景消费动作 | `scripts/action-gateway.mjs`、`officeActionDispatcher.ts`、`OfficeDashboardChrome.tsx` |

### 1.2 架构图

```
                    ┌────────────────────────────┐
                    │  openpaw --engine-host      │
                    │  127.0.0.1:<port>           │
                    │  ┌────────────────────┐    │
                    │  │ AgentLoop + 工具    │    │
                    │  │ ApprovalManager     │    │
                    │  │ sessions/*.jsonl    │    │
                    │  │ OPENPAW_HOME        │    │
                    │  └─────────┬──────────┘    │
                    │     AgentEvent（同一轮）    │
                    └──────┬──────────────┬──────┘
              A 线：翻译成   │              │  B 线：翻译成
        OpenCode v1 事件    │              │  office 动作
                           ▼              ▼
        ┌─────────────────────┐   ┌──────────────────────┐
        │ openwork-server      │   │ POST /actions (8765) │
        │ /workspace/:id/      │   │ GET  /office/*       │
        │   opencode/*         │   │ (gateway 直读 home)   │
        │ Basic → host         │   │                      │
        └─────────┬───────────┘   └──────────┬───────────┘
                  ▼                          ▼
        OpenWork 桌面 UI              ai-office-react 场景
        @opencode-ai/sdk v1           六人动画 + 侧栏四块
```

### 1.3 三条线

| 线 | 走向 | 用途 | 备注 |
|---|---|---|---|
| 聊天（A） | OpenWork ⇄ host | 流式文本、工具 part、审批、终止 | 依赖 T3、T4；审批另依赖 T5 |
| 动画（B-1） | host → 8765 → Pixi | 将工具调用和审批状态映射为动作 | 依赖 T10；POST 失败不得影响 agent 回合 |
| 数据（B-2） | gateway → `OPENPAW_HOME` | 会话、技能、文件和任务侧栏 | 依赖 T11、T12；不依赖 host 进程 |

### 1.4 硬约束

1. host 业务代码不超过 5000 行，以 `cargo run -p xtask -- lines` 的结果为准。超过上限时，只能拆分 host 内部模块，不得把兼容协议类型移入 `openpaw-core` 或 `openpaw-engine`。
2. 依赖方向为 OpenWork → host → OpenPaw 核心 crate。host 使用本地 serde 类型实现 HTTP 协议，不依赖 OpenWork 或 `@opencode-ai/*`。
3. `engine-host` feature 默认关闭。关闭时，默认 CLI、核心 9 个 crate、sidecar 和离线测试均不得引入 axum 或改变现有行为。
4. host 只绑 127.0.0.1，端口默认 0（随机分配）。
5. 协议以本文 §3.2 为准，兼容性基线为 `@opencode-ai/sdk` 1.18.15。SDK 版本变化时，先更新 §3.2 和协议夹具，再修改 host。
6. 8765 办公接口不属于 OpenCode 兼容协议。动画经 `/actions` 传递，侧栏经 `/office/*` 读取，不得添加到 A 线路由。

## 2. 引擎：openpaw-engine-host

### 2.1 决策与范围

OpenWork 的 attached 模式允许 `workspace.baseUrl` 或 `OPENWORK_OPENCODE_BASE_URL` 指向兼容 OpenCode HTTP/SSE 的进程。因此，本期由 host 实现桌面聊天所需的 OpenCode v1 子集，不修改 OpenWork 的 SDK 调用层。

host 使用独立 crate 和 axum。本期不实现完整 OpenCode v1，仅实现 §3.2 列出的路由、对象和事件。

本期不修改 `session-sync.ts` 的协议模型，不使用 sidecar 承载聊天协议，不实现 managed spawn，也不把 axum 加入默认构建。

### 2.2 闸门

| 文件 | 改动 |
|---|---|
| `xtask/src/lines.rs` | `openpaw-engine-host` 上限 2000 → 5000 |
| workspace `Cargo.toml` | 声明 host 使用的 axum 版本；tokio 的 `net` feature 仅由 host 启用 |
| `crates/openpaw-engine-host/Cargo.toml` | feature `engine-host` 打开 axum、serde_json、uuid、chrono；依赖 core / engine / store / llm / tools |
| `crates/openpaw-cli/Cargo.toml` | `engine-host = ["dep:openpaw-engine-host", "openpaw-engine-host/engine-host"]` |

将 §3.2 的 JSON 保存为 host 测试夹具。T1 完成标准同时包括：host 只监听 127.0.0.1；关闭 feature 后，默认构建树不含 axum。

### 2.3 模块结构

都放 `crates/openpaw-engine-host/src/`：

| 模块 | 职责 |
|---|---|
| `lib.rs` | `HostConfig`、`run()` |
| `protocol.rs` | §3.2 的协议结构（serde 类型） |
| `runtime.rs` | session 表、JSONL、`AgentLoop`、取消 |
| `http.rs` | axum 路由、Basic 认证 |
| `events.rs` | `AgentEvent` → OpenCode 事件翻译 |
| `approval.rs` | `ApprovalManager`：发 `permission.asked`，等 `/permission/:id/reply` |
| `office.rs` | T10 实现的办公室动作客户端；T1 仅声明模块 |

runtime 复用 `crates/openpaw-cli/src/chat.rs` 的构造顺序：`AgentLoop`、`arc_registry(workspace)`、`SessionWriter`。`prompt_async` 创建后台 turn 后返回 204；`abort` 调用该 session 对应的 `AgentLoop::cancel()`。工作区按 `x-opencode-directory`、`directory` 查询参数、CLI 默认工作区的顺序取值。

### 2.4 CLI 入口

沿用 sidecar 的全局旗标形式，不新增子命令。修改 `crates/openpaw-cli/src/cli.rs` 和 `main.rs`：

```
openpaw --engine-host --engine-host-hostname 127.0.0.1 --engine-host-port 0 --provider mock
```

- 旗标：`--engine-host`、`--engine-host-hostname`（默认 `127.0.0.1`）、`--engine-host-port`（默认 `0`）。provider、home 和 workspace 复用现有 chat 参数及其优先级。
- 启用 feature 后，`help_snapshot.rs` 必须包含 `--engine-host`，子命令数量仍为 12
- 成功监听后，stdout 输出且只输出一条就绪标记：`opencode server listening on http://127.0.0.1:<port>`

| 环境变量 | 用途 |
|---|---|
| `OPENPAW_HOME` | session、skill 和 goal 的根目录 |
| `OPENPAW_OFFICE_ACTIONS_URL` | 动画动作地址；默认 `http://127.0.0.1:8765/actions` |
| `OPENCODE_SERVER_USERNAME` / `OPENCODE_SERVER_PASSWORD` | host 的 Basic Authentication 凭据 |
| `OPENPAW_ENGINE_PERMISSIONS` | `manual` 启用人工审批；未设置时使用自动审批 |
| `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` | 对应 provider 的凭据 |

### 2.5 AgentEvent → OpenCode 事件翻译（events.rs）

`AgentLoop` 继续接收 `FnMut(AgentEvent)`；OpenCode 事件转换仅位于 host：

| 来源 | 输出 |
|---|---|
| `prompt_async` | 持久化 user message；发送 user `message.updated` 和 busy `session.status` |
| `MessageStart` | `message.updated`（assistant） |
| `TextDelta` | 首次收到时发送 text `message.part.updated`，随后发送 `message.part.delta` |
| `ThinkingDelta` | 首次收到时发送 reasoning `message.part.updated`，随后发送 `message.part.delta` |
| `ToolUseStart` | tool part `running`，`callID` 用 `tool_use_id`；T10 按 §4.2 生成办公室动作 |
| `ToolResult` | `is_error=false` 时将同一 part 标为 `completed`，否则标为 `error` |
| `ClassifierStatus::SuggestConfirm` | `permission.asked`，`ApprovalManager::request` 挂起等回复 |
| `ClassifierStatus::AutoDenied` | 不发送 permission 事件 |
| turn 返回 | 填写 `time.completed`；发送 idle `session.status` 和 `session.idle` |
| 名称为 `todo_add`、`todo_update` 或 `todo_list` 的 `ToolResult` | `todo.updated` |
| `CompactionPerformed` | 本期不发送 OpenCode 事件 |

SSE 使用 `broadcast`。订阅只接收连接建立后的事件；历史消息由 `GET /session/:id/message` 返回。自动化测试使用 `AutoApproveManager`；`OPENPAW_ENGINE_PERMISSIONS=manual` 时使用人工审批。事件测试断言 `AgentEvent::kind()` 对应的 SSE `type` 和顺序，不比较文本内容。

### 2.6 审批机制

`ApprovalManager::request` 创建 pending permission 并发送 `permission.asked`。`/permission/:id/reply` 收到 `once` 或 `always` 后返回 `Approved`，收到 `reject` 后返回 `Denied`；随后删除 pending 记录并发送 `permission.replied`。session 在等待审批时收到 `abort`，必须取消等待、清除 pending 记录并终止 turn。

## 3. A 线：OpenWork

### 3.1 接入方式

本期只支持 attached。启动 host 后设置：

```
OPENWORK_OPENCODE_BASE_URL=http://127.0.0.1:<port>
OPENWORK_OPENCODE_USERNAME=<与 OPENCODE_SERVER_USERNAME 相同>
OPENWORK_OPENCODE_PASSWORD=<与 OPENCODE_SERVER_PASSWORD 相同>
```

也可通过 CLI 参数或 `~/.config/openwork/server.json` 的 `opencodeBaseUrl` 配置地址。优先级为：工作区 `baseUrl`、CLI 参数、`OPENWORK_OPENCODE_BASE_URL`、配置文件 `opencodeBaseUrl`。

设置页显示 `Engine: OpenPaw (attached)`，并将浏览器、日历和插件标记为“不支持”，不得显示加载失败。

managed spawn 不在本期范围。用户必须先启动 host，再配置 attached 地址；OpenWork 不负责启动、重启或回收 host。

### 3.2 协议快照（OpenCode v1 子集，2026-09-08）

对的是 OpenWork 桌面现在用的 `@opencode-ai/sdk` v1 挂载 `/workspace/:id/opencode/*`，不含 `/opencode2`。路径是 OpenWork 剥掉前缀之后的。

**传输与认证**：host 只监听 127.0.0.1，端口默认 0。用户名和密码都未设置时关闭鉴权；只设置其中一个时启动失败；两者都设置时，所有路由均要求 Basic Authentication。工作区按 `x-opencode-directory`、`directory` 查询参数、CLI 默认工作区的顺序取值。SSE 路径为 `/event`、`/global/event` 和 `/api/event`，其他 GET 路径不得返回 SSE。

**路由**：

| 方法 | 路径 | 成功时 |
|---|---|---|
| GET | `/global/health` | `{ "healthy": true }` |
| POST | `/session` | session 对象（可带 `{ "title": "..." }`） |
| GET | `/session` | session 数组 |
| GET | `/session/:id` | session 对象 |
| GET | `/session/:id/message` | message 数组 |
| GET | `/session/:id/todo` | todo 数组，暂 `[]` |
| GET | `/session/status` | `{ "<id>": { "type": "busy" } }` 或 `{}` |
| POST | `/session/:id/prompt_async` | 204 空 body |
| POST | `/session/:id/abort` | `true` |
| GET | `/event` | SSE |
| GET | `/provider` | provider 列表 |
| GET | `/permission` | 待审批列表 |
| POST | `/permission/:id/reply` | 204 空 body |

本期未实现的 v1 能力必须显式降级：GET `/config`、`/mcp` 返回 `{}`；GET `/command`、`/project` 返回 `[]`；POST `/session/:id/command`、`/shell`、`/fork`、`/revert`、`/summarize` 返回 204。v2 路径（包括 `/api/health` 和 `session.execution.*`）返回 404。

错误状态固定为：JSON 解析失败或 provider 不支持返回 400；Basic Authentication 缺失或错误返回 401；session 或 permission 不存在返回 404；同一 session 已有 active turn 时再次调用 `prompt_async` 返回 409。不同 session 可以并行执行。

**session**：

```json
{
  "id": "<openpaw meta.id>",
  "title": "<创建请求中的 title；未提供时为 New session>",
  "slug": "<id>",
  "directory": "<工作区>",
  "time": { "created": 0, "updated": 0 }
}
```

`time` 使用 Unix 毫秒。创建时 `created` 与 `updated` 相同；收到首条用户消息时，如果标题仍是 `New session`，则用该消息前 80 个字符更新标题和 `updated`。

**prompt_async body**（SDK 把 sessionID 放路径里）：

```json
{
  "model": { "providerID": "mock", "modelID": "mock-1" },
  "parts": [{ "type": "text", "text": "user prompt" }]
}
```

按输入顺序拼接所有 `type === "text"` 的 part，part 之间不额外插入分隔符；忽略其他 part。`model.providerID` 仅接受 `mock`、`echo`、`anthropic` 和 `openai`；缺少该字段时使用 host 默认 provider，不支持的值返回 400。

**message / part**：

```json
{
  "info": { "id": "msg_...", "sessionID": "<id>", "role": "user", "time": { "created": 0, "completed": 0 } },
  "parts": []
}
```

`role` 还有 `assistant`。text part：

```json
{ "id": "prt_...", "messageID": "msg_...", "sessionID": "<id>", "type": "text", "text": "..." }
```

thinking 用 `"type": "reasoning"`。tool part：

```json
{
  "id": "prt_...", "messageID": "msg_...", "sessionID": "<id>",
  "type": "tool", "callID": "<tool_use_id>", "tool": "<工具名>",
  "state": { "status": "running", "input": {}, "output": "", "error": "" }
}
```

`status` 还有 `completed`、`error`。`message.part.delta` 的 properties：`partID`、`sessionID`、`messageID`、`field: "text"`、`delta`；`field` 不可靠，以之前的 `message.part.updated` 为准（`session-sync.ts`）。

**SSE 事件**（properties 用 `sessionID` 这种驼峰；帧格式一行一个 `data: {"type":"<event>","properties":{...}}`）：

| type | properties | 时机 |
|---|---|---|
| `session.created` / `session.updated` | session 字段 + `sessionID` | 创建、改标题 |
| `session.status` | `{ "sessionID", "status": { "type": "busy" 或 "idle" } }` | 回合起止 |
| `session.idle` | `{ "sessionID" }` | 回合结束 |
| `message.updated` | `{ "info": { "id", "sessionID", "role", "time" } }` | 新行 |
| `message.part.updated` | `{ "part": ... }` | 声明或写完 part |
| `message.part.delta` | 见上 | 流式 |
| `todo.updated` | `{ "sessionID", "todos": [] }` | `todo_*` 之后 |
| `permission.asked` | 下方 permission 对象 | 创建 pending permission |
| `permission.replied` | `{ "sessionID", "requestID" }` | reply 成功并删除 pending permission |

**permission（v1）**：

```json
{ "id": "prm_...", "sessionID": "<id>", "permission": "<工具名>", "patterns": ["<原因或命令摘要>"], "metadata": {}, "always": [] }
```

回复体为 `{ "reply": "once" }`、`{ "reply": "always" }` 或 `{ "reply": "reject" }`。本期将 `once` 和 `always` 都视为当前请求获批，不持久化 `always` 规则；`reject` 拒绝当前请求。

**provider.list 最小形状**：`all` 必须包含以下四项。

| provider id | model id |
|---|---|
| `mock` | `mock-1` |
| `echo` | `echo-1` |
| `anthropic` | `claude-sonnet-4` |
| `openai` | `gpt-4o` |

```json
{
  "all": [
    { "id": "mock", "name": "OpenPaw mock", "models": { "mock-1": { "id": "mock-1", "name": "mock-1" } } },
    { "id": "echo", "name": "OpenPaw echo", "models": { "echo-1": { "id": "echo-1", "name": "echo-1" } } },
    { "id": "anthropic", "name": "Anthropic", "models": { "claude-sonnet-4": { "id": "claude-sonnet-4", "name": "claude-sonnet-4" } } },
    { "id": "openai", "name": "OpenAI", "models": { "gpt-4o": { "id": "gpt-4o", "name": "gpt-4o" } } }
  ]
}
```

消息仍写入 `<OPENPAW_HOME>/sessions/<id>.jsonl`。本期不实现 `OPENCODE_CONFIG` 插件、Den provider、MCP 管理、`question.asked`、CDP 和 OpenCode v2。

### 3.3 OpenWork 侧测试

`packages/headless-threads` 只覆盖 create、prompt、snapshot 和 abort，不能证明桌面事件同步。桌面验收使用 SDK 和 `session-sync.ts`，旅程文件放在 evals：

- `evals/worlds/openpaw-engine.ts`：拉起 `openpaw --engine-host`，再 `bootServer`，设 `OPENWORK_OPENCODE_BASE_URL`，不开 `OPENWORK_MANAGE_OPENCODE=1`
- `evals/specs/openpaw-engine-attach.test.ts`：`spec.world(...)`，建 session，`prompt_async`，等到 `GET .../message` 有 assistant 文本
- spec 不得导入 `../../apps`。缺少 OpenPaw 二进制时抛出 `SkipError`，但正式验收不得以 skip 作为通过。验收命令为 `pnpm evals:pr specs/openpaw-engine-attach.test.ts`

sidecar 继续当 MCP 工具用，跟这条换引擎的路无关。

## 4. B 线：AI Office

### 4.1 动画线（B-1）：office actions

T10 修改 host 的 `office.rs`，并在 `runtime.rs`、`events.rs`、`approval.rs` 接入 turn 开始、工具开始、审批请求和 turn 结束四类动作。AI Office 的动作类型、轮询客户端和 dispatcher 不变。host 向 `OPENPAW_OFFICE_ACTIONS_URL` 发送 POST；默认地址为 `http://127.0.0.1:8765/actions`。请求失败不改变 turn 的执行结果。

gateway 接受以下三种 JSON：

```json
{ "type": "desk_visit", "visitor": 1, "host": 3, "message": "read_file" }
```

```json
{ "type": "desk_visit_tour", "visitor": 1, "hosts": [2, 3, 4], "message": "处理工具调用" }
```

```json
{ "type": "set_state", "rosterNo": 1, "state": "thinking", "task": "处理当前回合" }
```

`state` 仅允许 `idle`、`walking`、`working`、`talking` 和 `thinking`。`POST /actions` 成功返回 202；`GET /actions` 返回 `{ "actions": [...] }` 并清空已返回的队列。

T10 只发送 `desk_visit` 和 `set_state`。`desk_visit_tour` 保留为 gateway 的既有能力，本期没有触发规则。`office.rs` 使用单一发送队列，按事件产生顺序 POST；单次请求超时为 500 毫秒，不重试。

### 4.2 名册与动作映射

名册在 `ai-office-react/src/scene/layout/officeLayout.ts`：

| 号 | id | 名字 | 触发条件 |
|---|---|---|---|
| 1 | marvis | 王明 | turn 创建后设为 `thinking`；作为所有 `desk_visit` 的 visitor |
| 2 | code-agent | 李研 | `web_search`、`web_fetch`、`web_extract` |
| 3 | file-agent | 周理 | `read_file`、`grep`、`glob` |
| 4 | app-agent | 陈书 | `write_file`、`edit_file`、`notebook_edit` |
| 5 | review-agent | 刘市 | turn 正常结束或报错后设为 `working`，task 为“整理回合结果” |
| 6 | data-agent | 赵审 | `ClassifierStatus::SuggestConfirm` 时设为 `working`，task 为“等待审批” |

turn 开始时，先将上一轮处于 `working` 的角色设为 `idle`，再将王明设为 `thinking`。每个 turn 最多发送一个 `desk_visit`：第一个匹配上表的 `ToolUseStart` 决定 host；后续工具调用只更新对应角色的 `set_state`。收到审批请求时，发送赵审的 `set_state`，不追加 `desk_visit`。turn 结束时，将王明和工具角色设为 `idle`，将刘市设为 `working`；刘市保持该状态至下一轮开始。六个角色是同一 `AgentLoop` 的状态展示，不代表六个 session 或六个独立 agent。

### 4.3 数据线（B-2）：gateway 只读接口

T11 扩展 `ai-office-react/scripts/action-gateway.mjs`，端口仍为 8765。所有 `/office/*` 接口直接返回 JSON 数组，只读文件系统，不写入 `OPENPAW_HOME`。

| 接口 | 数据源与返回字段 |
|---|---|
| `GET/POST /actions`、`GET /health` | 响应形状按 §4.1；`GET /health` 返回 `{ ok, queued }` |
| `GET /office/sessions` | 扫描 `<home>/sessions/*.jsonl`，返回 `{ id, title, provider, model, createdAt, updatedAt }`；`title` 取首条用户文本的前 80 个字符，缺少文本时为 `New session`；`updatedAt` 取文件 mtime；按 `updatedAt` 倒序，最多 50 条 |
| `GET /office/skills` | 扫描 `<home>/skills/*/SKILL.md`，返回 `{ name, path }`；`name` 取 frontmatter，缺少时取目录名；`path` 相对 `<home>`；按 `name` 升序 |
| `GET /office/files` | 扫描工作区两层，返回 `{ path, type }`；`path` 相对工作区，`type` 为 `file` 或 `directory`；跳过 `.git`、`node_modules` 和名称以 `.` 开头的项；按 `path` 升序，最多 200 条 |
| `GET /office/tasks` | 扫描 `<home>/goals/*.md` 的 frontmatter，返回 `{ id, objective, status, priority, nextAction }`；按文件名降序 |
| `GET /office/agents` | 返回 `{ rosterNo, id, name, state, task }`；按 `rosterNo` 升序，状态来自最近一次 `set_state`，没有记录时为 `idle` |

`OPENPAW_HOME` 未设置时使用平台 home 下的 `.openpaw`；工作区由 `OPENPAW_OFFICE_WORKSPACE` 指定。目录不存在时返回 200 和 `[]`。单个文件无法解析时跳过该文件，不影响其他结果。

### 4.4 前端侧栏

- `OfficeDashboardChrome.tsx` 将“任务、技能、文件、对话”四个导航项绑定到对应 `/office/*` 接口。
- `officeMode.ts` 从 `VITE_OFFICE_HTTP_ACTIONS_URL` 解析 origin；未设置时使用 `http://localhost:8765`。
- 对话面板只显示 session 标题、provider、model 和更新时间，不加载消息正文。
- 浏览器和日历显示“不支持”空态，不发网络请求。
- 空数组显示“暂无数据”；请求失败显示“无法连接办公室网关”。删除现有静态任务数据。`OfficeActionConnector` 仍只负责动画。

## 5. 计划

三人于 09-09 同时开始，09-30 完成。OpenPaw 负责人按 T1→T2→T3→T4→T5 执行；OpenWork 负责人先完成设置页和 evals 骨架，T4 完成后开始真实联调；AI Office 负责人按 T11→T12 执行，并在 T10 完成后验收动画。T8 是保留编号，不计入本期里程碑。

### 5.1 任务

**引擎（Rust）**

| ID | 任务 | 交付 | 验收 | 前置 |
|---|---|---|---|---|
| T1 | 闸门与脚手架 | 行数上限、三处 Cargo.toml、七个模块声明 | `xtask lines` 退出码 0；feature 构建成功；默认依赖树不含 axum | — |
| T2 | 协议类型与夹具 | `protocol.rs`；§3.2 JSON 夹具 | host 测试退出码 0，夹具完成序列化往返 | T1 |
| T3 | HTTP 基础面 | §3.2 路由、Basic Authentication、就绪标记 | host 测试退出码 0；curl 验证所有必做与降级路由；mock 和 echo 均完成 create、prompt、message、abort | T2 |
| T4 | SSE | `events.rs` 和 broadcast channel | host 测试退出码 0；SSE `type` 序列与 §2.5 一致 | T3 |
| T5 | 审批 | pending、reply、abort 清理 | 自动审批测试通过；manual 模式验证 asked、reply、replied 和 abort 清理 | T4 |
| T6 | CLI 旗标 | 三个全局旗标；`help_snapshot.rs` | engine-host 和 sidecar 两组 CLI 测试退出码均为 0 | T3 |

**OpenWork**

| ID | 任务 | 交付 | 验收 | 前置 |
|---|---|---|---|---|
| T7 | attached 联调 | §3.1 配置；引擎与不支持能力标识 | 桌面完成建会话、流式文本、工具 part、审批和终止 | T4；审批部分依赖 T5 |
| T8 | managed spawn（延期） | 本期无交付 | 不计入 M1～M6 | — |
| T9 | evals | world 和 spec 两个文件 | `pnpm evals:pr specs/openpaw-engine-attach.test.ts` 输出 `Passed`，没有 skip | T7 |

**办公室**

| ID | 任务 | 交付 | 验收 | 前置 |
|---|---|---|---|---|
| T10 | 动画 | `office.rs` 和 §4.2 的事件映射 | mock turn 后 `/actions` 中的类型、rosterNo、visitor 和 host 与 §4.2 一致；每个 turn 不超过一个 visit | T4 |
| T11 | 只读接口 | 五个 `/office/*` GET 路由 | 五个接口字段、排序和条数限制符合 §4.3；空目录均返回 200 和 `[]` | — |
| T12 | 侧栏 | 四个导航面板和空态 | 四个面板显示网关数据；网关不可用时显示空态；代码中不再引用静态任务数组 | T11 |

**收尾**

| ID | 任务 | 交付 | 验收 | 前置 |
|---|---|---|---|---|
| T13 | 三方联调 | 聊天 → SSE → 动画 → 侧栏 | §8.1 六个里程碑全部满足 | T7、T9、T10、T12 |

### 5.2 排期（09-09 ~ 09-30）

| 负责人 | 09-09～09-10 | 09-11～09-16 | 09-17～09-22 | 09-23～09-25 | 09-26～09-30 |
|---|---|---|---|---|---|
| OpenPaw | T1、T2 | T3、T6 | T4、T5 | T10；修复 OpenWork 联调问题 | T13；回归修复 |
| OpenWork | 设置页引擎标识；T9 world 骨架 | T9 spec 骨架和 mock | T7 文本与工具联调 | T7 审批和终止；T9 正式运行 | T13；回归修复 |
| AI Office | T11 sessions、skills | T11 files、tasks、agents | T12 四个面板 | T10 动画验收；T12 空态 | T13；回归修复 |

| 里程碑 | 截止日 | 条件 |
|---|---|---|
| M1 | 09-10 | T1、T2 完成 |
| M2 | 09-16 | T3、T6 完成 |
| M3 | 09-22 | T4、T5 完成 |
| M4 | 09-25 | T7、T9 完成 |
| M5 | 09-25 | T10、T11、T12 完成 |
| M6 | 09-30 | T13 完成，§8.3 全部勾选 |

T4 若在 OpenWork 联调中出现事件结构不兼容，OpenPaw 负责人先修复 `message.updated`、text part 和 `session.status`，确保 T9 可运行；tool part 和 permission 必须在 09-25 前完成。manual 审批未完成时，M4 不通过，不得用自动审批替代。

## 6. 责任与交接

| 负责人 | 仓库 | 任务 | 交接物 |
|---|---|---|---|
| OpenPaw | openpaw | T1–T6、T10 | host 二进制、监听地址、测试命令与退出码 |
| OpenWork | openwork | T7、T9 | attached 配置、evals 结果、桌面手测结果 |
| AI Office | ai-office-react | T11、T12；T10 动画验收 | gateway 地址、接口样例、前端手测结果 |

分支：openpaw `feat/engine-host`，openwork `feat/openpaw-attach`，ai-office-react `feat/openpaw-sidebar`。

每个 PR 记录实际执行的命令、退出码和未执行项。skip 不计为通过。OpenPaw PR 必须附 `cargo run -p xtask -- lines` 的结果。协议变更先修改本文 §3.2，经三人确认后再修改 host。PR 作者不得批准自己的 PR。

三人在 09-16 检查 M2，在 09-25 检查 M4 和 M5。检查结果只记完成、未完成和阻塞项；协议结论直接更新本文 §3.2。

## 7. 风险与处理

| # | 风险 | 处理 | 负责人 |
|---|---|---|---|
| 1 | OpenCode SDK 升级导致协议不兼容 | 本期以 1.18.15 为兼容基线；版本变化时先更新 §3.2 和夹具，再修改 host | OpenWork |
| 2 | `message.part.delta` 的 `field` 不可靠 | 先发 `message.part.updated` 声明 part；测试对序列，不对全文 | 引擎 |
| 3 | 等待审批时无法终止 turn | abort 同时取消 `AgentLoop` 和 pending permission；manual 测试必须覆盖 | 引擎 |
| 4 | host 超过 5000 行 | 兼容类型保留在 host；拆分 host 子模块；每个 PR 运行行数检查 | 引擎 |
| 5 | abort 返回成功但 turn 继续运行 | `prompt_async` 后台执行；abort 调 `AgentLoop::cancel()`；使用 echo 长流验证停止 | 引擎 |
| 6 | visit 动作堆积 | 每个 turn 最多一个 visit，其余动作只更新 state | 引擎、办公室 |
| 7 | 空目录仍显示静态任务 | 删除静态数据；目录不存在和请求失败均显示空态 | 办公室 |
| 8 | Windows / macOS 路径不一致 | `OPENPAW_HOME` 走平台默认 `~/.openpaw`；工作区 `OPENPAW_OFFICE_WORKSPACE`；联调至少一台 Windows | 办公室 |
| 9 | UI 将六个角色表述为独立 agent | UI 统一表述为“状态角色”，不显示独立 session 或独立执行能力 | 办公室 |

## 8. 验收

### 8.1 里程碑命令

| 里程碑 | 命令或操作 | 通过标准 |
|---|---|---|
| M1 | `cargo run -p xtask -- lines`；`cargo build -p openpaw-cli --features engine-host`；检查默认 `cargo tree` | 两条命令退出码为 0；host 不超过 5000 行；默认依赖树不含 axum |
| M2 | `cargo test -p openpaw-engine-host --features engine-host`；分别用 mock、echo 走 health、create、prompt、message、abort | 测试退出码为 0；两种 provider 均返回 assistant 文本；abort 后 status 不再为 busy |
| M3 | 运行 SSE 序列测试、manual 审批测试和 `cargo test -p openpaw-cli --features sidecar` | 命令退出码均为 0；事件顺序符合 §2.5；reply 和 abort 后 `/permission` 不含原请求 |
| M4 | `pnpm evals:pr specs/openpaw-engine-attach.test.ts`；执行 §8.2 的 OpenWork 步骤 | 命令输出 `Passed` 且没有 skip；桌面完成文本、工具、审批和终止 |
| M5 | 执行 §8.2 的 AI Office 步骤 | `/actions` 符合 §4.2；五个 `/office/*` 接口符合 §4.3；四个面板分别显示数据、空数据和连接失败状态 |
| M6 | 重跑 M1～M5 | 所有命令满足对应通过标准，三方联调没有未解决的阻塞项 |

### 8.2 联调环境与步骤

```powershell
# 引擎
$env:OPENPAW_HOME = "<home>"
$env:OPENCODE_SERVER_USERNAME = "dev"
$env:OPENCODE_SERVER_PASSWORD = "dev"
openpaw --engine-host --engine-host-hostname 127.0.0.1 --engine-host-port 0 --provider mock
# 记下 stdout 就绪行里的端口 <port>

# OpenWork attached
$env:OPENWORK_OPENCODE_BASE_URL = "http://127.0.0.1:<port>"
$env:OPENWORK_OPENCODE_USERNAME = "dev"
$env:OPENWORK_OPENCODE_PASSWORD = "dev"

# 办公室
$env:OPENPAW_OFFICE_ACTIONS_URL = "http://127.0.0.1:8765/actions"
$env:OPENPAW_OFFICE_WORKSPACE = "<workspace>"
node scripts/action-gateway.mjs   # ai-office-react 侧，默认 8765
```

手测使用两个 session：

1. Session A 使用能产生文本、`read_file` 和 Confirm 的 mock 场景。确认流式文本持续追加，tool part 从 `running` 变为 `completed`，审批列表出现请求；回复 `once` 后收到 `permission.replied`，`/permission` 不再包含该请求。
2. Session B 使用 echo 长输出。在输出结束前调用 abort；确认 session status 变为 idle，之后不再出现该 session 的文本 delta。
3. 读取 `/actions`；确认本轮最多一个 `desk_visit`，visitor 为 1，`read_file` 的 host 为 3，并包含审批和回合结束的 `set_state`。
4. 打开任务、技能、文件和对话面板；分别验证有数据、空目录和 gateway 停止三种状态。

### 8.3 完成清单

PR 作者执行 §8.1 中与改动对应的命令，并记录退出码。未执行项必须注明原因。T13 由三人共同复跑 M1～M5。

- [ ] `cargo run -p xtask -- lines`：host ≤ 5000
- [ ] `cargo test -p openpaw-engine-host --features engine-host`：退出码 0
- [ ] `cargo test -p openpaw-cli --features sidecar`：退出码 0
- [ ] `pnpm evals:pr specs/openpaw-engine-attach.test.ts`：`Passed`，无 skip
- [ ] attached 桌面完成流式文本、工具 part、审批和终止
- [ ] 每个 turn 最多一个 visit，角色状态符合 §4.2
- [ ] 五个 `/office/*` 接口和四个侧栏面板符合 §4.3、§4.4
- [ ] 设置页显示 `Engine: OpenPaw (attached)`；浏览器、日历和插件显示“不支持”
- [ ] 关闭 `engine-host` feature 时，默认依赖树不含 axum
- [ ] 三个仓库的 PR 均记录验证命令、退出码和未执行项

## 9. 遗留

| # | 问题 | 影响 | 提出 | 状态 |
|---|---|---|---|---|
| 1 | managed spawn 不在本期范围 | 用户需手动启动 host 并配置 attached 地址 | 交付范围 | 09-30 后另行排期 |
