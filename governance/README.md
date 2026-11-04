# OpenPaw 工程治理入口

本目录是开发工具，不是Host运行服务。按用户提供的Kunlun/framework复用十二节CHG数据校验；保留原始快照和SHA-256，项目适配在check.py，不改参考工程。参考目录并非Git仓库，故使用内容哈希而非编造提交号。来源/许可边界见framework.lock.toml；不把无声明来源重新标为Apache，不打入运行时发布包。

## 实际命令

Python3.11+标准库，无新Python依赖。Rust stable/MSRV1.88、Node24.19.0、cargo-deny0.20.2及锁定npm依赖按测试规范准备。

~~~text
python3 governance/check.py validate --stage proposal
python3 governance/check.py validate --stage implementation
python3 governance/check.py index
python3 governance/check.py trace
python3 governance/check.py index --check
python3 governance/check.py trace --check
python3 -m unittest discover -s governance/tests -v
python3 governance/check.py pr-check --stage implementation --base <集成提交> --worktree
python3 governance/check.py pr-check --stage delivery --base <集成提交> --head <实际交付head>
python3 governance/check.py validate --stage delivery --base <集成提交> --head <实际交付head>
python3 governance/check.py validate --stage delivery --base <集成提交> --worktree
python3 governance/verify.py --suite all --source worktree --stage implementation --base <集成提交> --output <仓库外全新目录>
python3 governance/verify.py --suite all --source head --stage implementation --base <集成提交> --head <已检出的提交> --output <仓库外全新目录>
~~~

`validate`默认proposal，只验十二节结构、准确文件、可审说明、追踪、固定框架哈希及注册命令；不要求未来批准/结果。implementation要求当前新格式CHG的有效方案批准和一致版本，`pr-check`同时校验真实diff/必改/条件/保护。delivery再要求人工交付验收、完整提交号、两平台原生passed证据；交付版本后只允许审批元数据/追加履历和生成索引维护，代码或方案变化需重验。validate索引/追踪均从同一归一化结果读取，--check拒绝漂移。没有approve或merge入口。

`validate --stage delivery`不带比较上下文时仍是严格的全历史交付审计，旧CHG当年的Windows暂缓也会如实失败。检查本轮交付应同时指定`--base`及`--head`或`--worktree`，直接复用原`pr-check`：全部CHG结构、标识和固定来源仍检查，交付验收则按真实diff检查当前主CHG及显式关联修订。历史文件若在本轮修改，仍须满足原关联/受限维护规则；不能把“历史”当作跳过授权的理由。比较参数只用于delivery，缺参、互斥参数或其他阶段传范围都会拒绝。原全量runner在delivery阶段把同一准确base/head或worktree传到这两个入口，实际argv进入原报告；27项命令、平台矩阵和当前交付的准确SHA/双平台要求不变，不改写旧平台事实。

新CHG使用`openpaw/chg-review-v2`及稳定基础列，扩充“具体修改方向与复用方式”“完成标准与证据”。当前状态与原交付状态分离，方案、交付、PR、合入各有独立字段。非空姓名/URL不是身份认证；未知格式不会按legacy自动放行。legacy仅profile已登记的准确路径和完整历史提交，用于追溯，不授权新的代码修改。

方案批准对象优先填写`完整提案提交:docs/changes/本CHG.md`（该提交必须已包含完整提案），也支持本地完整blob SHA；对象中的方案与当前第2—11节、介绍、提案关键字段须相同。实质修改更新提案版本重新审核；批准登记和PR/结果元数据不进入方案摘要。Git对象在CI必须可获取，本地孤立blob不等于已提交/可共享的提案证据。固化对象记录实际形成时间，不倒填成过去已提交；旧r1原文仍在0001历史中定位。

共同PR由唯一主CHG明确列`关联修订=CHG编号@版本`，每份修订独立批准/验收；重叠文件用“共同修改职责”逐行解释双方工作。主CHG还可精确列“历史元数据维护”或获批“历史说明维护”路径（多项用顿号）；受限迁移不会把历史closed记录当本轮活动方案。未关联多CHG、旧范围/测试/保护变更、删除/重命名历史CHG均拒绝。Office不在本profile范围内，仍须Rx另仓预审批。

commands.json保留原26项，是注册命令单一来源。verify仅按明确source/stage展开治理上下文，实际argv逐条写report.json。worktree包括所有已跟踪、未提交、未跟踪内容；comments和pr-check统一使用--worktree，不能暗中拿HEAD替代。head要求干净且HEAD等于指定提交，所有命令在该检出运行；不自动stash、删用户文件或排除越界改动。需要验证其他提交时先使用独立干净检出，安装工具/依赖后再运行，未跟踪构建产物遵循既有ignore。

输出目录必须在仓库外且不能覆盖；日志保存真实stdout/stderr、参数、退出码、耗时、工具版本、source、stage及源摘要。超时124、缺命令127，执行失败仍记录其余命令；全部执行且源未变化才可能通过。UTF-8进度和Python子命令兼容Windows重定向，保留原始日志字节；不采集完整环境或模型密钥。运行途中不要改文档/代码，登记结果后形成新版本再执行最终交付验证。不能把Linux或交叉编译当Windows原生通过。

pr-check的固定head模式读取Git对象，不受脏工作区影响；完整runner的head模式更严格，避免其余编译/单测仍读脏工作区。base/head取更严格保护级别；重命名登记两端，文件操作必须一致，必改缺失与新增越界都失败，条件未用列出。ID需在权威需求/设计/审计/CHG有来源。

comments复用syn/quote/doc_lines，对变化的声明节点检查中文栏目；包含pub(crate)、trait定义方法、公有字段、关联项、嵌套模块。模块指纹不含私有正文，未变化的历史声明不阻断私有修改；新文件要求模块职责。--all --report-only列历史债。宏展开、重导出、trait实现继承说明、cfg运行语义需人工核查；机器只校验栏目和内容非空，无法证明文字正确或关联语义。源码解析失败直接失败，不以跳过当通过。

## 人工与机器边界

方案批准、交付验收和平台PR审核是三个独立事实。机器核对结构与Git方案一致性，不从非空字段认证用户、不自动证明批准时序或文案充分。closed必须有实际合入记录，原T0完成不意味着本次修订已完成。平台required checks/CODEOWNERS仍需真实账号和管理员设置，不宣称本地文件已启用平台保护。

T0—T8一阶段一主CHG、一PR，上一PR审核合入才继续；初期全部约定范围验收后正常CHG开发，取消Tx串行。届时人工登记profile.initial_tx_phase=false及阶段验收证据，normal_target_branch切换普通主分支规则；配置自身L4审核，工具不会自动结束阶段。Office任何文件改动先Rx并经人工明确批准。0001本次只完善工程基线，不实现Host业务、不修改Office、不自动合入。

CI依赖审计：Linux复用固定cargo-deny Action；其容器只支持Linux，Windows通过crates.io原生安装固定cargo-deny0.20.2并执行相同cargo deny --workspace check。双平台都实际扫描，不以条件跳过代替通过。

CI沿用原13作业与Linux/Windows矩阵、contents:read权限；push使用implementation取技术证据，PR使用delivery检查交付。CI checkout准确head并以--source head运行，不创建平行工作流。
