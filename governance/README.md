# OpenPaw 工程治理入口

本目录是开发工具，不是Host运行服务。按用户提供的Kunlun/framework复用十二节CHG数据校验；保留原始快照和SHA-256，项目适配在check.py，不改参考工程。参考目录并非Git仓库，故使用内容哈希而非编造提交号。来源/许可边界见framework.lock.toml；不把无声明来源重新标为Apache，不打入运行时发布包。

## 实际命令

~~~text
python governance/check.py validate
python governance/check.py index
python governance/check.py trace
python governance/check.py index --check
python governance/check.py trace --check
python -m unittest discover -s governance/tests -v
python governance/check.py pr-check --base <集成提交> --head HEAD
cargo run --locked -p xtask -- comments --base <集成提交> --head HEAD
python governance/verify.py --suite all --base <集成提交> --head HEAD --output <仓库外新目录>
~~~

Python3.11+标准库，无新Python运行依赖。Rust stable/MSRV1.88、Node24.19.0、cargo-deny0.20.2和锁定npm依赖先按测试规范准备。commands.json是命令单一注册源；套件rust/quality/contracts/governance/all。脚本使用argv且不经shell，保存每步退出码/时长/stdout/stderr/版本/工作树摘要；超时记124，缺命令记127。失败仍保存报告并执行其余命令，全部完成且源摘要不变才通过；报告目录已存在则拒绝覆盖。源码含密钥时不得把日志/报告公开，工具不收集完整环境。验证器显式使用UTF-8输出，并向Python子命令传递UTF-8模式，避免Windows重定向管道的CP1252编码失败；其他工具的原始日志仍原字节保存。验证工具不是运行服务的取消实现。

pr-check直接读取固定head Git对象，校验该版本CHG/文件操作/保护声明与真实base→head diff；工作区脏文件不能改变已提交检查结果。--worktree显式包括未跟踪文件和所有本地改动，越界即失败。恰好一主CHG，历史CHG不能删除/重命名；本仓profile不允许Office混入。重命名登记两端；必改缺失与新增越界失败；条件项未用输出清单。比较base/head更严格保护等级与风险。允许表只接受准确路径，不接受全仓通配。

validate检查表格、唯一ID、来源哈希、命令与文档/需求/设计/代码关联。索引与追踪矩阵自动生成，--check拒绝漂移。登记ID仅在权威需求/设计/审计/CHG中有来源才有效，scrub只豁免完整登记标识的匹配片段；同一行后续非法标识仍失败。

comments复用syn/quote/doc_lines，对变化的声明节点检查中文栏目；包含pub(crate)、trait定义方法、公有字段、关联项、嵌套模块。模块指纹不含私有正文，未变化的历史声明不阻断私有修改；新文件要求模块职责。--all --report-only列历史债。宏展开、重导出、trait实现继承说明、cfg运行语义需人工核查；机器只校验栏目和内容非空，无法证明文字正确或关联语义。源码解析失败直接失败，不以跳过当通过。

## 人工与机器边界

没有approve命令。review/in-progress/verified可以表达已获实施授权的开发和验证进度，不等于PR批准；approved/closed还要求批准字段及平台PR引用，closed要合入SHA。引用存在不等于身份/审核已认证，真实非作者针对当前提交的审核仍以GitHub与用户指令为准，作者不能自批。原型的宽松CLI和自动批准入口均不暴露给项目流程。平台required checks/CODEOWNERS需真实账号和仓库管理员设置，尚未配置不能宣称已生效。

T0—T8一阶段一主CHG、一PR，上一PR审核合入才继续；初期全部约定范围验收后正常CHG开发，取消Tx串行。届时人工登记profile.initial_tx_phase=false及阶段验收证据，normal_target_branch切换普通主分支规则；配置自身L4审核，工具不会自动结束阶段。Office任何文件改动先Rx并经人工明确批准。当前T0只实现工程基线，不实现Host业务、不修改Office、不自动合入。
