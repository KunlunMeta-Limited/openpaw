# 空回合事件丢失修复（Final 未被消费 / 流内 error 帧被丢弃）

## 记录信息

- 日期：2026-09-17
- 来源任务 ID：T7、T9（OpenWork A 线 attached 联调）；并修 T3/T4 事件契约
- 变更 ID：`2026-09-17-empty-turn-event-loss`
- 开发依据：`docs/engine-host/master.md` §2.5（AgentEvent 契约）、`crates/openpaw-core/src/provider.rs:433-439`（`LlmProvider::stream` 契约）、团队大纲 §2.5/§8.1 M4
- OpenPaw 基线：`3b85244e7206bc4cf6c6b98344d3b03d3e26e59e`
- 当前状态：文档完成，代码待执行

## 背景与复现证据

### 用户可见症状

OpenWork 前端对话不稳定，有时出现 `The task was accepted, but no result arrived` + Resume 卡片。

### 实测复现（引擎 8789，真实 Provider）

对一个真实回合订阅 `GET /event`，完整事件序列为：

```text
session.created / session.updated / message.updated(user) / message.part.updated(user)
session.status busy
session.status idle
session.idle
```

**零 assistant 事件，且没有 `session.error`。** `/session/:id/message` 仅返回 user 消息。两次复现分别耗时约 20s 与 4s。

### 上游证据

对上游 `https://york.988818.xyz/v1` 的流式请求返回 **HTTP 200**，响应体 361 字节：

```text
data: {"choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}
data: {"error":{"message":"Our servers are currently overloaded. Please try again later.","type":"upstream_error"}}
```

无 `content` 增量、无 `[DONE]`。连采 3 次样本：`HAS_ERROR_FRAME=1`、`HAS_CONTENT=0`，即当前持续过载。

### 第二症状：echo 也不返回正文

以 `OPENPAW_ENGINE_PROVIDER=echo` 运行同一链路，同样是 `busy → idle`、零 assistant 事件。`EchoProvider` 的 `complete()` 明确返回了 `ContentBlock::Text` 正文。

## 根因

调用链上有**三处对 `Final` 语义不一致**，任一 provider 只要不产生增量 delta，assistant 正文就整体丢失。

### 1. `LlmProvider::stream` 契约说 `Final` 携带完整响应

`crates/openpaw-core/src/provider.rs:433-439`：

```text
/// Contract: `MessageStart` is always first (streaming providers);
/// ... `Final` is always last and carries the fully assembled
/// [`CompletionResponse`]. Providers MUST emit exactly one `Final`.
```

### 2. 消费者没有按契约消费 `Final`

`crates/openpaw-engine/src/agent_loop.rs:374-395` 只用增量事件喂 sink，`Final` 仅作返回值：

```rust
openpaw_core::StreamEvent::MessageStart => { event_sink(AgentEvent::MessageStart); }
openpaw_core::StreamEvent::TextDelta { text, .. } => { event_sink(AgentEvent::TextDelta { text }); }
openpaw_core::StreamEvent::ThinkingDelta { text, .. } => { event_sink(AgentEvent::ThinkingDelta { text }); }
openpaw_core::StreamEvent::Final(resp) => { final_resp = Some(*resp); break; }   // 无 event_sink
```

随后 `:263-267` 推入 assistant 消息、`:291` 返回 `Ok`。**关键**：`:235-243` 只有 `Err` 分支会向宿主返回错误，而这里返回的是 `Ok`，宿主因此不发 `session.error`。

### 3. `OpenAiProvider` 的 `Final` 不含正文，且静默丢弃 error 帧

`crates/openpaw-llm/src/openai.rs:393-422` 的 `finish()` 只从累积的 `tool_calls` 构造 `final_content`，**从不放入流式文本**：

```rust
let mut final_content: Vec<ContentBlock> = Vec::new();
for (idx, id, name, args) in &calls { final_content.push(ContentBlock::ToolUse { .. }); }
```

`crates/openpaw-llm/src/openai.rs:347-349` 对没有 `choices` 的帧直接丢弃：

```rust
let Some(choice) = data.get("choices").and_then(|c| c.get(0)) else { return; };
```

上游的 `{"error":{...}}` 帧正好命中这一分支，被完全吞掉；随后流结束触发 `finish()`，产出一个 `content` 为空的 `Final`，被 AgentLoop 当作成功回合。

### 归纳

| 位置 | 对 `Final` 的处理 |
| --- | --- |
| 契约 `provider.rs:438` | 应携带完整组装响应 |
| `EchoProvider` + `LlmProvider::stream` 默认实现 | 符合契约（正文在 `Final` 里） |
| `OpenAiProvider::finish()` | 违反契约（只有 tool_calls） |
| `AgentLoop` 消费端 | 不使用 `Final.content` 产生事件 |

因此：**echo 是"正文被丢"，上游过载是"正文本来就没有"，两者在 `agent_loop.rs:390-393` 汇合成同一个空回合。**

附带发现：`capabilities().streaming` 在全仓无任何读取点（`grep 'capabilities\(\)\.streaming' crates/**/*.rs` 无匹配），`EchoProvider` 声明的 `streaming: false` 目前不产生任何行为。

## 目标

1. 让 `AgentEvent` 流在任何 provider 下都反映 assistant 正文，符合 `LlmProvider::stream` 契约。
2. 让流内的上游 `error` 帧变成可传播的错误，而不是静默空回合。
3. 不改变流式 provider 的既有事件序列（回归零影响）。

## 范围

### 拟影响文件

- `crates/openpaw-engine/src/agent_loop.rs`：`stream_with_retry` 在 `Final` 到达且本轮未转发任何正文增量时，按契约重放 `Final.content` 为 `MessageStart` / `TextDelta` / `ThinkingDelta`。
- `crates/openpaw-llm/src/openai.rs`：
  - `OpenAiStreamState` 增加累积文本字段；
  - `absorb` 在转发 `TextDelta` 的同时累积文本，并识别 `data.error` 帧上抛错误；
  - `finish` 把累积文本并入 `Final.content`（符合契约）。

### 明确不做

- 不修改 `openpaw-engine-host`（宿主按既有映射即可正确工作）。
- 不修改 `openwork`、`ai-office-react`、Compose。
- 不引入新依赖，不改变 `Final` 之外的事件语义。
- 不为流中途错误增加重试（见"未完成项"）。

## 设计

### agent_loop：重放 `Final.content`

在 `stream_with_retry` 内维护两个标记：

- `announced`：本轮是否已转发 `MessageStart`；
- `streamed`：本轮是否已转发 `TextDelta` / `ThinkingDelta`。

收到 `Final` 时，若 `streamed == false`：先补发 `MessageStart`（仅当 `announced == false`），再按块顺序补发非空 `Text` / `Thinking` 增量。

理由：契约规定 `Final` 携带完整响应，非流式 provider 与"正文只走 `Final`"的流都依赖它。仅在**完全没有增量**时重放，保证流式 provider 的序列一字不变，不产生重复事件。

空 `content` 的 `Final`（上游过载场景）重放不出任何正文，因此**不会**伪造"有回复"；该场景由 openai.rs 的错误上抛处理。

### openai：错误上抛与契约修正

- `absorb` 检测 `data.get("error")`：取出 `message`，push `Err(Error::server_error(503, "openai stream error: <message>"))`。消息含 `overloaded` 时经 `classify_server_error` 归为 `RetryableOverloaded`，分类语义正确。
- `absorb` 在转发 `TextDelta` 时同步 `self.text.push_str(text)`。
- `finish` 在 `final_content` 前插入累积的 `ContentBlock::Text`（非空时），使 `Final` 符合契约。

## 实施步骤

1. 记录改动前基线：`git -C openpaw status --short`、`git -C openpaw rev-parse HEAD`。
2. 修改 `crates/openpaw-engine/src/agent_loop.rs`。
3. 修改 `crates/openpaw-llm/src/openai.rs`。
4. 为两条新行为各补一个回归测试。
5. Docker `rust:1.88-bookworm` 内运行 `cargo fmt --check`、相关 crate 测试与 `xtask lines`。
6. 端到端验证 echo 与上游过载两条路径。

## 验证方法

```bash
cargo fmt --all -- --check
cargo test --locked --offline -p openpaw-llm
cargo test --locked --offline -p openpaw-engine
cargo test --locked --offline --features engine-host -p openpaw-engine-host
cargo test --locked --offline --features engine-host -p openpaw-cli
cargo run --locked --offline -p xtask -- lines
```

端到端（构建当前源码的引擎，分别验证）：

1. `OPENPAW_ENGINE_PROVIDER=echo`：SSE 应出现 `message.updated(assistant)` 与 `message.part.delta`，`/session/:id/message` 含 assistant 正文。
2. 默认 `openai` 且上游过载：SSE 应出现 `session.error`（`EngineError`），不再是无声的 `busy → idle`。

判读标准：

- 流式 provider 既有事件序列不变（现有 `openpaw-llm`、`openpaw-engine` 测试全绿）。
- echo 回合出现 assistant 事件与正文。
- 上游过载回合出现 `session.error`，前端不再落入 `unresolved` 兜底卡片。
- `xtask lines` 退出码 0，`openpaw-engine`、`openpaw-llm` 不超预算。

## 完成标准

- 交付物存在：两处源码修改 + 回归测试。
- 自动化验证通过：上述命令退出码均为 0。
- 端到端验证通过：echo 与过载两条路径结果符合判读标准。
- 两层本地记录完成（本文件 + `JYZS/docs/development/2026-09-17.md`）。
- 未提交状态明确标注。

## 实际实现（2026-09-17）

### `crates/openpaw-engine/src/agent_loop.rs`

- `stream_with_retry` 内新增两个标记：`announced`（已转发 `MessageStart`）、`streamed`（已转发 `TextDelta`/`ThinkingDelta`）。
- 收到 `Final` 时，若 `streamed == false`，调用新增的模块级函数 `replay_final_content(&resp, announced, event_sink)`：`announced` 为假时先补发 `MessageStart`，再按 `resp.content` 的顺序补发非空 `Text`/`Thinking` 增量。
- `replay_final_content` 在 `resp.content` 不含任何非空文本/思考块时直接返回，因此空 `Final`（上游过载场景）不会被伪造成"有回复"。

### `crates/openpaw-llm/src/openai.rs`

- `OpenAiStreamState` 新增 `text: String`，注释说明 `Final` 必须携带完整组装响应。
- `absorb`：
  - 新增 `data.get("error")` 分支，取出 `message` 并 `queue.push_back(Err(Error::server_error(503, format!("openai stream error: {message}"))))`，取代原先的静默丢弃；
  - 转发 `TextDelta` 的同时 `self.text.push_str(text)`。
- `finish`：在累积 `tool_calls` 之前，若非空则把 `ContentBlock::Text` 插入 `final_content` 首位，使 `Final` 符合契约。

### 新增测试

- `crates/openpaw-llm/tests/mock_server_contract.rs`：`openai_streaming_final_carries_assembled_text`、`openai_stream_error_frame_is_surfaced`。
- `crates/openpaw-engine/tests/engine_e1.rs`：`final_only_provider_still_forwards_content_events`。

未修改 engine-host、openwork、ai-office-react、Compose。未新增依赖。

## 验证结果（2026-09-17）

环境：Docker Desktop 中的 `rust:1.88-bookworm`；源码挂载 `/workspace`，复用既有 Cargo registry/git/target 卷；`RUSTUP_TOOLCHAIN=1.88.0`；容器内 `rustup component add rustfmt` 后执行 fmt。

### 自动化

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 退出码 **0** |
| `cargo test --locked --offline -p openpaw-llm` | lib **17 passed**；`mock_server_contract` **9 passed**（含新增 2 个）；0 failed |
| `cargo test --locked --offline -p openpaw-engine` | compaction 2 + e2_wrapup 4 + **engine_e1 12**（含新增 1 个）+ hook 2 + parity 6 + tools_integration 2；0 failed |
| `cargo test --locked --offline --features engine-host -p openpaw-engine-host` | **19 passed**，与改动前一致 |
| `cargo test --locked --offline --features engine-host -p openpaw-cli` | **13 passed**，与改动前一致 |
| `cargo run --locked --offline -p xtask -- lines` | 退出码 0，`lines: all budgets respected` |

行数变化：`openpaw-engine` 1524 → **1583**（cap 8200）；`openpaw-llm` 1483 → **1512**（cap 5200）；`aux` 9556 → **9638**（cap 10000）；`openpaw-engine-host` 保持 1481。

### 端到端（真实引擎二进制，非单元测试）

**CASE A：echo provider**（`OPENPAW_ENGINE_PROVIDER=echo`，`GET /event` 订阅）

改动前：`busy → idle`，零 assistant 事件。改动后：

```text
message.updated        x3
message.part.updated   x3
message.part.delta     x1
session.status         x2 (busy, idle)
session.idle           x1
```

`/session/:id/message` 快照出现 assistant 消息，正文为 `"Echo: Say hi in one short sentence."`。

**CASE B：上游过载**（确定性 mock 上游，复刻当日 `york.988818.xyz` 的 361 字节响应：一条 `delta.role` 帧 + 一条 `{"error":{...overloaded...}}` 帧，无正文无 `[DONE]`）

- **B1（error 帧）**：SSE 出现 `session.error` **1 次**，序列为 `busy → session.error → idle → session.idle`；assistant 事件 0 次。改动前同一输入产出的是无错误的空回合。
- **B2（正常流式）**：`message.part.delta` 2 次，assistant 快照含正文，`session.error` **0 次**——流式路径序列未变。

### 既有 parity fixture 变更说明

`crates/openpaw-engine/tests/parity/fixtures/01..05*.json` 出现改动：`event_kinds` 新增 `message_start` / `text_delta`。

这是**预期且正确的**：这些 `.json` 是 `export_fixture`（`tests/common/mod.rs:409`）每次运行导出的**入库证据**，记录实际观测到的事件序列；断言门是 `assert_expectations`（`:357`），只校验工具调用序列、error 工具序列、最终 stop、final_reply、并发下限与 compaction，**不校验 `event_kinds`**，因此门禁未被削弱。fixture 内容变化如实反映了"Final-only 回复现在会转发内容事件"。

### 环境限制

- 本机缺少 MSVC `link.exe`，编译与测试均按规则在 Docker 内执行。
- 验证使用的 mock 上游写入容器 `/tmp`，验证后随容器销毁；临时脚本未进入仓库。

## 未完成项与下一步

- **未提交**：本变更仍在工作树，未 `git add` / `commit` / `push`。
- 流**中途**出现的错误当前不重试（`agent_loop.rs:374` 的 `?` 直接上抛）。`RetryableOverloaded` 分类已正确，但只在 `provider.stream()` 建流阶段生效。若上游过载为短时抖动，建议后续在"本轮尚未转发任何正文"的前提下对中途错误重试。
- **上游挂起未覆盖**：`stream_client()`（`crates/openpaw-llm/src/http.rs:19-24`）只设 15 秒连接超时、**无总超时**。实测 CASE B 首次运行（真上游）出现过 25 秒仍未结束、既无 `session.error` 也无 `idle` 的挂起；该路径需独立处理。
- 上游 `york.988818.xyz` 的可用性属外部依赖；本变更只保证错误可见，不解决可用性。

## 待确认事项

- 本变更触及 `openpaw-llm`、`openpaw-engine` 两个核心 crate，超出 `openpaw-engine-host` 范围；按团队大纲 §6 责任划分，核心 crate 改动是否需先经团队确认。
- 飞书团队大纲、个人工作计划、09-17 每日记录本次仍未能读取（OAuth 设备授权未完成）。
