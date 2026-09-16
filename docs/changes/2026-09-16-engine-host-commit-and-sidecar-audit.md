# Engine Host 提交与初始 Sidecar 审计记录

## 记录信息

- 日期：2026-09-16
- 来源任务 ID：T2、T3、T4、T5、T7、T9
- 来源补充：用户要求提交现有 OpenPaw 变更，并检查初始提交 `60a531a51172d14fc3db1a8e7eb6762c2c8369d8` 的 Sidecar 是否存在问题。
- 变更 ID：`2026-09-16-engine-host-commit-and-sidecar-audit`
- 开发依据：飞书《OpenPaw 引擎宿主》§2.3、§2.5、§3.2、§8；`2026-09-14-engine-host-provider-integration`；`2026-09-15-engine-host-session-ui-contract`。
- OpenPaw 基线：`12a44e5172ca89aed2498171efa3da526f958f57`
- Engine Host 实现提交：`75a3183`（`feat(engine-host): complete attached runtime integration`）

## 提交范围

提交 `75a3183` 包含：

- `Cargo.lock`
- `crates/openpaw-engine-host/Cargo.toml`
- `crates/openpaw-engine-host/src/approval.rs`
- `crates/openpaw-engine-host/src/events.rs`
- `crates/openpaw-engine-host/src/http.rs`
- `crates/openpaw-engine-host/src/lib.rs`
- `crates/openpaw-engine-host/src/protocol.rs`
- `crates/openpaw-engine-host/src/runtime.rs`
- 三份既有 Engine Host 项目变更记录

明确排除并保留在工作树中的既有内容：

- `crates/openpaw-cli/src/sidecar.rs`
- `crates/openpaw-sandbox/src/platform/linux.rs`
- `.cargo/`
- 根 `Dockerfile`

本次没有推送远端。

## 提交前验证

验证环境为 Docker Desktop 中的 `rust:1.88-bookworm`；源码挂载到 `/workspace`，复用既有 Cargo registry、git 和 target 卷。

- `cargo test -p openpaw-engine-host --features engine-host`：19 passed、0 failed，退出码 0。
- `cargo test -p openpaw-cli --features engine-host`：CLI 集成测试 13 passed、0 failed；Sidecar 因 feature 未启用而为 0 tests；命令退出码 0。
- `cargo run -p xtask -- lines`：`openpaw-engine-host` 1481/5000，`aux` 9556/10000，全部预算满足，退出码 0。
- `git diff --cached --check`：通过。

既有 warning 为 Linux sandbox 未使用函数和 goal-ledger 未使用导入，不影响本次退出码；本次未修改这些 warning 所在文件。

## 初始 Sidecar 审计

### 版本证据

`60a531a` 与提交前 HEAD 的 Sidecar 实现和 E2E 测试 blob 完全一致：

- `crates/openpaw-cli/src/sidecar.rs`：`87ea343e3b87137afb0991ac009d067a29201165`
- `crates/openpaw-cli/tests/sidecar_e2e.rs`：`7200840585cda9063ed7d6148b1f46b9d7ca8308`

当前工作树对 `sidecar.rs` 的既有未提交差异只涉及 HTTP 监听地址环境变量，不涉及 stdio 并发请求、响应读取或测试门控。因此本次复现可以证明初始版本已包含相同问题。

### 复现结果

定向命令：

```shell
timeout 20s cargo test -p openpaw-cli --features sidecar --test sidecar_e2e sidecar_concurrent_calls_serialize_set_before_get -- --exact --nocapture
```

结果：测试进入 `running 1 test` 后没有结束，20 秒后 `timeout` 返回 124。用户此前运行完整 Sidecar 测试时，同一用例超过 60 秒仍未结束。

### 根因判断

- 测试连续发送 `memory_set`（id 1）和 `memory_get`（id 2），然后先等待 id 1。
- `recv_response(1)` 会读取并直接丢弃 id 不等于 1 的帧；如果 id 2 先返回，该响应永久丢失。
- 随后 `recv_response(2)` 永久等待已经被丢弃的响应，因此测试卡死。
- 服务端通过 `tokio::sync::Mutex` 串行执行工具调用，但并发请求任务取得锁的先后不等于 wire 接收顺序；因此该测试也暴露了“set 必须先于 get”的顺序契约没有被当前门控严格保证。

## 结论与下一步

- Engine Host 现有实现已按限定范围提交，验证通过。
- 初始提交 `60a531a` 的 Sidecar 确实存在并发 E2E 卡死问题；不能把完整 `cargo test -p openpaw-cli --features sidecar` 记为通过。
- 本次只做历史审计，没有修改或提交 Sidecar。
- 修复时应先明确协议顺序语义，并让测试缓存乱序响应而不是丢弃；若服务端要求保持请求到达顺序，还需引入显式排队机制，而不只依赖异步 Mutex。
- 待确认事项：是否另立 Sidecar 修复任务并按文档流程实施。
