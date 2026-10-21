# ADR-001: engine-host 归档为社区实验路线（E8 验证关卡不通过）

- **Status**: accepted
- **Date**: 2026-08-30
- **Deciders**: OpenPaw boundary review（会话 18）

## Context

方案 §4.4 规定 openpaw-engine-host（≤2,000，[待验证]）以 Phase 2 第 1 周验证关卡
（E8）为前置：**OpenWork 引擎替换面存在公开接口规范，或上游 RFC 接受**。风险 R4
同口径：OpenCode 引擎接口未冻结。

M5 收口核查（2026-08-30，公开渠道证据）：

1. OpenCode（sst/opencode → anomalyco/opencode）的公开扩展面为
   `@opencode-ai/sdk`（OpenAPI 生成）、`@opencode-ai/plugin`（hook 框架）与
   ACP（编辑器集成协议）——三者均为"驱动 OpenCode agent"的客户端接口，
   **不存在"替换 OpenCode 引擎为第三方引擎"的宿主侧公开规范**；
2. OpenCode 仍处于快速演进期：V2 domain layer（`packages/core` Catalog/PluginV2）
   重构进行中，接口未冻结（R4 的 v0.1.9 快照判断仍成立）；
3. OpenWork 侧未检索到引擎替换面的公开接口规范或 RFC。

## Decision

**E8 验证关卡不通过。** 按方案 §15.4 预设路径执行：

- `openpaw-engine-host` 归档为**社区实验路线**：crate 骨架保留（当前 6 行占位），
  不进入 1.0 的 12 crate 正式发布清单，占位包发布撤回；
- 主线（M5/M6）不受阻塞——E6 sidecar 通道独立存在（ADR 不影响 sidecar）；
- 重启条件（满足其一即重开关卡）：OpenWork 发布引擎替换面公开接口规范；
  OpenCode SDK v2 / ACP 冻结并声明第三方引擎替换支持。

## Consequences

- 更容易：M6 发布工程无需等待外部接口冻结；"12 crate 正式清单"口径成立
  （sidecar 为 cli feature 不占 crate 名额）；
- 更难：OpenWork/Cursor 深度引擎替换场景延后（M5 的三客户端接线教程以
  **MCP sidecar** 通道为准——sidecar 是与宿主解耦的标准协议通道，不受本裁决影响）；
- 重访时点：OpenCode SDK v2 冻结公告或 OpenWork RFC 公开时（M6 排期内复估一次）。

## Rejection notes

- **强行实现 protocol/transport/bridge**：接口未冻结意味着每个 OpenCode 版本都可能
  破坏 bridge——维护成本不可控，违反"单点押注前置验证"的方案纪律；
- **将 engine-host 推迟到 1.0 后无限搁置**：丢失重启条件记录——ADR 归档保留重启
  路径优于静默搁置。
