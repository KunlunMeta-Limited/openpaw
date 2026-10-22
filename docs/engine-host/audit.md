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
