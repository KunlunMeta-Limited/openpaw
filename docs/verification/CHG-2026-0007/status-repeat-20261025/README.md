# 任务终态重复验证（2026-10-25）

本目录保存当前工作区修复后的可重复证据。脚本在 Linux 上启动临时 Office 协议服务、确定性 Provider 和新的 `openpaw engine-host`，通过真实 HTTP `POST /office/chat` 逐次提交独立会话，不调用模拟的投影函数。

执行入口：

```text
node tests/engine-host-contracts/status-repeat-100.mjs
```

脚本依次执行 100 次单 Agent 和 100 次多 Agent。每次在最终判定前等待同一会话的 Office 快照，检查主任务 `completed`、进度 100、白板 `completed`、五个不同子角色和无 `failed` 任务；HTTP 流必须包含 `done`。报告中的 `failures` 为空即表示 200 次最终状态全部通过。

多 Agent 交接期间可能出现短暂 `blocked` 或 `running` 快照：这是后端按 Office `task.assigned` 顺序等待前一人物交接展示的中间状态，不是终态失败。脚本不会把中间快照当作最终结果；若交接不能收敛，轮次会超时并进入 `failures`。

`report.json` 记录次数、准确源码提交、当前工作区差异哈希、脚本哈希和断言结果；`run.stderr` 保存每轮状态变化，`run.stdout` 保存脚本最终 JSON 与退出信息。临时 Host、Provider、Office 端口和测试数据均由脚本在 `/tmp` 下创建，测试结束后进程关闭。
