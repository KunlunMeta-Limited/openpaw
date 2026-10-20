# 源码复用审核与缺陷台账

审核基线、阶段及验收以 [master](master.md) 为准。下面的源码位置相对 OpenPaw 基线，不把静态阅读当成运行通过。

## 复用、延展、修改、新增

| 子系统/源码 | 判定 | 必要性和边界 | 阶段 |
|---|---|---|---|
| engine::AgentLoop、llm Provider | 复用并修改 | 保留推理/重试/压缩/预算；修生命周期，不另写循环 | T1/T3 |
| cli::chat Provider 装配 | 抽取延展 | CLI/Host 共用配置无关逻辑，实际请求模型不能占位 | T1/T3 |
| store::SessionReader/Writer、SessionDb | 复用修改 | JSONL 事实、索引可重建，修完整性，不复用错误的 CLI role 过滤 | T1/T3 |
| tools::ToolRegistry/ToolHost | 复用延展 | 注入环境/调用上下文，包装授权和锁，不拷贝 21 个工具 | T2/T3 |
| tools::exec::Environment | 复用修改 | 现有执行缝隙，增加托管取消；默认兼容 | T2 |
| sandbox::platform::linux | 复用修改 | bwrap、PID namespace、die-with-parent、环境清理可复用 | T2 |
| sandbox::platform::windows | 复用修改 | 已有 restricted token、ACL guard、挂起创建、Job Object；不是从零重写 | T2 |
| sandbox::file_lock | 复用 | 已有 Unix/Windows 共享/独占 RAII 锁，不新增 fs2/假锁文件 | T3 |
| memory::FsSkillStore、goal-ledger::FsGoalStore | 复用读取 | 不调用会隐式写状态的列表管理逻辑，不重建文件解析 | T3/T5 |
| tools::meta::todo | 修复并延展 | 原模块负责原子更新、错误和读取，Host 只消费结果 | T2/T5 |
| engine-host | 新增共享宿主与两适配器 | 当前只有说明；Run/Approval/恢复/投影属宿主缺失责任 | T3—T7 |
| Office 原服务/场景/详情 | 复用 | 原 Fastify/SSE/SceneRuntime/detail-v1 足够常规接入 | T4/T5 |
| Office ChatSession | 复用v1；R1条件扩展 | 固定180秒/无续接是现协议边界；完整长任务体验需提案扩展 | T6基础不改Office；具体Rx批准后实施 |
| sidecar | 原样兼容 | 演示 ask 不是执行后端，不沿用其全局串行 gate | 全程 |

## 已识别缺陷

D01—D11 状态统一为“已定位、未修复”，直到对应缺陷 PR 有回归证据。T0 不修业务缺陷；新增 D12—D15 是建立可验证基线的编译、测试清理和 lint 修正，各独立提交。

| ID | 具体证据（基线源码） | 修复要求/关键注释 | 必须验证 | 阶段 |
|---|---|---|---|---|
| D01 | engine/src/agent_loop.rs 默认 openpaw-default；cli/src/chat.rs 未 with_model；llm 两 Provider 优先非空 request.model | 默认模型正确装配，显式配置优先；不以 mock 掩盖 | 捕获真实 Provider HTTP 请求模型，不需要调用公网模型 | T1 |
| D02 | run_turn 覆盖外部 token；stream_timeout 未读取；select 只围住 stream.next；取消返回 EndTurn | 兼容 RunContext/Outcome；分空闲/总预算；标注取消边界 | 建连、流、退避、压缩、审批/工具等待取消与正常终态区分 | T1 |
| D03 | cli/src/chat.rs 跳过 Role::User，工具结果也是 User；空 call ID，忽略 append 错误 | 不按 role/数组长度过滤；传播失败，保留工具链 | 工具配对、压缩、部分输出、磁盘错误、恢复不重复 | T1 |
| D04 | store/src/session.rs reader 容忍尾损，open_append 不修复 | 备份尾部再修复；中间损坏停止；说明原子边界 | 半行后追加仍可读、备份可查、中间损坏不吞 | T1 |
| D05 | agent_loop 串行校验而并行未统一；忽略 ToolSpec metadata；BeforeToolCall 在审批后改输入 | final-input validate/classify/approve/execute 一致 | 两路径同样拒无效输入；Hook 改输入不绕审批；NeverParallel 生效 | T2 |
| D06 | join_all 等所有工具再发结果；等待污染 duration；panic fallback unknown | 完成即发、任务内计时、外层保留 call ID；等待所有清理 | 快慢工具、panic、取消不留后台 task | T2 |
| D07 | bash::run_with_env 遇 RequiresApproval 直接报错；host 固定 LocalEnvironment | 注入环境和调用级授权，不永久改判 Safe | 批准只允许这次最终输入、拒绝无副作用、自定义 Environment 真被调用 | T2 |
| D08 | LocalEnvironment kill_on_drop 只直接子进程；Windows resume_and_wait 后才读管道、读取截断即停 | 复用 Job，双管道持续排空，RAII 清理所有失败点；FFI 仅 sandbox | 大输出、孙进程、取消/关闭、Job 分配失败、句柄泄漏 | T2 |
| D09 | Windows exec_restricted 失败走 basic_fallback；Restricted 环境健康失败可降本地 | 限制不可用明确拒绝；环境提示不是保障 | 无权限/缺 backend 不执行；网络/文件限制能力如实报告 | T2 |
| D10 | Windows LocalEnvironment 实际 cmd /C，平台分析默认可落到 Bash；cmd parser 占位 | launcher 与分析 shell kind 一致，未知命令保守审批 | 普通用户 cmd、引号/中文/空格/转义、无 Git Bash | T2 |
| D11 | tools/meta/todo.rs 无锁读改写、直接覆盖、吞读取/坏行 | 原模块类型读取/锁/原子更新；区分不存在/损坏/权限 | 并发修改不丢、半写恢复、占用/损坏报错 | T2 |
| D12 | sandbox/src/platform/linux.rs 将 root 移入 create_dir_all 后又在 map_err 中使用，E0382 | 仅借用 root，保留错误上下文和行为；T0 独立修复构建阻塞 | cargo check/test workspace 与全 feature，原有 Linux sandbox 测试 | T0 |
| D13 | mcp/tests/mcp_e2e.rs 只 abort 外层 waiting task 即断言服务已死；真正服务异步停止，存在竞态 | fixture 保留实际 RunningService，cancel().await 后断言；不改 MCP 生产逻辑 | mcp_e2e 与全 feature suite | T0 |
| D14 | Linux 未使用的测试诊断编译进生产；平台尾 return；Goal 的 Windows Path 导入在 Linux 未用 | 测试诊断 cfg(test)，平台尾表达式，Path 按 cfg；不降低 -D warnings | 双 feature clippy、默认/全 feature、MSRV | T0 |

| D15 | cli/tests/sidecar_e2e.rs 等待 ID 时丢弃其他响应，响应乱序后永久阻塞；子进程缺清理 | 缓存其他 ID 响应、超时诊断、Drop 回收；移除并发 RPC 按发送顺序执行的错误假设，依赖读取等待写入确认；不改生产行为 | 并发请求测试与全 feature suite | T0 |

D16（待查，未修复）：默认 workspace 一次运行中 `file_lock::tests::exclusive_lock_blocks_second_acquire` 在释放后立即重获失败；全 feature、首次默认测试、单项复查与最终默认复跑均通过。没有足够证据认定生产锁实现有错，不能把重新通过标为已修复。T3 使用该锁建立资源服务前必须查明、补跨进程回归；Windows CI 同时观察。未删除测试或放宽断言。

| ID | 新发现的问题 | 处理/阶段 |
|---|---|---|
| D17 | T0 原始 Office 快照在 Windows checkout 被转换为 CRLF，哈希不同 | .gitattributes 仅该测试目录 -text，保留原始字节校验；T0 |
| D18 | tools safety 测试把返回的 canonical 路径同非 canonical temp 根比较，macOS /var 链接失败 | 断言 canonical 根，保留越界/祖先校验；生产安全逻辑不改；T0 |

| D19 | 原 cargo-deny-action v1 不能读取现有 RustSec 公告 CVSS 4.0，扫描在加载数据库时失败 | 升级官方 v2（cargo-deny 0.20.2），固定提交；不忽略公告；T0 |
| D20 | lru 0.12.5 与 rustls 0.23.43 命中实际公告 | lru 0.18.5、rustls 0.23.46；原缓存/HTTP 实现复用，MSRV 和全量回归；T0 |
| D21 | serde_yml 0.0.12 命中 RUSTSEC-2025-0068、已归档且无安全升级 | package alias 替换为维护分支 serde_norway 0.9.42，原 golden 不变；公开 YAML 类型身份变化见 dependency-baseline.md 和 CHANGELOG；T0 |
| D22 | 既有 option-ext/webpki-roots 许可证与 deny.toml 政策不一致 | 精确版本例外作为明确待审提案，附原文/源码来源/校验，发布必须携带；不扩大全局列表，不关闭扫描；T0 |
| D23 | parity 确定性测试与场景测试同时写读相同 fixture 文件，可读到截断瞬间空内容 | 回放测试独立 temp 输出目录，保留确定性断言及并行场景；生产引擎不改；T0 |
| D24 | MCP 熔断器测试假定 10ms sleep 必在 50ms 冷却前恢复，macOS 调度延迟导致假失败 | 公共方法保留真实 Instant，私有状态转换显式传时间；原断言加精确边界/新期限验证，不扩大冷却值或重试；T0 |

每项缺陷独立逻辑提交、原因、回归和审核；关键代码注释描述不变量和失败原因，可关联 D 编号，不铺满无意义编号。新增缺陷以 D30 起登记；不把功能缺失硬当 bug。

## 能力缺口和已知风险（不伪装已修复）

| 项目 | 处理 |
|---|---|
| Host 只有占位 | T3 新增；T0 feature 只保证依赖连线，不意味着 HTTP/运行可用 |
| clarify/plan 工具为占位行为 | 首期不宣称真暂停/计划禁写，不自动开放 |
| Goal ledger 有数据，没有自动运行调度器 | 详情只读，不宣称长期自治/调度 |
| Skill 库不等于运行注入已接通 | 首期真实列表/详情，不新增自动注入/技能写管理 |
| SkillManager.list_active 会触发生命周期更新 | T3/T5 使用 FsSkillStore 真正只读能力 |
| Store 每 new 的内存锁不跨实例 | 同一根复用服务；home 单写和工作区所有权复用 OS 锁 |
| 全局 SessionDb OnceLock 绑定 home | 固定 home 显式依赖注入，不在请求变环境 |
| MCP HTTP 连接可能忽略 headers | 未启用本期 MCP 管理；登记风险，未来启用前单独修复，不顺手扩大 T2 |
| 外部编辑器不遵守资源锁 | 不承诺锁能防外部改动；写入校验前置内容及路径，隔离靠真实 backend |
| Office ui.next 无 ACK/requeue | R2 已知窗口，业务审批由 Host CAS 控制；不声称 UI 投递恰好一次 |
| Windows 受限 token 不等于所有网络限制已实施 | 逐项报告实际能力；不能落实请求策略则拒绝，不以环境提示降级 |
| Windows Goal fallback 先删再写不是原子更新 | 本期 Goal 只读，不采用该写路径；将来开放 Goal 写管理前独立修复和故障注入 |
| 固定 SDK 不等于真实 OpenWork 桌面 | T7 SDK 实测；桌面另附指定源码后的追加验收 |

## Office 修改记录

| Rx | 状态 | 内容与必要性 | 审批/实施 |
|---|---|---|---|
| R1 | 提案，待用户批准 | 长任务、刷新续接、detach/cancel、可靠停止；见 r1-chat.md | T0提案；若批准才纳入T6并提交两仓PR，不阻塞T6基础/T7 |
| R2 | 本期不改 | UI 领取租约/ACK/重投；明确丢失窗口 | 后续需求再审 |
| R3 | 本期不做 | 侧栏/富文本/新管理页，现 detail-v1 可承载 | 不建立新 UI |

## 新增代码必要性审核

PR 必须回答：现有模块能否直接复用？能否加无协议接口？为什么必须新增？数据谁权威？是否重复解析/落盘/注册？协议字段是否渗入核心？失败边界和 Windows 是否覆盖？

本台账是相关调用链审核记录，不声称全仓每一行已动态验证；实际命令和结果见 validation.md。

## T0 治理增量与独立缺陷

| ID | 根因与修改 | 回归与归属 |
| --- | --- | --- |
| D25 | scrub只查每规则首个命中；登记的REQ/DES完整ID误报且首个豁免遮住同行后续非法ID。只豁免有权威来源的完整ID覆盖片段，find_iter检查所有命中 | xtask::scrub::tests::registered_identifier_does_not_hide_later_hit；T0独立逻辑提交 |
| D27 | scrub递归遍历忽略的依赖/生成目录，在本机NTFS依赖操作中阻塞；改用Git枚举已管理及未忽略的新源码，读取错误显式失败 | xtask::scrub::tests::source_enumeration_skips_ignored_but_includes_new_files；T0独立逻辑提交 |
| D26 | scrub诊断按字节截100可能切到中文UTF-8中间并panic；改为字符截断 | xtask::scrub::tests::chinese_context_truncates_without_utf8_boundary_panic；T0独立逻辑提交 |

治理复用：固定参考framework的CHG格式/校验原型；项目适配修补严格表格、真实Git范围、保护等级、命令追踪和索引，不运行原型approve。注释复用xtask的syn/quote/doc_lines；全量执行器新增，因为现有xtask没有跨Rust/Node/Python证据汇总职责。详细限制见governance/README.md。历史macOS发现记录保留，当前CI/nightly/release仅Linux/Windows。

| D29 | Windows原生CI的CP1252重定向管道无法打印验证器中文进度，检查开始前崩溃 | 验证器显式UTF-8输出并设置Python子进程UTF-8；治理真实子进程回归；T0独立逻辑提交 |
