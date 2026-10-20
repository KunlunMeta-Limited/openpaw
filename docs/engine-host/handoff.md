# Tx 交接模板

复制本节到每个 Tx PR 描述；不能只填“已完成”。T0 当前状态见 validation.md。

## 任务与审核基线

- Tx / 分支 / 本次提交：
- 上一 Tx PR、审核人、获批提交、合入 SHA：
- 本 PR base：merge/ai-ofice-react-overagent（Office 的 PR 使用其本仓基线）
- 目标和触发条件；修改前后行为：
- 当前状态：Draft / Ready / Changes requested / Approved / Merged
- 审核人、审核提交号、合入号：未发生时明确“待审核”，不自批。

## 产物与必要性

| 产物/入口 | 复用/延展/修改/新增 | 源码或测试定位 | 新增必要性/数据权威 |
|---|---|---|---|

- Dxx：根因、独立提交、关键注释与回归。
- Rx：设计批准记录、两仓 PR；未批准不修改 Office。
- 公共接口/协议/data version/迁移兼容影响：
- 本阶段不支持的能力和剩余阻塞：

## 验证

| OS/版本 | 工具链 | 命令 | exit code | 结果/日志 |
|---|---|---|---|---|

分别写 Linux 和 Windows 原生结果，区分 contract/type/build/unit/runtime/browser。未执行、skip、编译通过不等于运行通过；CI 绿不等于审核。

## 下一阶段输入

- 可直接复用的接口、启动参数、fixtures 和复现步骤：
- 已验证的不变量及资源/取消/审批限制：
- 必需产物逐项定位：
- 未关闭阻塞及负责人：
- 回退方法：

进入下一阶段的条件：本 Tx 必需检查通过，当前提交由非作者明确批准，PR 已合入；跨仓 Tx 的所有必要 PR 均已合入。
