# R1：Office 长任务聊天扩展（待用户单独批准）

状态：提案。T0 只提交设计，不修改 Office。实施请求不自动批准此跨仓修改；只有具体设计经用户明确批准后才纳入T6扩展范围；T6基础恢复和T7不依赖批准。Office任何文件（含文档/测试/配置）在批准前均不得修改。

## 必要性与复用

现 ChatSession 默认 180000ms 总期限，刷新将 pending 改 stopped，dispose/stop 共用取消；v1 无 turnId/runId/cursor。心跳不改变总期限，Host 无法独自补齐续接。

只扩展聊天协议/聊天源、ChatSession、存档和停止生命周期；复用 OfficeChatSource、聊天组件、IndexedDB 原子版本校验、错误/草稿显示。不重写 Fastify/SSE 场景服务、白板、任务或侧栏。

默认保留 v1。新增显式配置的扩展聊天源 historyKey，避免将 v1 和续接模式档案混用；部署方 opt-in，旧部署无需改配置。

## Host 接口

所有接口由受信同源代理/宿主保护，校验会话访问范围，runId 不作为授权凭据。v1 地址不变。

| 方法/路径 | 语义 |
|---|---|
| POST /office/chat/runs | body：version=1.1、teamId、conversationId、turnId、messages；持久化接受后返回 runId/status |
| GET /office/chat/runs/:runId | 一致 text/state/cursor 快照及本次用户/助手消息标识 |
| GET /office/chat/runs/:runId/events?after=N | NDJSON；严格递增 cursor；应用 cursor 大于本地值的事件 |
| POST /office/chat/runs/:runId/cancel | 幂等请求；返回当前状态；实际清理前为 cancelling |

创建按 (teamId, conversationId, turnId) + 请求内容摘要幂等，同键异内容 409；同会话不同新请求遇 active Run 为 409；容量满 503；非法 400、无授权 401/403、无资源 404。UTF-16/字节和 v1 内容上限保留，不因移除总时间限制无限输出。

扩展事件有 cursor、runId、type；type 为 status、delta、done、error，status 包含 running/awaiting_approval/waiting_resource/cancelling/cancelled/interrupted。取消/中断不发正常 done。delta 只追加；失败后不重头拼文本。快照游标在相同一致性边界捕获。

事件仅保留有界缓冲。游标超出保留范围返回 409 CURSOR_EXPIRED，客户端重新取快照并从返回 cursor 订阅；订阅建立需原子衔接快照与缓冲，不能漏窗口。completed/error/取消的最终快照持久化，不能仅留内存。

## 浏览器与存档

1. 先原子写入 turnId 和输入消息，再提交；提交重试复用同 turnId。
2. 保存 runId、最后应用 cursor、累积文本和状态；对同消息持续更新，不重复创建助手行。
3. 刷新/卸载/网络断开只解除订阅，重新载入时查询 Run 并续接；不调用取消。
4. 停止按钮调用 cancel，状态 cancelling，仍订阅/查询直到清理后的终态。
5. 扩展模式以连接空闲 watchdog 重连替代 180 秒总期限；Host 的独立 Run 总预算保留。
6. 旧档案按原 v1 读取；新字段可选迁移，冲突遵守现有 expected revision；读取/保存失败保留原记录与可见内容。
7. interrupted 单独显示并允许用户明确发起新 turn，绝不自动重跑原工具。

## 测试与批准记录

必测：超过 180 秒；刷新多次同一 Run；创建响应丢失重试；同键异内容；游标过期；断开不取消；停止回收；取消期间刷新；Host 崩溃；IndexedDB 冲突/失败；v1 全量回归；Linux/Windows 浏览器。

批准记录：待填批准人、设计提交、日期、允许修改范围。批准前不改 Office。交付以 Host 与 Office 两仓 PR 均获批合入为准；未批准时不得将长任务能力标为完成。

## 供人工审核的准确范围、影响与回退

提案版本：R1-design-2026-10-20；关联CHG-2026-0001；Office冻结提交b221a44ce22fe08956356b5002d17df59a6a4f50。类型为必要扩展，批准证据为未批准。下表只提出候选范围，没有授权任何Office修改。

| Office拟允许文件（均待批准） | 最小修改与复用理由 |
|---|---|
| packages/protocol/src/chat.ts | 保留v1类型/Schema与source契约，增加独立扩展类型及可选能力；不往v1 strict事件塞runId |
| apps/web/src/chat/sources.ts | 复用HttpChatSource与工厂，增加显式opt-in扩展源和独立historyKey；公开配置仅含地址/模式 |
| apps/web/src/chat/ChatSession.ts | 复用订阅/草稿/保存队列，扩展创建、恢复和cancelling；仅扩展模式区分dispose与stop |
| apps/web/src/chat/persistence.ts | 保留旧version1读取，新档案记录turn/run/cursor和迁移；冲突/损坏保留原记录 |
| apps/web/src/components/OfficeChat.tsx | 复用聊天UI，准确呈现恢复/取消中/中断及停止行为；不新增管理页 |
| tests/chat.test.mjs | 扩展现有聊天单测、取消和兼容回归，不复制测试框架 |
| tests/chatDatabaseLifecycle.test.mjs | 扩展旧档案、增量存档、迁移冲突及失败保护 |
| tests/integration-e2e/office.spec.ts | 复用浏览器夹具，加入超过180秒、刷新、断网、重复提交与真实停止 |
| integration/docs/chat-integration.md | 说明新模式、接口、部署、兼容、限制与回退；文档也须先获批 |

其他Office文件均不在候选范围。若实施时必须触及OfficeTeamSession装配、其他导出/Schema、配置或依赖，先修订Rx并重新人工审核，不以“必要关联”推导隐含许可。预计无需改Fastify、场景/白板、package.json或锁文件；T6开工前须按获批Office基线再次核实。

影响：v1仍默认，其source和旧存档读取不变；扩展仅在显式选择后生效。扩展刷新变detach，但工具审批和工作区权限不变，runId/cursor不充当认证。模型/Office密钥仅服务端持有，不放VITE_*或IndexedDB。Linux/Windows均通过同源HTTP和浏览器存储工作，无WSL或shell依赖。状态由真实Host终态驱动，不以停止点击代替清理完成。

发布顺序：先部署兼容v1且提供扩展接口的Host，再更新可选Office源，最后人工启用。回退先关闭扩展选择并显式处理活动Run，再切回v1源；不删除扩展档案、旧存档或JSONL。先备份浏览器导出及Host数据；旧前端只读取旧historyKey，扩展档案保留供修复恢复。不承诺活动Run能无损降级为v1会话。

人工审核须确认必要性及不修改Office的限制、准确文件范围、v1兼容/档案版本、权限/部署、长任务/重试/断线/取消/迁移测试和回退。批准人、日期、准确设计提交及未来两仓PR审核/合入仍待发生；T0只交付提案。
