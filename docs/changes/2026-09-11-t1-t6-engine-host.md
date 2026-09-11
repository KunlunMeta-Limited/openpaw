# T1-T6 Engine Host 实施记录

- 来源任务 ID：T1、T2、T3、T6
- 变更 ID：2026-09-11-t1-t6-engine-host
- 基线 commit：`60a531a` (`main`)
- 目标：按团队大纲建立 `engine-host` feature、OpenCode 子集协议基础、HTTP Host 和 CLI 旗标，推进至 T6。
- 范围：`Cargo.toml`、`openpaw-engine-host`、`openpaw-cli`、`xtask`；不改动 OpenWork、AI Office，也不覆盖已有用户改动。

## 修改前检查

- `openpaw` 存在未提交用户改动：`crates/openpaw-cli/src/sidecar.rs`、`crates/openpaw-sandbox/src/platform/linux.rs`、`.cargo/`、`Dockerfile`。
- `openpaw-engine-host` 当前只有占位 `lib.rs`，T1-T3 需要新增实现。
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
- `openpaw-engine-host/src/lib.rs`：加入 loopback 动态端口 Host、health/session/message/prompt/status/abort 路由。
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

T1、T2、T6 的实现和对应验证证据已具备；T3 目前是最小 HTTP 基础面，尚未包含团队大纲要求的完整 AgentLoop、SSE、审批和 abort 行为，因此不能把 M2/M3 或 T4/T5 记为完成。未提交前仍不宣称已有 Git 交付。

## 未完成项

- AgentLoop/SSE/审批的完整 T4/T5 行为尚未纳入本变更；若 T3 实现触及这些边界，必须扩展本记录后再修改。
- 提交前需保留并单独说明已有用户改动。
- 下一步：补齐 AgentLoop/SSE/审批前，先与团队确认协议 fixture 和本轮 T3 范围。
