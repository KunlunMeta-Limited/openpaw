# Tx 交接模板

复制本节到每个 Tx PR 描述；不能只填“已完成”。T0原交付完成及0001当前修订分别见master第14节和0001首页，validation.md保留原时点证据。

## 任务与审核基线

- 编码前CHG提案版本、逐文件原因/方法/复用/标准、方案批准人/时间/证据及Git对象：
- Tx / 分支 / 本次提交：
- 上一 Tx PR、审核人、获批提交、合入 SHA：
- 本 PR base：merge/ai-ofice-react-overagent（Office 的 PR 使用其本仓基线）
- 目标和触发条件；修改前后行为：
- 方案批准 / 技术验证 / 交付人工验收 / 平台PR / 合入：各自填写真实状态，不用单一状态代替
- 审核人、审核提交号、合入号：未发生时明确“待审核”，不自批。

## 里程碑与提PR前核查

- master第14节M编号/对应Tx任务卡/具体实施锚点：
- 上游Office固定提交、实际文档章节/Schema符号 → master实施章节 → 本次代码/测试 → 证据：
- 本Tx逐项必需产物和里程碑核查表：复制master第14.3节，写被测SHA/摘要、两平台结果、失败整改和技术核查结论；未满足不得创建新PR（含Draft）。
- 当前状态：待实施 / 实施中 / 待里程碑核查 / 阻塞 / 待PR审核 / PR已批准待合入 / 已完成；创建PR/绿CI不能填写完成。
- 本PR是在先行里程碑核查及用户交付验收通过后创建，还是已存在T0 #4继续Draft修订：如实填写。
- 批准/合入后：更新master第14.1里程碑行，引用Tx导航和主CHG履历；填写实际PR、非作者批准提交、合入SHA、日期与交接入口。未发生填待发生。

| 里程碑必核项 | Office/master具体条款 | 产物/代码/测试入口 | 被核查版本与Linux/Windows证据 | 结论/阻塞 |
|---|---|---|---|---|

## 产物与必要性

| 产物/入口 | 复用/延展/修改/新增 | 源码或测试定位 | 新增必要性/数据权威 |
|---|---|---|---|

- Dxx：根因、独立提交、关键注释与回归。
- Rx：设计批准记录、两仓 PR；未批准不修改 Office。
- 公共接口/协议/data version/迁移兼容影响：
- 本阶段不支持的能力和剩余阻塞：

## 用户场景与端到端交付

- 本阶段服务master第1.3节哪项用户能力、第1.4节哪些步骤：
- 用户操作、实际执行、页面结果分别是什么；尚未有UI的核心阶段保护了哪个后续行为：
- 第10节本Tx的实施步骤/产物/完成标准逐项定位；偏差原因及审核决定：

| 场景步骤 | 真实执行/存储证据（Session/Run/call） | 聊天/messages/seq证据 | 页面截图/文件哈希/进程证据 | 状态/未通过原因 |
|---|---|---|---|---|

说明实际支持的按钮和动画。任务/白板必须来自真实运行，人物是功能角色；202/forwarded/delivered分别不是页面应用/动画完成/副作用成功。T4/T5产品演示必须真服务真浏览器；T0的Schema夹具只证明协议基线。R1未获批时写明180秒/刷新取消，不以SDK通过代替Office产品旅程。

## 验证

| OS/版本 | 工具链 | 命令 | exit code | 结果/日志 |
|---|---|---|---|---|

分别写 Linux 和 Windows 原生结果，区分 contract/type/build/unit/runtime/browser。未执行、skip、编译通过不等于运行通过；CI 绿不等于审核。

## 下一阶段输入

- 可直接复用的接口、启动参数、fixtures 和复现步骤：
- 已验证的不变量及资源/取消/审批限制：
- 必需产物逐项定位：
- 未关闭阻塞及负责人：
- 回退方法：

进入下一阶段的条件：本 Tx 必需检查通过，当前提交由非作者明确批准，PR 已合入；跨仓 Tx 的所有必要 PR 均已合入。

## CHG与阶段结束

每Tx一主CHG，补REQ/DES/文档影响/准确允许文件/命令与证据；使用docs/governance/templates模板。
T0—T8全部约定范围验收后记录初期阶段结束，取消Tx串行，后续按CHG与正常PR开发。
Office任意文件修改先Rx人工批准；R1/R2/R4条件提案未批准时如实记未实施，不阻塞T6基础/T7。
本轮治理交接见docs/changes/CHG-2026-0001.md与governance/README.md。

## 共同交付和历史维护

0001治理修订按用户明确批准随T3同一个PR交付，编号不变。T3主CHG实际起草时显式关联0001准确版本，两份各自方案批准、专项验证和交付验收；不预造T3编号。共同文件写双方职责和更严格保护。前序完成记录先补登再开下阶段；历史说明扩充按获批准确路径，不能改旧范围/测试或删履历。

完整验证明确source与stage，复用governance/verify.py；head必须匹配干净检出，worktree不能忽略用户dirty文件。共同PR只在两部分验收都通过后创建。T0—T8结束后取消Tx串行，正常CHG/审核/测试/Rx规则仍适用。

## T3专项交接（已批准实施，尚未交付验收）

主方案[CHG-2026-0004](../changes/CHG-2026-0004.md)第6/7节定义可直接调用的Host API/字段，第9节定义可靠顺序，第10节H01—H12定义完成标准。T4输入应包括实际签名、config/端口/health、稳定Session/Run/call、状态/游标/错误、只读工具白名单、确定性Provider及两平台证据。Office DTO/NDJSON、完整快照seq、菜单/人物动画属于T4/T5，本阶段交接不得称已接通前端。0001共同职责补充用同一0001 r3审核，不新建治理任务；双方各自批准/验收后同一个T3 PR。


### T3实际Rust入口与启动

- `Host::start(HostConfig)`复用原Provider装配；`Host::with_provider(config, Arc<dyn LlmProvider>)`用于确定性测试和可信装配，恢复后才返回。
- `open_session(ClientScope)`返回固定SessionBinding；`get_session(&scope, id)`恢复原core模型上下文，不直接作为前端完整思考展示。
- `submit_run(&scope, session_id, input)`保存接受后返回RunView；`get_run/cancel_run`逐项校验scope，取消返回cancelling时应继续查询到清理完成。
- `snapshot_and_subscribe(&scope)`在同一存储序列返回HostSnapshot与有界broadcast receiver；Lagged后重取，drop仅detach。通知不包含其他scope的Run ID。
- `pending_approvals`与`decide_approval(scope,id,revision,完整ToolCallBinding,approve)`共享持久审批；重复同决定不产生第二次执行。
- `query(scope, QueryRequest)`支持History/ToolResults/Skills/Goals/Todo/File；默认5项，上限50，文件256KiB；history展示隐藏Thinking，原账本不改。
- `wait_idle`等待当前活动清理；`shutdown`先拒新/取消/等待，再等待所有存储句柄关闭。

Linux/Windows共用[配置样例](host-config.example.toml)，先建授权工作根，home不能落在根内。启动：`cargo run --locked -p openpaw-cli --features engine-host,sidecar -- engine-host --config <host.toml> --port 0`。ready输出`openpaw engine host listening on http://127.0.0.1:<port>`，GET /health只返回status/schemaVersion。Ctrl-C正常关闭；Linux另处理TERM。Windows原生Job/cmd仍由原Environment实现，不需要WSL。

无Office的真实验收：`cargo run --locked -p openpaw-engine-host --features engine-host --example host_smoke -- <全新隔离临时目录>`。专用runtime/recovery/CLI测试使用原引擎、JSONL、工具、OS锁和真实子进程；不会复制用户配置进docs。普通文件/目录只读查询不应创建Skill/Goal/Todo数据。数据库一致备份须停服务后复制完整home，运行时备份不能只复制host.db而漏WAL。

T4应直接Rust调用上述接口，按Office固定Schema新增自己的DTO/NDJSON/messages投影；不要再实现Run/审批或从浏览器历史覆盖Host完整账本。实际可开放读取工具为read_file/glob/grep/todo_list/think，写工具和bash只在显式本机配置且人工批准后执行。当前T3未开放Office或SDK业务HTTP，后续接口须继续遵守Rx人工批准边界。M3/两平台验证未完成时不能把本交接当作T4开工许可。

### T3当前技术交接版本

9ed545c准确实现head Linux完整26/26通过，旧CLI帮助测试四feature组合已适配，Host真实shell子孙进程取消/关闭、凭据环境及链接越界均有专项证据。H01—H12逐项定位、失败历史及M3核查见0004验证记录；Windows和交付验收仍缺，当前交接仅供审核，不是T4开工许可。已push开发分支，未创建PR；取得原生证据后对准确交付版本分别验收0004与0001。

## T3交付验收登记（2026-10-21）

用户已明确“审批通过”，分别验收0004 r2及0001 r3共同交付，准确提交`4263529059234ae913da3b40a3efe3eb6e9f7f56`。该head Linux完整26/26，默认966/全feature1001 passed，既有ignored各2单列；原始持久报告见0004验证记录。Windows本轮用户明确暂缓，未验证、不计通过。原段落保留历史时点；当前PR待创建/审核，目标merge/ai-ofice-react-overagent，未合入，不作为T4开工许可。

## T3 PR状态（2026-10-21）

共同[PR #7](https://github.com/KunlunMeta-Limited/openpaw/pull/7)已创建，目标merge/ai-ofice-react-overagent，来源feat/engine-host-t3；创建head `b5a2c2a2a44902139ad27cf6f2cd9a2b8859e27d`。0004/0001交付验收已通过，平台非作者审核与合入尚未发生，T4仍不开始。CI平台账单未启动、本地交付门禁不能表示Windows暂缓两种情况分列于0004最新记录，不自改规则或伪填通过。PR元数据登记版本仍执行完整验证，后续获批并合入后再同步真实merge及M3完成。
