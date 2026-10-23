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

初始审核时D01—D11为“已定位、未修复”。当前D01—D04随T1 #5合入；D05—D11按下述T2增量跟踪，未完成验收/PR不得宣称关闭。T0 不修业务缺陷；新增 D12—D15 是建立可验证基线的编译、测试清理和 lint 修正，各独立提交。

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

D16历史观察（根因尚未查明时的原始记录）：默认 workspace 一次运行中 `file_lock::tests::exclusive_lock_blocks_second_acquire` 在释放后立即重获失败；全 feature、首次默认测试、单项复查与最终默认复跑均通过。没有足够证据认定生产锁实现有错，不能把重新通过标为已修复。T3 使用该锁建立资源服务前必须查明、补跨进程回归；Windows CI 同时观察。未删除测试或放宽断言。

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

每项缺陷独立跟踪原因、回归和审核；相关修复可按完整逻辑整合提交，关键代码注释描述不变量和失败原因，可关联 D 编号，不铺满无意义编号。新增缺陷以 D31 起登记；不把功能缺失硬当 bug。

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
| D26 | scrub诊断按字节截100可能切到中文UTF-8中间并panic；改为字符截断 | xtask::scrub::tests::chinese_context_truncates_without_utf8_boundary_panic；T0独立逻辑提交 |
| D27 | scrub递归遍历忽略的依赖/生成目录，在本机NTFS依赖操作中阻塞；改用Git枚举已管理及未忽略的新源码，读取错误显式失败 | xtask::scrub::tests::source_enumeration_skips_ignored_but_includes_new_files；T0独立逻辑提交 |
| D28 | Windows自动换行将openpaw-core.api.json改成CRLF，xtask严格比较报假漂移 | 仅该快照-text；真实Git core.autocrlf=true检出字节回归，不改快照或放宽接口断言；T0独立逻辑提交 |
| D29 | Windows原生CI的CP1252重定向管道无法打印验证器中文进度，检查开始前崩溃 | 验证器显式UTF-8输出并设置Python子进程UTF-8；治理真实子进程回归；T0独立逻辑提交 |
| D30 | 将既有quality-gates扩到Windows后，cargo-deny容器Action仅支持Linux，原生runner无法执行 | Linux保留固定Action；Windows从crates.io安装固定cargo-deny0.20.2原生执行相同workspace check，规则不降级；原生CI验证；T0独立逻辑提交 |

治理复用：固定参考framework的CHG格式/校验原型；项目适配修补严格表格、真实Git范围、保护等级、命令追踪和索引，不运行原型approve。注释复用xtask的syn/quote/doc_lines；全量执行器新增，因为现有xtask没有跨Rust/Node/Python证据汇总职责。详细限制见governance/README.md。历史macOS发现记录保留，当前CI/nightly/release仅Linux/Windows。

## D16：T0基线阻塞的确定性复现与修复

2026-10-20用户要求重新审核T0、不满足继续。原来把D16留到T3与T0全量基线要求冲突，本轮提前在原sandbox::file_lock修复；不新增Host资源调度。

根因机制：Unix flock属于open file description。File设置CLOEXEC也不能防止其他线程fork至exec期间临时继承该描述符；原守卫只close自身描述符，继承者仍使锁有效。用File::try_clone保留相同描述对象，在守卫drop后、继承副本关闭前立即申请新独占锁，旧实现确定性失败（WouldBlock/errno11）；证明该机制可造成历史症状，不能断言已追踪到每次历史运行的具体子进程。

修复：守卫Drop先显式LOCK_UN，遇EINTR重试系统调用，再由File关闭兜底；Windows以匹配范围的UnlockFileEx显式释放，不依赖CloseHandle后的隐式清理时机。公开类型/构造签名不变，无新依赖；不暴露提前解锁API。Drop不能传播系统错误，失败后仍关闭句柄且不panic；进程被强杀时仍由OS按句柄/继承生命周期清理，不承诺不存在的跨进程取消保证。

回归全部在原file_lock私有测试模块，不为目录形式扩大API：独占及共享继承描述符、原立即重获断言、独立子进程共享/独占互斥及分别释放。父进程在临时中文/空格路径持锁，子进程真实调用同一API；共享持有者未全部退出时仍阻止独占。没有sleep、放宽断言或跳过测试。

Linux定向6项通过；修复前日志/tmp/openpaw-d16-before.log exit101，修复后/tmp/openpaw-d16-after.log exit0。Windows以及当前交付全量仍按最终准确提交CI核查，未通过前不关闭M0。历史失败保留，本修复以独立D16逻辑提交交审。


## T1修复与新增阻塞缺陷（CHG-2026-0002）

D01：默认模型改取Provider真实默认，显式值优先，共享llm装配供CLI/Host复用；引擎请求与真实OpenAiProvider HTTP捕获分别验收。D02：外部父token不被覆盖，独立运行上下文及四终态，取消覆盖建连/流/退避/压缩/前置校验/分类/审批；工具通知取消后等待清理，迭代预算不再假正常结束。D03：RunObserver顺序等待JSONL保存完整消息、call ID、usage和终态，压缩快照恢复取代长度切片；写失败停止并禁止副作用自动重试。D04：共享字节扫描，先完整唯一备份再截尾，中间损坏拒绝，合法尾补换行。

| ID | 发现、原因及本阶段必要性 | 回归定位与状态 |
|---|---|---|
| D31 | OpenAI/Anthropic在缺少协议结束标记时仍从EOF合成Final，导致假完成；工具JSON组装失败不能降为空对象。阻塞REQ-OP-003可靠终态，归T1 | CLI真实HTTP断流红→绿；llm mock_server_contract::truncated_streams_are_errors_for_both_network_providers；完整双平台待验 |
| D32 | OpenAI只向显示流发送文本，Final.content仅含tool_calls，CLI回复与账本丢文字。阻塞REQ-OP-004消息一致性，归T1 | CLI真实HTTP输出断言红→绿；llm::openai_streaming_text_contract检查Final.text；完整双平台待验 |
| D33 | SSE逐网络块from_utf8_lossy会把跨块汉字改为替换字符，事实已损坏；原字节缓存至完整行再解码。阻塞中文Office产品事实，归T1 | sse::tests::utf8_network_boundaries_do_not_corrupt_chinese；独立旧基线复现/双平台待验 |

相关修改按用户本轮要求整合逻辑提交，D编号、根因、回归、证据逐项保留。T2的审批输入绑定/进程树/隔离/Todo缺陷尚未实施，T1不能宣称已完成这些能力；Office未修改。


T1/D04 Windows整改：原生CI 5a9cc0f的Store两项尾损恢复在set_len返回OS5/Access denied；append句柄只有追加写权限。本轮复用原SessionWriter，先read+write备份修复，再重开append继续写，调用方独占所有权要求不变。现有两个失败用例及备份失败无损用例均保留，准确原生日志见CHG-2026-0002验证记录，新提交重验后才能关闭该交付阻塞。

## T2新增阻塞缺陷D34：命令builder参数与配置

源码核查：sandbox::SandboxedCommand::full_command直接拼接包含shell元字符的参数，部分含空格参数仅双引号包裹而未阻止替换；env builder收集后未使用。此入口目前无产品调用者，但属于T2正在修改的统一执行出口。D34在T2修复：Unix按字面量引用每个必要参数；Windows无法可靠字面化的元字符参数在exec前明确拒绝；未支持的env覆盖明确拒绝，不静默忽略。原bash工具接收的完整命令保持原授权语义，不把builder参数与有意脚本混为一谈。回归与兼容证据见CHG-2026-0003。


## T2缺陷实现增量（CHG-2026-0003，M2尚未通过）

以下为本阶段源码修改，最终关闭需当前双平台全量/M2及独立PR批准合入；不是Office功能完成。专用回归入口由所属Cargo.toml显式注册，原始修复前/后摘要见专用验证记录。

| ID | 修改与关键不变量 | 回归定位 |
| --- | --- | --- |
| D05 | 原prepare_tool统一Hook最终输入、校验、metadata、分类审批和执行前重验；完整Value绑定；实际开始记录失败不得执行或假完成 | engine工具回归d05_*；tools观察器失败零写入 |
| D06 | 原调度按ParallelSafe/屏障；FuturesUnordered即时发、task内计时、外保ID、取消join清理 | engine d06_*及原lifecycle取消等待 |
| D07 | 原ToolRegistry注入Environment和本次上下文，bash精确批准仅覆盖最终输入，危险仍阻止 | tools d07_*，exec_e2e批准后真实echo/exit |
| D08 | sandbox统一原普通/受限托管路径；Linux进程组、Windows挂起Job与即时RAII；双管道超限持续排空 | managed_process d08_*、Windows failed_job_assignment_reaps_the_suspended_process |
| D09 | 删除工厂/平台失败本地兜底；不可落实域名/Windows网络限制建目录/ACL/启动前拒绝 | d09_*；能力矩阵不虚称受限成功 |
| D10 | actual shell显式解析、Windows系统cmd未知须批准；UNC/device cwd拒绝目录回退 | exec_e2e Windows raw cmd、managed_process UNC、原bashast解析 |
| D11 | 原Todo结构只读、严格错误、OS锁完整事务、可取消锁等待、原子替换与大小上限 | tools d11_*：48并发、损坏/权限/锁/超大/链接、Windows大小写/盘符/junction/占用 |
| D34 | 原builder字面参数安全引用，不支持env/Windows字符明确拒绝，不静默忽略 | managed_process d34_*；ac1b582原实现三项真实失败 |

共享根资源锁、Run/Approval CAS和Office DTO属于T3/T5；T2不以复制实现抢跑。Linux普通进程组不保证恶意脱组隔离；当前Windows受限组合明确不可用，原生本地Job执行与受限拒绝分别验收。


### D35：T2全量扫描发现的原TLS依赖公告

当前cargo-deny真实命中RUSTSEC-2026-0382/rustls-webpki0.103.15与RUSTSEC-2026-0381/webpki-roots1.0.9，可能信任已撤销emSign中间证书。原Provider/HTTP工具使用同一TLS链，阻塞全量。计划最小补丁升为0.103.16/1.0.10；保留TLS实现/验证和全部公告门禁，复核精确新版本许可证/来源，不加全局例外或新backend。CHG范围已先登记；完成证据见验证记录，未通过当前双平台前不关闭。

D35修复结果：0.103.16/1.0.10已锁定，原两包许可证实核、精确例外/分发通知同步；70d0385 Linux全量26/26（含cargo-deny与MSRV1.88）通过。原生Windows仍被GitHub付款/额度阻塞；D35及T2其他D项不提前标已关闭/合入，M2/非作者PR审核仍未完成。

## T3开工只读复核（0004方案待审）

Host仍占位。T1的llm::assemble_provider、RunObserver/SessionWriter与T2的ToolCallContext/ToolExecutionObserver、ToolRegistry/Environment可直接复用，无须改core/engine/sandbox。新增Host只负责绑定、接受/并发、持久审批、根资源和恢复。SkillManager::list_active会写telemetry，Host不能调用；FsSkillStore::list_scope与FsGoalStore::list_all旧容错列表会跳过坏记录，本次拟在原Store复用扫描/解析增加checked只读入口，不复制到Host。此为明确的能力延展，未新列D缺陷；实施发现真实缺陷另行登记。T2 PR #6已合入，但其Windows原生验证仍按原安排待补。具体逐文件原因、方法及H场景在CHG-2026-0004，尚未实施。


### T3实施复用复核（0004已获批准，验收待完成）

原AgentLoop/RunContext/RunObserver、Provider装配、SessionWriter/Reader/SessionDb、ToolRegistry与Environment、OS文件锁均直接复用；新增Host模块只管理核心没有的运行注册/绑定/审批消费/根锁/恢复及协议无关查询。Skill/Goal新增checked只读入口仍调用原解析器，不改旧容错列表；有界文本提取沿用原read_file，带行号输出保留。没有修改core/engine/sandbox/chat.rs，也未修改Office。

新Host开发中已核查取消与状态竞争、真实句柄关闭、数据库故障锁存、根身份配置变更及索引损坏恢复；这些为本次新增实现的测试修正，不补造既有OpenPaw D编号。对照0004 H01—H12的准确交付与剩余限制以专用验证记录为准。

## T4 r6：真实网页发现的D38/D39

| ID | 根因、修复与边界 | 回归/当前状态 |
|---|---|---|
| D38 | service.live与store::get_run分别直接拼接跨assistant文字，工具前说明紧贴下一条标题；在原types.rs引入Host内部共用RunText，可靠消息保存后延迟分隔，消息内原文不变，4MiB计入分隔；核心/JSONL不修改 | 原host_office精确文本断言修复前失败，修复后通过；inline验证分块换行、空消息、中文预算；真实网页/精确HEAD完整结果登记于0005验证记录，不提前关闭 |
| D39 | send/resume接受拒绝原用409/503，Office按官方协议转通用502；仅六类可恢复拒绝用HTTP200单条官方error，安全白名单，无新Run/假done；前置4xx和存储503、JSON控制操作保留 | host_office修复前409不满足官方错误事件断言；修复后覆盖请求/历史冲突、忙、容量、缺失及无副作用；真实双浏览器与完整结果见0005验证记录 |

复用判定：延展原live/Store、原错误分类、原Office事件和原产品runner；新增仅Host内部展示累积器与接受错误私有映射。无独立解析器/存储/Provider或Office改动。D36/D37仍开放，T4仅只读工具，不因通过本轮测试关闭shell清理风险。

r6修复实现6530dec的D38/D39已获定向、完整27项和实际网页/在线DeepSeek验证；属于“修复已验证、待交付与PR审核”，不伪造已合入关闭。D36/D37仍开放，Windows原生未执行。原失败报告及复验哈希见0005验证记录。

## T4 r7：原Web工具复用与缺陷

Host默认五项与两层配置校验未开放Web、模型缺当前时间属于接入能力缺口；原搜索/抓取主体可复用，不另写天气服务。按0005 r7仅开放web_search/web_fetch，web_extract职责重叠暂不开放；Office无改动。

| ID | 源码原因与修复 | 复现/回归/状态 |
|---|---|---|
| D40 | 注册表Web分支未传调用级取消，搜索自动正文使用分离JoinHandle；原tools内增加取消入口、搜索托管任务取消并等待，Host原观察器/锁不复制 | tool_runtime的d40_inflight修复前等待超时退出101，修复后实际HTTP等待取消；私有d40_auto_fetch_tasks_close_before_cancel_returns检查两个真实连接关闭；预取消旧实现已正确，作为保留回归，不虚称修复前失败；最终验证见0005记录 |
| D41 | 未知长度响应先bytes读完才判断5MiB；改为chunk追加前检查，超限即错误且不缓存 | web_fetch_e2e::d41_unknown_length_body_is_rejected_before_end_of_stream修复前等待终止块超时退出101，修复后提前拒绝；最终验证见0005记录 |

抓取fresh只绕本地缓存，不保证站点/CDN日期；DNS预查不是连接时地址固定，当前不声称完全防DNS重绑定。D36/D37保持开放，不因本轮Web修复开放shell/写工具。Windows原生仍按用户安排暂缓；不以Linux验证替代。


### D42：在线DeepSeek发现的Bing结果误解析（开放，未修复）

真实8765/DeepSeek调用web_search的tool_result第一项包含“rguments.length”等大段脚本，模型明确反馈搜索无有效天气条目。证据：`artifacts/t4-r7-deepseek-20261022-b/本轮专用会话.jsonl`，call `call_00_B8MtJSVzQJ7t6ryl6lcb5779`；c轮独立再现。源码`web_search/adapters/bing.rs::parse_bing_html`遍历`html.split("class=\"b_algo\"")`，连结果容器前的首段也走宽泛href/标题兜底，页面脚本可进入输出并消耗模型上下文；不能由HTTP200/非空列表认定有效结果。

本轮r7未修改该适配器，不写已修复/已关闭。拟在同一T4主CHG后续修订中登记准确适配器/既有测试路径与批准对象：只解析真实结果容器，剔除script/style等非结果，零结果/挑战页保留明确语义；扩充原web_search_e2e的无b_algo、头部脚本加真实结果、相邻结果及原重定向/域名过滤回归，再用真实网络验证。不能用Host临时字符串清洗或降低断言掩盖。当前天气查询通过实际web_fetch获得有效来源，不等于该搜索缺陷已解决。


### D43：已启用能力下旧会话仍沿用“无联网/时钟”断言（现场复现，待修正）

用户原session `50f53b9517a94dac9aefde208f73d58c`含多轮r7前否定能力的assistant历史；升级后短句追问仍重复否定。2026-10-22真实8765浏览器原会话重现，`artifacts/t4-r7-existing-before-20261022-a/report.json`failed，无Web任务；不是Office预览，也不是新模型配置未加载。源码确认每Run已构建当前system和工具，原r7提示未明确旧自述与本轮能力的优先关系；此前只测新会话漏此路径。

拟仅增强原Host私有system的能力/历史边界，原引擎仍决策并实际调用允许工具；原历史不清空、不写伪事实。回归覆盖禁网历史→同根同Session启用→再次关闭及原网页短句/刷新连续追问。模型是否遵守以在线真实旧会话为准，捕获system字段存在不能单独判为修好；不通过重写历史、强制每个问题联网或天气硬编码修补。


### D43复核与D44：实际请求对比（2026-10-22，仍待修复）

仅增强Host开头system的候选在原对话短句“长沙天气”真实浏览器仍失败：`artifacts/t4-d43-existing-after-20261022-a/report.json`，无Web调用。原先“明确历史边界即可解决”的私有方案不成立；runtime 16项通过只证明结构，不能作为模型实际使用工具的证据。

用隔离复制的同Session、原Host/AgentLoop/OpenAiProvider截获实际HTTP正文：当前时钟与七项tool均存在，末条真实用户问题不变。用户原生DeepSeek端点上同历史temperature=0对比：原开头/短中文开头各零tool_calls；相同可信system放到当前用户文本前，两次均请求web_search/web_fetch。诊断没有执行工具，不能当作天气或浏览器成功；D43保持开放。证据：`artifacts/t4-d43-wire-diagnostic-20261022-a/{actual-wire-body.json,native-compare.json,manifest.json}`；不含模型凭据，隔离数据未回写原会话。

D44：CompletionRequest.system_blocks已有“优先于system”的公开约定，而openpaw-llm/src/openai.rs::build_messages完全忽略它；这不等于原Host本轮信息丢失（原Host未设置blocks），但阻碍使用既有动态上下文接口。拟在0005第9.11节r8补齐原Provider及实际wire回归、Host私有委托；D42在原Bing解析器独立修复。新增范围均待人工审核，尚未实施，不借r7批准越界。Office不改，D36/D37仍开放。


### D42/D43/D44修复候选专项结果（r8，未代替交付验收）

D44原HTTP请求回归修复前两项失败，真实wire只发IGNORED_FALLBACK而丢弃blocks；修复后原mock_server_contract全部11项通过，含complete/stream、优先级/空块、历史原序、混合结果文本及多工具响应连续。D42两项污染用例修复前均失败（首项导航而非真实条目、5个伪结果），修复后原web_search_e2e全部34项通过；容器解析不承诺远端检索相关性。

D43原Host运行/恢复套件及旧历史开关回归通过；真实原会话首次未用特殊提示，“长沙天气”实际2次搜索+3次抓取。a取证误读原始metadata.tools导致报告失败，原账本与官方投影补对账5个call均匹配；首轮AM/PM本地时间解释不可靠，不据其认定实况有效。候选b加强通用时区/数据日期约束后，同conversation刷新追问2次fresh抓取，原API current时间2026-10-22T20:15、Asia/Shanghai和18.7等字段与页面相符；随后日期和再次普通短句均实际验证。来源current估计不等于本站独立观测，模型自然语言其他推断不作已核实事实。

此处均为专项/候选事实，最终准确干净HEAD完整27项与同二进制部署复验另在交付清单核对；三缺陷仍待最终技术核查/用户验收/PR，不因候选成功写已关闭。旧失败与r7批准对象完整保留，D36/D37开放，Office零修改，Windows原生暂缓。


## D42/D43/D44最终Linux技术核查（已验证，待交付与PR审核）

当前结论覆盖前述“未修复/候选”时点，旧失败保留。准确实现932e278完整27/27及原产品旅程通过；D42原解析回归验证只产出结果容器、不混入脚本/导航，不宣称公网相关性完美；D44原HTTP请求测试验证blocks优先与工具链相邻；D43同会话开关权限/日期产品回归和原用户会话在线三轮全部通过。工具、来源URL/时间、Run/call、任务和账本对账，证据集中0005验证记录M4核查与外部delivery清单。三项为“修复已验证，待用户交付验收及PR审核/合入”，不写closed或已发布。

输出质量观察（未定性为新增代码缺陷）：原生DeepSeek将Open-Meteo daily.weather_code=55称作“小毛毛雨”，与代码释义不符；“不用带伞/无雨”和更新频率等推断亦不由原始字段充分证明。本轮逐项数值、时区/日期、URL确已匹配，但不把全部气象解释算通过；后续若将气象解释正确性作为产品承诺，先在本主CHG评审范围和可验证标准，不新增天气专用后端来掩盖通用模型输出问题。D36/D37及Windows暂缓边界不变；T4只读不受未开放shell能力冒充保护，T5开放前仍必须审核D37。


## T4合入闭环与T5只读预审（2026-10-23）

D38/D39（消息边界/接受错误）、D40/D41（Web取消/有界正文）、D42/D43/D44（搜索污染/旧会话能力/原Provider blocks）已随PR #10合入37f0a35，修复范围按各原用例及0005证据闭环；不是所有模型输出质量都已解决。用户确认通过、平台API已核实MERGED，Windows原生仍未验，云端账单/额度未启动均单列。旧“待合入”段落是历史时点。

D36旧Mock空块仍开放，本T5原HTTP确定性Provider不经过该路径；D37取消后进程状态/ESRCH仍开放，不能把重跑成功当修复。T5 0006第9.7节拟先复现区分真实清理与探针竞态，回原sandbox/process及原回归修复，原生Windows未验不得称双平台清理已证明。其余T5缺口（15秒客户端、UI领取/详情、删除生命周期、query等待取消）为宿主延展责任，不新增伪bug编号。准确文件/方法/门禁先见0006待审提案，本轮无生产代码改动。


## T5 D37调查与候选修复（2026-10-23）

原managed_process_regressions的取消用例修复前100轮中13次失败：10次/proc退出竞态ESRCH、3次清理后仍判存活。不能只改探针把全部问题归为误报。原process.rs在SIGKILL后等待leader与EOF仍不能证明关闭管道的后代退出；新增Linux进程组成员状态确认，只把Z/X等不可运行状态视为停止，未知错误不能当退出；发送终止后不再向可能复用组号重复发信号，无全局subreaper。原探针只处理内核ENOENT/ESRCH退出证据，增加实际孙进程ready和PID起始身份记录，保留存活/延迟写入断言。

候选修复后首100组共400次取消/leader正常退出/延迟副作用/预取消检查无失败；最终带PID身份记录的重复和全量结果另登记，暂不据该轮关闭D37。Windows原Job实现未修改，原生按用户决定暂缓；D36 Mock空块仍开放。原始证据artifacts/t5-implementation-20261023-a/d37-before-100.jsonl、d37-after-100.jsonl。


D37开发审查补充：原leader先wait再kill存在PGID复用窗口；Linux使用waitid(WNOWAIT)保留直接子进程身份，原组kill后确认无可运行成员，再交回tokio wait回收。不以固定等待或10秒失败提前放掉仍存活/无法确认的资源。最新100轮报告独立绑定测试二进制；中间版本通过不等于最终全量通过。权限错误不按已退出处理，Z/X为不可执行态；普通模式仍不承诺约束恶意setsid脱组。Office原安全策略拒绝node -e不是新缺陷，验收改为授权范围内实际脚本，不降低危险命令分类。


### D37 Linux验证结论（T5待交付/PR审核）

最新产品复检另见本文件末尾D45/D46；D37原验证不代表最新公开任务/进度需求通过。

原sandbox进程组修复已在准确c951b2b完整27项和真实网页停止中验证。最终回归二进制100轮×4检查均通过，逐轮PID身份/stat/hash见0006验证记录和d37-delivery-100.jsonl；真实DeepSeek停止业务回执后父子均ENOENT，20.5秒无延迟副作用。源码没有全局subreaper或新沙箱；WNOWAIT锚定leader，组清理确认前不reap、不释放资源，普通模式不防恶意setsid脱组。无法确认时维持cancelling所有权，不以超时伪装结束。当前为“修复已验证，待用户交付/PR审核”，不虚填已合入关闭；Windows原Job未改且原生暂缓。D36仍开放。新增删除崩溃恢复属于宿主能力验证，不伪造新D编号。

## T5产品复检D45/D46（2026-10-23，开放待r2审核）

| 编号 | 复现/原因 | 拟修复与边界 |
|---|---|---|
| D45 | projection::prepare直接以tool_name和call_id生成公开任务，非完成统一progress=-1，白板包含实现说明；真实8765截图与快照seq223显示5张失败卡片及“进度未提供” | 0006 r2第9.9将业务目标/子步骤与工具证据分开，公开聚合重试、保留内部错误；真实进度需有计划/完成依据。新计划报告是能力补充，不伪称已有；尚未编码 |
| D46 | service::execute将非取消/预算类异常统一interrupted，turns::message通用化，用户不能知道实际Provider参数格式失败；原Run已failed但页面只有回复未完成 | 复用原RunFinished及安全分类，聊天/历史/resume解释一致；不展示原报文，不改严格JSON解析，不发假done。核心Provider是否有兼容bug尚无原SSE证明，不预判/越界修 |

原读取weather_changsha.md失败是不存在；读取../README.md被授权根正确拒绝；若干公网502/连接失败是实际外部结果，不能通过扩大根/网络权限或伪造结果解决。原8765仅T4只读，不能写Markdown或领取详情；部署版本未统一属于交付缺口。真实人物详情约65秒浮窗超时已复现，T5隔离服务同动作能返回，先升级正确Host/poller，不能因此要求改Office。空白板link仍生成“查看任务拆解”是Office既有模板行为，用户要求另列R11，不误记为OpenPaw核心缺陷。证据定位0006验证记录产品复检；D45/D46仍开放，不以源码阅读关闭。

### D47：浏览器续接丢失审批展示（2026-10-23，修复已验证、待本PR审核）

真实8765/DeepSeek在c9107ab提出天气文件写入后，换浏览器只恢复聊天/快照，没有重放旧菜单。Office仍等待原decision HTTP，Host以为展示活动中，最终审批过期且无文件。证据`/home/cheng/.cache/t5-live-weather-20261023-b/report.json`。在0006原第9.2节和允许文件内修复Host：通过已校验的history/resume触发当前作用域撤旧展示/新display重开，沿用原业务绑定与期限；不把未知、408或重连当批准。补真实reload回归并重跑全量/真实模型，未验证前不关闭。不修改Office、不新增Rx。

### D48：进度报告格式/证据错误缺少恢复信息（2026-10-23，开放）

f9d3d32真实天气c轮已联网、刷新审批并实际写入/回读，但模型在report_progress连续漏type、引用工具名:路径/时间戳或空证据，泛化错误未能引导恢复，任务仍显示未完成。严格拒绝无效证据正确，缺陷是Host新增元数据工具的可用性与恢复诊断不足。原0006第12节登记允许范围方法：格式示例、当前Run已成功且真实started的有界证据列表，所有原校验保持；增强原上下文步骤更新要求及原生命周期回归。原失败证据保留，不修改Office或Provider、不自动确认计划步骤，不以文件存在冒充进度通过。

D47补充验证：准确f9d3d32完整27/27、产品50记录及真实DeepSeek c轮均确认reload后新display、原业务审批/期限不变，批准前无文件、后一次写入成功；c轮总报告因独立D48仍failed，分项依据不能扩大为整轮通过。


D48继续开放：25c6426真实d轮未提交completed，因此错误恢复未触发，虽已实际写入/回读5264字节，公开进度仍25%。追加本轮私有迭代：成功原结果可靠保存后提供精确Host回执，进度接受响应回显有界清单；失败和非Office结果不变、不自动完成、不放宽校验。真实失败位于/home/cheng/.cache/t5-live-weather-20261023-d/report.json，后续准确全量及真实模型待执行。


### D49：查询已有记录被误判为再次写入（2026-10-23，开放）

1fcacf4真实e轮天气已100%，日期追问却因needs_write按标题子串“已保存”“记录”判断写入而拒绝成功glob证据，任务0%/failed。原报告自动passed不足：脚本仅核对回复日期/路径，文件详情也匹配了切换前的列表。原report保留，/home/cheng/.cache/t5-live-weather-20261023-e/review.json明确复检failed。修复原私有标题判别，区分过去事实、名词和明确新增操作；显式新写入仍须原write/edit路径证据，不自动完成步骤。原生命周期/HTTP Provider浏览器补“核对已保存的记录文件位置”真实glob→精确证据→100%和读后写拒绝回归；真实脚本收紧任务终态/正文/关闭等待。未完整/真实复验前不关闭，Office工程不改。


## T5最新缺陷技术状态（2026-10-23，优先于前文历史开放记录）

D45/D46/D47/D48/D49均为“修复已验证，待用户交付及本PR审核”，未填已合入关闭：0df3282完整27/27、浏览器产品53记录，原8765真实h-resume及correlation均passed。D45业务目标/3步骤100%且无call卡/技术note；D46原Provider错误/停止/恢复按安全分类回归，详情3015ms、65249ms无浮窗；D47原审批期限/输入相同、展示更新后只写一次；D48真实成功回执驱动查询和保存完成；D49追问位置任务100%，不强制再次写文件，混合新写仍被读证据拒绝。原c/d/e/f/g/h失败或复检否决记录均保留，外部验证器错误与生产缺陷分列，不以这些记录伪造全量一次成功。D37原验证结论保持，D36仍开放；Windows原生暂缓。最终文档SHA完整复验与hash绑定见artifacts/t5-delivery-20261023/final-review.json及0006最新验证章节；缺少该准确passed绑定时不能据本段提前交付。


## T6本轮产品审查：D50/D51（已复现，未修复）

OpenPaw审查c1e297a/当前Host0df3282。D50为成员投影与六常驻人物入口不一致：默认仅assistant/files/search三绑定，app-agent/review-agent/本现场code-agent真实actor.inspect返回scope_denied。这是用户点击常驻人物不能获知空闲/未启用信息的产品缺陷，原范围校验本身正确，禁止删除校验修复。拟以共同目录和可信成员解析补足。

D51为actions::dispatch丢失目标：actor/employee校验后统一kind=menu；details::render只给空fields的通用会话目录，左侧三成员和已绑定人物都相同。拟保留目标身份/当前任务/结果与导航作用域，六类会话查询继续复用。真实修前HTTP/截图/trace在/home/cheng/.cache/office-product-audit-20261023-r2，详细见0007产品差距审查；修复/新回归待0007准确r2批准，不提前关闭。

多Agent执行器、通用clarify、任务暂停/改派和模型附件是能力缺口，不为凑缺陷编号记成已存在实现的bug。新版Officefocus回归属于集中Rx R14；白板字形/原位菜单分别R12/R13，OpenPaw不能私改Office。大规模全Run/JSONL遍历只是待测性能风险，未实测不当已确诊缺陷。

历史状态补登：D45—D49相关修复已随T5 PR#11合入f22cca1；前文“待本PR审核”保留原时点。D36未修、Windows原生暂缓不计通过。本轮D50/D51不受T5批准覆盖，不能因旧套件通过关闭。


2026-10-23 D50/D51修复进展：agents.rs统一六人物与成员，actions不再丢employee/actor，details含真实字段与作用域导航，原HTTP夹具逐一验证六人物/六成员同目标、返回保留身份及陌生人物拒绝通过。独立子任务详情只经持久父子引用定位准确子账本，工作区共享分类明确标注。尚待真实浏览器逐点击证据与准确完整套件，当前不将D50/D51登记为最终关闭；原修前截图/HTTP保留。


## T6真实模型再核查：D52/D53（已复现，实现修正待完整重验）

- D52：真实DeepSeek五子Run正常结束、目标报告存在且标记/hash可核验，但计划最后一步“王明汇总五位真实结果并引用报告路径”不匹配原纯答复白名单（只有整理/总结/答复/交付），导致页面主目标failed/71%。修复只补“汇总”同义业务动作；仍要求非写入步骤、真实非空答复且Run正常终态，不将失败/取消或文件步骤自动完成。源码task_progress::project；回归delegated_business_step_keeps_one_card_owner_and_stable_assignment同时验证纯汇总和真实派发证据。
- D53：同一真实业务步骤已通过原run_progress/tool_started准确关联委派，但projection额外添加同一子任务卡片，导致6步骤+5子卡重复、白板分母11。修复沿用原office_task_assignments，明确关联时复用原步骤卡/ID、保留真实子角色、独立task.assigned稳定ID；未关联子任务仍单独显示，不按相似标题猜所属。子执行完成但父未核验的步骤不能涂绿；协作模式的父核验read_file不再重发已委派步骤的交接动画。旧单Agent工具归属逻辑保留。

修前证据/home/cheng/.cache/t6-real-model-20261023-b/report.json：执行层检查曾记passed，但最终快照tasks[0]=failed/71%，因此整体产品验收必须判未通过；保留该原报告，追加更严的UI终态/有界无重复卡片断言，不能沿用其passed做交付。原失败a为验收脚本审批locator错误，也保留。Office工程未改。


## D54：流式当前消息仅在内存，Host硬停后部分回复丢失（已复现，修复待增量批准）

原recovery子进程扩展在接受/输入、TextDelta可观察、终态三个边界真实kill。TextDelta边界重启后Run=interrupted且工具不重放，但text由“硬停前已落盘的部分文本”变为空；修前日志/home/cheng/.cache/t6-recovery-sync-20261023.log。进一步审查确认修前标记名“已落盘”是测试意图，现有get_run实际读取live，不证明持久化；测试保留这一用户可见丢失复现，不将内存读当持久屏障。当前store::save_run始终清text，get_run终态只还原完整JSONL assistant消息，尚未完成的Provider消息无检查点。

拟复用原host_runs.data内已有RunView.text/text_truncated，增加有界、合并、非权威的展示检查点；消息/工具/usage仍原JSONL，检查点不进入模型上下文、不恢复执行。不得在同步delta回调阻塞磁盘。具体生命周期、失败处理、兼容/回退和文件范围在0007第12.2节；涉及持久字段语义超出原仅协作控制元数据批准，先人审，尚未修改生产。原失败断言不删除。


## D55：新上游employee.create已领取但旧Host校验拒绝（源码确认，适配待增量批准）

Office fa6ae1f新动作含name/role/sceneActorId，dto::ui_action未知动作返回false，Client::post(ui.next)将本次已消费结果判protocol_error，poller不能发送明确业务拒绝，页面最终超时。当前固定六角色满员会在浏览器禁加，故本轮没有声称从默认页面已复现添加；这是逐协议源码核对确定的新上游兼容缺口，不把它登记成Office缺陷。

最小方案见0007第12.3节：严格识别合法新增动作，复用原唯一领取/持久动作/范围/期限，明确返回不支持动态角色且无副作用；不静默创建有权限Agent。新冻结/DTO/action修改尚未批准，未实施。动态添加的真实业务和权限方案另审，不拿“明确拒绝”说成完整创建成功。

## 2026-10-24 D54/D55已批准实现后的专项结果

本会话用户已分别批准0007第12.1安全补丁、第12.2展示检查点、第12.3上游兼容。D54已复用原Run元数据和单写Store实现，不新增消息库/Schema；真实SQL提交屏障后kill恢复原部分文本，官方history/resume一致为error/interrupted，独立Provider计数为零，原9种接受/终态/父子审批写入边界保留。原HTTP注入真实UPDATE ABORT，Run失败并清理、无done；原私有Store测试覆盖不变不写、错误会话无操作、前缀冲突/4MiB拒绝、账本追上清冗余及删除后无INSERT。证据t6-approved-host-20261024-f.log，非最终准确候选交付。

D55已严格识别合法employee.create，经原唯一领取和作用域检查立即返回固定六角色限制，原HTTPfixture验证无新增Run/Session/审批/文件。源码按真实Zod Unicode码点/ECMAScript trim校验，不按旧假设UTF-16误拒非BMP姓名；原Schema契约13/13。Office仍未改；成员布局真实浏览器及原完整27项待新准确候选重验。Tokio锁只改1.53.3和checksum，cargo update产生的无关Windows依赖边选择变化未纳入，locked构建已验证此最小锁可用。旧失败均保留，暂不登记最终关闭。
