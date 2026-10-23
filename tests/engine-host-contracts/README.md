# Host contract baseline (T0)

This package is test-only, outside the production Rust dependency graph.
It does not implement the Host or claim Office/OpenWork runtime acceptance.

Node 24.19.0:

    npm ci --ignore-scripts --no-audit --no-fund
    npm run prepare:office
    npm run check
    npm test

For a source audit, additionally run:

    node prepare-office.mjs --office-source /absolute/path/to/ai-office-react

The latter reads the pinned commit with git show, preserving uncommitted Office
changes. It verifies all linked source hashes and regenerates only the fixed
test schemas. Default CI validates the stored schemas without private repo
credentials. Never hand-edit upstream-office.

Office fixtures execute exact upstream Zod schemas, including refinements and
normalization; wire null/depth checks are independent. SDK fixtures compile
with satisfies against v1 types, with negative type cases for old mixed-version
examples. SDK method tests use its actual HTTP request builder; SSE uses a real
loopback fixture server because this SDK's SSE ignores config.fetch.

T4/T5/T6 must add real Office/browser acceptance. T7 must run the fixed SDK
against the real Host. Passing T0 fixtures is not evidence of those features.


T5扩展仍使用office-product.mjs：真实冻结Office生产Node、独立Chromium、Host、原OpenAI HTTP Provider及原读/写/bash工具。新增22—27截图和trace/video关联审批前文件不存在、拒绝不落盘、批准后真实报告内容/哈希、六类详情、命令运行/停止防延迟副作用及删除后文件保留。测试只使用新建临时根和确定性凭据，不读取用户.env，不删除用户原天气会话；报告明确sourceDirty和提交号，不把开发运行当最终准确版本证据。

新增Rust显式入口host_office_interactions（真实HTTP协议呈现）与host_office_delete（账本/索引/墓碑/恢复）。D37保留原managed_process_regressions目标，重复记录每次退出码及PID/状态，不能只取最后一轮成功。真实DeepSeek演示须另记，不能由确定性Provider冒充。Linux本机验证与Windows原生未执行分开；不通过WSL或交叉编译声明Windows已验。


T6产品入口继续使用office-product.mjs，调用office-resilience.mjs执行真实一主五子。原单Agent/长流/审批/重启/缓存断言完整执行后，隔离Host才显式启用[agents]并重启原隔离数据目录；原8765与用户home不动。确定性HTTP Provider门控每个真实子任务，使浏览器能够导出task.assigned→office.visit完成→正确人物当前任务的证据，再释放模型继续原read_file执行；产品本身不等待动画ACK。另逐一实际点击六个Pixi人物和六个侧栏成员，核对官方UI HTTP身份、详情和返回目标，以及五份子JSONL/Run事实。录像、trace、截图和导出JSON保存在同一外部证据根。真实DeepSeek另验，不能用确定性模型代替真实模型能力结论。
