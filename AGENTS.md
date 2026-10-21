# OpenPaw AI 开发入口规则

本文件是 AI 开始任何编码任务前必须阅读的项目规则。项目目标是完成
`../master-plan.md` 的 B 线任务；OpenWork 和 AI Office 的跨仓库接口以
本仓库 `docs/governance/项目设计基线.md` 及 `contracts/` 为准。

## 开始任务前

1. 先阅读 `docs/governance/项目设计基线.md`、`docs/governance/需求验收基线.md`、
   相关 `docs/b-line/` 文档和当前有效的 `docs/changes/CHG-*.md`。
2. 先只读分析需求、调用链、数据流、受保护路径和现有测试；不得先改代码再补说明。
3. 如果没有已批准的 CHG，AI 只能起草 `status=草案（draft）` 的 CHG，不能修改业务代码。
4. CHG 必须逐项写明每个要改文件为什么必要、会影响什么、每个禁止路径为什么不能改、
   API/字段如何变化、实现方式、替代方案、测试和回滚。
5. 发现需要修改 CHG 未列出的文件、符号、公共接口、依赖或数据流时，立即停止并要求
   先更新 CHG；已有明确用户授权覆盖该范围时记录授权后继续，否则由人工补充范围。

## 编码边界

- 只有 CHG“允许修改文件”表中的文件可以出现在本次分支 diff；新增、删除、重命名同样需要登记。
- `crates/openpaw-core/**`、公共 trait、`Cargo.toml`、`Cargo.lock`、Host 协议/事件/权限、
  Office 动作投影和 CI 属于 L3/L4，必须由对应 CODEOWNER 审查。
- 不得把 `axum` 引入默认 feature；`engine-host` 关闭时默认依赖树必须保持无 `axum`。
- 不得绕过 `runtime` 直接操作 session，不得让 Office 请求结果改变 AgentLoop 回合结果，
  不得把凭据、完整 Prompt 或敏感文件内容写入日志或 Office 动作消息。
- 每项任务使用其当前 CHG 登记的范围，历史治理接入记录不作为后续编码授权。

## 中文注释要求

新增或修改的公共 Rust 类型、trait、函数、字段和模块必须使用中文 Rustdoc；规范见本仓
`docs/governance/注释规范与示例.md`。公共函数和方法固定填写“功能、参数、返回、错误、状态、并发与取消、
权限、关联”八个栏目，不适用项写“无”，不能省略；机器标识保留原文，不能只复制函数名。

## 完成前自检

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- lines
cargo run -p xtask -- scrub --check
cargo run -p xtask -- snapshot --check
cargo run -p xtask -- golden
cargo run -p xtask -- telemetry
```

涉及 Host 时再执行显式 `--features engine-host` 的严格 Clippy、Host 测试与 CLI/Host parity，
并检查默认 CLI 依赖树隔离。依赖检查以 CI 的 cargo-deny 为准。逐文件核对 diff 与 CHG，
人工核对中文 Rustdoc；命令、退出码和未执行原因写入当前 CHG。外部治理框架已于
CHG-2026-0024 停用，无须安装外部工具或执行其 approve/pr-check/comments-diff 命令。
