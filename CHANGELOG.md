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


## T1（开发中，待审核合入）

- 共享Provider装配与显式模型覆盖，修复占位模型请求；CLI增加可选`--model`、`--base-url`。
- 保留原AgentLoop，新增运行上下文/明确终态、可等待账本观察边界、有效取消和独立超时；工具取消通知后等待实际结束。
- 保存完整工具链/usage/终态，压缩上下文快照及结构化恢复，损坏尾完整备份后修复再追加。
- 修复网络Provider断流假成功、OpenAI最终文本丢失、SSE中文跨块损坏；旧run_turn取消现在返回明确错误。
- 新JSONL事件旧reader可Unknown保留，回退前须备份，不用旧引擎直接恢复含快照的新账本。T2进程/授权和Office接入尚未实现。
