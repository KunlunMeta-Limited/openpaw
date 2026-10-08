# B 线测试策略

> 当前审计日期：2026-09-20
> 当前裁决：**核心真实对话缺陷已修复并通过本轮回归，但完整发布门禁仍未通过。** 历史绿测不能替代当前候选的真实 Provider、故障注入、压力与资源门禁。

## 本次 Rust/Host/LLM 复核结论

CHG-2026-0013 已获当前会话批准并覆盖 SSE 跨 chunk UTF-8、严格终止、回合线性化、工具安全、取消和
`fsync` 回滚的主要根因；CHG-2026-0015 已覆盖按 session 懒加载、全量未终态恢复、任务
所有权和 graceful shutdown 的主要根因。两份草案可以作为实现起点，但**尚不足以批准为
完整修复合同或基线发布合同**，原因是以下用户可观察边界还没有明确设计、允许文件和测试
seam。实施前必须先扩展对应 CHG 并完成人工审核，不能在编码时临时决定协议语义。

| 缺口编号 | 未闭环的用户风险 | 当前代码事实 | CHG 必须补充的设计决策和文件范围 |
| --- | --- | --- | --- |
| G01 | `prompt_async` 已写入但 204 在网络中丢失，用户重试后产生重复回合 | `MessageRequest` 没有幂等键；turn/message id 全由 Host 每次生成 | 选择并登记请求幂等契约。推荐可选 `requestID` 或等价 header；同 session 同 key 必须返回原 turn，不得重复写 user、调用 Provider 或工具；同 key 不同正文必须 409/422 |
| G02 | SSE 慢消费者丢事件后界面仍继续等待，用户不知道需要重载 | session/global broadcast 容量分别为 256/512；`BroadcastStream` 的 lag error 被直接过滤 | 明确 gap/reconnect 契约。允许不重放 delta，但不得静默丢失；客户端必须能检测 gap，并由 status/turns/messages 在 2 秒内恢复最终事实 |
| G03 | session 文件中段损坏、缺 meta、权限拒绝或磁盘耗尽时，历史对话可能表现为普通 404 | `load_existing` 把 `SessionReader`、修复、工具加载和 writer 打开错误统一折叠为 `None` | 定义 `not_found`、`corrupt`、`permission_denied`、`unavailable` 的可观察差异、隔离/只读策略和原文件保护；若需要 Store 或协议变化必须扩大 CHG |
| G04 | 进程在 outcome/user/assistant/terminal 的 fsync 边界被强杀后，可能重复、丢失或永久 busy | CHG-0015 重点是 graceful shutdown；当前真实测试主要丢弃 Router/AppState | 增加真实子进程硬杀状态矩阵，明确每个提交点重启后的唯一合法结果；不得只用正常 Drop 模拟崩溃 |
| G05 | 停止服务时已连接的无限 SSE、同步 `/message` 或半上传请求可能阻止 5 秒退出 | 当前 `axum::serve` 没有 shutdown；CHG-0015 未明确 SSE stream 和 HTTP connection 的关闭令牌 | shutdown token 必须覆盖 listener、SSE、同步 handler 和 task registry；明确首次/二次 Ctrl+C、超时强制终止和响应状态 |
| G06 | 全局容量、排队和多 session 压力下可能饥饿、队列位置倒退或资源持续增长 | 默认并发 4、排队 32；已有 FIFO 测试规模有限 | 固定容量边界、FIFO/取消后的晋升规则、429 语义、内存/句柄/task 上限和 30 分钟 soak 门禁 |
| G07 | 50K/180K 虽通过单次哈希，Markdown、长难句、组合字符、工具 JSON 或多 block 仍可能截断/重排 | 现有 LLM contract 只有 8 个普通场景；SSE 单元只有 3 个 | fixture 扩展为文本、Markdown、代码块、多 block、tool arguments 和 Unicode 边界矩阵；逐层记录字节长度、UTF-16 长度和 SHA-256 |
| G08 | 巨型或恶意 SSE/request/session 文件造成无界 CPU、内存或启动时间 | CHG-0013 只给单行/单 event 2 MiB 上限；session 读取会加载完整历史 | 明确请求体、事件数、累计响应、session 文件/消息数的资源预算及超限错误；增加大历史冷加载性能和内存门禁 |

G01、G02、G03、G04、G05 任一未形成批准后的设计合同，或 G06-G08 没有量化预算时，
不得开始“基线发布”验收。若产品负责人明确拒绝某项能力，必须把限制写入协议和用户界面，
并验证不会静默丢数据；不能用“当前没有测试”代表风险不存在。

## 自动化测试层次

1. Rust 单元测试覆盖协议 helper、延迟 mock stream 和动作映射。
2. Rust router 集成测试覆盖路由状态和 JSON 线格式。
3. AI Office Vitest 测试覆盖 schema 校验、文件系统扫描和 React 面板状态。
4. Playwright 覆盖 Vite 外壳、导航，以及浏览器助手、截图/OCR、日程和通知入口的真实交互。
5. `chenningbo/kunlun` 执行跨项目命令门禁，并记录输出和退出码。

## 必需门禁

```text
cargo fmt --all -- --check
cargo test -p openpaw-engine
cargo test -p openpaw-engine-host --features engine-host
cargo test -p openpaw-cli --features sidecar
cargo test -p openpaw-cli --features engine-host --test help_snapshot
cargo build -p openpaw-cli --features engine-host
cargo clippy -p openpaw-engine-host --features engine-host --all-targets -- -D warnings
cargo clippy -p openpaw-cli --features engine-host --all-targets -- -D warnings
cargo run -p xtask -- lines
cargo tree -p openpaw-cli --no-default-features
npm ci
npm run lint && npm run build && npm run test
npx playwright test
```

验收还检查 Basic Auth、格式错误的 JSON=400、同一 session 返回 409、不同 session
并发、SSE 顺序、manual approval 清理、stream abort、动作队列顺序，以及全部五个
`/office/*` 数组接口。

需要外部 API key 的测试明确列为未执行，不得标记为通过。OpenWork attached 的
Session mutation journey 属于 CHG-2026-0019 的跨仓验收范围，必须同时经过直接 Host
和 `bootAttachedWorld` 代理，且结果为零 skip。

## CHG-2026-0019 会话变更矩阵

| 能力 | Host 可观察条件 | OpenWork 可观察条件 |
| --- | --- | --- |
| Rename / Archive | PATCH 同时或独立更新 title/archived；空 patch、空标题、负数为 400；未知为 404；活动归档为 409 且无部分更新 | archive/unarchive 经代理可见；owned Host 重启后保持归档状态 |
| Branch | full/boundary fork 返回新 ID，复制当前可见前缀，重排消息和工具 ID，不修改源会话，不返回 `parentID` | 完整和边界分支均显示为根会话；源 transcript 不变 |
| Revert / Unrevert | revert 隐藏边界及尾部并返回游标；`partID` 为 400 `unsupported_part_revert`；unrevert 恢复且无游标时幂等 | staged revert/unrevert 可观察；替换 prompt 后 committed 尾部不可恢复 |
| SSE / Auth | 四条写路由均要求 Basic Auth；Session 事件固定 `{sessionID,info}` | direct Host 与代理缓存都能消费完整 Session |
| 持久化 / 并发 | JSONL 只追加；archive/fork/revert 与 prompt/delete 竞争只产生一个线性化结果；追加失败时文件与内存不变 | journey 不依赖内部函数或产品测试替身 |

Host 定向 HTTP 矩阵已通过 32/32；OpenWork `openpaw-engine-attach.test.ts` 已通过 8/8、
零 skip。`pnpm typecheck` 退出 0；`pnpm evals:typecheck` 因缓存镜像中既有的 Bun、企业 schema、
NodeNext import 和 Node 22 typings 环境问题退出 2，输出没有指向本次修改的测试文件，仍按阻断记录。

## CHG-2026-0020 工具结果历史投影矩阵

| 场景 | Host 可观察条件 | OpenWork 可观察条件 |
| --- | --- | --- |
| completed ToolResult | REST 省略内部 user ToolResult，并将原 assistant tool part 更新为 completed 和实际 output | 刷新前后的工具状态一致，不产生新的 user admission |
| Revert 最终 assistant | Core 游标仍严格隐藏边界及尾部；可见 REST 历史以 completed assistant tool part 结束 | 不显示 `The task was accepted, but no result arrived` 恢复卡 |
| fork / restart | fork 后 callID 成对重排；重启前后 wire 完全一致 | Branch 与重新进入会话均显示相同工具结果 |
| 无法完整匹配 | 普通用户文本、混合内容或 orphan ToolResult 保持原逐条投影 | 不因部分折叠丢失旧数据 |

Docker Linux 环境的定向回归 1/1 和 Host 全量回归 92/92 通过；最终页面复验和治理门禁
结果记录在 CHG-2026-0020 与 verification report。

## CHG-2026-0021 按回合回退矩阵

| 场景 | Host 可观察条件 | OpenWork 可观察条件 |
| --- | --- | --- |
| user 边界 | 返回请求 user ID，并严格隐藏该边界及尾部 | 既有行为不变 |
| assistant / `assistant_n` 边界 | 通过 `TurnOutcome` 返回同回合 user ID，持久化后重启仍使用该权威游标 | 点击 assistant Revert 后该轮输入、输出和后续消息一起隐藏 |
| 旧 assistant 无可靠 outcome | 保留原 assistant 游标，不猜测或误删此前 user | 兼容旧会话与外部写入 |
| Unrevert / replacement prompt | Unrevert 恢复完整尾部；下一条 prompt 继续沿用既有 committed 语义 | Restore 恢复输入、输出和后续消息 |

Linux Docker 定向 Revert 回归 3/3、Host HTTP 33/33、Host 全包 93/93 通过；格式检查退出 0。
OpenWork `main` 保持无源码 diff，从主线镜像与当前 Host 镜像重建后两个容器均健康。容器内
REST 验证 assistant 请求返回 user 游标，页面验证目标 user/assistant 同时隐藏且 Restore 恢复。

## 当前发布阻断矩阵

| 编号 | 风险 | 当前证据 | 必须新增的门禁 |
|---|---|---|---|
| B01 | 旧 turn 迟到清理覆盖新 turn | 只读代码审查确认；既有 Host 53/53 未覆盖 | barrier 竞争 1000 次，事件/cache/owner 零逆序 |
| B02 | 默认并行工具绕过 validator | 只读代码审查确认 | 串行/并行 validator/guardrail/approval 调用矩阵 |
| B03 | `BeforeToolCall::Replace` 后不重校验 | 只读代码和插件契约确认 | Replace 为非法/deny/confirm 的双路径测试 |
| B04 | 首条 outcome write 成功、fsync 失败不回滚 | 持久化路径审查确认 | `AfterWriteBeforeSync` 三阶段故障注入和重启恢复 |
| B05 | 同一 `Arc<AgentLoop>` 并发污染 cancel/budget | 实例字段审查确认 | 1000 组恰好一执行一拒绝，RAII 零泄漏 |
| B06 | compaction/approval/validator/guardrail 不可及时取消 | await 路径审查确认 | 四类永不返回 future，取消小于 250ms |
| B07 | T2 固定 JSON fixtures 缺失 | fixtures 目录无计划要求的三个文件 | 固定文件 parse/serde/关键字段断言 |
| B08 | T6 help snapshots 缺三个 Host 旗标 | snapshot 审查确认 | feature on/off 和 sidecar 组合 help 测试 |
| B09 | T10 第 5 号角色语义与根计划冲突 | 当前测试要求全部释放，根计划要求保持 working | 人工选择基线方案后再实施和验收 |
| B10 | SSE 按 HTTP chunk 有损 UTF-8 解码 | 50K/180K 中文和 emoji 出现 `U+FFFD`，哈希不一致 | 每字节边界、随机分块、OpenAI/Anthropic、50K/180K 精确哈希 |
| B11 | SSE 缺少显式 sentinel、严格终止和缓冲上限 | `[DONE]` 被转为 `null`；缺终止标志、截断 JSON/tool arguments 仍可能 finalize；缓冲无界 | 显式 `SseData`、25%/50%/99% EOF、重复/迟到终止、2 MiB 边界和恰好一个 `Final` |

## Host 生命周期新增阻断

| 编号 | 风险 | 当前证据 | 必须新增的门禁 |
| --- | --- | --- | --- |
| L01 | 冷 session 持有全局 lifecycle 锁跨磁盘和 `list_tools().await` | 只读代码审查确认；现有 7/7 reliability 未覆盖 | A 冷加载阻塞时，B abort/status 和 C prompt 在 250ms 内完成 |
| L02 | 重启只恢复最后一个未终态 turn | `turn_outcomes.last()` 路径确认 | 至少 5 个混合孤儿全部恰好闭合，连续重启 10 次零重复 |
| L03 | prompt 的 `JoinHandle` 被丢弃且 listener 无 graceful shutdown | 当前 `tokio::spawn` 无 registry，`axum::serve` 无 shutdown future | 真实 listener、Windows Ctrl+C、五阶段关闭、5 秒期限和恢复测试 |
| L04 | session slot、404、失败加载和 tombstone 需要有界回收 | 当前尚无按 session slot 实现 | 10,000 随机 404、1,000 失败 load/load-delete 后 map 有界 |

可靠动画另由协调编号 `XCHG-2026-ANIMATION-001` 管理。当前 v1 `OfficeAction` 无 session/turn/eventId/sequence，Host `try_send` 和 500ms POST 可丢且不可恢复；不得用人物“正在动”作为任务完成证据。

## 用户视角功能与故障矩阵

以下测试从真实 HTTP 客户端行为出发，不允许直接调用私有函数代替。单元测试可用于精确
failpoint，但同一风险至少要有一条经过真实 listener、HTTP、JSONL 和重启恢复的系统证据。

| 编号 | 用户操作或实际感受 | 注入方式 | 可量化通过标准 | 当前状态 |
| --- | --- | --- | --- | --- |
| U01 | 发送包含中文长难句、emoji、Markdown 表格、代码围栏、链接、引号和反斜杠的长问题，持续看到回复 | OpenAI/Anthropic fake server；1/2/3/4/7/31/1024 字节及固定随机分块 | 50K、180K 的 Provider 原文、LLM Final、Host JSONL、message REST 全文逐字节相等；SHA-256 相同；`U+FFFD=0`；恰好一个 completed | 失败，B10/B11 待实施 |
| U02 | 回复到 25%、50%、99% 时网络断开 | 正常 EOF、RST、半截 UTF-8、半截 JSON、缺终止、重复终止 | 零 `Final`、零 completed、零部分 assistant 持久正文；turn 为可解释 failed；下一轮成功 | 待实施 |
| U03 | 点击发送后客户端在收到响应前断网，再重新进入会话 | 分别在 reserve 前、首 outcome 前、user fsync 后、spawn 后断开 | 提交前断开：零 user/outcome；提交后断开：回合继续且最终可恢复；不得永久 busy | 局部覆盖，系统证据缺失 |
| U04 | `prompt_async` 已被 Host 接收，但客户端没有收到 204 并重试 | 丢弃响应、相同幂等键重放 2/10/100 次 | 恰好一个 user、一个 turn、一次 Provider/工具副作用；重放返回相同 turn 身份；冲突正文确定性拒绝 | 设计缺失 G01 |
| U05 | 在 A、B、C 等多个对话同时发送并来回查看 | 4 个 running、32 个 queued、再提交第 37 个 | 前 36 个按合同接收，下一请求稳定 429；不同 session 真并行；同 session 第二回合快速 409；零串线 | 局部覆盖，规模证据缺失 |
| U06 | 排队中的会话点击取消，随后立即重新发送 | permit 前、permit 交接点、Provider 首帧前各取消 1,000 次 | abort HTTP P99 小于 250ms；终态 P99 小于 2s；Provider/工具调用为 0 或按唯一线性化结果完成；容量和队列位置全部释放 | 待实施 |
| U07 | 回合流式输出中连续点击取消、刷新、重新进入 | 首 delta、工具开始、审批等待、持久化前后取消 | idle 后零新 delta/tool/permission；pending permission 为 0；失败/取消记录重启后仍存在；下一轮成功 | 局部覆盖，B06/L03 待实施 |
| U08 | 浏览器或客户端消费 SSE 很慢、暂停读取或断线重连 | 生成超过 256/512 个事件并保持慢 reader | 不得静默继续；客户端收到 gap/关闭信号或明确重连；2 秒内通过 REST 恢复精确 terminal 和完整正文 | 设计缺失 G02 |
| U09 | 等待回复时另一个冷历史会话正在加载或损坏 | A 的磁盘/`list_tools` 永不返回；B abort/status；C prompt | B、C P99 小于 250ms；A 不得持有跨 session 全局锁；失败 slot 可重试且最终回收 | 待实施 L01/L04 |
| U10 | 正常关闭应用，稍后重启继续查看 | queued、stream、approval、tool、terminal 五阶段 Ctrl+C | 5 秒内端口释放和进程退出；每个已接受 turn 恰好一个 terminal；连续重启 10 次零新增恢复事件 | 待实施 L02/L03 |
| U11 | 电脑断电或进程被强杀后重启 | 独立进程在 outcome、user、assistant、terminal 的 write/fsync 前后 `taskkill /F` | 每阶段至少 20 次；JSONL 全部可读或仅最后 torn line 被修复；零中段删除、零重复 user/terminal、零永久 busy | 设计缺失 G04 |
| U12 | 磁盘写满、目录只读或 fsync 失败时发送和结束回合 | 精确 failpoint + Windows 隔离临时目录 ACL/配额 smoke | HTTP/turn 返回稳定脱敏错误；内存、cache、JSONL 一致；修复环境后下一轮成功；原文件哈希前缀不变 | B04 仅覆盖部分阶段 |
| U13 | 打开尾部撕裂、中段损坏、缺 meta、错误 session id 的历史对话 | 四类固定损坏 fixture，连续加载和重启 | torn tail 可幂等修复；其他损坏不得伪装普通 404、不得改写原文件；诊断不含正文/凭据 | 设计缺失 G03 |
| U14 | 加载会话的同时删除、反复打开不存在会话 | 1,000 load/delete，10,000 随机 404，1,000 失败 load | 零删除后复活、零重复 handle、零永久 tombstone；slot 数量回落到真实 session 数加固定小余量 | 待实施 L04 |
| U15 | 关闭应用时仍开着 SSE、同步消息请求或慢上传 | 空闲 SSE、lagged SSE、同步 `/message`、半 body 各触发 shutdown | 首次 Ctrl+C 后停止接收；所有连接在 5 秒总期限内闭合；第二次 Ctrl+C 可立即强制退出；端口释放 | 设计缺失 G05 |
| U16 | Provider、approval、validator、guardrail 或 compaction 永不返回 | 每类可观察 Drop 的 pending future，分别循环 200 次 | 取消单测小于 250ms，Host 终态 P99 小于 2s；future 被 Drop；零迟到事件、零 task/permit 泄漏 | 待实施 B06 |

## 压力、容量与破坏性测试矩阵

### 固定环境和测量口径

- 每次压力运行记录 Git commit、tracked/untracked 清单哈希、Windows 版本、CPU、内存、磁盘、
  Rust toolchain、Tokio worker 数、测试线程数、feature、随机种子、开始/结束时间和日志路径。
- 延迟从客户端发出请求到收到完整 HTTP 状态计算；终态延迟从 abort/shutdown 发出到
  `/session/:id/turns` 出现 terminal 计算。首次冷加载和已加载路径分别统计，不能混在一起。
- 资源基线在预热 5 分钟后采集；使用进程 private bytes、handle 数、线程数、Host task registry、
  scheduler running/queued 和 session slot 数。缺少可观测计数器时必须先补测试 seam，不能靠猜测。
- 所有循环必须零 panic、零 hang、零 data race 可观察结果、零 unexpected 5xx、零 JSONL 中段损坏。
  timeout、skip、ignored 或失败后仅放宽阈值不算通过。

| 编号 | 类型 | 负载/故障 | 发布关闭标准 |
| --- | --- | --- | --- |
| P01 | 调度容量 | 4 个阻塞 running + 32 个 queued + 100 个额外提交 | running 始终不超过 4、queued 不超过 32；额外请求全部 429；释放 permit 后严格按已批准的 FIFO/取消规则晋升，零饥饿 |
| P02 | 多会话循环 | 32 session，每 session 顺序执行 1,000 个短回合，固定 10 个 seed | 32,000 个 turn 全部有且仅有一个 terminal；user/assistant/turn 不串 session；失败数 0 |
| P03 | 取消风暴 | 32 session 在 queued/stream/approval/tool 阶段随机取消，共 10,000 turn | abort 响应 P99 小于 250ms、终态 P99 小于 2s；最终 running=0、queued=0、pending approval=0、registry=0 |
| P04 | 长回复并发 | 4 个 180K + 28 个 50K 混合回合，OpenAI/Anthropic 各一轮 | 每条全文哈希一致、恰好一个 Final/terminal；进程峰值 private bytes 不超过基线加全部在途原文 UTF-8 字节数的 12 倍和 256 MiB 中较大者 |
| P05 | SSE 慢消费者 | 32 个正常、32 个每秒读取一次、32 个完全停读的订阅者，持续 10 分钟 | 正常消费者无缺口；慢/停读消费者显式 gap/关闭且不拖慢 turn；断开后连接/task 数回到基线加 5% 或 8 个中的较大者 |
| P06 | 冷加载压力 | 1,000 个 session 文件，每个 100 turn；并行随机打开 100 个并同时对已加载会话 abort/status | 已加载路径 P99 小于 250ms；冷加载无跨 session 阻塞；单文件读取时间随文件字节数近似线性，不出现平方级退化 |
| P07 | 生命周期槽 | 10,000 随机 404、1,000 失败 load、1,000 load/delete 竞争，连续 10 轮 | 每轮后 slot 数回落到真实 session 数加 32 以内；private bytes 相对首轮增长小于 10% 或 64 MiB 中较大者 |
| P08 | 持久化故障 | outcome/user/assistant/terminal 各 100 次 write、after-write-before-sync、truncate/reopen 失败 | 每次只有“完全未接收”或“一个可恢复 terminal”两类结果；零 user-only、零 assistant-only、零孤儿非终态 |
| P09 | 硬杀恢复 | 五个持久化阶段每阶段 20 次真实子进程强杀 | 100 次重启全部可枚举；除最终 torn line 外 JSONL 哈希前缀不变；全量孤儿恰好闭合一次 |
| P10 | graceful shutdown | 五阶段各 20 次，含 SSE/同步 HTTP；另做 100 次 shutdown-admission barrier | 每次总退出时间不超过 5 秒；关闭侧 prompt 503 且零写入，接收侧完整登记；端口 1 秒内可重新绑定 |
| P11 | 资源 soak | 20 session 持续短/长/工具/取消混合 30 分钟，每 5 分钟重连 SSE | 最后 10 分钟 private bytes、handle、线程和 task 无单调增长；相对预热基线增长小于 10% 或 64 MiB/32 handles 中较大者 |
| P12 | 畸形输入 | 0 字节、超请求上限、深层 JSON、无效 UTF-8、2 MiB 边界、超 2 MiB SSE、10,000 小帧 | 4xx/Provider error 可预测；无 panic/OOM；失败零 Final/零 completed/零部分正文；后续健康请求成功 |
| P13 | 反复发布门禁 | 全部单元/集成/系统测试连续 3 轮，压力矩阵使用 10 个固定 seed | 三轮和全部 seed 零失败；任何 flaky 必须保留首败证据、修复根因后重新从第 1 轮开始 |

P04 的内存预算是候选基线的初始上限，不表示允许每条消息永久复制 12 份。若真实测量远低于该值，
应由性能负责人收紧；若超过，不得直接放宽，必须先用 heap/profile 解释每个主要持有者并消除
不必要复制。P06 的“近似线性”以 10 MiB、50 MiB、100 MiB 三档测量，100 MiB 的中位加载
时间不得超过 10 MiB 的 15 倍。

## 分层关闭标准

1. **LLM 层**：B10/B11、U01/U02、P04/P12 全过；合法流逐字节一致，非法流零 Final。
2. **Engine 层**：B02/B03/B05/B06、U16 和对应压力全过；最终输入只能经过一次合法执行，取消无迟到副作用。
3. **Host 状态层**：B01/B04、L01/L02/L04、U03-U14 全过；JSONL、内存 cache、status、turns、messages 在每个终态一致。
4. **进程层**：L03、G05、U10/U11/U15、P09/P10 全过；正常关闭和强杀恢复都有真实二进制证据。
5. **用户链路层**：G01/G02/G03 已形成批准协议并完成真实 listener 测试；重试、断线、慢消费者和损坏数据不会静默重复或丢失。
6. **候选稳定性**：P13 通过，且治理、注释、Clippy、feature-off tree、行数预算和人工 CODEOWNER 审核全部完成。

任一层仍为失败、阻断、待设计、待实施、未执行或只能依赖 mock/私有函数时，最终裁决保持
“未通过，不建议发布”。测试报告必须列出缺陷编号、首次失败日志、根因、修复 commit 和复跑
证据；不得删除失败记录后只保留最后一次绿色结果。

## Rust 覆盖矩阵

| 风险/功能 | 测试位置 | 判定 |
|---|---|---|
| OpenCode event serde roundtrip | `protocol.rs` unit | type/properties 无字段丢失 |
| 必需、降级、SSE 和 v2 路由 | `tests/http.rs` route matrix | 对应 200/204/404 与 content-type |
| Basic Auth 缺失/错误 | `tests/http.rs` | 401；正确凭据 200 |
| prompt 文本拼接与 provider 切换 | `tests/http.rs` | mock session 请求 echo，返回 echo 文本；回合 override 不改 session provider/model |
| OpenAI base URL/model | `tests/http.rs` fake server | 命中 `/v1/chat/completions` 且 body model 正确 |
| workspace 优先级 | `tests/http.rs` | header 覆盖 query/body |
| workspace 重启恢复 | `runtime.rs` unit | 新 AppState 仍使用原绝对路径 |
| delete/load/prompt 竞态 | `runtime.rs` unit | stale handle 不可 reserve，删除后不复活 |
| approval asked/replied/abort | `approval.rs` unit | pending 清理；abort 后无迟发 asked |
| approval 后取消工具 | `engine_e1.rs`、`agent_loop.rs` | cancel 后 ToolHost 调用计数为 0；批准后再次检查 token |
| stream abort | `runtime.rs` unit | idle 后无 text delta |
| Provider 建流与逐帧超时 | `engine_e1.rs`、`agent_loop.rs` | 建流/无帧等待有界；无输出可重试；已有输出不重复；取消优先；耗尽返回 `Error::Timeout` |
| 多 response completion | `runtime.rs` unit | 每个 assistant id 都有 completed |
| Office mapping/单 visit | `office.rs` unit | ToolUseStart 映射，SuggestConfirm 不加 visit |
| Todo 与 Office 动作隔离 | `office.rs::todo_tools_do_not_emit_office_actions` | 三个 Todo 工具均不产生动作、角色占用或 visit |
| Office 多 session 仲裁 | `office.rs` unit | A 结束不 idle B 正在使用的角色 |
| 回合 idle 发布顺序 | `runtime.rs::idle_event_observes_final_cache_and_inactive_turn` | 收到 `session.idle` 时 `active=false` 且最终 assistant 已进入 `message_cache` |
| Office 回合终态归零 | `office.rs::turn_ended_releases_all_roles_without_fake_review_work` | 单 session 结束后全部角色释放，不保留虚构整理工作 |
| Office gateway 超时隔离 | `office.rs` unit | 不响应服务不阻塞事件投影 |

## 测试数据与隔离

Rust 文件测试使用 `tempfile`，不会读写真实 `OPENPAW_HOME`。fake OpenAI 仅绑定
随机回环端口，并在测试结束时终止。Kunlun 使用
`chenningbo/kunlun/.runtime` 下的独立 home/workspace。外部 API key、生产数据、
远程系统和 OpenWork 均不进入测试。

## 验收记录规则

每次候选版本都重新运行命令，报告记录日期、工作目录、命令和退出码。源码修改时间
晚于报告时，旧报告自动视为失效证据。Windows 缺少 Clippy component 时只记录
“未执行”，Ubuntu/WSL Clippy 必须真实退出 0；任何 skip 都不能替代计划内门禁。

2026-09-16 Windows 历史记录：`cargo fmt --all -- --check` 退出 0；
`cargo test -p openpaw-engine-host --features engine-host` 当时退出 0；`CARGO_TARGET_DIR=target-windows cargo build -p openpaw-cli
--features engine-host --locked` 退出 0。该记录早于 2026-09-16 新确认阻断，不能作为当前发布通过。

2026-09-17 当前强但有限的复测证据：`openpaw-llm` mock contract `8/8`、SSE 单元
`3/3`、Host reliability `7/7`、runtime `10/10`、HTTP `15/15`。这些测试不覆盖上表
B01-B11 或 L01-L04；尤其 B10 已通过真实 50K/180K 链路稳定复现，因此当前
LLM/Host/Engine 不能判定发布通过。

## 长回复失败证据

- 50K provider 原文长度 50000，SHA-256 `1df9604431a8093408ed7159a473bd226f66b11465071000efe89d31268fa194`。
- Host/JSONL 长度 50004，SHA-256 `920532cfed5980ae61ba76d73ea8b29e982c14c02032cd988afd01ed5a909aa5`，首个差异约在 UTF-16 索引 2823。
- 180K JSONL assistant 长度 180004，替换字符 7 个，SHA-256 `515f4e8aa19ee545fe5907ac94d1811b049b67c7a4041db38667af48282d86c0`。
- 根因位于 `crates/openpaw-llm/src/sse.rs`：每个 HTTP byte chunk 独立 `String::from_utf8_lossy`，没有保存未完成 UTF-8 后缀。

完整修复设计、允许文件和 TEST-018/019 见 `docs/changes/CHG-2026-0013-回合线性化与工具安全闭环.md`。冷加载、全量恢复和受控关闭见 CHG-2026-0015。两份 CHG 的批准只表示允许按登记范围实施，不等价于发布通过；当前 Rust 修改和真实验证见下节。

## 治理与注释复核

- 2026-09-17 重新生成索引，共 13 个 CHG；`validate` 和 2 个现有 JSON Schema `contracts` 退出码均为 0。
- 本轮文档范围 `git diff --check` 退出码 0。
- `comments --root crates --language rust --all --report-only` 报告 902 个源码缺口；全仓 2648 的原始数字包含 `target-windows` 生成文件，不作为源码缺口数量。
- Host/Engine/LLM 关键文件定向报告仍有 10 个缺口，全部位于 `crates/openpaw-llm/src/sse.rs`。该文件已经进入 CHG-2026-0013 B10 的允许范围，批准实施时必须同时补齐公共 Rustdoc。

因此只能判定 CHG/Schema 机器结构通过，不能判定公共中文注释合规，也不能用早期“107 个选定声明通过”覆盖当前 `sse.rs` 缺口。

## 2026-09-19 至 2026-09-20 对话缺陷修复与真实回归

### 根因与修复

| 根因 | 修复 | 证据 |
| --- | --- | --- |
| DeepSeek 工具续轮需要保留 `reasoning_content`，旧实现丢失后第二次请求可能被服务端中断 | OpenAI adapter 解析 reasoning，工具续轮只在 DeepSeek 方言下回传 reasoning/tool call/tool result | 精确文件提示已真实执行 `read_file -> write_file -> final` |
| 流内 `service_unavailable_error`、`aborted`、资源不足可能落成空或不完整成功 | 映射为 503/协议失败并拒绝 Final | LLM mock contract 终止矩阵通过 |
| 工具后的 Provider 失败把部分 assistant/tool 历史留在 session，污染下一轮 | Provider 错误恢复回合前 checkpoint，保留 user 与 failed terminal | Host rollback/restart 测试通过 |
| 失败流回滚后仍可能补发不存在的 assistant completed | 仅未取消且整个回合成功时补发最终完成事件 | 新增失败流 completed 缺失断言 |
| DeepSeek reasoning 消耗默认 4096 输出预算，`finish_reason=length` 仍被当作成功 | Host 接入通用 `OPENPAW_MAX_OUTPUT_TOKENS=32768`；Engine 在 session 写入前拒绝 `MaxTokens` | 原始长提示真实完成；MaxTokens 回归测试证明 session 不写截断 assistant |
| 未知 `finish_reason` 被映射为 `Other` 后仍可能 completed | OpenAI-compatible 只接受 stop/tool_calls/function_call/length，其他值显式失败 | terminal matrix 增加 `content_filter` 拒绝 |

### 当前候选自动化

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 退出码 0 |
| `cargo test -p openpaw-llm` | 25 单元 + 15 contract，40/40 通过 |
| `cargo test -p openpaw-engine` | 69/69 通过 |
| `cargo test -p openpaw-engine-host --features engine-host` | 37 单元 + 13 reliability + 19 HTTP，69/69 通过 |
| `cargo test -p openpaw-cli --features engine-host --test help_snapshot` | 4/4 通过 |
| `cargo test -p openpaw-cli --features sidecar` | 26/26 通过，独立 chat/sidecar 未回归 |
| `CARGO_TARGET_DIR=target-windows cargo build -p openpaw-cli --features engine-host --locked` | 退出码 0 |
| `cargo run -p xtask -- lines` | 退出 1；Host 6156/5700、Host tests 2707/1900 超限，aux 10008/10100 通过；不得以压缩测试或提高未批准预算伪造通过 |
| `governance validate` / `contracts` | 14 个 CHG、2 个 Schema 通过 |

Windows 候选二进制：`target-windows/debug/openpaw.exe`，SHA-256
`01A9FDD3807426EB77F84EE2C10B5D1D861300C9CA762789AEA9D08B0D5B1A1C`。

### 真实 Gateway -> Host -> DeepSeek 验证

模型为 `deepseek-v4-pro`，服务端口为 Gateway `8765`、Host `4096`、Vite `5173`。
密钥和完整 Base URL 不进入文档。

| 场景 | session / turn | 结果 |
| --- | --- | --- |
| 精确旅游文件提示 | `dc2042549da24f0b8b57caddd93d70a8` / `1_d06399632da54836a7c480002be4dd27` | 154.91 秒 completed；读取五日攻略并生成十日优化版；最终摘要 680 字，无超时、限流、协议错误 |
| 同 session 第二轮 | 同 session / `2_dfd8bf57bb37435dbbec168aa16ac9e0` | 18.45 秒 completed；返回 340 字行程冲突复核；前一轮未污染 |
| 两个 session 并行 | 开发候选两独立 session | 两轮均 completed，正文分别为 356/814 字，无串线；最终候选仍需按发布矩阵扩大样本 |

真实文件证据：`贵州前往长沙及湘西十日旅游攻略（优化版）.md` 于 2026-09-20 生成，
大小 18576 字节，包含 10 天总览、逐日安排和交通/住宿调整。JSONL 记录了 `read_file`、
工具续轮、`write_file` 和最终 assistant，turn 完成后同 session 可立即进入第二轮。

### 当前仍未闭环

1. 尚未执行用户要求的 100/200 次真实 Provider 连续对话；不能把少量真实成功写成 200/200。
2. CHG-2026-0019 行数门禁未通过：Host 超 456 行、Host tests 超 807 行；aux 仍有 92 行余量。修复需精简实现/测试，或先由人工重新批准预算。
3. 工具副作用不能由 session checkpoint 回滚，写文件/外部调用仍需业务幂等证据。
4. Gateway terminal 与 assistant 正文的有界对账、turns/messages 单路降级、跨标签活动 turn 接管仍需系统测试。
5. 完整 4 running + 32 queued + 第 37 个拒绝、30 分钟 soak、真实断流/限流/重启矩阵尚未在本候选执行。
6. CHG-0013 是覆盖多阶段计划的宽范围 CHG；治理 `diff` 要求实际 diff 与全部声明文件完全相等，当前只实施其中 6 个文件，因此边界命令按“声明文件缺失”失败，不能记为治理 diff 通过。

因此本节只关闭用户当前“不完整响应、第二轮失败”的已复现主缺陷，不把整个发布矩阵标记为完成。
