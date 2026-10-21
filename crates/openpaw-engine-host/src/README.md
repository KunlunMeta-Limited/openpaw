# 源码模块

- `lib.rs`：feature 边界与公开启动 API。
- `protocol.rs`：OpenCode v1 请求、响应、session、permission 和 event 类型；
  `MessageRequest::prepare` 共用输入准备，私有 `projection` 共用 REST/SSE 附件、错误及展示属性。
- `runtime.rs`：每 session 独立的 AgentLoop、JSONL/cwd 恢复、删除/取消生命周期和
  per-turn provider 接线；摘要扫描不会实例化运行时。私有 `desktop` 模块封装桌面登记、
  操作与视觉 ACK，只用于父 Agent，不随 Fork/恢复继承；历史变换直接调用 Store。
- `http.rs`：回环 Axum 路由、鉴权和 HTTP 状态映射。
- `events.rs`：AgentEvent 到 OpenCode SSE 的转换。
- `approval.rs`：主会话审批适配及每次子任务独立审批登记，统一查询、回复和取消清理。
- `delegation.rs`：文字子任务、工作区写锁、模型并发限制及可可靠失效的结果缓存；
  子空间身份与结果分离，staged Revert 清结果，unrevert 不恢复结果。
- `office.rs`：有序、尽力而为的 AI Office 动作队列、名册映射和跨 session owner
  仲裁。

依赖从 HTTP/protocol 适配器流向 runtime 和现有 OpenPaw 公共 trait。core crate
绝不导入本适配器。

运行时锁顺序固定为 `AppState.session_lifecycle -> SessionHandle.turn_gate -> writer`。
approval lifecycle 只在 turn gate 内由 reserve/abort 进入；异步等待前释放内部锁。
新增代码不得反向取得这些锁。
