# Parity 夹具并发写读竞态修复记录

- 来源任务 ID：用户 2026-09-18 指示（提交前验证时发现的既有偶发失败）；关联 M1/M2 质量门与"对拍是 engine 唯一正确性证据源"（`docs/OpenPaw 开发方案.md` 关键链）
- 变更 ID：`2026-09-18-parity-fixture-race`
- 开发依据：`crates/openpaw-engine/tests/parity_harness.rs`（对拍用例）、`crates/openpaw-engine/tests/common/mod.rs`（`export_fixture`）、`docs/OpenPaw 开发方案.md`（对拍证据口径）
- 基线 commit：`9e5be37`（openpaw `dev`）
- 流程状态：执行中；本变更为当日 T3/Sidecar/world 收尾后的追加范围（用户 2026-09-18 确认「1 好」）

## 现象与证据（已复现并抓取现场）

- 命令：`cargo test -p openpaw-engine --test parity_harness` 连跑 30 次。
- 结果：**第 8 次失败**（exit 101），其余 29 次通过；此前也观察到"单独跑 10 次挂 1 次"。
- 失败断言（`parity_harness.rs:52`）：

```text
assertion `left == right` failed: parity transcripts must be deterministic
  left: ""                                                   ← 第一次读到的内容
 right: "{ ...完整 JSON... \"event_kinds\": [\"message_start\", \"text_delta\", \"turn_usage\"] ... }"
```

- 失败瞬间夹具文件大小 356 字节（内容完整）。**`left` 是空字符串**，说明读取发生在文件被"截断但尚未写入"的窗口内。
- 结论：**不是引擎两次跑出不同结果**（右侧完整内容正是期望值），而是**两个用例并发写读同一个文件**。

## 根因

1. `crates/openpaw-engine/tests/common/mod.rs:409 export_fixture` 把对拍转录**写回仓库** `crates/openpaw-engine/tests/parity/fixtures/<scenario>.json`（计划 §6.2 要求夹具作为可提交证据，设计如此）。
2. 同一个测试二进制内的用例由 cargo 并行执行：`scenario_01_pure_chat` 与 `fixtures_are_deterministic_across_replays` **导出同一个路径**。
3. `std::fs::write` 是"先截断、再写入"，**非原子**；确定性用例在两次运行之间 `read_to_string` 该路径，恰好可能读到被截断后的空文件（或半截内容）→ 两次比较不等 → panic。

因此该失败与引擎行为无关，属于**测试自身的写读竞态**；`parity_harness.rs` 在 HEAD 未改动，问题一直存在。

## 范围

- 只改 `crates/openpaw-engine/tests/common/mod.rs` 与 `crates/openpaw-engine/tests/parity_harness.rs`。
- 不改引擎代码、不改夹具 JSON 格式与内容、不改剧本、不新增依赖。
- 保留"夹具写回仓库"的设计（它是提交进仓库的证据）。

## 方案

1. **主修（比对不经过文件）**：在 `tests/common/mod.rs` 中把"渲染 JSON"抽成纯函数 `fixture_json(&ParityTranscript) -> String`，`export_fixture` 复用它并继续落盘；`fixtures_are_deterministic_across_replays` 改为比较 `fixture_json(&t1) == fixture_json(&t2)`。比较过程不再读文件，竞态从根上消失，用例意图（同一剧本两次运行结果一致）保持不变。
2. **加固（原子落盘）**：`export_fixture` 改为"写 `<path>.tmp` → `std::fs::rename` 覆盖目标"，使任何读者只会看到完整的旧内容或完整的新内容，不会看到截断状态（仓库既有同类做法：`openpaw-store` 的 `write_doc_atomic`；`std::fs::rename` 在三大平台上都会替换已存在的目标）。

不采用的改法：

- **串行化**（`--test-threads=1` 或加全局锁）：只降低出现概率，读方仍在读会被别人覆写的路径，换个跑法即复发。
- **改写到其它目录**：能绕开，但违背"夹具落仓库作为证据"的约定。

## 拟影响文件

| 文件 | 改动 |
|---|---|
| `crates/openpaw-engine/tests/common/mod.rs` | 新增纯函数 `fixture_json`；`export_fixture` 复用它并改为原子替换落盘 |
| `crates/openpaw-engine/tests/parity_harness.rs` | 确定性用例改为比较内存中的 JSON 字符串 |

## 验证方法

| 命令 | 通过标准 |
|---|---|
| `cargo test -p openpaw-engine --test parity_harness` 连跑 30 次 | 30/30 通过、0 失败（修前 30 次挂 1 次） |
| `cargo test --workspace` 连跑 2 次 | 均通过 |
| `cargo fmt --all --check` | 退出码 0 |
| `cargo run -p xtask -- lines` | 退出码 0；`aux` ≤ 10000（改前 9961，本次预计 +10 上下） |
| 跑完 `git status crates/openpaw-engine/tests/parity/fixtures` | 无输出（夹具内容未被测试改写） |

环境：Docker `rust:1.88-bookworm`（既有容器 `hopeful_proskuriakova`），源码挂 `/workspace`，复用既有 Cargo 卷，`RUSTUP_TOOLCHAIN=1.88.0`。

## 实施结果（2026-09-18）

### 实现

- `crates/openpaw-engine/tests/common/mod.rs`：抽出纯函数 `fixture_json(&ParityTranscript) -> String`（只渲染、不碰磁盘）；`export_fixture` 改为复用它，并以"写 `<path>.json.tmp` → `std::fs::rename` 覆盖"的方式落盘（原子替换，读者永远看不到截断态）。
- `crates/openpaw-engine/tests/parity_harness.rs`：`fixtures_are_deterministic_across_replays` 改为比较 `fixture_json(&t1) == fixture_json(&t2)`，**不再读文件**；用例意图（同一剧本两次运行结果一致）保持不变。

### 命令与退出码（Docker `rust:1.88-bookworm`，既有容器）

| 命令 | 结果 |
|---|---|
| `cargo test -p openpaw-engine --test parity_harness` 连跑 30 次 | **30/30 通过、0 失败**（修复前实测"30 次挂 1 次"，第 8 次即复现） |
| `cargo test --workspace` ×2 | 第 1 次失败（原因是**另一个**既有偶发失败，见下）、第 2 次通过 |
| `cargo fmt -p openpaw-engine --check` | 退出码 0 |
| `cargo run -p xtask -- lines` | 退出码 0；`aux` 9961 → **9976**/10000（预算内） |
| 跑完 `git status crates/openpaw-engine/tests/parity/fixtures` | 无输出（夹具未被测试改写） |

### 验证期间发现的另一个既有偶发失败（未修，属另一 crate）

- 用例：`openpaw-mcp` 的 `tests/mcp_e2e.rs::breaker_trips_on_transport_failure_then_reconnects`（断言 `mcp_e2e.rs:226` "dead server must fail calls"）。
- 实测：单独连跑 10 次 **挂 5 次**（exit 101）。
- 初步定位：用例在 `handle.abort()`（异步取消）之后**立即**断言"死服务器的调用必须失败"；取消尚未生效时服务器仍能应答，断言偶发不成立。属测试侧竞态，与本次 parity 改动无关（`openpaw-mcp` 本次未改动）。
- 建议修法：`handle.abort()` 后 `let _ = handle.await;`（等任务真正结束再断言），另立变更。

## 关联提交

- 按用户指示**暂不提交**（工作树状态：`tests/common/mod.rs`、`tests/parity_harness.rs`、本记录）。

## 未完成项与下一步

- `openpaw-mcp` 的 `breaker_trips_on_transport_failure_then_reconnects` 偶发失败（5/10）未修，需另立变更；在此之前 `cargo test --workspace` 仍不能稳定记为通过。
- 本次改动未提交、未推送。
