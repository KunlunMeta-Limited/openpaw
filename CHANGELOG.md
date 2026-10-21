# Changelog

## Unreleased — pre-1.0 T0 integration baseline

- **Breaking (Rust types):** replace archived `serde_yml` with the maintained
  `serde_norway` package, retaining `serde_yml` as the workspace dependency alias.
  Public YAML `Mapping`, `Value`, and `Error` types exposed by Store, Memory,
  Skill, Goal, and Reflect now have the new package's type identity. Downstream
  Rust consumers passing these types must use the same package alias or migrate
  to the new types. See [migration and rollback](docs/engine-host/dependency-baseline.md).
  Existing frontmatter and session data remain readable; no bulk migration.
- Upgrade vulnerable LRU/TLS dependencies. Propose exact existing-package
  license exceptions with complete distribution notices, subject to PR review.
- Freeze Office/OpenCode test contracts and the T0–T8 handoff process; extend
  native platform CI and retire numeric line limits. The opt-in `engine-host`
  dependency is wired, but its runtime and protocol endpoints are not implemented.

- Fix D16 advisory-lock release in the existing sandbox module: explicitly
  unlock before closing the guard, so a Unix descriptor inherited during a
  concurrent spawn does not prolong ownership. Keep the public API and add
  deterministic descriptor and native cross-process ownership regressions.


## T1（已通过用户审核并合入，PR #5）

- 共享Provider装配与显式模型覆盖，修复占位模型请求；CLI增加可选`--model`、`--base-url`。
- 保留原AgentLoop，新增运行上下文/明确终态、可等待账本观察边界、有效取消和独立超时；工具取消通知后等待实际结束。
- 保存完整工具链/usage/终态，压缩上下文快照及结构化恢复，损坏尾完整备份后修复再追加。
- 修复网络Provider断流假成功、OpenAI最终文本丢失、SSE中文跨块损坏；旧run_turn取消现在返回明确错误。
- 新JSONL事件旧reader可Unknown保留，回退前须备份，不用旧引擎直接恢复含快照的新账本。该阶段未实现T2进程/授权或Office接入。


## T2（本轮范围已验收并合入PR #6；Windows原生待补验）

- D05/D06：原引擎统一Hook后最终输入校验/审批，落实工具并发声明；完成即发、真实任务耗时、panic保留ID；记录实际开始失败则整Run失败并等待清理。
- D07/D10：注册表注入Environment，批准仅适用Run/call/工具/完整输入；危险命令仍拒绝。Windows固定系统cmd、未知语法保守审批，不依赖Git Bash；UNC/device命令工作目录明确拒绝，避免cmd静默切目录。
- D08/D09：普通执行复用Linux进程组/Windows Job，取消后等待回收；两条输出管道超限继续排空，保留退出原因。请求隔离不能落实即拒绝；当前Windows受限组合不宣称网络隔离，也不降本地。
- D11：Todo原JSONL增加真正只读结构API、原OS锁完整事务、同目录原子替换；损坏/权限错误不伪装空列表，失败保留旧文件。
- D34：原SandboxedCommand字面参数不能注入另一条命令；不支持的cmd参数及env覆盖执行前明确拒绝，修正此前静默忽略env的行为。
- Rust兼容：ToolExecutor/Environment新增带默认实现的方法，旧实现可继续编译，但取消及时性由实现负责。ApprovalRequest新增serde可选binding，旧构造函数/JSON兼容，外部直接struct literal需补binding字段。新增ToolCallContext是可信进程内接口，不是远程认证或防重放服务。Todo格式不迁移，Session格式不变化。
- 工具继承必要系统环境，模型/Office密钥不自动传入；执行能力复用锁定的tokio-util和tempfile，不增加包族；D35另对现有TLS传递依赖作必要补丁升级。Office任何文件均未修改；协议适配仍在T3以后。
- D35：修复全量扫描发现的emSign信任公告，rustls-webpki升级0.103.16、webpki-roots升级1.0.10；保持证书/公告校验，核对实际许可证并同步精确版本通知。


## T3（已批准实施，交付及PR审核尚未完成）

- 可选`engine-host`现在包含共享Rust Host和CLI入口：`openpaw engine-host --config <host.toml> --port 0`，只监听loopback健康接口。
- 每Run复用独立AgentLoop，JSONL保存消息事实；新增宿主绑定、生命周期、审批及恢复元数据，不建立第二套消息账本。
- 持久一次审批、真实工具开始、共享/独占根锁和可取消等待；关闭等待原进程/写入及数据库句柄实际结束。
- 六类授权只读查询复用原Store和有界文本读取，Skill/Goal新增checked入口，旧容错列表不变。
- Office聊天/快照/菜单/动画留T4/T5，OpenCode留T7；本阶段健康接口不冒充前端接通。双平台和M3结果以CHG-2026-0004验证记录为准。
