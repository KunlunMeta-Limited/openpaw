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

## Office main 与子会话设计调查（2026-10-20）

状态：调查及设计记录，尚未实施业务代码。子 Agent 模式已于 Q1–Q3 明确
确认；Q4 的文本去重建议已撤回，用户随后要求继续。用户调用 to-spec 后，
完整规格已发布到工作区本地 Markdown tracker，状态为 ready-for-agent；
后续需求以该规格为准，本记录保留调查依据与现有文件候选范围。

规格：[AI Office main 接入 Engine Host 与持久子会话](../../../../../.scratch/office-main-engine-host/spec.md)。

### 已确认边界与领域关系

- 源码只允许编辑本 crate 的现有文件；不新增源文件、依赖、服务或容器，
  不修改 AI Office main、feature 或 OpenWork 源码。允许重建现有镜像及设置
  构建、启动参数。四个既有容器继续由 Compose 管理。
- 工作目录（workspace）是实际文件操作的目录；会话（session）是有独立
  身份和历史的交互。同一目录可包含多个会话。
- main 的办公室按规范化工作目录获得稳定 teamId；每个 conversationId
  对应独立父 session，不与已有 OpenWork session 合并历史。
- 独立 session 不构成访问隔离：读取同一 Host 会话目录的已认证 OpenWork
  仍可能列出并打开 main 的 session。
- 本轮 main 接真实聊天和人员状态，不接任务管理；人员来自真实父会话和
  委派事实，无事实的场景角色不伪造在线状态。
- Host 同源代理 main 页面及 gateway，沿用现有 Basic 鉴权。main 故障不
  改变引擎回合结果；恢复后重新初始化并同步当前办公室状态。

### 一手调查依据

1. OpenCode v1.18.18，固定提交
   `31406ccc51b4bd2a4e1e086b2bcaa5f7f804f26d`：
   [task.ts](https://github.com/anomalyco/opencode/blob/31406ccc51b4bd2a4e1e086b2bcaa5f7f804f26d/packages/opencode/src/tool/task.ts)
   的 task_id 参数明确用于恢复已有子 session；无该参数时新建带 parentID
   的 session。父工具 metadata 携带 parentSessionId、sessionId、model；
   子任务默认以前台执行，父取消会取消子执行。没有根据名字或任务相似度
   自动合并身份、根据相同 prompt 自动返回旧结果的逻辑。
2. OpenCode dev，固定提交
   `d30e990c69efd8a954413a724b35c5a27462f320`：
   [subagent.ts](https://github.com/anomalyco/opencode/blob/d30e990c69efd8a954413a724b35c5a27462f320/packages/core/src/tool/plugin/subagent.ts)
   恢复已有 session 时检查 parentID，缺失或其他父会话的目标拒绝使用。
   dev 的 subagent/sessionID 与 v1 的 task/task_id 不同，不能混称同一接口。
   v1 的目标查找失败会退回新建，且没有同等的 parentID 校验；本方案不
   照搬这两点。新建子会话只接收委派 prompt，恢复才保留其先前消息和工具
   输出，见同版本 [task.txt](https://github.com/anomalyco/opencode/blob/31406ccc51b4bd2a4e1e086b2bcaa5f7f804f26d/packages/opencode/src/tool/task.txt#L15)。
3. Codex 官方[子 Agent 文档](https://developers.openai.com/codex/subagents/)
   描述独立 Agent thread、后续指令、状态观察及汇总结果。开源实现固定提交
   `e18a20d6eb6f5ede32a69fb56e3fe276d5e9d914`：
   [spawn.rs](https://github.com/openai/codex/blob/e18a20d6eb6f5ede32a69fb56e3fe276d5e9d914/codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs)
   创建独立 thread，可选择继承父历史或空上下文；
   [followup_task.rs](https://github.com/openai/codex/blob/e18a20d6eb6f5ede32a69fb56e3fe276d5e9d914/codex-rs/core/src/tools/handlers/multi_agents_v2/followup_task.rs)
   使用 TriggerTurn，
   [send_message.rs](https://github.com/openai/codex/blob/e18a20d6eb6f5ede32a69fb56e3fe276d5e9d914/codex-rs/core/src/tools/handlers/multi_agents_v2/send_message.rs)
   使用 QueueOnly；向空闲 Agent 发送消息和启动下一回合是不同操作。
4. 本地 OpenWork 源码的 `parse-tool-parts.ts` 只把 task 工具的
   state.metadata.sessionId 转为子会话入口；`subagent-run-line.tsx` 可从父
   任务卡打开子会话；sidebar 的 utils.ts 将有 parentID 的会话从根列表
   隐藏。运行中镜像的具体呈现仍需后续回归验收，不以源码阅读代替验收。
5. 本 Host 当前 delegation.rs 每次创建临时 Session，只保留身份和结果缓存；
   office.rs 已通过 workspace_output 的 model_delta/model_final 向 feature
   子空间发送过程文字和结果。这种展示不等于持久子会话。

### 已确认的子 Agent 方案

采用有明确父子关系的持久子 session；每次委派执行是该 session 的一个
turn。新建时仅接收父 Agent 提供的任务及必要背景；明确指定子 session ID
时继续其已保存历史。名字仅用于展示，不用名字或相似文本决定恢复身份。
每条新任务真实执行；同一请求的幂等重放与旧任务结果缓存分开处理。

保留现有 delegate_task(task, name) 调用，增补可选 task_id 用于明确继续；
返回真实子 session ID。旧逻辑 workspaceId 与合法子 session ID 分别管理：
前者可能包含冒号，不可直接用作 sessions/*.jsonl 的文件名。恢复须核验父
session、工作目录和当前执行代际；无有效目标时明确报错，不静默改成新会话。

runtime 继续拥有 session 创建、历史提交、恢复和取消的写入权。持久历史
使用现有 JSONL/Store 机制，新对话产生正常运行数据，不引入第二套数据库。
旧子空间没有保存的 transcript 不伪造；保留已有结果、产物和空间身份。

main 输入框继续显示父 Agent 回复，子结果通过父工具结果进入父上下文；
子 Agent 完整历史可通过既有 session API 读取。OpenWork 的展示适配需要
提供 parentID 及原生 task 工具 metadata，而不能假定 delegate_task 已被
识别为可点击任务卡。适配仅在 Host 的协议投影内实现，保留内部工具事实。

feature 继续接收既有 /actions：sessionId 为父会话，workspaceId 为原子
空间身份，parentWorkspaceId 为父空间；保留状态、往返交接、model_delta、
model_final 和产物归属，不能用子 session ID 替换整条动作的 sessionId。
main 则将同工作目录的父子人员汇总到一个办公室。

默认前台委派，父执行等待结果，独立子 session 可以并行；同一个子 session
仍只允许一个活动 turn。取消当前父回合取消其活动子执行，保留已提交历史，
不波及其他父会话。保留 ADR-006 的桌面和审批隔离：子执行不继承父 always，
其 own always 不跨委派执行延续。Codex 式后台邮箱、独立 wait/send/close
工具属于可选的后续范围，不作为本草案已确认需求。

### 本轮 Q1–Q3 的确认记录

- 调度：用户确认本轮实现持久子会话、显式恢复、前台等待、独立子会话并行
  和父取消联动；后台执行、消息邮箱、wait/interrupt 等完整异步编排后续再做。
- 新建上下文：用户确认只传任务及父 Agent 明确提供的必要背景；不自动
  复制完整父历史。恢复子会话则保留它自身已有历史。
- 用户直接操作：用户确认 OpenWork 打开的空闲子会话允许
  用户直接续聊，活动 turn 期间拒绝并发输入，始终保留 parentID；直接
  对话不会自动追加进父会话，父 Agent 再次委派时可利用子历史获取总结。
  直接续聊仍使用子执行的工具和审批边界，不因此取得父桌面能力或递归委派。

### main 接入的实施设计

1. 入口与身份：保持四个容器；main 的原版静态页面和浏览器 gateway 经
   Host 同源代理，公开入口尽量保持 `http://localhost:15180/office/`，
   现有 OpenWork/feature/Host 端口保持可用。代理及聊天都经过 Host Basic
   鉴权，模型和 gateway 密钥只在服务端使用。Host 的 main 上游地址和
   密钥通过构建/启动参数提供，不增加 HostConfig 的必填公共字段。
2. 聊天协议：实现 main 原版要求的 POST/NDJSON；每个 teamId +
   conversationId 绑定独立父 session，仅当前最后一条 user 输入触发
   新回合。Host 持久历史是引擎上下文，浏览器提供的最近十轮只用于核对，
   不反复追加历史、不覆盖工具记录。核对使用适配器实际交付且正常完成的
   文本；失败/停止的局部文字不作为已完成轮次。来源不明、过期或分歧的
   历史明确报错，不静默合并不同分支。
3. 公共接收语义：复用 Host 既有 requestID/messageID、接收记录、并发 gate
   和取消生命周期，不新增按相同文字及历史位置合并回合的 main 专属规则。
   main 原版请求未传每次提交的 requestID；适配器可为一次接收到的 HTTP
   提交分配内部请求身份，但该身份不能识别另一条 HTTP 请求是否是它的
   网络重试。同一 ID 的内部重放沿用既有机制，不同新提交按新回合处理；
   同 session 活动回合期间拒绝并发输入。接收结果不确定时不自动重提
   执行，不承诺跨请求恰好执行一次。没有稳定客户端 ID 时，不能靠文字
   相同判断重试；用户无需为重新执行而改写输入或新建对话。
   原 Q4 的文本去重建议已撤回：对接文档没有此要求，额外的内容哈希及
   重放策略会混淆主动重做与网络重试。请求去重属于 Host 接收层职责，
   不放入 AgentLoop 推理或委派结果缓存；子 Agent 仍按 task_id 显式续聊。
4. 取消与时限：main 点击停止、连接断开或原版页面三分钟绝对超时，均取消
   本次父执行及其前台委派子执行；保留实际已提交历史和文件副作用。此次
   不改前端时限、不假称 status 心跳能够延长它；长任务可使用既有 OpenWork。
5. 审批：通过 main 已有 decision.request 的选择菜单呈现 once、always、
   reject；子执行 always 明确标注仅本次子执行有效。Host 原 permission
   gate 保持唯一决定权，OpenWork 仍可处理同一请求，最先有效的回复生效。
   按 session、permission ID、执行作用域关联菜单，验证返回身份与选项；
   取消或另一入口已处理后关闭等待连接。离线、超时或投递失败不自动授权，
   不将 Office 网络异常当成 AgentLoop 错误，也不盲目重试可能已投递的菜单。
6. 人员：同目录父子会话投影到同一办公室；名册来自真实会话，online 只
   根据已知在线事实设置。main employee schema 没有 busy 字段，所以
   运行/等待/空闲等详情通过已有查看入口呈现；六个固定场景人物按其绑定
   的真实会话汇总姿态，不生成假任务。snapshot 后和浏览器重连后重同步
   姿态，避免 main 将无任务人物重置为 idle；多人共享人物时不承诺每人
   独立动画。未落实的职责不伪装成真实子 Agent。
7. 恢复与兼容：main 的 seq 持久且单调，收到未初始化错误先重新初始化；
   main 不可用时旧入口和引擎继续工作，有限重试后在恢复时同步。现有
   feature 的 workspaceId、父 sessionId、流式文字、交接和产物归属保持
   兼容。旧缓存没有的子 transcript 不伪造，正常新历史继续使用既有
   Store/JSONL，运行元数据与构建产物均放当前工作区，不新增源代码文件。
8. 构建与发布：main 不改源码，以构建参数启用同源聊天地址；必要的
   Compose 参数覆盖和派生构建配置作为当前目录内的构建产物留存，四个
   容器继续用 Compose 构建/运行。feature 和 OpenWork 继续复用现有镜像。
   记录可重现命令和原镜像/原参数，实际验收后才记为已完成。

### 候选改动与验收

以下为已讨论设计的现有文件范围；本次 to-spec 未修改业务代码：

| 现有文件 | 必要改动 |
| --- | --- |
| runtime.rs | 父子 session 元数据、真实子会话恢复/回合生命周期、main 对话映射 |
| delegation.rs | 显式新建/继续、复用 runtime 执行、取消关联、移除隐式相似合并及旧结果短路 |
| protocol.rs | 可选 parentID 和子任务展示投影；main 聊天请求/响应校验 |
| events.rs | 对应真实子会话的消息和状态，父任务的子 session metadata |
| office.rs | 保留 feature 作用域和投影，增加 main 办公室/人员同步与恢复 |
| http.rs | main 同源代理/聊天与已有 session API 的父子读取和并发边界 |
| approval.rs | 如有必要，仅适配子执行审批路由与清理，保持 ADR-006 |
| README.md | 本调查、最终批准范围、验收和回滚记录 |

公共变化仅包括 session REST/SSE 的可选 parentID、父 task 展示的真实
子 session metadata、delegate_task 的可选 task_id 及上述 main 专用
适配路由。已有 HTTP、审批、取消和 feature 动作字段保持兼容。新增或
修改公共 Rust 项按现有中文 Rustdoc 栏目补齐。Core/Engine/Store/Tools、
CLI、Cargo、其他仓库源码及 CI 不在允许编辑范围，不靠移动类型或调整
预算绕过门禁。仅复用 Engine/Store 的现有能力；runtime 保持 session
写入和生命周期所有权。

验收至少包含：同一子 session 连续两次真实委派确实使用先前历史；不同
父会话或目录不能恢复对方子 session；同 session 并发拒绝；父取消、审批
隔离、Host 重启恢复；main 真实流式对话及停止；OpenWork 父子历史和任务
入口；feature 子空间文字、交接、状态和产物不串会话。新增测试只写现有
文件的 inline test seam，不新增依赖或文件。执行既有 Host、parity 和
仓库门禁，继承的行数预算等失败如实记录，不调预算。
额外验收已有同 requestID 重放不重复执行、不同请求的相同文字仍可新执行、
main 不确定接收结果不自动重提、过期历史拒绝、同 conversation 多轮
历史不重复、真实父子审批竞争/取消、main 三分钟边界和断连取消、gateway
重启重同步及既有四容器健康。先用本地确定性 provider/HTTP 测试验证
边界，再用现有模型配置做必要的真实聊天验收；不以假回复代替真实接通。

回滚先恢复原 Host 镜像及原构建/启动参数；不得清空会话、Office 投影或
用户产物。新私有记录必须允许旧读取器忽略；旧缓存不能冒充不存在的历史。
该草案放在既有 Host README，是遵守用户禁止新增文件的范围决定；不把
历史 CHG 当作本次实现授权，也不声称 CODEOWNER 审查已经完成。
