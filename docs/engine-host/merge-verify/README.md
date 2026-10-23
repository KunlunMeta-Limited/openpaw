# 合并验证脚本（A 线 / B 线合并，CHG-2026-0017）

这三个脚本用于对**运行中的**合并后 engine-host / sidecar 做前端契约断言。它们不是
`cargo test` 的替代品：单元与集成测试证明实现自洽，这里证明两个前端各自实际调用的
**线上契约**成立。

## verify.mjs — engine-host（OpenWork §3.2 + AI Office §4.1/§4.2）

按两个前端真实调用的面逐条断言：

- **OpenWork**：`/global/health`、Basic Auth（缺失/错误 → 401）、`POST /session` → 200、
  会话列表/详情/provider（含 `all`/`connected` 与 models 深层形状）/todo/status、
  `/event`+`/global/event`+`/api/event` 三条 SSE、`/session/:id/event` → 404、
  `/config`+`/mcp` → `{}`、`/command`+`/project` → `[]`、
  `POST /session/:id/{command,shell,fork,revert,summarize}` → 204、v2 路径 → 404、
  未知 permission 回复 → 404。
- **SSE 序列**：`session.status`、`message.updated`、`message.part.updated`、
  `message.part.delta`、`session.idle`，manual 审批下加 `permission.asked` /
  `permission.replied`。
- **AI Office**：从**真实的** `ai-office-react/scripts/action-gateway.mjs`（端口 8765）
  `GET /actions` 抽干并断言 §4.2 的名册与动作映射（每回合至多一个 `desk_visit`、
  `visitor=1`、`read_file` → `host=3`、`set_state` 词表与终态）。

```sh
# scenario = tools（脚本化 read_file，覆盖工具/审批/office 动作）| stream（流式文本）
docker run --rm --network <net> \
  -e OPENPAW_BASE=http://openpaw-engine-host:8788 \
  -e OFFICE_GATEWAY=http://office-gateway:8765 \
  -v "$PWD/docs/engine-host/merge-verify:/h" node:22-alpine node /h/verify.mjs tools
```

退出码 0 表示全部断言通过。

## mcp-check.mjs — sidecar MCP（AI Office 的 `OPENPAW_URL`）

对 `openpaw --sidecar --sidecar-transport http` 暴露的 `/mcp` 走
`initialize` → `notifications/initialized` → `tools/list`。

```sh
docker run --rm --network <net> -v "$PWD/docs/engine-host/merge-verify:/h" \
  node:22-alpine node /h/mcp-check.mjs
```

## start-host.sh — 在构建容器内拉起 engine-host

以 `tools`/`stream` 场景前台启动，供上面两个脚本连打。需要 `OPENPAW_ENGINE_MOCK`、
`OPENPAW_ENGINE_PERMISSIONS=manual`、`OPENPAW_OFFICE_ACTIONS_URL` 与 Basic Auth 凭据。

## 已知限制

- 断言的是 engine-host 暴露的线上契约，不含 openwork-server 代理层（§3.3）与
  AI Office 侧栏（§4.3/§4.4）——后者属于 openwork / ai-office-react 仓库。
- `POST /session` 断言 200（不是 201），依据是 openwork
  `evals/specs/openpaw-engine-attach.test.ts` 第 210 行的代理端到端断言。
