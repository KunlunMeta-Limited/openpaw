# Engine Host 文档引用页

本路径原有的 Engine Host 总体架构、协议、计划和验收正文已归并到 AI Office 统一文档，避免 OpenPaw 与 AI Office 各自维护一份跨仓协议。

权威入口：

- [AI Office / OpenPaw 需求基线](../../../ai-office-react-main/docs/01-需求基线.md)
- [AI Office / OpenPaw 设计与实现](../../../ai-office-react-main/docs/02-设计与实现.md)
- [AI Office / OpenPaw 测试大纲](../../../ai-office-react-main/docs/03-测试大纲.md)
- [AI Office / OpenPaw 验收记录](../../../ai-office-react-main/docs/04-验收记录.md)
- [两仓逐行变更事实清单](../../../ai-office-react-main/docs/变更文件详细说明.md)
- [跨仓图文件](../../../ai-office-react-main/docs/diagrams/)

`D:/workSpace/KunlunMeta_Git/master-plan.md` 仍是 T1-T13、M1-M6 和原始范围的唯一计划权威源。OpenPaw 的 CHG 文件继续保留在 `docs/changes/**`，不因文档归并而删除或合并。

当前不能把依赖隔离通过写成 Host 发布通过：默认 OpenPaw 仍可独立构建，`engine-host` 为可选 feature；但当前候选的回合终态、行数预算、固定协议夹具、启动恢复、强杀矩阵、OpenWork 联调和可靠动画仍须按统一验收记录闭环。
