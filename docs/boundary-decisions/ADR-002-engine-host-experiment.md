# ADR-002: 重启 E8 实验（OpenCode v1 兼容层）

- Status: accepted
- Date: 2026-09-08
- 和 ADR-001 的关系：1.0 正式清单仍然不含 engine-host。这里只把实验从空 crate 往前推。
- 实现：[../engine-host/master.md](../engine-host/master.md)

## Context

ADR-001（2026-08-30）说 E8 没过：OpenWork / OpenCode 没有「第三方替换引擎」的宿主规范。`openpaw-engine-host` 停在 6 行占位，不进 12 个正式 crate。sidecar 不受影响。

原文重启条件是 OpenWork 拿出替换面，或 OpenCode SDK v2 / ACP 冻结并声明支持第三方引擎。这两条现在还没有。

2026-09-08 看 OpenWork 代码：桌面 UI 不直接连 OpenCode 进程，请求打到 `openwork-server` 的 `/workspace/:id/opencode/*`（Bearer），服务器再用 Basic 转到引擎。引擎可以是 managed 的 `opencode serve`，也可以 attached：`workspace.baseUrl` / `OPENWORK_OPENCODE_BASE_URL` 指向任意讲同一套 HTTP/SSE 的进程（`opencode-connection.ts`）。

所以不必等 RFC，也不必改 OpenWork 私有 API：host 做出 OpenCode v1 的子集，走 attached 就行。完整 v1 会超过 xtask 给 host 的 2000 行。sidecar 那层手写 HTTP（D-12）不够用。

## Decision

E8 作为社区实验继续做，不进 1.0 正式清单。

1. crate 继续 feature-gate（`engine-host`）。默认二进制、核心 9 crate、reflect-lite、goal-ledger 不加载这条路径。`cargo publish` 可以继续 0.x。
2. 业务行上限 2000 改成 5000（`xtask` 的 `CRATE_CAPS`）。再超就拆 protocol / http，不要把兼容类型塞进 `openpaw-engine` 或 `openpaw-core`。
3. 只有 `openpaw-engine-host` 可以依赖 hyper/axum 和 tokio `net`。sidecar 和核心 crate 不加 axum。没开 feature 时这些依赖不进默认二进制。
4. 协议以 master.md §3.2 为准。OpenWork 升 SDK 先改那一节的日期和 JSON。MVP 不做 `/opencode2`、OpenCode 插件 ABI、完整 provider 列表。

依赖方向和 ADR-001 一样：host 自己写 serde 类型去贴 HTTP 形状，不引用 `@opencode-ai/*` 或 OpenWork 的包。核心 crate 不出现 OpenWork / OpenCode 符号。OpenWork 可以 spawn `openpaw --engine-host`，反过来不行。

## Consequences

OpenWork 聊天可以用 attached URL 接到 OpenPaw，不用改 UI SDK。办公室可以从同一串 `AgentEvent` POST 到现有 8765。

协议会过期，升 SDK 要对一下 diff。5000 行还是紧，先做桌面主路径。正式清单、sidecar、断网烟测默认不加载 host，和 ADR-001 一致。以后如果 OpenCode 真出了官方替换面，再决定继续跟 §3.2 还是改对接。

## 没选的路

等 ADR-001 原文那两条：OpenWork 的 attached 已经存在很久，再等等于 crate 继续空着。

改 OpenWork 私有 API 或换掉 `@opencode-ai/sdk`：桌面 `session-sync.ts` 太大，而且 OpenPaw 会绑死 OpenWork。

把 sidecar 加成「换引擎」：OpenWork 主聊天走 session / prompt / event，不是 MCP `tools/call`。

axum 放进 workspace 默认依赖：和 D-12、断网默认面冲突。

行数留在 2000：xtask 会红；硬压会把类型挤进 core。
