# T2 协议类型与 §3.2 JSON 夹具

## 记录信息

- 日期：2026-09-17
- 来源任务 ID：T2
- 变更 ID：`2026-09-17-t2-protocol-fixtures`
- 前置：T1（已完成，见 `2026-09-11-t1-t6-engine-host.md`）
- 开发依据：`docs/engine-host/master.md` §3.2（协议快照，2026-09-08）、§5.1 T2 行；`docs/个人工作计划-A线.md` §T2；`docs/个人任务分析与理解-A线.md`
- OpenPaw 基线 commit：`3b85244e7206bc4cf6c6b98344d3b03d3e26e59e`
- 当前状态：实现与验证完成，未提交（详见「实际实现」「验证结果」）

## 背景

T2 的交付是「`protocol.rs` 定义协议类型 + 固化 §3.2 JSON 夹具 + 完成序列化往返」。核对发现实际只落地了类型与内联测试：

- `crates/openpaw-engine-host/` 下只有 `Cargo.toml` 和 `src/`，**`tests/` 目录不存在**；`git log --all` 显示该 crate 历史上从未出现 `tests/`。
- §3.2 的 JSON 仅以 `protocol.rs` 内联 `#[cfg(test)] mod tests` 的 6 个测试存在，覆盖 `Status`、`Event<Status>`、`PermissionReply`、`Prompt`、`Message`（仅 assistant + text part）、`ProviderList`。
- 未覆盖：session 对象、user message、reasoning part、tool part、permission 对象、todo，以及除 `session.status` 外的 SSE 事件。
- `ToolState.output` / `error` 带 `skip_serializing_if = "String::is_empty"`，导致 §3.2 的 tool part 快照 `{"output":"","error":""}` 反序列化后再序列化会丢键，**往返不成立**。

因此 T2 的「夹具」与「每条夹具均可往返」两项均未满足。

## 目标

1. 让 §3.2 的每一类代表性 JSON 都有对应冻结类型，并能**反序列化 → 序列化原样往返**。
2. 把夹具从内联字符串提升为 `tests/fixtures/**` 文件，并让夹具清单本身受测试约束，避免新增类别被遗漏。
3. 保证线上 ID 字段一律驼峰（`sessionID`/`messageID`/`partID`/`callID`/`providerID`/`modelID`/`requestID`），且不存在 snake_case 泄漏。
4. 把与 §3.2 快照的差异显式列表，不宣称「零差异」了事。

## 范围

### 拟影响文件

- `crates/openpaw-engine-host/src/protocol.rs`
  - `ToolState.output` / `error` 去掉 `skip_serializing_if = "String::is_empty"`（空串始终上线，对应 §3.2 的 tool 快照）。
  - `CreateSession` / `SessionQuery` 的可选字段补 `skip_serializing_if = "Option::is_none"`，使空体夹具 `{}` 可往返（空体行为）。
  - 新增 §3.2「SSE 事件」表中各事件的 properties 类型，以及 `session.error`（实现已发出、§3.2 表未列）。
  - 保留既有 6 个内联测试，另加事件发射与冻结类型的一致性断言。
- `crates/openpaw-engine-host/tests/fixtures/*.json`：§3.2 代表性 JSON。
- `crates/openpaw-engine-host/tests/protocol_fixtures.rs`：遍历夹具做往返、大小写与清单完整性断言。
- 本记录与 `JYZS/docs/development/2026-09-17.md`。

### 明确不做

- **不修改 `runtime.rs`**，`ProviderList.connected` 保持「按已配置凭据动态生成」的既有语义（见下「协议字段差异」D-1）。
- 不修改 `openpaw-core`、`openpaw-engine`、`openpaw-llm`、`openpaw-cli`、`openwork`、`ai-office-react`、Compose。
- 不重构 `events.rs` 的事件构造方式（仍用 `json!`），只把冻结类型与发射结果对齐——T2 不扩大为 T4 的事件重构。
- 不引入新依赖，不改 `Cargo.toml`（用文件内 `#![cfg(feature = "engine-host")]` 门控，保持改动范围只含协议与测试）。

## 设计

### 夹具形态与「和现有框架匹配」的核对结论

先核对仓库既有约定，再定落点：

| 机制 | 位置 | 运行方式 | 是否适用 T2 |
|---|---|---|---|
| crate 内集成测试 | `crates/<c>/tests/*.rs`（14 个 crate 中 10 个有 `tests/`） | `cargo test -p <c>` | **适用**，T2 验收命令正是它 |
| crate 内 fixture 目录 | `crates/openpaw-engine/tests/parity/fixtures/*.json`，经 `tests/common/mod.rs` 用 `env!("CARGO_MANIFEST_DIR")` 读取 | `cargo test -p openpaw-engine` | **适用**，命名与读取方式照此 |
| 工作区根 golden | `tests/golden/**`（`frontmatter`、`sessions`、`knowledge-format`） | `cargo xtask golden` | 不适用，验收命令不含 xtask |
| 契约快照 | `openpaw-core.api.json` | `cargo xtask snapshot --check` | 不适用，仅覆盖 `openpaw-core` 的 trait/type 签名 |
| xtask 自身测试 | `xtask/src/{lines,stability,telemetry}.rs` 内联 `#[test]` | `cargo test -p xtask` | 不适用，测的是 xtask 自己的检查逻辑 |

结论：**其他组件的测试不写在 xtask**。xtask 只持有工作区级工程门（行数、禁词、快照、golden、遥测、冒烟），本身是 `publish = false` 的独立 crate；组件测试留在各自 crate。T2 夹具因此落在 `crates/openpaw-engine-host/tests/fixtures/`，由 `cargo test -p openpaw-engine-host --features engine-host` 覆盖，与 `openpaw-engine` 的 parity fixtures 同构。

### 类型化 SSE 事件

外壳沿用既有 `Event<T>`（`{ "type": ..., "properties": ... }`）。为每类事件补 properties 类型，使 `Event<T>` 真正绑定到冻结形状，而不是 `serde_json::Value`：

`SessionSnapshotProperties`（`session.created`/`session.updated`）、`SessionStatusProperties`、`SessionIdleProperties`、`MessageUpdatedProperties`、`PartUpdatedProperties`、`PartDeltaProperties`、`TodoUpdatedProperties`、`PermissionRepliedProperties`、`SessionErrorProperties`（含 `SessionErrorBody`/`SessionErrorData`）。`permission.asked` 的 properties 就是 permission 对象本身，复用既有 `PermissionRequest`。

### 夹具清单（28 个）

| 类别 | 文件 | 目标类型 |
|---|---|---|
| session | `session.json` | `Session` |
| session（空体行为） | `create-session-empty.json`、`create-session-title.json` | `CreateSession` |
| prompt body | `prompt-body.json` | `Prompt` |
| message | `message-user.json`、`message-assistant.json` | `Message` |
| part | `part-text.json`、`part-reasoning.json` | `Part` |
| part（tool 三态） | `part-tool-running.json`、`part-tool-completed.json`、`part-tool-error.json` | `Part` |
| todo | `todo-list.json` | `Vec<TodoItem>` |
| provider | `provider-list-default.json`、`provider-list-fully-configured.json` | `ProviderList` |
| permission | `permission-request.json`、`permission-reply.json` | `PermissionRequest`、`PermissionReply` |
| status | `status.json` | `Status` |
| SSE | `sse-session-created.json`、`sse-session-status.json`、`sse-session-idle.json`、`sse-message-updated.json`、`sse-message-part-updated.json`、`sse-message-part-delta.json`、`sse-todo-updated.json`、`sse-permission-asked.json`、`sse-permission-replied.json`、`sse-session-error.json` | `Event<...>` |

夹具内容按**实现实际发出的形状**编写（`events.rs`、`approval.rs`、`http.rs`），而不是按 §3.2 的松散文字重写，这样夹具才是真实契约。

### 测试结构

`tests/protocol_fixtures.rs` 用一张 `(文件名, 往返函数)` 表作为**唯一事实来源**：

- `every_fixture_round_trips`：逐条反序列化到目标类型 → 重新序列化 → 与夹具 `serde_json::Value` 全等。
- `fixture_inventory_is_complete`：把表里的文件名与 `tests/fixtures/` 目录实际内容比对，防止加了文件却没接入测试。
- `snapshot_uses_camel_case_ids` 等：在 ID 承载类别上逐层断言驼峰键存在、snake_case 键不存在。

## 协议字段差异（相对 §3.2 快照）

| 编号 | 差异 | 处理 |
|---|---|---|
| D-1 | §3.2 的 `provider.list` JSON 只给 `all`，**没有 `connected` 字段**；团队大纲要求 `connected` 列四项；`runtime.rs` 按凭据动态生成（默认 `["mock","echo"]`，两把 Key 齐备时为四项） | 夹具忠实记录运行时真实状态：`provider-list-default.json` 为 `["mock","echo"]`，`provider-list-fully-configured.json` 为四项。`runtime.rs` 不动。团队大纲文字更正列为待确认（既有 `2026-09-15-provider-list-connected-contract.md` 已把「固定四项」方案撤回） |
| D-2 | `ToolState.durationMs` 不在 §3.2 快照中 | 保留为 `Option<u64>`，`None` 时不输出，因此不含该字段的夹具仍可原样往返；含该字段的 `part-tool-completed.json` 记录实现真实形状 |
| D-3 | `MessageTime.completed` 为 `Option`，`None` 时不输出；§3.2 示例中 `completed` 始终存在 | 与实现一致（用户消息与助手消息在本实现中都带 `completed`），夹具据此编写；`Option` 保持往返安全 |
| D-4 | `session.error` 事件未列入 §3.2 的 SSE 事件表 | 实现已在引擎失败与持久化失败两处发出；本次一并冻结并按 D-4 记录为「实现已发出、快照表未列」 |
| D-5 | `session.created`/`session.updated` 的 properties 形状为 `{sessionID, info}`；§3.2 文字写作「session 字段 + `sessionID`」，未明确 `info` 包裹 | 以实现形状为准冻结（与 OpenCode SDK 的 `{info}` 一致），§3.2 措辞待团队确认 |
| D-6 | §3.2 快照把 running tool 的 state 写成 `{"status":"running","input":{},"output":"","error":""}`；**线上字节实测为空串键被省略**：running 是 `{"input":{},"status":"running"}`，completed 只带 `output`/`durationMs`、无 `error` | **不修改 host 行为**。初版曾把 `skip_serializing_if` 去掉以迁就 §3.2 文字，被线上对拍证明是错的，已回退；夹具按线上形状编写。§3.2 的 `""` 写法列为待团队确认 |
| D-7 | §3.2 的 SSE 事件表含 `message.part.delta`（带 `partID`） | 已类型化为 `PartDeltaProperties`，`partID` 驼峰固定 |
| D-8 | `CreateSession` 原先只派生 `Deserialize`，无法参与往返断言 | **本次修正**：补 `Serialize` 与可选字段的 `skip_serializing_if`，使空体夹具 `{}` 原样往返。纯增派生，不改请求解析行为 |
| D-9 | §3.2 把 `type` 写在 part 对象中部；`#[serde(tag = "type")]` 序列化时把 `type` 写在最前 | **不算字段差异**：JSON 键序无语义，所有往返断言按 `serde_json::Value` 比较。夹具文件保留 §3.2 的书写顺序 |
| D-10 | §3.2 只固定 todo 为「数组，暂 `[]`」，未给单个 todo 对象的字段；`TodoItem` 的 `{id, content, status}` 是从 OpenCode 推断的 | 保留以便 `TodoUpdatedProperties.todos` 有确定元素类型。注意当前 host **尚不产出非空 todo**：`http.rs:249` 的 `/session/:id/todo` 返回 `[]`，`events.rs:294` 的 `todo.updated` 恒发 `todos: []`。因此 `todo-list.json` 的 3 条是**目标形状快照**，不是线上观测值；元素形状需团队确认 |
| D-11 | 线上 tool part 的 `state.input` **恒为 `{}`**（新旧两次抓取均如此）：真实工具入参只出现在 permission 的 `metadata.input`，不进 tool part | 夹具按线上写 `{"input":{}}`。这是 host 的功能缺口（前端从 tool part 拿不到工具入参），不是 §3.2 文字冲突，单独记录待修 |

D-1、D-5、D-6、D-10 需团队按协议维护流程确认；其余为已在本次固化并记录的形状。

## 实施步骤

1. 写本记录（先文档）。
2. 修改 `protocol.rs`：D-6 修正、请求体 `skip_serializing_if`、新增 SSE properties 类型、补事件一致性断言。
3. 新增 `tests/fixtures/*.json` 共 28 个。
4. 新增 `tests/protocol_fixtures.rs`。
5. Docker `rust:1.88-bookworm` 内运行 fmt、`cargo test -p openpaw-engine-host --features engine-host`、`xtask lines`，并做无 feature 的反向编译检查。
6. 追加 `JYZS/docs/development/2026-09-17.md`。

## 验证方法

```bash
cargo fmt --all -- --check
cargo test --locked -p openpaw-engine-host --features engine-host
cargo test --locked -p openpaw-engine-host            # 无 feature：默认构建树不含 axum，集成测试应被 cfg 排除
cargo run --locked -p xtask -- lines
```

通过标准：

- 上述命令退出码均为 0。
- 28 个夹具逐条反序列化并原样序列化往返。
- 往返结果中不存在 snake_case ID 键，驼峰 ID 键按预期存在。
- `provider-list-default.json` 的 `all` 含四项、`connected` 为 `["mock","echo"]`；`provider-list-fully-configured.json` 两项均为四项。
- `xtask lines` 的 `openpaw-engine-host` 预算内。

## 完成标准

- 交付物存在：`protocol.rs` 类型与修正 + `tests/fixtures/` 28 个夹具 + `tests/protocol_fixtures.rs`。
- 自动化验证通过：上表命令退出码均为 0。
- 协议字段差异已列表（本文件「协议字段差异」），未伪称零差异。
- 两层本地记录完成（本文件 + `JYZS/docs/development/2026-09-17.md`）。
- 未提交状态明确标注。

## 实际实现（2026-09-17）

### `crates/openpaw-engine-host/src/protocol.rs`

- `ToolState`：`output` / `error` 去掉 `skip_serializing_if = "String::is_empty"`，保留 `#[serde(default)]`；`durationMs` 保持 `Option` + `None` 不输出（D-6）。
- `CreateSession`：派生补 `Serialize`，可选字段加 `skip_serializing_if = "Option::is_none"`，空体夹具可原样往返（D-8）。`SessionQuery` 保持只读请求类型，未加序列化属性。
- 新增 SSE properties 类型：`SessionSnapshotProperties`、`SessionStatusProperties`、`SessionIdleProperties`、`MessageUpdatedProperties`、`PartUpdatedProperties`、`PartDeltaProperties`、`TodoUpdatedProperties`、`PermissionRepliedProperties`、`SessionErrorProperties`（含 `SessionErrorBody`、`SessionErrorData`）。
- 新增事件类型别名：`SessionCreatedEvent`、`SessionUpdatedEvent`、`SessionStatusEvent`、`SessionIdleEvent`、`MessageUpdatedEvent`、`MessagePartUpdatedEvent`、`MessagePartDeltaEvent`、`TodoUpdatedEvent`、`PermissionAskedEvent`、`PermissionRepliedEvent`、`SessionErrorEvent`。
- 新增 6 个内联测试：`tool_part_snapshot_round_trips_with_empty_output_and_error`、`reasoning_part_snapshot_round_trips`、`empty_create_body_round_trips`、`emitted_timestamps_are_unix_milliseconds`、`emitted_session_events_match_frozen_properties`、`emitted_message_events_match_frozen_properties`。既有 6 个内联测试保留未改。
- 后两者是**防漂移断言**：把 `events.rs` / `approval.rs` 真实发出的 JSON 反序列化进冻结类型再序列化，任何一个发射点改了字段名或形状都会立刻失败，避免「冻结类型」退化成第二份会漂移的真相。

### `crates/openpaw-engine-host/tests/fixtures/`（28 个）

```text
session.json                      create-session-empty.json          create-session-title.json
prompt-body.json                  message-user.json                  message-assistant.json
part-text.json                    part-reasoning.json                part-tool-running.json
part-tool-completed.json          part-tool-error.json               todo-list.json
provider-list-default.json        provider-list-fully-configured.json
permission-request.json           permission-reply.json              status.json
sse-session-created.json          sse-session-updated.json           sse-session-status.json
sse-session-idle.json
sse-message-updated.json          sse-message-part-updated.json      sse-message-part-delta.json
sse-todo-updated.json             sse-permission-asked.json          sse-permission-replied.json
sse-session-error.json
```

### `crates/openpaw-engine-host/tests/protocol_fixtures.rs`

- `fixture_cases!` 宏一次声明「文件名 + 冻结类型」，同时生成 28 个逐文件用例和 `FIXTURE_FILES` 清单，避免清单与用例两份事实。
- 夹具内容经 `fixture!` 宏用 `include_str!(concat!("fixtures/", $file))` **编译期内嵌**，不做运行时路径查找；文件缺失即编译失败。
- `fixture_inventory_matches_disk`：把清单与 `tests/fixtures/` 目录内容排序比对，新增文件未接入用例（或反之）即失败。
- `snapshot_uses_camel_case_ids`：逐层断言 `sessionID` / `messageID` / `partID` / `callID` / `requestID` / `providerID` / `modelID` 存在。
- `running_tool_part_keeps_empty_output_and_error`：断言 `output` / `error` 空串保留、缺失的 `durationMs` 不被凭空添加。
- `provider_snapshots_cover_the_four_snapshot_ids`：`all` 断言四项；默认快照 `connected` 为 `["mock","echo"]`，全配置快照为四项（D-1）。
- `assert_no_snake_case_keys`：递归禁止 7 种 snake_case ID 键上线。
- 文件头 `#![cfg(feature = "engine-host")]`，不改 `Cargo.toml`，默认构建树仍不含 axum。

## 验证结果（2026-09-17）

环境：Docker Desktop；镜像 `rust:1.88-bookworm`；`RUSTUP_TOOLCHAIN=1.88.0`；复用既有 Cargo registry/git/target 卷；源码统一挂载 `/workspace`（清理 `/src` 残留后）；rustfmt 经 `rustup component add rustfmt` 现场安装。本机缺 MSVC `link.exe`，全部编译与测试在容器内执行。

| 命令 | 结果 |
| --- | --- |
| `cargo test --locked -p openpaw-engine-host --features engine-host` | 退出码 **0**；lib **25 passed**、`tests/protocol_fixtures` **33 passed**、doc-tests 0；0 failed |
| 同上，挂载点换成 `/src`（不带代理、不带 `--locked`） | 退出码 **0**；25 + 33 passed，验证夹具不再依赖编译期路径 |
| `cargo test --locked -p openpaw-engine-host`（无 feature） | 退出码 **0**；集成测试被 cfg 排除，0 failed |
| `cargo test --locked --features engine-host -p openpaw-cli` | 退出码 **0**；清理 `/src` 残留后 **13 passed**，无回归 |
| `cargo test --locked -p openpaw-engine` | 退出码 **0**；8 个测试二进制全绿 |
| `cargo test --locked -p openpaw-store` | 退出码 **0**；19 + 3 + 9 passed |
| `cargo fmt --all -- --check` | 退出码 **0** |
| `cargo run --locked -p xtask -- lines` | 退出码 **0**，`lines: all budgets respected` |

行数变化：`openpaw-engine-host` 1481 → **1610**（cap 5000），其余 crate 未变。

测试数变化：engine-host 19 → **25**（lib，+6 内联）+ **33**（新增集成测试）。

### 环境注意点

`xtask` 把 `CARGO_MANIFEST_DIR` 编进产物，挂载点必须与编译时一致。源码挂到 `/src` 时 `xtask lines` 报 `No such file or directory (os error 2)`；挂到 `/workspace` 后正常。后续 xtask 命令统一用 `/workspace`。

### 挂载点陷阱（本轮实测并修复）

同一类问题会打到集成测试上，且**本变更初版确实中招**：`env!("CARGO_MANIFEST_DIR")` 是编译期写死的绝对路径，而 `openpaw-main_openpaw-target` 是跨会话共享的卷。在 `/src` 编译出的测试二进制被缓存后，改从 `/workspace` 运行（源码内容未变，Cargo 判定指纹一致而复用旧二进制），夹具查找就指向 `read /src/crates/openpaw-engine-host/tests/fixtures/...: No such file or directory`，33 个用例全红。

修复方式：

- 夹具内容改为 `include_str!` 编译期内嵌，彻底不依赖运行时的源码路径。
- 只有需要枚举目录的 `fixture_inventory_matches_disk` 仍需路径：改为**先按当前工作目录解析**（Cargo 以包根作为集成测试的工作目录），解析不到才回退到 `CARGO_MANIFEST_DIR`。

验证：同一个共享 target 卷，分别在 `/workspace` 与 `/src` 两个挂载点运行同一命令，均退出码 0（lib 25 passed、集成 33 passed）。`/workspace` 那次刻意使用不带代理、不带 `--locked` 的原始命令。

因此**不需要修改容器挂载命令**：`/workspace` 与 `/src` 都可以，前提是测试不再依赖编译期绝对路径（本变更已修）。

### 共享 target 卷被挂载点污染（本轮发现并清理）

`grep 'CARGO_BIN_EXE_|env!("CARGO_MANIFEST_DIR")'` 显示受编译期绝对路径影响的远不止本变更：

| 位置 | 编译期绝对路径 |
| --- | --- |
| `openpaw-cli/tests/{chat_e2e,cli_session_e2e,hello_e2e,sidecar_e2e}.rs` | `CARGO_BIN_EXE_openpaw`（共 11 处） |
| `openpaw-store/tests/golden.rs` | `CARGO_MANIFEST_DIR` → `../../tests/golden` |
| `openpaw-engine/tests/common/mod.rs` | `CARGO_MANIFEST_DIR` → parity scenarios / fixtures |
| `openpaw-engine-host/tests/protocol_fixtures.rs` | 仅作解析回退（已改为工作目录优先） |

本轮曾误用 `/src` 挂载点编译，**污染了共享的 `openpaw-main_openpaw-target` 卷**。症状：从 `/workspace` 运行 `cargo test --features engine-host -p openpaw-cli` 时 `chat_e2e` 2 例失败，报 `spawn openpaw chat: Os { code: 2, kind: NotFound }`（测试用 `CARGO_BIN_EXE_openpaw` 指向写死的 `/src/target/debug/openpaw`）；**同一个测试改从 `/src` 运行则 2 passed**，据此定位为挂载点问题而非代码回归。

处理：在 `/workspace` 下 `cargo clean -p openpaw-cli -p openpaw-store -p openpaw-engine`（Removed 3014 files, 976 MiB）后重建，`chat_e2e` 恢复 2 passed、CLI 整体退出码 0。

结论：**共享 target 卷必须固定使用同一挂载点**（本项目约定 `/workspace`）。混用挂载点会让所有依赖编译期绝对路径的测试互相污染，报错形态是「找不到二进制/夹具」，很容易被误判为代码回归。

## 线上字节对拍（独立证据）

夹具往返只能证明「类型与夹具一致」，**不能证明夹具与真实线上一致**——这是我最初证据链的薄弱点。为此补做了对拍：用当前源码现建 host 二进制并在容器内起服务，驱动一轮真实回合，把抓到的字节与夹具机械比对。

### 方法

1. `cargo build --features engine-host -p openpaw-cli`（当前工作树源码）→ `./target/debug/openpaw --engine-host --engine-host-port 8799`。
2. `OPENPAW_ENGINE_MOCK=tools` + `OPENPAW_ENGINE_PERMISSIONS=manual`，`OPENPAW_HOME=/tmp/pawhome`（默认 provider 环境）。
3. 订阅 `GET /event`，建 session（空体）→ `prompt_async`（mock）→ 等 `permission.asked` → `POST /permission/:id/reply`（once），抓取全部 SSE 帧。
4. 另抓 `GET /provider`、`GET /session/:id/message`。
5. 用脚本把「真实帧的递归 JSON 路径集合」与「夹具的路径集合」做**双向差集**比对（不是肉眼核对）。

### 结果：12/12 一致

| 比对对象 | 路径数 | 结果 |
| --- | --- | --- |
| `session.created` ↔ `sse-session-created.json` | 11 | MATCH |
| `session.updated` ↔ `sse-session-updated.json` | 11 | MATCH |
| `session.status` ↔ `sse-session-status.json` | 5 | MATCH |
| `session.idle` ↔ `sse-session-idle.json` | 3 | MATCH |
| `message.updated`(user) ↔ `sse-message-updated.json` | 9 | MATCH |
| `message.part.updated`(text) ↔ `sse-message-part-updated.json` | 8 | MATCH |
| `message.part.updated`(tool/running) ↔ `part-tool-running.json` | 12 | MATCH |
| `message.part.updated`(tool/completed) ↔ `part-tool-completed.json` | 14 | MATCH |
| `message.part.delta` ↔ `sse-message-part-delta.json` | 7 | MATCH |
| `permission.asked` ↔ `sse-permission-asked.json` | 10 | MATCH |
| `permission.replied` ↔ `sse-permission-replied.json` | 4 | MATCH |
| `GET /session/:id/message[0]` ↔ `message-user.json` | 13 | MATCH |
| `GET /provider`（默认环境）↔ `provider-list-default.json` | 17 | MATCH |

同时确认驼峰 ID 全部一致：`sessionID` / `messageID` / `partID` / `callID` / `requestID`，且 `message.part.delta` 的 `field`/`delta` 与 `todo.updated` 的 `todos` 形状如 §3.2。

### 对拍纠正了两处「我自己的错」

1. **D-6 我改错了方向。** 初版为了让 §3.2 的 `"output":""`/`"error":""` 能往返，我去掉了 `skip_serializing_if`，**这等于改了 host 的线上输出**。对拍显示真实字节里 running tool 是 `{"input":{},"status":"running"}`，空串键根本不存在。已回退 host 行为、改夹具跟随线上。
2. **D-11 `state.input` 恒为 `{}`。** 我的夹具原本写了 `{"path":...}`，与线上不符（真实入参只在 permission 的 `metadata.input`）。夹具已按线上改为 `{}`。

这次对拍也说明：**「夹具往返通过」不等于「夹具正确」**——真正判定夹具正确性的锚点是线上字节，而线上字节必须由当前源码现建的二进制产生（旧容器镜像是 09-15 的，反映不了本次改动）。

### 仍未由线上验证的部分

- `todo.updated`：本轮 mock 未触发 todo 工具，未抓到真实帧（与 D-10 一致，`todo-list.json` 仍是目标形状）。
- `session.error`：本轮无错误回合，未抓到。
- `part-tool-error.json`：结构取自旧镜像抓到的真实 error 帧（`durationMs` + `error` + `input:{}`，无 `output`），本轮未复现 error 回合。
- `provider-list-fully-configured.json`：默认环境下 `connected` 只有 `["mock","echo"]`；运维容器里 `OPENAI_API_KEY`/`OPENAI_MODEL` 已配置，实测 `connected=["mock","echo","openai"]` 且 openai 模型是配置值 `gpt-5.6-sol` 而非 `gpt-4o`。即 `connected` 与模型 ID 由运行时配置决定，夹具记录的是默认环境。

## T2 验收对照

| T2 要求 | 状态 | 证据 |
| --- | --- | --- |
| `protocol.rs` 定义 session / message / part / provider / permission / todo / status / SSE envelope | 完成 | `Session`、`Message`+`MessageInfo`+`MessageTime`、`Part`（text/reasoning/tool）、`Provider`+`ProviderModel`+`ProviderList`、`PermissionRequest`+`PermissionDecision`+`PermissionReply`、`TodoItem`、`Status`、`Event<T>` 外壳 + 11 个 properties 类型 |
| 字段命名对应快照 | 完成 | 7 处 `#[serde(rename = ...ID)]`；集成测试递归禁止 7 种 snake_case ID 键；线上对拍确认 5 类驼峰 ID 实测一致 |
| 空体行为对应快照 | 完成（协议层） | 线上 `POST /session` 空体实测返回 `{"id","title":"New session","slug","directory","time":{"created","updated"}}`，与 `session.json`/`create-session-empty.json` 一致；§3.2 的 204 空 body 属路由行为，归 T3 |
| 时间单位对应快照（Unix 毫秒） | 完成 | `snapshot_timestamps_are_unix_milliseconds`（夹具层）+ `emitted_timestamps_are_unix_milliseconds`（发射层）；线上实测 `"created":1789691685031` 为毫秒 |
| `tests/fixtures/**` 保存 §3.2 代表性 JSON | 完成，28 个 | `tests/fixtures/`，清单受 `fixture_inventory_matches_disk` 约束 |
| 覆盖 user / assistant message | 完成 | `message-user.json`、`message-assistant.json`；前者经线上 `GET /session/:id/message` 对拍 MATCH |
| 覆盖 text / reasoning / tool part | 完成 | `part-text.json`、`part-reasoning.json`、`part-tool-{running,completed,error}.json`；running/completed 经线上 SSE 对拍 MATCH（reasoning 与 error 态未由线上复现） |
| 覆盖 provider list | 完成 | `provider-list-default.json`（线上 `/provider` 默认环境对拍 MATCH，17 路径）、`provider-list-fully-configured.json` |
| 覆盖 permission | 完成 | `permission-request.json`、`sse-permission-asked.json`、`permission-reply.json`、`sse-permission-replied.json`，后两者经线上对拍 MATCH |
| 覆盖 SSE 事件 | 完成 | §3.2 事件表 10 类全覆盖（`session.created`/`updated`/`status`/`idle`、`message.updated`/`part.updated`/`part.delta`、`todo.updated`、`permission.asked`/`replied`）+ `session.error`（D-4）。除 `todo.updated` 外均有线上对拍 |
| 不向 `openpaw-core` / `openpaw-engine` 移动兼容协议类型 | 完成 | 全仓检索 `sessionID\|messageID\|partID\|callID\|providerID\|modelID`，命中只在 `openpaw-engine-host`；本轮未改这两个 crate |
| `cargo test -p openpaw-engine-host --features engine-host` 退出码 0 | 完成 | 见「验证结果」，全部命令退出码 0 |
| 每类夹具均能反序列化并序列化往返 | 完成 | 28/28 逐文件用例通过（lib 25 + 集成 34） |
| `sessionID`/`messageID`/`partID`/`callID` 等大小写与快照一致 | 完成 | `snapshot_uses_camel_case_ids` + 递归 snake_case 禁令 + 线上路径对拍 |
| `provider` 的 `all` 和 `connected` 均包含四个 ID | **未按字面满足** | `all` 在两份快照与线上均为四项；`connected` 仅「凭据齐备」快照为四项，默认环境实测 `["mock","echo"]`。按用户裁决（方案 A）夹具忠实记录运行时语义，差异见 D-1，待团队裁决 |
| 夹具与**真实线上字节**一致（不止自洽） | 完成 | 「线上字节对拍」节：当前源码现建 host，12 项端口路径集合双向差集全部 MATCH。该项在初版证据链中缺失，由本轮补做 |
| 完成证据：测试命令与退出码 | 完成 | 见「验证结果」 |
| 完成证据：夹具清单 | 完成 | 28 个，见「实际实现」 |
| 完成证据：协议字段差异为零，或列出已先行更新的协议变更 | 完成 | D-1 ~ D-11 已逐条列出，未宣称零差异 |

除「`connected` 均含四项」需团队裁决（D-1，且用户已明确选择不改 `runtime.rs`）外，T2 其余各项均已完成并有可复现证据。**注意 D-6 与 D-11 是线上对拍推翻了本变更初版做法后修正的结果**，说明「夹具自洽」曾给出过错误保证。

## 未完成项与下一步

- **未提交**：本变更仍在工作树，未 `git add` / `commit` / `push`。
- D-1、D-5、D-10 属协议文字/形状问题，需团队按协议维护流程确认后再决定是否改 §3.2 或 `runtime.rs`。
- `events.rs` / `approval.rs` / `http.rs` 仍用 `json!` 构造事件；本次用「发射结果反序列化进冻结类型」的断言约束它们，未把构造改为类型化（属 T4 的事件重构范围）。若后续要做，这些断言即是现成的回归网。
- 未验证真实 HTTP 响应与夹具逐字节一致（需要跑起 host 抓包）；本次只验证了协议层往返与发射点一致性。
- **`aux (tests+xtask)` 行数预算只剩 74 行余量**（9926 / 10000）。本变更新增的集成测试计入该桶；后续再往 `tests/` 加用例会顶到上限，需要先做预算复核或把夹具断言收敛，不要静默超限。

## 待确认事项

- 飞书团队大纲、个人工作计划、09-17 每日记录本次仍未能读取（OAuth 设备授权未完成）；开发依据取自 `docs/个人工作计划-A线.md`、`docs/个人任务分析与理解-A线.md` 与本会话用户转述。飞书原文核对列为待确认。
- D-1：`connected` 的协议文字（团队大纲四项 vs 运行时动态）需团队按协议维护流程裁决。
- D-5：`session.created`/`session.updated` 的 properties 是否以 `info` 包裹需团队确认。
- D-10：§3.2 只固定 todo 为「暂 `[]`」，`TodoItem` 的 `{id, content, status}` 元素形状需团队确认（当前 host 不产出非空 todo）。
