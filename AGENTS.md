# OpenPaw coding agent 开发规则

本仓库复用 OpenPaw 引擎、工具和存储，在 openpaw-engine-host 中适配 Office 官方 HTTP/NDJSON 与固定版 OpenCode SDK。Linux 主开发，Windows 10/11 x64 原生支持；不适配 macOS。背景、交互和 T0—T8 以 docs/engine-host/master.md 为准，不能把计划写成已实现。

## 开始前

1. 阅读本文件、docs/requirements/产品需求说明.md、docs/design/软件设计说明.md、相关规格、当前主 CHG 与 docs/governance/依赖与资料基线.md。
2. 先只读审核调用链、数据写入、权限、平台与现有测试；在 CHG 写明复用、延展、修改、新增及必要性。禁止复制 AgentLoop、Session、工具注册表、审批或已有解析器。
3. 每项功能、缺陷、文档和工程变更均进入 docs/changes/ 的有效 CHG。一 Tx 一主 CHG，缺陷以独立 D 条目/回归跟踪，相关修复可按完整逻辑整合提交，提交信息优先中文；后续普通变更一 CHG 一范围。
4. 需求和设计批准、实施授权、当前提交 PR 审核是不同事实。用户明确实施指令可作为范围内开发授权，须引用真实证据；AI 不执行 approve、不自填人工批准、不自行合入。
5. 已授权范围内可迭代实现；新增公共协议、权限、依赖、数据格式或越出允许文件时先更新方案并取得所需授权。不得顺手重构。

## 不可越过的边界

- Office 源码、文档、测试、配置、锁文件、生成文件和格式化均不得未经人工批准修改。缺陷、必要扩展和新增功能先提交 Rx 的原因、方案、影响、准确范围、验证和回退，用户明确批准该版本后才能实施；批准总计划不等于批准 Rx。
- Office API Key 和模型密钥不进浏览器 VITE_*、日志、事件或工具环境；teamId/runId 不是授权凭证。
- 协议 DTO/路由/映射留 Host，通用缺陷修所属 crate；默认 feature 不引入 axum，CLI/离线/sidecar 保持兼容。
- 审批绑定最终输入；等待不持资源锁，取消后等待实际清理；不得以降级隔离、放宽权限或削弱测试获得通过。
- 不覆盖、还原或提交用户已有工作区改动，除非用户明确授权对应路径。
- 不设文件/crate 行数上限，不复制或搬运代码规避门禁。
- 历史 ADR、报告和日志保留当时语境；纠正当前规则使用新基线，不删除历史来掩盖差异。

## 文档、注释和测试

新增功能和 bug 必须同步 CHG 与对应需求/设计/使用说明；文档影响表写明章节或不涉及理由。规范分别见 docs/governance/开发与变更规范.md、注释规范与示例.md、测试与验证规范.md。

新增或行为变化的公共/跨模块 Rust 声明使用中文 Rustdoc。函数/方法含“功能、参数、返回、错误、状态、并发与取消、权限、关联”八栏目，不适用写“无”；类型/模块按专用栏目。保留 SAFETY、Stability 等机器标识。历史未触及缺口报告，不批量翻译；复杂逻辑注释解释原因与不变量。

功能/回归测试优先扩展既有测试；专用目录使用显式 Cargo 入口，私有不变量保持 inline，不为测试扩大 public。每个可交付版本，包括文档变更，都完成规定全量验证；定向测试不能替代交付验证，失败修复后重新完整运行。未运行/skip/历史绿灯不能写作通过。

## 命令与交接

~~~text
python governance/check.py validate
python governance/check.py index --check
python governance/check.py trace --check
python -m unittest discover -s governance/tests -v
cargo run --locked -p xtask -- comments --base <集成基线> --head HEAD
python governance/check.py pr-check --base <集成基线> --head HEAD
python governance/verify.py --suite all --output <仓库外证据目录>
~~~

当前实现/精确行为见 governance/README.md。工作区尚未提交时可用 comments/pr-check 的 --worktree；该模式包括用户未提交改动，越界必须报告，不能偷偷忽略。提交前只暂存本 CHG 授权文件。

初期按 docs/engine-host/master.md 第14节先核查本Tx里程碑及Office文档→方案→代码/测试证据，必需项和双平台验证全部满足才创建PR（含Draft）。先push Tx开发分支取得原生CI，不为触发CI先开PR；已有T0 #4保留Draft迭代，不能以旧PR绕过核查。主CHG记录核查版本/结论，技术核查不是人工批准。PR批准并合入后同步master里程碑、主CHG及交接的实际PR/审核/合入记录，才标已完成。

初期每 Tx 独立 PR，目标 merge/ai-ofice-react-overagent；上一 Tx 当前提交获批并合入才开始下一 Tx，等待只处理当前 PR。T0—T8 全部约定范围验收后登记阶段结束，转 CHG 持续开发，取消 Tx 串行要求，正常 PR/文档/测试/Rx 规则保留。CI 通过不代替非作者审核。使用 DCO Signed-off-by，禁止自动合入和强推。
