# T4 SSE 事件序列契约

## 记录信息

- 日期：2026-09-17
- 来源任务 ID：T4
- 变更 ID：`2026-09-17-t4-sse-sequence`
- 前置：T3（按 `docs/engine-host/master.md` §5.1；**T3 当前未完成**，见「待确认事项」）
- 开发依据：`docs/engine-host/master.md` §2.5（AgentEvent → OpenCode 事件翻译）、§3.2（SSE 路径与帧格式）、§5.1 T4 行
- OpenPaw 基线 commit：`3b85244e7206bc4cf6c6b98344d3b03d3e26e59e`
- 当前状态：实现与验证完成，未提交（详见「实际实现」「验证结果」）

## 目标

把同一轮 `AgentEvent` 稳定翻译成 OpenWork 当前消费的 OpenCode v1 事件序列，并用测试把 §2.5 的映射与顺序**钉死**，防止后续改动无声破坏序列。

改动性质：**§2.5 的映射已全部实现**（逐条核对见下），本变更以「补测试 + 把回合边界事件归位到 `events.rs`」为主，不改变线上行为。

## 现状核对（§2.5 表逐条）

| §2.5 行 | 实现位置 | 状态 |
| --- | --- | --- |
| `prompt_async`：持久化 user message；user `message.updated` + busy `session.status` | `http.rs:152-156`、`http.rs:162` | 已实现，**无顺序测试** |
| `MessageStart` → `message.updated`(assistant) | `events.rs::start_assistant_message` | 已实现，部分覆盖 |
| `TextDelta` → 首次 text `part.updated`，随后 `part.delta` | `events.rs::append_delta` | 已实现，仅测首个 delta |
| `ThinkingDelta` → 首次 reasoning `part.updated`，随后 `part.delta` | `events.rs::append_delta` | 已实现，**无测试** |
| `ToolUseStart` → tool part `running`，`callID` = `tool_use_id` | `events.rs::start_tool` | 已实现，**无测试** |
| `ToolResult` → `completed` / `error` | `events.rs::finish_tool` | 已实现，**无转换测试** |
| `ClassifierStatus::SuggestConfirm` → `permission.asked` | `approval.rs::request_for` | 已实现，有审批测试 |
| `ClassifierStatus::AutoDenied` → 不发 permission 事件 | `events.rs:46-48` 归空 | 已实现，**无测试** |
| turn 返回 → 写 `time.completed`；idle `session.status` + `session.idle` | `events.rs::finish`、`http.rs:209-210` | 已实现，**无整回合顺序测试** |
| `todo_*` 的 `ToolResult` → `todo.updated` | `events.rs:240-246` | 已实现，有测试 |
| `CompactionPerformed` → 不发事件 | `events.rs:46-48` 归空 | 已实现，**无测试** |
| `/event`、`/global/event`、`/api/event` 提供 SSE；其他 GET 不返回 SSE | `lib.rs:30-33`、`http.rs::events/global_events/sse` | 已实现 |
| broadcast；订阅只收连接后事件，不补历史 | `runtime.rs:200-201,346-350`（`tokio::sync::broadcast`） | 已实现 |

结论：**缺口是测试覆盖（实验标准 2~5），不是功能。**

## 改动范围

### 拟影响文件

- `crates/openpaw-engine-host/src/events.rs`
  - 新增回合边界构造函数：`turn_start_events(session_id)`（busy `session.status`）与 `turn_finish_events(session_id)`（idle `session.status` + `session.idle`）。§2.5 表第 133、141 行本就属于本映射表，此处只是把散落在 `http.rs` 的构造**归位**，内容不变。
  - 新增内联测试模块，覆盖实验标准 2~5。
- `crates/openpaw-engine-host/src/http.rs`
  - 回合开始改为调用 `events::turn_start_events`，回合结束改为调用 `events::turn_finish_events`。**不改变发射内容与时序**。
- 本记录与 `JYZS/docs/development/2026-09-17.md`。

### 明确不做

- 不修改 `openpaw-core`、`openpaw-engine`、`openpaw-llm`、`openpaw-cli`、`openwork`、`ai-office-react`、Compose。
- 不新增依赖，不改 `Cargo.toml`。
- **不改 `openpaw-engine-host/tests/`**：`xtask lines` 的 aux 预算（tests 目录 + xtask/src）当前 **9926 / 10000，仅剩 74 行**；`src/` 内 `#[cfg(test)]` 代码从业务行数中扣除，因此测试写在内联模块里成本为 0（`xtask/src/lines.rs:4,135`）。
- 不为「时间维度」类验收改动 mock（见 D-3）。

## 实验标准的措辞校正

按 §2.5 原文，两条标准需精确化，否则**标准本身不可满足**：

1. **「对每个 `AgentEvent::kind()` 断言 SSE type」**：`kind()` 有 8 个（`openpaw-engine/src/events.rs:53`），但其中 **3 个的期望是「不发送任何事件」**：`compaction_performed`（§2.5 第 143 行）、`turn_usage`（表中无此来源）、`classifier_status` 的 `AutoDenied` 分支（§2.5 第 140 行）。断言必须写成「期望映射」，含空映射。
2. **「text/reasoning 必须先有 `message.part.updated`，再有 `message.part.delta`」**：§2.5 第 135/136 行是「**首次收到时**发送 `message.part.updated`，随后发送 `message.part.delta`」。实现只在创建 part 那一次发 `part.updated`，后续 delta 只发 `delta`。按字面逐条断言会在第二个 delta 处失败——必须按「首次」断言。

## 设计

### 回合边界归位

`http.rs` 当前在 spawn 任务里内联构造 busy/idle（162、209、210 行）。抽出为：

```rust
// events.rs
pub fn turn_start_events(session_id: &str) -> Vec<Value>   // [busy session.status]
pub fn turn_finish_events(session_id: &str) -> Vec<Value>  // [idle session.status, session.idle]
```

`http.rs` 改为遍历并 emit。发射内容与时序逐字不变，但使「busy 先于 assistant 流、idle 与 `session.idle` 位于回合尾部」可以在单元测试里用**同一段代码**断言，而不必搭 HTTP 级测试基建。

### 测试设计（全部内联于 `events.rs`，成本 0 行预算）

1. `every_agent_event_kind_maps_to_expected_sse`：遍历 8 个 kind，断言期望 SSE `type` 序列（含 3 个期望为空）。
2. `busy_precedes_assistant_stream_and_idle_closes_turn`：用 `turn_start_events` + `TurnEventMapper` 逐个 `AgentEvent` + `turn_finish_events` 拼出整回合序列，断言 busy 索引 < 首个 assistant 事件索引，且 `idle` 与 `session.idle` 是序列最后两项。
3. `first_text_and_reasoning_delta_declare_part_before_delta`：分别对 text/reasoning 断言「首次 delta → `part.updated` 后紧跟 `delta`」，并断言**后续 delta 不再重复 `part.updated`**。
4. `tool_part_running_becomes_completed` / `tool_part_running_becomes_error`：断言同一 `part.id` 的 `state.status` 从 `running` 变为 `completed` / `error`，且 `callID` 等于 `tool_use_id`。
5. `compaction_and_usage_emit_nothing`：断言 `CompactionPerformed`、`TurnUsage` 不产生事件。

所有断言只比较 `type` 与顺序，**不比较文本全文**（§2.5 第 145 行）。

## 实施步骤

1. 写本记录（先文档）。
2. `events.rs` 增加两个回合边界函数与内联测试模块。
3. `http.rs` 改为调用这两个函数。
4. Docker `rust:1.88-bookworm` 内运行 fmt、host 测试、`xtask lines`。
5. 追加 `JYZS/docs/development/2026-09-17.md`。

## 验证方法

```bash
cargo fmt --all -- --check
cargo test --locked -p openpaw-engine-host --features engine-host
cargo test --locked -p openpaw-engine-host              # 无 feature
cargo run --locked -p xtask -- lines                    # aux 预算必须仍在 10000 以内
```

通过标准：

- 命令退出码均为 0，`xtask lines` 的 `aux (tests+xtask)` 不超 10000。
- 8 个 `AgentEvent::kind()` 均有期望映射断言（含 3 个空映射）。
- 整回合顺序断言通过：busy 在 assistant 流之前，idle 与 `session.idle` 在尾部。
- text/reasoning 首次 delta 前有 `part.updated`；tool part 的 `running → completed/error` 转换成立。
- **未映射事件清单只包含 §2.5 明确排除的三类**：`compaction_performed`、`turn_usage`、`classifier_status` 的 `AutoDenied` 分支。

## 完成标准

- 交付物存在：`events.rs` 回合边界函数 + 内联序列测试；`http.rs` 调用归位。
- 自动化验证通过：上述命令退出码均为 0。
- 完成证据齐备：测试命令、退出码、实际事件序列、未映射事件清单。
- 两层本地记录完成。
- 未提交状态明确标注。

## 实际实现（2026-09-17）

### `crates/openpaw-engine-host/src/events.rs`

- 新增 `turn_start_events(session_id) -> Vec<Value>`（busy `session.status`）与 `turn_finish_events(session_id) -> Vec<Value>`（idle `session.status` + `session.idle`）。内容与原 `http.rs` 内联构造逐字相同，只是归位到 §2.5 映射表所在的模块。
- 新增内联测试 5 个（全部放在 `#[cfg(test)] mod tests` 内，**不占任何行数预算**）：
  - `every_agent_event_kind_maps_to_expected_sse`：遍历 8 个 `AgentEvent::kind()` 断言期望 SSE `type` 序列，含 3 个「期望为空」；并断言 8 个 kind 各覆盖一次、无重复。
  - `unmapped_kinds_are_exactly_the_planned_exclusions`：未映射清单必须恰为 `classifier_status`、`compaction_performed`、`turn_usage`。
  - `busy_precedes_assistant_stream_and_idle_closes_turn`：用 `turn_start_events` + 逐个 `AgentEvent` + `TurnEventMapper::finish` + `turn_finish_events` 拼出整回合，断言 busy 索引 < 首个 assistant 事件索引、尾部两项为 `session.status`/`session.idle` 且该 status 为 `idle`；同时断言 `finish()` 填了 `time.completed`。
  - `first_delta_declares_part_then_delta_and_later_deltas_do_not`：text 与 reasoning 各自断言首次 delta 为 `part.updated`+`delta`、`delta.partID` 指向被声明的 part，后续 delta 不再重复声明。
  - `tool_part_transitions_from_running_to_completed_or_error`：断言同一 `part.id` 就地从 `running` 变为 `completed` / `error`，`callID` 等于 `tool_use_id`，并断言空串字段的行为（completed 带 `output` 无 `error`，error 反之）。
- 新增测试辅助：`types_of`、`text_delta`、`thinking_delta`、`tool_result`。

### `crates/openpaw-engine-host/src/http.rs`

- 回合开始改为遍历 `events::turn_start_events(&session_id)` 发射；回合结束改为遍历 `events::turn_finish_events(&session_id)` 发射。**发射内容与时序不变**。

### 未改动

- `/event`、`/global/event`、`/api/event` 的 SSE 接线与 `runtime.rs` 的 broadcast 机制**已满足要求，未改动**。
- 未改 `openpaw-core`、`openpaw-engine`、`openpaw-llm`、`openpaw-cli`、`openwork`、`ai-office-react`、Compose；未加依赖；未改 `Cargo.toml`；**未在 `tests/` 下加任何文件**。

## 验证结果（2026-09-17）

环境：Docker Desktop；镜像 `rust:1.88-bookworm`；`RUSTUP_TOOLCHAIN=1.88.0`；源码挂载 `/workspace`；复用既有 Cargo registry/git/target 卷。

| 命令 | 结果 |
| --- | --- |
| `cargo test --locked -p openpaw-engine-host --features engine-host` | 退出码 **0**；lib **30 passed**（含新增 5 个）、`tests/protocol_fixtures` **34 passed**、0 failed |
| `cargo test --locked -p openpaw-engine-host`（无 feature） | 退出码 **0** |
| `cargo fmt --all -- --check` | 退出码 **0** |
| `cargo run --locked -p xtask -- lines` | 退出码 **0**；`openpaw-engine-host` 1610 → **1627**；`aux (tests+xtask)` **9946 / 10000** |

行数预算结论：本轮改动使 engine-host **业务行数 +17**（即两个新函数与 `http.rs` 接线），而 `aux` 的增量为 **0**——因为内联 `#[cfg(test)]` 代码既不计入业务行数（`lines.rs:135`），也不在 `tests/` 目录内（`lines.rs:54-63`）。`aux` 相比上次读数 +20 是**上一轮**向 `tests/protocol_fixtures.rs` 新增集成测试所致，与本轮无关。

### 实际事件序列（当前源码现建 host，容器内抓取）

场景：`OPENPAW_ENGINE_MOCK=tools` + `OPENPAW_ENGINE_PERMISSIONS=manual`，建 session → `prompt_async` → `permission.asked` → reply `once`。

```text
 1 session.created
 2 session.updated
 3 message.updated            (user)
 4 message.part.updated       (user text)
 5 session.status             (busy)          <- busy 先于 assistant 流
 6 message.updated            (assistant)
 7 message.part.updated       (text)
 8 message.part.delta                        <- 先声明 part，再 delta
 9 message.part.updated       (tool running)
10 permission.asked
11 permission.replied
12 message.part.updated       (tool completed) <- running -> completed
13 message.part.updated
14 message.part.updated
15 message.updated
16 message.updated
17 message.part.updated
18 message.part.delta
19 message.part.updated
20 message.updated
21 session.status             (idle)         <- 回合尾部
22 session.idle               <- 回合尾部
```

逐条对照实验标准：busy（#5）先于 assistant 流（#6）✅；`idle` 与 `session.idle` 是最后两项（#21、#22）✅；text/reasoning 首次 `part.updated`（#7/#17）先于对应 `delta`（#8/#18）✅；tool 从 `running`（#9）变为 `completed`（#12）✅。**与本次重构前的抓取序列结构一致，确认重构未改变线上时序。**

### 未映射事件清单（完成证据）

只有三类，与 §2.5 的明确排除一致：

| kind | §2.5 依据 |
| --- | --- |
| `compaction_performed` | 第 143 行「本期不发送 OpenCode 事件」 |
| `turn_usage` | 表中无此来源 |
| `classifier_status`（`AutoDenied` 分支） | 第 140 行「不发送 permission 事件」 |

由 `unmapped_kinds_are_exactly_the_planned_exclusions` 断言固定。

## 未完成项与下一步

- **未提交**：本变更仍在工作树，未 `git add` / `commit` / `push`。
- 判据只覆盖 `AgentEvent` → SSE 的映射与顺序；HTTP 层的降级路由与 Basic Authentication 属 T3，仍未实现。
- 未验证 OpenWork 侧真实消费：本轮只验证 host 发出的序列，未跑 `pnpm evals:pr specs/openpaw-engine-attach.test.ts`。
- `busy`/`idle` 的顺序现由 `events.rs` 的构造函数保证；`http.rs` 是否在异常路径（如 `run_result` 为 `Err`）也走到 `turn_finish_events` 未加断言——当前代码在 `if let Err` 之后仍会调用，但没有测试覆盖该分支。

## 待确认事项

- **T3 未完成而 T4 前置为 T3**（§5.1）。本变更只触及 SSE 映射，不依赖 T3 的降级路由与 Basic Authentication；按大纲字面则应先补 T3。是否允许 T4 先行，待确认。
- 飞书团队大纲、个人工作计划、09-17 每日记录仍未能读取（OAuth 设备授权未完成）；开发依据取自 `docs/个人工作计划-A线.md`、`docs/个人任务分析与理解-A线.md` 与本会话用户转述。
- 实验标准措辞是否按「措辞校正」一节改写。
- D-3：`OPENPAW_ENGINE_MOCK=stream` 因 mock 无时间轴（`openpaw-llm/src/mock.rs:245-261` 用 `futures::stream::iter` 一次性装填、块间零延时），无法作为「中途 abort」的验收场景；该条应改用审批挂起回合验证。
