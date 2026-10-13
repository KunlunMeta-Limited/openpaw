# OpenPaw 本地变更记录

CHG-2026-0024 经用户授权停用外部治理框架。CI workflow、转发入口和外部 profile 已移除，
日常开发不需要 `AI_GOVERNANCE_FRAMEWORK` 或特定本机目录。

继续使用本仓 `docs/changes/` 记录变更范围、用户授权、接口影响、测试和回滚；
`docs/governance/` 保存设计基线、验收要求及中文注释规范，供人工审阅。历史 CHG 的
外部工具命令和失败结果是历史证据，不是当前检查入口。

机器检查由 `.github/workflows/ci.yml` 和仓库自带 `cargo` / `xtask` 提供；
当前本地自检入口见 `AGENTS.md`。PR 描述记录实际退出码，CODEOWNER 继续承担人工审查。
移除外部工具不代表构建、测试、lint 或本地工程预算可以跳过。
