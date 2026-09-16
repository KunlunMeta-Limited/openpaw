# Engine Host 会话与 OpenWork 前端协议修复计划

## 记录信息

- 日期：2026-09-15
- 来源任务 ID：T3、T4、T7、T9
- 变更 ID：`2026-09-15-engine-host-session-ui-contract`
- 开发依据：`docs/engine-host/master.md` §2.3、§2.5、§3.2、§3.3、§8
- 飞书每日记录：`https://my.feishu.cn/wiki/XJyqwn1PQieHWHkHQE9ctVGcn5e`
- OpenPaw 基线 commit：`12a44e5172ca89aed2498171efa3da526f958f57`
- 当前状态：Engine Host 实现与真实链路验证已完成；Git 提交待授权

## 背景与复现证据

OpenWork 前端在新会话发送首条消息前执行：

```text
POST /workspace/ws_2801c08e0351/opencode/session?directory=%2Fopt%2Fopenwork
Content-Length: 0
```

`@opencode-ai/sdk` 在请求正文为空时删除 `Content-Type`。OpenWork Server 保留请求并将其转发给 Engine Host；Host 的 `POST /session` 当前使用强制 `Json<CreateSession>` 提取器，因此在创建 session 前返回 `415 Unsupported Media Type`：

```text
Expected request with `Content-Type: application/json`
```

这不是 Provider、API Key、认证或浏览器缓存问题。团队协议规定 `POST /session` 成功返回 session 对象，请求正文只“可带” `title`；工作区按 `x-opencode-directory`、`directory` 查询参数、CLI 默认工作区取值。

当前 OpenWork 容器实际安装 `@opencode-ai/sdk 1.18.25`，团队文档的兼容基线为 1.18.15。本次不新增或改变协议形状：§3.2 已明确 create body 可选，修复属于让 Host 落实既有契约。测试同时固定当前 1.18.25 的空正文行为；若后续发现 1.18.15 与 1.18.25 在其他字段上存在差异，再先更新团队协议和 fixture，不在本变更内推断扩展。

进一步检查发现：

- HTTP create 当前独立生成 `ses_<uuid>`。
- `prompt_async` 每轮新建 `EngineSession`，该对象再次生成自己的 `meta.id`。
- 同一 OpenWork session 没有复用同一个 OpenPaw `EngineSession`，多轮上下文和持久化身份不一致。
- 历史记录仍是原始事件数组，没有形成 OpenWork 需要的 `{ info, parts }` message snapshot。
- SSE 的部分 message/part 事件缺少稳定的 `sessionID`、`messageID`、`partID`，桌面同步层会忽略这些事件。

## 目标

保持 OpenWork 现有 SDK 和代理层不变，在 Engine Host 内完成 OpenCode v1 会话契约：

1. 兼容空正文与可选 JSON 正文创建 session。
2. 从协议规定的输入读取工作区，并返回完整 session 对象。
3. 使用 OpenPaw `EngineSession.meta.id` 作为对外唯一 session ID。
4. 同一 session 的多次 prompt 复用同一个 `EngineSession`。
5. 返回正式 message snapshot，并输出可被 OpenWork `session-sync.ts` 消费的 SSE 事件。
6. 在现有 Docker/镜像环境中完成 Host、OpenWork 代理和真实前端验证。

## 范围

代码仅修改 `crates/openpaw-engine-host/src/` 中的既有文件：

- `lib.rs`：注册团队协议要求的 `/event`、`/global/event` 和 `/api/event` 全局 SSE 路由。
- `http.rs`：解析可选 create body、查询参数和目录 header；创建/获取运行态 session；提交 prompt。
- `runtime.rs`：保存并并发访问真实 `EngineSession`；统一 session ID、历史、事件、取消和持久化关联。
- `protocol.rs`：补齐 create 输入、message info、part 和事件字段；增加 serde fixture。
- `events.rs`：将 `AgentEvent` 转换为带稳定 ID 的正式 OpenCode message/part/SSE 序列。
- `approval.rs`：使 pending permission 携带所属 session ID，并把审批事件发送到同一全局 SSE 流。

本次不修改 OpenWork，不新增 Provider 模块，不修改 `openpaw-llm`、CLI、sandbox、Dockerfile 或 Engine Host 目录外代码。若实现证明必须扩大范围，先更新飞书每日记录、本文件和 JYZS 总体记录。

## 设计

### 创建会话

`POST /session` 同时支持：

- 空正文，无 `Content-Type`；
- JSON 正文 `{ "title": "..." }`；
- JSON 正文中的可选 `directory`，用于兼容现有调用。

工作区优先级遵循团队协议：

```text
x-opencode-directory
→ directory 查询参数
→ JSON directory（兼容输入）
→ Host 默认工作区
```

若实际代码中的 Host 默认工作区尚无配置入口，本变更只保留明确的安全默认值并记录该限制，不扩大到 CLI。

### Session 身份与运行态

创建请求生成一个真实 `openpaw_core::Session`，以其 `meta.id` 构造 wire session。运行态按该 ID 保存 EngineSession、wire metadata、消息聚合器、broadcast sender、active AgentLoop 和取消状态。

`prompt_async` 必须取得并锁定已有 EngineSession，在其中追加 user message并运行 turn。后台任务结束后保留更新后的 EngineSession，后续 prompt 延续同一上下文。不同 session 可并行；同一 session 已有 active turn 时返回 409。

### Message 与 SSE

每轮至少建立：

- 一个 user `messageID` 及对应 text `partID`；
- 一个 assistant `messageID`；
- assistant text、reasoning、tool 各自稳定的 `partID`。

事件顺序符合 `master.md`：

```text
session.updated（需要时）
→ user message.updated
→ user message.part.updated
→ session.status busy
→ assistant message.updated
→ message.part.updated
→ message.part.delta...
→ 完成后的 message.part.updated / message.updated
→ session.status idle
→ session.idle
```

`GET /session/:id/message` 返回：

```json
[
  {
    "info": {
      "id": "msg_...",
      "sessionID": "<session-id>",
      "role": "assistant",
      "time": { "created": 0, "completed": 0 }
    },
    "parts": []
  }
]
```

## 实施步骤

1. 为无正文 create、目录优先级和 session 响应 ID 增加失败测试。
2. 为 message snapshot 和 SSE 所需字段增加协议 fixture。
3. 调整运行态，使 create 时创建并保存 EngineSession，删除 prompt 时的临时 EngineSession。
4. 调整 HTTP create 和 prompt，处理 415、404、409，并统一 ID。
5. 注册全局 SSE 路由并调整事件聚合，确保历史快照、SSE 和审批使用同一组 session/message/part ID。
6. 运行格式化、Host tests、CLI engine-host 回归和行数预算。
7. 增量构建 `jyzs-openpaw-engine-host:latest`，重新启动现有 Compose 服务。
8. 运行 OpenWork T9 attached eval，并在浏览器完成创建、真实模型回复和 Network 检查。
9. 将实际文件、命令、退出码、证据、未完成项和 commit 回填本文件与 9 月 15 日两层记录。

## 验证方法

优先使用现有 JYZS Docker Compose、`jyzs-openpaw-engine-host:latest` 和 `jyzs-openwork-web:latest`。运行阶段不得下载依赖；源代码修改后只执行必要的增量构建。

### Engine Host

```powershell
cargo fmt --check
cargo test -p openpaw-engine-host --features engine-host
cargo test -p openpaw-cli --features engine-host
cargo run -p xtask -- lines
```

检查：

- 空正文 create 返回 200，不出现 415。
- header/query/body 工作区选择符合既定顺序。
- response ID、slug、EngineSession meta ID、历史和事件 ID 一致。
- 同一 session 第二轮可以看到第一轮上下文。
- 同一 session 并发 prompt 返回 409；不同 session 可并发。
- snapshot 与 SSE fixture 符合团队协议。

### OpenWork 与浏览器

```powershell
docker compose --profile dev up -d --build openpaw-engine-host openwork-web
docker compose --profile dev exec -T -e OPENPAW_ENGINE_HOST_URL=http://openpaw-engine-host:8788 openwork-dev pnpm --dir evals exec vitest run specs/openpaw-engine-attach.test.ts
```

浏览器打开 `http://127.0.0.1:5178`，检查：

- `POST .../session?directory=...` 返回 200。
- response body 包含有效 session ID。
- 后续 `POST .../session/:id/prompt_async` 返回 204。
- SSE 和 message snapshot 出现相同 session/message/part ID。
- 页面显示真实 Provider 回复，结束后 session 为 idle。

## 安全与工作树边界

- Provider 配置继续从 Git 忽略的本机环境加载。
- 不在命令输出、文档、测试夹具、镜像层或 Git 中记录 API Key。
- 当前 OpenPaw 工作树已有 `sidecar.rs`、sandbox、`.cargo/`、Dockerfile 等既有改动；本变更不得编辑或提交这些内容。
- 当前 Engine Host 的 Provider 接入改动尚未提交，本变更应在提交前按文件和 diff 精确审查，避免错误归属。
- OpenWork 当前 SDK 版本高于团队基线；本次仅实现两版共同允许的可选 create body，不据此扩大协议范围。

## 完成标准

- 交付物存在：Engine Host 会话、消息和 SSE 契约实现。
- 自动化验证通过：Host/CLI/行数/T9 均有退出码和无 skip 证据。
- 人工验证通过：OpenWork 前端能创建 session 并显示真实模型回复。
- 两层本地记录和飞书每日记录完成收尾。
- Git 提交只包含经确认的 Engine Host 与项目文档文件。

## 实际实现（2026-09-15）

- `protocol.rs`：补齐可选 create 输入、`messageID`、正式 `{info, parts}` 快照、OpenCode 字段命名、tool state、Provider 列表和 permission reply 兼容反序列化。
- `runtime.rs`：创建并按 `EngineSession.meta.id` 保存真实 EngineSession；保存正式 message snapshot；复用同一会话上下文；实现同 session 运行中冲突、状态、取消和全局事件总线。
- `http.rs`：空正文 create 不再要求 `Content-Type`；按 header、query、body、默认值解析工作区；prompt 复用 EngineSession；返回 404/409；统一向快照和 SSE 写入事件。
- `events.rs`：聚合 assistant message/part，生成稳定的 `sessionID`、`messageID`、`partID`；处理引擎未先发送 `MessageStart` 的首个 delta；映射 text、reasoning、tool 和完成事件。
- `approval.rs`：pending approval 关联 session，session 独立清理，审批事件同步发送到 session 与全局 SSE。
- `lib.rs`：注册 `/event`、`/global/event`、`/api/event`。

未修改 OpenWork、Provider crate、CLI、sandbox 或 Dockerfile。Provider 密钥只存在于 Git 忽略的 `.env`，未进入命令输出、文档、源码或镜像层。

## 验证结果（2026-09-15）

实际环境：Docker Desktop、Rust 1.88 构建镜像、`jyzs-openpaw-engine-host:latest`、`jyzs-openwork-web:latest`，Compose 服务保持运行。

- `cargo fmt --all -- --check`：通过。
- Docker Rust 1.88 release build：通过。
- `cargo test --locked --features engine-host -p openpaw-engine-host`：17 passed，0 failed。
- `cargo test --locked --features engine-host -p openpaw-cli`：通过；CLI 集成测试共 13 passed，0 failed。
- `cargo run --locked -p xtask -- lines`：通过；`openpaw-engine-host` 1367/5000，全部预算满足。
- Host 直连空正文 create：无 Content-Type 和 `application/x-www-form-urlencoded` 两种情况均为 200；返回 32 位 OpenPaw meta ID；目录解析为 `/opt/openwork`。
- Provider：`connected` 包含 `openai`，配置模型可见；未记录密钥。
- Host 真实模型：prompt 204，快照 2 条消息，assistant 返回 `ENGINE_HOST_OK`。
- 全局 `/event`：收到 session、user/assistant message、part delta、busy/idle 完整事件，ID 关联一致。
- OpenWork 代理实测：`POST :8778/.../session` 空正文 200；`prompt_async` 204；快照返回 `OPENWORK_PROXY_OK`。
- 多轮与并发实测：第一轮 204，并发第二轮 409；完成后下一轮 204；四条消息保留上下文，模型正确回忆 `BLUE-731`。
- Compose：`openpaw-engine-host` 和 `openwork-web` healthy。

T9 attached eval 结果为 2 passed、1 failed。失败仅来自 `evals/specs/openpaw-engine-attach.test.ts:20` 的旧断言 `Array.isArray(providers)`；团队协议、OpenWork `ProviderListResponse` 和当前前端均要求 `{all, connected}`。不在本变更中把正确接口退回数组，也不越界修改 OpenWork 测试文件。

隔离构建还确认 OpenPaw `HEAD` 的 `openpaw-sandbox` 存在既有 borrow 编译错误；最终临时构建上下文带入工作区中已有的 sandbox 修复才完成 release build。该文件不是本变更产物，不纳入本变更提交。

## 未完成项与下一步

- 未完成：浏览器自动化不可用，最终页面视觉确认需刷新 `http://127.0.0.1:5178` 后人工查看；T9 旧 Provider 断言需由 OpenWork 测试维护任务单独更新；Git 提交待用户授权。
- 下一步：刷新前端并发送一条消息完成视觉确认；获得授权后仅提交六个 Engine Host 源文件和本变更记录。
- 待确认事项：是否授权本次 Git 提交；是否另开 OpenWork 测试维护任务修正 T9 旧断言。

## 2026-09-15 范围追加：真实工具与人工审批闭环

- 来源任务 ID：T3、T4、T5、T7、T9。
- 追加目标：保持 OpenWork、CLI、`openpaw-tools` 和 Provider crate 不变，只在 Engine Host 装配现有 OpenPaw 工具、会话存储和审批接口，使工具 part、人工审批和 Todo 事件由真实 `AgentLoop` 路径产生。
- 设计边界：复用 `openpaw_tools::arc_registry(workspace)`、`ToolHost::list_tools()`、`AgentLoop::with_tool_host()`、`openpaw_store::SessionWriter`、`AutoApproveManager` 和既有 `ApprovalManager` trait；不得在 Host 内复制工具、Todo、Provider 或 JSONL 编解码实现。
- 拟影响文件：
  - `Cargo.lock`：记录 Engine Host 对既有 workspace `openpaw-store` 的直接依赖，不新增第三方依赖。
  - `crates/openpaw-engine-host/Cargo.toml`：将既有 `openpaw-store` 工作区 crate 接入 `engine-host` feature。
  - `crates/openpaw-engine-host/src/runtime.rs`：按 session 工作区保存现有 ToolHost/specs；提供自动与 manual 审批装配；支持团队验收规定的 mock tools 模式；使用正式 session JSONL writer。
  - `crates/openpaw-engine-host/src/http.rs`：向 `AgentLoop` 注入 ToolHost、guardrail 和真实 ToolSpec，持久化本轮 EngineSession 消息。
  - `crates/openpaw-engine-host/src/events.rs`：补齐 `todo_*` ToolResult 到 `todo.updated` 的映射。
  - `crates/openpaw-engine-host/src/approval.rs`：按本期协议让 `once` 与 `always` 仅批准当前请求，并覆盖允许、拒绝、始终允许和 abort 清理。
- 明确不纳入：Host 进程启动时恢复旧会话；`GET /session/:id/todo` 继续按团队 §3.2 暂返回 `[]`。两项均不阻塞本期 A 线最低验收。

### 实施步骤

1. 为 mock tools、manual/auto 审批选择、`always` 当前请求语义和 Todo 事件增加回归测试。
2. 在 session 创建时使用目录构造 `arc_registry(workspace)` 并读取现有 ToolSpec。
3. 在 prompt 执行时调用 `with_tool_host`，把 ToolSpec 传入 `run_turn`，不复制 CLI 执行代码。
4. 依据 `OPENPAW_ENGINE_PERMISSIONS=manual` 选择 Host 审批；未设置时使用 `AutoApproveManager`。
5. 将 EngineSession 的 meta/message 通过 `SessionWriter` 写入 `<OPENPAW_HOME>/sessions/<id>.jsonl`，停止把 OpenCode SSE wire event 当作会话记录写入 `engine-sessions`。
6. 使用 `OPENPAW_ENGINE_MOCK=tools` 在 Docker 中完成 tool running/completed、permission asked/replied、once/always/reject 和 abort 验证。
7. 通过 OpenWork attached 代理和前端验证工具 part、审批弹层、回复与 idle 状态。

### 验证方法

```powershell
cargo fmt --all -- --check
cargo test --locked --features engine-host -p openpaw-engine-host
cargo test --locked --features engine-host -p openpaw-cli
cargo run --locked -p xtask -- lines
docker compose --profile dev up -d --build openpaw-engine-host openwork-web
```

Docker 手测使用团队规定的两组环境：`OPENPAW_ENGINE_MOCK=tools` 与 `OPENPAW_ENGINE_PERMISSIONS=manual` 验证工具和审批；`OPENPAW_ENGINE_MOCK=stream` 验证 abort。运行阶段不下载依赖。

- 当前状态：文档准备完成，代码待执行。
- 待确认事项：无。

### 实际实现与验证结果

- `Cargo.lock`、`crates/openpaw-engine-host/Cargo.toml`：Host feature 直接引用现有 workspace `openpaw-store`，没有新增第三方依赖。
- `runtime.rs`：支持团队规定的 `OPENPAW_ENGINE_MOCK=tools/stream`；tools 模式使用现有 `MockProvider::scripted` 发起一次 `read_file`；会话创建后通过 `SessionWriter` 写入 `<OPENPAW_HOME>/sessions/<id>.jsonl`；停止写入自定义 `engine-sessions` wire event 文件。
- `http.rs`：每轮按 session directory 构造 `openpaw_tools::arc_registry`，用 `list_tools()` 取得已有 21 个 ToolSpec，注入 `AgentLoop::with_tool_host` 并把 specs 传入 `run_turn`；manual 模式使用 Host 审批与确认护栏，其他模式使用现有 `AutoApproveManager`；将 user/assistant EngineMessage 写入正式 transcript。
- `approval.rs`：manual 护栏通过现有 `ToolGuardrail` 返回 `Confirm`；删除跨请求 `always` 记忆，`once` 和 `always` 均只批准当前请求；保留 reject 和 abort 清理。
- `events.rs`：真实工具结果继续更新同一个 tool part；`todo_add`、`todo_update`、`todo_list` 结果增加团队协议要求的 `todo.updated {sessionID,todos:[]}`。

Docker 验证：

- `cargo fmt --all -- --check`：通过；`git diff --check`：通过。
- `cargo test --locked --offline --features engine-host -p openpaw-engine-host`：19 passed，0 failed。
- `cargo test --locked --offline --features engine-host -p openpaw-cli`：13 passed，0 failed。
- `cargo run --locked --offline -p xtask -- lines`：通过；Host 1481/5000，全部预算满足。
- 默认 `openpaw-cli` dependency tree：不包含 axum。
- Compose release 镜像构建成功，镜像 `jyzs-openpaw-engine-host:latest`；Host 与 OpenWork Web healthy。
- manual HTTP/SSE：`read_file` tool part 从 running 到 completed；`permission.asked`、`permission.replied`、`session.idle` 均到达；正式 transcript 含 meta、workspace metadata 和 3 条消息记录。
- 审批路径：once 通过；reject 产生 error tool part；always 只批准当前请求，下一轮再次出现 permission；abort 清空 pending 并结束 busy；auto 模式无 pending 且工具 completed。
- OpenWork 8778 attached 代理：create 200、prompt 204、permission list/reply 204、message 200、tool completed。代理使用容器内运行态 token，凭据未输出或写入文档。
- T9 eval：仍为 2 passed、1 failed；唯一失败仍是 OpenWork 旧断言要求 `/provider` 返回数组，团队协议和 SDK 当前要求 `{all, connected}`，与本次工具/审批实现无关。

环境证据与限制：

- 本机缺少 MSVC `link.exe`，因此编译和测试按规则使用 Docker。离线测试容器复用现有 Cargo volumes；Compose 镜像构建阶段下载依赖，最终运行镜像启动与联调没有下载依赖。
- OpenWork 容器报告 workspace `/opt/openwork`；Engine Host 容器默认未共享这一目录。本次为联调在当前 Host 容器建立同路径 fixture 后完成真实 `read_file`。生产容器若要让文件读写作用于真实 OpenWork workspace，Compose 仍需把同一 workspace 以相同容器路径挂载给双方；该根 Compose 变更不在本次“只改 Engine Host”范围。
- 当前调试容器保持运行，`OPENPAW_ENGINE_PERMISSIONS=manual`、`OPENPAW_ENGINE_MOCK=tools`；默认 provider 配置保持原状。

- 当前状态：Engine Host 真实工具与审批代码完成并通过 Host、CLI、HTTP/SSE、auto/manual 和 OpenWork 代理验证；Git 提交待授权。
- 未完成项：OpenWork T9 旧 Provider 断言；生产 Compose 共享 workspace 挂载；浏览器视觉自动化受 Codex 浏览器鉴权不可用影响。
- 待确认事项：是否授权提交本次 Engine Host 代码和项目记录；是否另行安排根 Compose 共享 workspace 与 OpenWork T9 断言修复。
