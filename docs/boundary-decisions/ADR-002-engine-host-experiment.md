# ADR-002: B 线重新启用 engine-host 实验

## 决策

仅通过显式启用 `engine-host` feature 的方式重新启用 `openpaw-engine-host`。它
公开一个有意收敛的 OpenCode v1 子集，并提供独立的 AI Office 动作观察器。host
默认只监听回环地址，采用每 session 独立状态，且不进入 OpenPaw 1.0 兼容性承诺
范围。

## 原因

B 线验收需要真实的 AgentLoop 事件源、确定性的 mock provider 和本地 HTTP 边界。
复用 OpenWork 或耦合 `@opencode-ai/*` 会使实验失去可移植性，并把范围扩展到 A 线。

## 影响

只有启用该 feature 时才会增加 Axum 和传输层依赖。Office 投递采用尽力而为
策略，只读仪表板扫描不会修改 OpenPaw 状态。`always` permission 有意不持久化。
现有 chat、sidecar 和 core crate 的行为保持不变。

Session 仍独占 AgentLoop，但 OpenCode prompt 可以为单个回合选择 provider/model；
所选 provider 作为回合参数进入 AgentLoop，不创建跨 session 的共享运行器。workspace
以绝对路径写入既有 `Metadata.cwd` 字段。DELETE 使用 application/session 两级
生命周期门闩并在 unlink 前关闭 writer。

Office 的六人名册是全局 UI，因此 dispatcher 维护每个角色的 session owner 集合。
这不会把六个角色变成独立 agent，只用于防止并行 session 的结束事件把另一个
session 的活动角色置为 idle。

## 迁移与回滚

不启用 `engine-host` 的用户仍看到原有 CLI 和依赖树。回滚实验时，停用该 feature
并移除 B 线 gateway 进程即可；JSONL session 仍可由现有 store 工具读取。

就绪日志沿用项目的中文日志约定，单行包含稳定的
`http://127.0.0.1:<port>`。这取代 master-plan 早期草案中的英文固定前缀；OpenCode
客户端和 Kunlun 均只解析 URL。该变化不影响 HTTP 协议。

## 验证

参见 `docs/b-line/testing.md` 和生成的 Kunlun 报告。本 ADR 是工程决策记录，不是
外部 GB 或监管认证。
