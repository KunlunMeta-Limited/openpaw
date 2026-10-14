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
| CHG-2026-0025 | Host 运行职责下沉 | approved / 本地实施、验证与依赖审计完成 | high | 用户授权；Codex 实施 | 0 | 25 |
| CHG-2026-0026 | [图片与音频附件理解](CHG-2026-0026-图片与音频附件理解.md) | approved / 本地实施、验证及 Docker Host 部署完成；真实图片直传通过 | high | 用户决策；Codex 实施 | 现有合同扩展，详见 CHG | 69 |
| CHG-2026-0026（独立来源） | [AI PC 专属当前上下文及历史工具](CHG-2026-0026-AI-PC专属上下文工具.md) | approved / 源码实现与验证完成，部署待审阅 | high | 用户批准并负责接口审阅；Codex 实施 | 2 | 7 |
| CHG-2026-0027 | 附件辅助信息展示与标题修复 | approved / 实施与验证完成；按用户指示直接提交多模态 feature 分支；独立测试前端运行于 5188；IME 单独保留 | medium | 用户批准；Codex 实施 | 现有消息合同扩展，详见 CHG | 实际 19 / 批准 20（全部 OpenPaw） |
| CHG-2026-0028 | [多模态与 AI PC 工具集成](CHG-2026-0028-多模态与AI-PC工具集成.md) | approved / 本地集成、检查及两轴审查完成；来源 DCO 与人工接口审阅待处理；未推送/部署 | high | 用户批准并负责接口审阅；Codex 实施 | 沿用来源合同 | 9 |

CHG-2026-0025 的文件数量为当前 diff 的 25 个实际文件，不包含原有 `.ua/`。
Q1–Q5 已确认，迁移、本地回归及 cargo-deny 0.20.2 依赖审计完成；审计经本机 7890
代理执行，退出 0。2026-10-13 用户授权新建 `bugfix/shared-engine-store-runtime`，
本轮以独立本地提交保存，未推送，新 CI 尚未执行；证据见 CHG-2026-0025 第 8 节。
