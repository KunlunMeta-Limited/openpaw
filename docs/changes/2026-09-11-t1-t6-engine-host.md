# T1-T6 Engine Host 实施记录

- 来源任务 ID：T1、T2、T3、T6
- 变更 ID：2026-09-11-t1-t6-engine-host
- 基线 commit：`60a531a` (`main`)
- 目标：按团队大纲建立 `engine-host` feature、OpenCode 子集协议基础、HTTP Host 和 CLI 旗标，推进至 T6。
- 范围：`Cargo.toml`、`openpaw-engine-host`、`openpaw-cli`、`xtask`；不改动 OpenWork、AI Office，也不覆盖已有用户改动。

## 修改前检查

- `openpaw` 存在未提交用户改动：`crates/openpaw-cli/src/sidecar.rs`、`crates/openpaw-sandbox/src/platform/linux.rs`、`.cargo/`、`Dockerfile`。
- `openpaw-engine-host` 当前已有 T1/T2 基础实现；本轮继续补齐 AgentLoop、SSE 和 abort。
- 当前工作区未执行通过验证；T1/T2 在个人工作计划中没有完成证据。

## 实施步骤

1. 先完成本记录，再修改代码。
2. 接入 `engine-host` feature 和可选依赖，保持默认 feature 关闭。
3. 建立协议类型、health/session/prompt/message/status/abort 路由和动态端口就绪标记。
4. 接入 CLI 的 `--engine-host`、`--engine-host-hostname`、`--engine-host-port`，更新 help snapshot 断言。
5. 依次运行 fmt、lines、feature build、Host 测试和 CLI 测试；记录退出码。

## 验证方法

- `cargo run -p xtask -- lines`
- `cargo build -p openpaw-cli --features engine-host`
- `cargo test -p openpaw-engine-host --features engine-host`
- `cargo test -p openpaw-cli --features engine-host`
- 默认 `cargo tree` 不出现 axum，且默认 CLI 测试保持通过。

## 实际实现

- `Cargo.toml`：加入 axum 和 tokio `net`，仅由 Host feature 使用。
- `openpaw-engine-host/Cargo.toml`：加入可选 `engine-host` 依赖。
- `openpaw-engine-host/src/lib.rs`：加入 loopback 动态端口 Host、health/session/message/prompt/status/abort 路由、AgentLoop 后台执行和 per-session SSE broadcast。
- `openpaw-engine-host/src/protocol.rs`：加入 status/event wire 类型和 serde 往返 fixture 测试。
- `openpaw-cli`：加入 `engine-host` feature 及三个全局旗标。
- `xtask/src/lines.rs`：Host 预算从 2000 调整为团队大纲要求的 5000 行。

## 验证结果

执行环境：Docker Desktop `desktop-linux`，`rust:1.88-bookworm` 容器；JYZS compose 栈仍作为运行联调环境，当前 OpenPaw compose 镜像本身不含 Cargo。

| 命令/操作 | 结果 |
|---|---|
| `cargo run -p xtask -- lines` | 通过；Host 179/5000 行，所有预算通过 |
| `cargo build -p openpaw-cli --features engine-host` | 通过（Docker） |
| `cargo test -p openpaw-engine-host --features engine-host` | 通过，2 个 protocol fixture 测试 |
| `cargo test -p openpaw-cli --features engine-host` | 通过，CLI 集成测试全部通过 |
| `openpaw --help` | 通过；显示三个 engine-host 旗标 |
| Host health/session 冒烟 | 通过；监听 `127.0.0.1:18765`，health 200，session 创建成功 |

## 当前状态

T1、T2、T3 基础面、AgentLoop/SSE 接线和 T6 的实现与验证证据已具备；人工审批协议和完整 tool guardrail 仍未完成，因此 T5/M3 不能记为完成。当前变更尚未形成新的 Git commit。

## 未完成项

- manual permission pending/reply/reject 和完整 tool guardrail 尚未纳入本变更。
- 提交前需保留并单独说明已有用户改动。
- 下一步：接入 manual permission pending/reply/reject 和 abort 清理，补齐 T5 验收。

## 2026-09-14 T5 实施计划

- 来源任务 ID：T5
- 目标：将 OpenPaw `ApprovalManager` 接入 Engine Host，提供 pending 查询、人工回复和 SSE 审批事件。
- 范围：`crates/openpaw-engine-host/src/lib.rs`、协议类型和 T5 测试；不修改用户已有未提交文件。
- 开发环境：优先使用 Docker Desktop 的 `rust:1.88-bookworm` 镜像；容器不可用时停止代码开发。
- 实施步骤：定义审批状态与回复模型；为 AgentLoop 注入审批管理器；增加 permission 路由和事件；补充 once/always/reject、abort 清理测试。
- 验证方法：Docker 中执行 `cargo fmt --all -- --check`、Host 测试、CLI feature 测试和 `cargo run -p xtask -- lines`。

## 实际交付结果（2026-09-14）

- 正式模块已拆分为 `lib.rs`、`protocol.rs`、`runtime.rs`、`http.rs`、`events.rs`、`approval.rs`。
- T3/T4：HTTP 路由、session 运行态、AgentLoop 后台执行、SSE 事件转换和 abort 已接入模块边界。
- T5：Host `ApprovalManager` 支持 pending 查询、`once`、`always`、`reject`、`permission.asked`、`permission.replied` 和 abort 清理。
- T6：补充 Session/Permission 协议类型及 serde fixture；完整 JSONL 历史持久化和全部 OpenCode 字段仍属于后续扩展。
- Docker 验证：Host 6 项测试通过；CLI engine-host 测试 11 项通过；`xtask lines` 全预算通过；代码已执行 `cargo fmt`。
- 变更提交：`1bd42c6` (`feat(openpaw): modularize engine host and approvals`)。
- 未完成项：真实工具 mock 驱动的端到端审批 HTTP 测试、session JSONL 持久化、完整 OpenCode tool/usage/todo 字段。
