# D88 多 Agent 交接与进度真实复验（2026-10-26）

本目录保存 OpenPaw Host 使用全新临时数据目录、临时 Office HTTP 服务和确定性 OpenAI 兼容 Provider 完成的一轮真实五人协作证据。测试没有修改 Office 仓库，也没有读取用户 DeepSeek 密钥。

## 复现命令

```text
PATH=/home/cheng/.cargo/bin:$PATH cargo build --locked -p openpaw-cli --features engine-host,sidecar
node /tmp/openpaw-multi-one-trace.mjs
```

脚本从 `tests/engine-host-contracts/status-repeat-100.mjs` 生成临时副本，只运行 1 次 `MULTI-0`；Host、Provider、Office 均绑定临时 loopback 端口和临时 home/workspace。

## 关键证据

`transition-trace.txt` 记录 Office 快照的真实变化：

```text
children=0
blocked 16% children=1
blocked 33% children=2
blocked 50% children=3
blocked 66% children=4
completed 100% children=5
```

`final-snapshot.json` 保存最终快照，包含王明及李研、周理、陈书、刘市、赵审五个不同 `assigneeId`，五个 `task.assigned` 和五个 `task.completed` 事件，白板为 `completed` 且描述为 `5/5`。

## 修复覆盖

- 新完成 Run 按上一次成功投影 revision 判断，不再把本轮新交接误判为历史并一次性公开。
- 主任务进度按已公开的交接成员计算，隐藏但已完成的子 Run不提前计入。
- 交接变体的白板说明使用“已确认完成 N 项”，最终快照才使用完整 `N/N`。
- Host 重启时仍跳过已经消费过的旧终态交接，避免重复动画。

这是一轮确定性 Provider 的 Host/协议证据，不能替代真实 DeepSeek 的模型质量结论；真实 DeepSeek 无效流式工具参数的失败仍按真实失败显示。
