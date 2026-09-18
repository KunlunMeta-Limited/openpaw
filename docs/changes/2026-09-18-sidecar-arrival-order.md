# Sidecar 工具调用到达顺序修复记录

- 来源任务 ID：用户 2026-09-18 指示「先把 sidecar 的 bug 解决了，再考虑提交」；关联 M3 闸门（`cargo test -p openpaw-cli --features sidecar` 退出码 0）与 09-16 审计遗留项
- 变更 ID：`2026-09-18-sidecar-arrival-order`
- 开发依据：飞书《OpenPaw 引擎宿主》§8.1（M3：`cargo test -p openpaw-cli --features sidecar` 退出码 0）、§5.1（T6 前置 T3）；`openpaw/docs/changes/2026-09-16-engine-host-commit-and-sidecar-audit.md`（复现记录与修复方向）；`crates/openpaw-cli/src/sidecar.rs:55-66`（既有契约声明）
- 流程状态：执行中；本变更是当日 T3 收尾后由用户追加的范围（已同步当日记录）
- 基线 commit：`3b85244`（`dev`）；工作树含 09-17/09-18 既有未提交改动，本变更只叠加 `sidecar.rs` 与 `sidecar_e2e.rs`

## 目标

让 `SidecarServer` 已声明的契约——**工具调用按客户端发出的到达顺序串行执行**——真正成立；并让并发 E2E 在回归时快速失败而不是无限挂起。

## 根因（只读核对，2026-09-18）

1. **rmcp 对每个请求并发起任务**：`rmcp 1.8.0` 的 `serve_inner`（`src/service.rs` 主循环）在收到 `JsonRpcMessage::Request` 时构造 `RequestContext` 后 `spawn_service_task(async move { service.handle_request(request, context) })`，同一连接内请求处理并发，**wire 到达顺序不会传递到执行顺序**。
2. **互斥锁不等于顺序**：`SidecarServer::call_tool`（`sidecar.rs:401`）只用 `tokio::sync::Mutex<()>` 做互斥；`tokio::sync::Mutex` 按任务**首次轮询 `lock()`** 的先后授予锁，而任务轮询顺序由调度器决定（tokio 的 LIFO slot 还会让后 spawn 的任务先被轮询），因此后到的 `memory_get` 可以先于先到的 `memory_set` 执行。
3. **测试把乱序当挂起**：`sidecar_concurrent_calls_serialize_set_before_get` 的 `recv_response(id)` 读取并**丢弃** id 不匹配的帧；一旦 id=2 的响应先到就被丢弃，随后 `recv_response(2)` 永久等待已被丢弃的帧，测试挂死（`timeout` 124）。

实测证据（`rust:1.88-bookworm` 容器内直接对二进制喂帧）：

```text
{"id":0 initialize} → 正常响应
{"id":1 memory_set  race-note} 与 {"id":2 memory_get race-note} 连续发出
实际响应顺序：id=2 → {"code":-32602,"message":"no memory with id `race-note`"}，id=1 → "saved memory `race-note`"
```

即：get 先于 set 执行并读空，契约被违反。

## 范围

- 只改 `crates/openpaw-cli/src/sidecar.rs` 与 `crates/openpaw-cli/tests/sidecar_e2e.rs`。
- 不改 memory store 语义、不改 rmcp 版本、不改 engine host、不改 MCP 工具集。
- 不在本轮：把同序保证扩展到 HTTP 传输（该路径由 `http_accept_loop` 逐连接串行处理，语义已成立）。

## 拟影响文件

| 文件 | 改动 |
|---|---|
| `crates/openpaw-cli/src/sidecar.rs` | 新增到达顺序（票据 + 顺序游标）与 stdio 传输包装；`call_tool` 先按票据等轮次再取互斥锁；`run_stdio_server` 用包装后的传输；新增单元测试 |
| `crates/openpaw-cli/tests/sidecar_e2e.rs` | `recv_response` 改为缓存乱序帧而不是丢弃，回归时快速失败 |

## 实施步骤

1. 在 `sidecar.rs` 增加 `ArrivalOrder`：`next: AtomicU64` 发票据、`turn: Mutex<u64>` 记录当前允许执行的票据、`Notify` 唤醒；`wait(ticket)` 返回 RAII 守卫（`Drop` 时推进游标并唤醒，包含 panic 路径）。
2. 增加 `ArrivalTicket(u64)` 携带类型与传输包装 `ArrivalOrderTransport<T: Transport<RoleServer>>`：只在 `tools/call` 请求上盖票据（写入请求的 `extensions`，rmcp 会把它搬进 `RequestContext`），`send`/`close` 直接转发。
3. `SidecarServer` 持有 `Arc<ArrivalOrder>`；`call_tool` 从 `context.extensions` 取票据：有票据则先 `wait`（不持锁等待，避免死锁），再取既有 `gate` 互斥锁；无票据（HTTP 路径）保持原行为。
4. `run_stdio_server` 用 `rmcp::transport::async_rw::AsyncRwTransport::new(stdin, stdout)` 构造内层传输并包一层顺序传输。
5. 测试侧：`recv_response` 维护 `HashMap<i64, Value>` 缓存，乱序帧入缓存，命中即返回。
6. 验证：sidecar 测试连续多轮 + engine host/CLI 回归 + 行数预算 + fmt。

## 验证方法

| 命令 | 通过标准 |
|---|---|
| `cargo test -p openpaw-cli --features sidecar` | 退出码 0；`sidecar_concurrent_calls_serialize_set_before_get` 通过且不再挂起 |
| 同一命令连跑 5 次 | 5/5 通过（证明竞态已消除，而不是碰运气） |
| `cargo test -p openpaw-cli --features engine-host` | 退出码 0（不回归 T3） |
| `cargo test -p openpaw-engine-host --features engine-host` | 退出码 0 |
| `cargo fmt -p openpaw-cli --check` | 退出码 0 |
| `cargo run -p xtask -- lines` | 退出码 0；aux ≤ 10000（改动前 9949） |

## 关键决策

- **D-1 顺序语义**：保留 `sidecar.rs:58-62` 已声明的「按到达顺序串行」契约（本地、单用户、文件型工具，严格顺序成本可忽略），而不是把契约弱化为「不保证顺序」——弱化会让流水线客户端静默读到旧数据。
- **D-2 实现位置**：顺序信息只在传输层可得（`receive()` 是唯一能观察到 wire 到达顺序的位置），因此用票据 + 顺序游标，而不是继续依赖异步互斥锁。
- **D-3 不阻塞读循环**：采用「票据 + 游标」而不是「读到响应前不再读下一帧」，避免请求未回响应（异常路径）时把连接读循环永久堵住；执行端用 RAII 守卫保证 panic 也能推进游标。
- **D-4 测试仍需缓存乱序帧**：即使服务端已保证顺序，测试也不应再因乱序而挂死；缓存让回归表现为断言失败而非超时。

## 实施结果（2026-09-18）

### 实现要点

- `crates/openpaw-cli/src/sidecar.rs` 新增到达顺序机制：
  - `ArrivalTicket(u64)`：传输读到 `tools/call` 时盖章，写入该请求的 `extensions`；rmcp 会把请求 extensions 搬进 `RequestContext::extensions`，`call_tool` 据此取回票据。
  - `ArrivalOrder`：`AtomicU64` 按到达顺序发票据 + `tokio::sync::watch` 游标；`wait(ticket)` 等到轮到自己，**等待期间不持互斥锁**，因此后到的调用只等轮次、不会卡住当前调用。
  - `ArrivalTurn`：RAII 守卫，`Drop` 时推进游标；工具调用 panic/unwind 也能让队列继续前进。
  - `ArrivalOrderTransport<T>`：包装传输，只在 `tools/call` 上打票据，`send`/`close` 直接转发；不阻塞读循环（异常时不会把连接堵死）。
  - `call_tool`：有票据时先等轮次再取既有 `gate` 互斥锁；无票据（HTTP 壳）保持原行为。
  - `run_stdio_server`：用 `rmcp::transport::async_rw::AsyncRwTransport::new(stdin, stdout)` 构造内层传输并包一层顺序传输。
- `crates/openpaw-cli/tests/sidecar_e2e.rs`：`recv_response` 增加 `pending` 缓存，乱序帧不再被丢弃；回归表现为断言失败而不是无限挂起。
- 新增 2 个单元测试：按票据顺序执行（后到票据先起跑也必须等）；调用 panic 时守卫仍推进游标。

### 命令与退出码

环境同前：Docker `rust:1.88-bookworm`（既有容器），源码挂 `/workspace`，复用既有 Cargo 卷。

| 命令 | 结果 |
|---|---|
| `cargo fmt -p openpaw-cli --check` | 退出码 0 |
| `cargo test -p openpaw-cli --features sidecar` | **退出码 0**；lib **11 passed**（含 2 个新顺序测试）、`sidecar_e2e` **3 passed**、其余 suite 全绿（改动前该命令因并发用例挂死而 timeout 124） |
| `cargo test … --test sidecar_e2e` 连跑 5 次 | **5/5 退出码 0**，每次 3 passed、0.01s（改动前单次即挂死 >60s） |
| `cargo test -p openpaw-cli --features engine-host` | 退出码 0（T3 不回归） |
| `cargo test -p openpaw-engine-host --features engine-host` | 退出码 0；38 + 34 passed |
| `cargo run -p xtask -- lines` | 退出码 0；`aux` 9949 → **9961**/10000、business 31202 → 31338 |

### 行为证据（同一探针，修复前后对比）

同一帧序列：`initialize` → `initialized` → `tools/call memory_set(id=1)` → `tools/call memory_get(id=2)`，四帧一次性写入 stdin。

- 修复前（09-16 记录 + 今日复现）：
  ```text
  id=2 → {"code":-32602,"message":"no memory with id `race-note`"}
  id=1 → "saved memory `race-note`"
  ```
- 修复后（同一探针，2026-09-18）：
  ```text
  id=1 → "saved memory `race-note`"
  id=2 → "# race-note  [Fact] … gate orders concurrent calls"
  ```

即到达顺序契约恢复，且 `M3` 的 sidecar 命令首次可以记为通过。

## 未完成项与下一步

- HTTP 传输未打票据（`http_accept_loop` 按连接串行处理，语义已成立）；若将来 HTTP 需要同序保证，需另开变更。
- 本次改动**未提交**（用户 2026-09-18 明确「先不提交」）。
- 未重建 `jyzs-openpaw-engine-host` / sidecar 相关镜像，compose 侧仍跑旧代码。
- 下一步建议：把 sidecar 修复与 T3 一起提交时，把 `cargo test -p openpaw-cli --features sidecar` 的退出码写入 PR 记录（M3 闸门）。
