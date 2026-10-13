# CHG 索引

本文件人工维护，用于定位各 CHG；范围、授权与验证事实以对应 CHG 为准。

2026-10-12：通用治理框架缺失，索引命令退出 2；人工同步 CHG-2026-0023 的批准范围及实施状态。
该变更已完成编码和功能验证，当时正式行数/治理门禁未通过。
2026-10-13：用户授权 CHG-2026-0024，停用外部框架并以本地检查修复 PR 门禁。
接口数量按已编号的 Host API 变化统计，CLI 与共享 Rust 接口范围见该 CHG。

| 变更编号 | 标题 | 状态 | 风险 | 负责人 | 接口数量 | 文件数量 |
| --- | --- | --- | --- | --- | ---: | ---: |
| CHG-2026-0001 | OpenPaw B 线治理接入和历史实现反向审查 | approved | high | OpenPaw 项目负责人（待确认） | 0 | 15 |
| CHG-2026-0003 | B 线新增公共 Rust 接口中文注释补齐 | approved | high | OpenPaw 项目负责人（待确认） | 0 | 11 |
| CHG-2026-0005 | 对话任务技能与动画闭环兼容复核 | approved | medium | OpenPaw 项目负责人（用户授权） | 0 | 1 |
| CHG-2026-0006 | 实时活动状态接口 | review | medium | OpenPaw Host 接口负责人（待确认） | 1 | 7 |
| CHG-2026-0007 | 审批生命周期闭合 | review | high | OpenPaw 安全负责人（待确认） | 1 | 5 |
| CHG-2026-0008 | 统一运行配置与便携启动 | review | medium | OpenPaw CLI 负责人（待确认） | 1 | 25 |
| CHG-2026-0009 | Todo 动作映射收敛 | review | medium | OpenPaw 跨项目接口负责人（待确认） | 0 | 7 |
| CHG-2026-0010 | 回合终态与办公室空闲闭环 | review | high | OpenPaw Host 运行时负责人（待确认） | 1 | 5 |
| CHG-2026-0011 | Provider 流超时闭环 | review | high | OpenPaw 引擎负责人（待确认） | 0 | 5 |
| CHG-2026-0012 | 回合失败终态与会话并发队列 | review | high | OpenPaw Host 运行时负责人（待确认） | 4 | 13 |
| CHG-2026-0013 | 回合线性化与工具安全闭环 | approved | high | OpenPaw Host/Engine 项目负责人（待确认） | 14 | 28 |
| CHG-2026-0014 | 会话因果动画可靠投递 | approved | high | OpenPaw Host/Office 项目负责人（待确认） | 7 | 32 |
| CHG-2026-0015 | 会话懒加载隔离与受控关闭 | approved | high | OpenPaw Host 运行时负责人（待确认） | 8 | 18 |
| CHG-2026-0016 | 运行配置回退与 sidecar 顺序收敛 | approved | medium | OpenPaw CLI 负责人（待确认） | 1 | 24 |
| CHG-2026-0019 | 会话分支、回退与归档 | approved | high | OpenPaw Host 运行时负责人（待确认） | 9 | 22 |
| CHG-2026-0020 | 工具结果历史投影一致性 | approved | medium | OpenPaw Host 接口负责人（待确认） | 1 | 8 |
| CHG-2026-0021 | 按回合回退会话 | approved | medium | OpenPaw Host 接口与运行时负责人（待确认） | 1 | 9 |
| CHG-2026-0022 | 回退替换与 API 错误透传 | approved | medium | OpenPaw Host 接口与运行时负责人（待确认） | 4 | 20 |
| CHG-2026-0023 | CLI 与 Host 运行能力统一 | approved | high | CLI、LLM、Store、Engine 与 Host 负责人（待确认） | 1 | 41 |
| CHG-2026-0024 | PR 门禁修复与本地检查 | approved / 本地验证完成 | medium | 用户授权；Codex 实施 | 0 | 31 |
