# SSE 路径收敛记录（移除 per-session /event）

- 来源任务 ID：T4（SSE 事件映射）验收复核；用户 2026-09-18 确认「1 是的」执行
- 变更 ID：`2026-09-18-sse-route-conformance`
- 开发依据：`openpaw/docs/engine-host/master.md` §3.2「SSE 路径为 `/event`、`/global/event` 和 `/api/event`，其他 GET 路径不得返回 SSE」；T4 任务书同款要求；`docs/changes/2026-09-17-t4-sse-sequence.md`（T4 主体实现）
- 基线 commit：`9e5be37`（openpaw `dev`）
- 流程状态：执行中；本变更为 T4 复核后的收口项（用户确认）

## 现象与证据（2026-09-18 只读复核）

- 对全部 GET 路径做 `content-type` 审计（现建 host，`--provider mock`）：

```text
SSE   /session/<id>/event     ← 不在 §3.2 允许清单内
SSE   /event
SSE   /global/event
SSE   /api/event
json  /global/health /session /session/status /session/<id> /session/<id>/message
      /session/<id>/todo /provider /permission /config /mcp /command /project
```

- 引用面核查：`/session/{id}/event` 只在两处出现——`lib.rs:144` 的路由与 `evals/specs/openpaw-engine-attach.test.ts` 的 SSE 用例；OpenWork 产品代码（`apps/`、`packages/`）零引用（桌面走全局 `/event`）。
- 该路由是早期（09-11）"per-session SSE broadcast" 的遗留，从未进入 §3.2 的路径清单，属于"其他 GET 路径返回 SSE"。

## 目标

让 SSE 路径与 §3.2 完全一致：只有 `/event`、`/global/event`、`/api/event` 提供 SSE，其余 GET 一律 JSON/404；并补一条回归测试把这条约束固定下来。

## 范围

- `crates/openpaw-engine-host/src/lib.rs`：删除 `/session/{id}/event` 路由。
- `crates/openpaw-engine-host/src/http.rs`：删除随之失效的 `events` 处理器；在契约测试模块新增一条"只有三条路径提供 SSE"的回归测试。
- `openwork/evals/specs/openpaw-engine-attach.test.ts`：SSE 用例改打 `/event`（唯一使用方）。
- 不改 SSE 事件内容、映射逻辑、broadcast 语义。

## 实施步骤

1. 删除 `/session/{id}/event` 路由与 `http::events` 处理器。
2. 在 `http::contract_tests` 增加 `only_the_agreed_paths_serve_sse`：断言 `/event`、`/global/event`、`/api/event` 返回 `text/event-stream`；`/session/{id}/event` 返回 404；`/global/health`、`/session/{id}/message` 返回 JSON。
3. eval spec 的 SSE 用例改为 `${hostUrl()}/event`。
4. 复跑 host 测试、CLI 测试、fmt、行数预算；再跑一次 GET content-type 审计与 T9 eval。

## 验证方法

| 命令 | 通过标准 |
|---|---|
| `cargo test -p openpaw-engine-host --features engine-host` | 退出码 0；新增回归用例通过 |
| `cargo test -p openpaw-cli --features engine-host` / `--features sidecar` | 退出码 0（不回归） |
| `cargo fmt -p openpaw-engine-host -p openpaw-cli --check` | 退出码 0 |
| `cargo run -p xtask -- lines` | 退出码 0（engine-host ≤ 5000） |
| GET content-type 审计脚本 | 只有 `/event`、`/global/event`、`/api/event` 是 SSE，其余全 JSON 或 404 |
| `pnpm evals:pr specs/openpaw-engine-attach.test.ts` | 7 passed / 0 skipped |

## 关键决策

- **D-1 删除而非保留**：§3.2 与 T4 要求都把 SSE 收敛到三条路径；per-session 流无人使用（除本仓 eval），删除零客户端影响。若将来确有 per-session 需求，应作为协议变更先改 §3.2 再实现。
- **D-2 补回归测试**：把"其他 GET 不得返回 SSE"写成断言，避免以后又长出第四条 SSE 路径。

## 实施结果（2026-09-18）

### 实现

- `crates/openpaw-engine-host/src/lib.rs`：删除 `.route("/session/{id}/event", get(http::events))`。
- `crates/openpaw-engine-host/src/http.rs`：删除随之失效的 `events` 处理器；`global_events` 保留并加注释说明"SSE 仅这三条路径"；在契约测试模块新增回归用例 `only_the_agreed_paths_serve_sse`（断言三条路径为 `text/event-stream`、`/session/{id}/event` 为 404、`/global/health` 与 `/session/{id}/message` 为 `application/json`）。
- `openwork/evals/specs/openpaw-engine-attach.test.ts`：SSE 用例由 `/session/{id}/event` 改为 `/event`。

### 命令与退出码（Docker `rust:1.88-bookworm`，既有容器）

| 命令 | 结果 |
|---|---|
| `cargo fmt -p openpaw-engine-host -p openpaw-cli --check` | 退出码 0 |
| `cargo test -p openpaw-engine-host --features engine-host` | 退出码 0；lib **39 passed**（含新增回归用例）、集成 34 passed |
| `cargo test -p openpaw-cli --features engine-host` / `--features sidecar` | 退出码 0 / 0 |
| `cargo run -p xtask -- lines` | 退出码 0；`aux` 9978/10000、business 31329 |
| 现场 GET content-type 审计（现建 host，14 条 GET 全扫） | `/session/<id>/event` → **404**；SSE 仅 `/event`、`/global/event`、`/api/event`（`sse_path_count=3`）；其余全部 JSON 200 |
| `pnpm evals:pr specs/openpaw-engine-attach.test.ts`（world 自拉模式、新二进制） | 1 file passed；**7 passed**；0 skipped |

### 未完成项与限制

- 运行中的容器 `jyzs-openpaw-engine-host-1`（宿主 8789）**仍是旧镜像**：实测对真实会话 `GET /session/<id>/event` 仍以 SSE 保持连接。重建镜像后即与源码一致（本轮用户未要求重建）。
- 本次改动未提交（按用户指示）。
