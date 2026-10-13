# OpenPaw B 线追踪矩阵

## CHG-2026-0025 当前重构追踪

| 本轮要求 | 最终职责与实现 | 验收入口 |
| --- | --- | --- |
| runtime 通用职责下沉 | Engine scheduler/turn_execution；Store journal/transcript/catalog；Host 留协调及协议适配 | Engine/Store 单元与既有 Host HTTP/reliability |
| CLI 实际复用 | CLI chat 调用相同 Engine 执行包装及 Store 消息事务 | chat_e2e/chat_approval_e2e；session_host_parity |
| ToolResult 不漏存 | 只排除已保存输入，保留 User 角色的工具结果 | 真实 CLI 保存 6 条消息；基线探针 0 个结果、当前 1 个结果 |
| 完整状态恢复 | 完整 Session checkpoint；异常、取消、future drop 或提交失败时恢复 | Engine usage/压缩/丢弃测试；Host 取消与提交失败的完整状态断言 |
| 保留严格行数门禁 | Store 按实测由 3000 调至 3500；其他上限和计数规则不变 | xtask lines 及 CHG 实测表 |

本节只追踪当前已批准范围，历史审计、跨平台 CI、真实服务与完整发布裁决分别记录。

> 历史审计快照：本文件保留 2026-09-17 早期反向审计结论，不能作为当前候选事实源。当前实现状态和未关闭发布项以 `../../ai-office-react-main/docs/01-需求基线.md`、`03-测试大纲.md`、`04-验收记录.md` 为准。

审计日期：2026-09-17
发布裁决：**未通过，不建议发布。**

状态限定为：`已编码`、`局部验证`、`失败`、`待批准`、`待实施`、`阻断`、`范围外`。
“已编码”不等于测试通过或已获人工批准。

| master-plan 条款 | 当前代码与设计 | 当前证据 | 状态 |
| --- | --- | --- | --- |
| §2.2 feature 隔离 | Host/CLI feature 与 cfg 已编码 | 历史 feature-off tree 无 Axum，本候选未完成全门禁连续复跑 | 局部验证 |
| §2.3 模块结构与行数 | protocol/http/runtime/events/approval/office 已拆分 | 历史 lines 预算有记录 | 局部验证 |
| §2.4 CLI 与 Host flags | 参数和启动代码存在 | help snapshot 未覆盖三个 Host 全局旗标 | 阻断 |
| §3.2 回环、鉴权和工作区优先级 | HostConfig、middleware、create_session 已编码 | HTTP 定向测试 15/15 | 局部验证 |
| §3.2 Session REST/JSONL | create/list/get/message/status/turns/prompt/abort/delete 已编码；纯 ToolResult 历史折叠到 assistant tool part | CHG-0020 Docker Host 92/92；生命周期缺口未覆盖 | 局部验证 |
| §3.2 Session 归档/分支/回退 | PATCH/fork/revert/unrevert、追加式私有事件、Session SSE 包络、ToolResult REST 投影和 assistant 到 user 回合边界规范化已编码 | CHG-0021 Docker Host 93/93；完整 staged snapshot、已隐藏边界 400、assistant REST 游标与未修改 OpenWork 主线页面 Revert/Restore 无刷新实机通过；OpenWork direct/proxy 既有 8/8、0 skip | 局部验证 |
| §3.2 多 session 并行与队列 | 默认 4 running/32 queued、同 session 1 | 基础并发/FIFO/409 有证据；冷加载全局锁和关闭所有权未修 | 阻断 |
| §3.2 provider/model | per-turn provider/model 已编码 | fake OpenAI 和 mock contract 局部通过 | 局部验证 |
| §2.5/§3.2 SSE | 路由、events 和 Provider decoder 已编码 | SSE 单元 3/3、mock 8/8；跨 UTF-8 chunk、截断和终止矩阵失败/缺失 | 失败 |
| §2.6 审批 | asked/replied/reject/abort 代码存在 | 普通单元路径有证据；永不返回 approval 的及时取消未证明 | 阻断 |
| §4.1 Office 队列 | v1 MPSC worker 和 HTTP POST 已编码 | 队满 `try_send` 可丢，无 outbox/ACK/replay | 失败 |
| §4.2 名册动作 | 工具映射、owner 和单 visit 已编码 | 第 5 号角色终态与根计划冲突 | 阻断 |
| T2 固定协议 fixtures | 只有类型与动态测试 | 三个计划要求的固定 JSON fixture 未落地 | 待实施 |
| T3 生命周期恢复 | lazy load、delete、restart 代码存在 | 只恢复最后一个未终态 turn；冷加载可阻塞其它 session | 阻断 |
| T4 长回复 | Provider/Host/JSONL 链路存在 | 50K/180K 中文和 emoji 已确认损坏 | 失败 |
| T10 因果动画 | v1 可产生动作 | v2 唯一主合同 CHG-2026-0014 仍为 draft | 待实施 |
| T13/M6 三方联调 | OpenPaw + AI Office 有基础联调；OpenWork attached Session mutation 已验证 | direct Host 与 `bootAttachedWorld` 代理 journey 8/8、0 skip；完整三方候选仍缺失 | 局部验证 |

## 主阻断与变更单

- `CHG-2026-0013`：B01-B16；其中 B12-B16 正式归档 G01、G02、G06 稳态边界、G07 和 G08 流预算，覆盖请求幂等、SSE gap/reconnect、FIFO 公平性、组合长回复和累计流资源限制。
- `CHG-2026-0014`：唯一跨仓动画合同，覆盖 `taskID` 来源、因果键、outbox、journal、ACK、lease、snapshot 和物理场景投影。
- `CHG-2026-0015`：主合同及附录正式归档 G03、G04、G05、G06 关闭边界和 G08 session 历史预算，覆盖 typed load error、真实 `taskkill /F` 五阶段恢复、完整 shutdown、任务注册和资源预算。

三份 CHG 均为 `draft`，当前只能登记设计与证据，不能修改运行时代码或宣称缺陷关闭。

## G01-G08 可靠性追踪

下表的统一状态为 **待批准 / 待实施 / 阻断**。已有局部测试只能说明旧路径可运行，不能关闭新合同。

| 测试大纲项 | CHG 与阻断归属 | 设计和允许实施范围 | 对应验收 | 当前状态 |
| --- | --- | --- | --- | --- |
| G01 请求幂等 | CHG-2026-0013 B12，§13.1 | `http.rs`、`protocol.rs`、`runtime.rs`、message request Schema；同 session 同键同指纹重放原 turn，冲突确定性失败 | CHG0013-020；与 G04 强杀恢复联合 | 待批准 / 待实施 / 阻断 |
| G02 SSE gap/reconnect | CHG-2026-0013 B13，§13.2 | session/global lag 显式 `stream.gap`、断开连接、REST 2 秒恢复、SSE control Schema | CHG0013-021 | 待批准 / 待实施 / 阻断 |
| G03 session load 错误分类 | CHG-2026-0015 §13.1 | `not_found`、`corrupt`、`permission_denied`、`unavailable`；Host typed error、HTTP 映射及条件错误 Schema | CHG0015-012 | 待批准 / 待实施 / 阻断 |
| G04 真实进程强杀恢复 | CHG-2026-0015 §13.2 | 独立 Host 二进制、`taskkill /F` 五阶段、至少 100 次；与 CHG-2026-0013 B04/G01 联合验证 | CHG0015-013、CHG0013-006/020 | 待批准 / 待实施 / 阻断 |
| G05 完整 shutdown | CHG-2026-0015 §13.3 | SSE、同步 `/message`、异步 prompt、半上传、queued/running、approval/tool、首次/二次 Ctrl+C | CHG0015-014 | 待批准 / 待实施 / 阻断 |
| G06 容量、队列、公平性和关闭收敛 | CHG-2026-0013 B14/§13.3 与 CHG-2026-0015 §13.4 | 0013 负责稳态 4/32、FIFO、公平性和取消晋升；0015 负责关闭期 admission、registry、permit 与任务收割 | CHG0013-022、CHG0015-016 | 待批准 / 待实施 / 阻断 |
| G07 组合长回复 fixture | CHG-2026-0013 B15，§13.4 | 两种 Provider、Markdown/代码/多 block/组合字符/tool arguments、50K/180K 和确定性切块逐字节哈希 | CHG0013-023 | 待批准 / 待实施 / 阻断 |
| G08 累计流与 session 历史预算 | CHG-2026-0013 B16/§13.5 与 CHG-2026-0015 §13.5 | 0013 负责请求、Provider/Host 流和事件预算；0015 负责文件、行、事件、消息、冷加载和峰值内存预算 | CHG0013-024、CHG0015-015 | 待批准 / 待实施 / 阻断 |

任何 G01-G08 项只有在 CHG 获真实负责人批准、允许范围内代码和 Schema 完成、对应测试在同一候选上通过并附真实证据后，才允许从“阻断”转为通过。公共 wire、Schema、Core/Store 或未登记文件若成为必需范围，必须先扩展 draft 并重新审批。

## 候选身份

2026-09-17 审计输入快照为 dirty `main`，HEAD
`60a531a51172d14fc3db1a8e7eb6762c2c8369d8`。在本轮文档收口前，清单包含 Git
tracked 与未忽略 untracked 的 10,044 个条目，其中 10,043 个现存文件、1 个删除项，
总现存字节 5,097,648,311。按 `present|deleted<TAB>sha256<TAB>bytes<TAB>path`
排序并以 LF 连接后，SHA-256 为
`ca62b47296a736244d5a92b0b9d9ccc45d30980b479fbcd741e3317a17f35d74`。
该值标识审计输入快照；随后仅修改审计文档，因此最终发布候选必须重新生成独立清单，
不能用本值冒充发布制品哈希。

## CHG-2026-0022 三项用户缺陷

| 缺陷 | 实现 | 验收证据 | 状态 |
| --- | --- | --- | --- |
| 回退替换后缓存复活 | runtime 提交后按共享投影发 message.removed，随后清游标 | HTTP 完整 ID 与顺序、幂等负例；真实 OpenWork revert/resend 页面 1/1 | 局部验证；新旅程 Passed，发布门禁仍阻断 |
| accepted/no-result 隐藏 503 | 安全 provider 摘要、私有 finish metadata、assistant error + session.error、REST/restart 投影 | reliability 故障矩阵与上下文隔离；真实 SSE/REST attached 7/7；真实 503 页面三步 Passed | 局部验证；旧错误测试与完整门禁结果见 verification-report |
| 输入直到输出结束才可见 | HTTP 接收后在执行前发布完整 user text part | 阻塞 provider 断言；真实 OpenWork 首输出前唯一正文可见，刷新一致 | 局部验证；新旅程 Passed，发布门禁仍阻断 |

此追踪只针对已批准 CHG-0022；不改变上文完整 master-plan、压力和发布验收状态。

## CHG-2026-0023 修复追踪

| 缺陷 | 共享实现 / 接线 | 回归 |
| --- | --- | --- |
| CLI 第三方配置与模型占位 | LLM config；唯一 ChatArgs；chat/config/model/doctor 与 Host 工厂 | CFG-001～008；chat_e2e/provider_config_e2e、Host provider/override |
| Host 会话未被 CLI 发现，隐藏历史泄露风险 | Store transcript/catalog；SQLite 原子刷新；CLI 与 Host 形状 adapter | HIST-001～010；session_host_parity、Store 与既有 Host 恢复/回退 |
| CLI 无人工审批与重复 pending | Engine gate/manual；CLI 输入所有者；Host permission adapter | APR-001～007；chat_approval_e2e 与 Host approval/HTTP |
| 历史/分支回复 Revert 仅隐藏输出并出现 unresolved | Host normalize_revert_boundary 保留回合关联，缺记录时从已验证真实 assistant 定位最近输入并跳过纯工具结果 | HTTP 两项 red→green；Host 99/99、CLI 互通 3/3、OpenWork UI 2/2 零 skip；本机已部署、报告旧游标可撤销修复后 DOM 无残留输入/提示，其他会话游标保持不变；细节及既有门禁限制见 CHG 第 13 节 |

初始红灯和修复候选的实际命令输出保存在仓库外 `../artifacts/chat-runtime-audit/`；当前验收结果在 CHG-0023 第 12 节。
当前编码与功能验证完成，Rust 回归及 OpenWork attached 契约通过；行数门禁失败、外部治理框架缺失，正式验收仍受阻。

## CHG-2026-0026 对应关系

| 需求 | 实现 | 验收证据 |
| --- | --- | --- |
| Q1–Q5、Q7、Q15：媒体理解与显式模型 | Core.media、LLM config/OpenAI/Anthropic | Core/config单元、mock_server_contract |
| Q6、Q9、Q17：内联原件与自动压缩 | Store.media、Engine compaction/TurnExecution、CLI/Host完整历史 | media_history、multimodal_e2e、Host inline_media |
| Q8、Q11：工具读取与协议适配 | read_file/read_attachment、ToolResult.media、Provider | media_read_e2e、HTTP工具配对、Hook单元、Host并行工具 |
| Q10、Q12–Q14：入口、限制与客户端范围 | CLI附件命令、Host file parts、schemas | chat_e2e、Host inline_media、schema样例校验 |
| Q16：本地验收 | 临时目录与假服务，不使用真实模型 | 实际命令/退出码见CHG；真实理解效果暂未验收 |
