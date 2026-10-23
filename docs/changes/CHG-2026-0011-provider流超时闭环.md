# CHG-2026-0011：Provider 流超时闭环

## 先说结论

本变更让 AgentLoop 既有的 `stream_timeout` 同时约束 Provider 建流和每一帧等待，消除无界 busy。无可见输出的瞬时超时可按既有重试策略重试；一旦已经发布可见事件，后续超时不重放整个请求；取消始终优先。它不修改 Provider trait、Host HTTP/SSE 字段或默认 120 秒配置。历史测试已有记录，但正式 L4 审核、PR 和当前候选版本复测仍缺失。

## 1. 变更审批

| 审批项目 | 内容 |
| --- | --- |
| 变更编号 | `CHG-2026-0011` |
| 变更标题 | Provider 流超时闭环 |
| 当前状态 | 评审中（review） |
| 变更类型 | 可靠性修复（fix） |
| 风险等级 | 高（high） |
| 负责人 | OpenPaw 引擎负责人（待确认） |
| 必须审核 | 核心架构负责人、引擎负责人、Host 运行时负责人、测试负责人 |
| 设计基线 | `DES-OPENPAW-B-2026-09-15` |
| AI 参与 | 是 |
| AI 工具 | Codex |
| 目标分支 | `main`（历史工作树；正式合并前需迁移到受控分支） |
| 批准人 | 待审批 |
| 批准时间 | 待审批 |

## 2. 变更原因

AgentLoop 虽有默认 120 秒 `stream_timeout`，旧实现没有把它应用到 `provider.stream()` 建流和 `stream.next()` 逐帧等待。Provider 不返回连接结果，或 SSE 连接后长期没有下一帧时，回合会永久 busy，Host 无法释放 active，客户端只能持续显示正在思考。该无界等待位于 Provider/AgentLoop 路径，不能由前端轮询延时解决。

## 3. 涉及仓库

| 仓库编号 | PR 或说明 |
| --- | --- |
| openpaw-main | 当前为历史脏工作树，尚未创建独立 PR；L4 文件尚无可核验人工审查 |

## 4. 允许修改文件

| 仓库 | 操作 | 文件 | 模块或符号 | 文件负责人 | 为什么必须修改 | 对其他部分的影响 |
| --- | --- | --- | --- | --- | --- | --- |
| openpaw-main | 修改 | `crates/openpaw-engine/src/agent_loop.rs` | `stream_with_retry`、取消竞争 | 引擎负责人 | 对建流和逐帧等待应用既有超时，并约束重试与取消顺序 | 改变 Provider 故障收敛时机，不改变成功流事件 |
| openpaw-main | 修改 | `crates/openpaw-engine/tests/engine_e1.rs` | Provider 停滞与取消测试 | OpenPaw 测试负责人 | 覆盖建流超时、逐帧超时、重试、防重复和取消优先 | 只增加可靠性测试 |
| openpaw-main | 修改 | `docs/b-line/testing.md` | Provider 流超时矩阵 | OpenPaw 测试负责人 | 记录超时、重试和取消验收 | 不改变运行时 |
| openpaw-main | 修改 | `docs/changes/CHG-2026-0011-provider流超时闭环.md` | 受控变更记录 | OpenPaw 项目负责人 | 迁移到统一 12 章节格式并保留未完成项 | 不改变运行时 |
| openpaw-main | 修改 | `docs/changes/INDEX.md` | CHG 索引 | OpenPaw 项目负责人 | 由治理工具登记本变更 | 不改变运行时 |

## 5. 明确禁止范围

| 仓库 | 禁止路径 | 为什么禁止 |
| --- | --- | --- |
| openpaw-main | `crates/openpaw-core/src/error.rs` | 复用既有 `Error::Timeout`，不扩展公共错误类型 |
| openpaw-main | `crates/openpaw-core/src/provider.rs` | 不改变 Provider trait 或能力契约 |
| openpaw-main | `crates/openpaw-engine-host/**` | Host 只消费引擎结果，本变更不修改 HTTP、SSE、持久化或 Office 协议 |
| openpaw-main | `Cargo.toml` | 超时使用现有 Tokio 能力，不新增依赖 |
| openpaw-main | `Cargo.lock` | 本变更不改变依赖解析 |

## 6. API 变化

无公共 HTTP、SSE、CLI 或 Provider trait 变化。内部超时错误继续使用既有 `Error::Timeout`，由 Host 的既有错误分类映射为脱敏失败终态。

| 接口编号 | 类型 | 原始标识符 | 中文名称 | 兼容性 | 功能和边界 |
| --- | --- | --- | --- | --- | --- |
| 无 | 不涉及 | 无 | 不改变公共接口 | 不适用 | 只使 AgentLoop 既有超时配置在建流和逐帧等待处生效 |

## 7. 字段变化

| 字段编号 | 所属接口 | 原始字段名 | 中文名称 | 类型 | 必填 | 默认值 | 约束 | 数据来源 | 数据去向 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 无 | 不涉及 | 无 | 不改变协议字段 | 不适用 | 否 | 无 | 继续使用现有错误和事件字段 | AgentLoop 内部 | Host 既有错误处理 |

## 8. 保护路径

| 仓库 | 文件或路径 | 等级 | 负责人 | 为什么需要保护 |
| --- | --- | --- | --- | --- |
| openpaw-main | `crates/openpaw-engine/src/agent_loop.rs` | L4 | 引擎负责人 | 该文件决定 Provider 流、事件发布、重试、工具调用和取消顺序，错误修改会影响全部回合 |

## 9. 影响、数据流和实现

### 影响分析

成功请求的事件和消息不变；停滞请求从无限等待变为默认 120 秒有界。每次收到帧后重新计时，允许正常长输出。无可见输出时沿用 `RetryPolicy`；已有 MessageStart、文本或思考增量后不重试，避免重复消息。取消令牌使用有偏选择，优先于超时和重试退避。权限、依赖、日志和持久化格式不变。

### 数据流和失败路径

```text
run_turn
  -> timeout(stream_timeout, provider.stream(request))
  -> for each frame: timeout(stream_timeout, stream.next())
       -> frame arrives: publish event and reset frame timer
       -> timeout before visible event: classify transient and retry within budget
       -> timeout after visible event: return Error::Timeout without replay
       -> cancel token: stop immediately, do not wait for timeout
  -> Host maps terminal result and releases active state
```

重试预算耗尽后保留超时类型；原始 provider 错误不得直接进入 Office 正文或协议字段。

### 实现方案和选择理由

在 AgentLoop 统一包装建流和逐帧 future，确保所有 Provider 实现获得一致行为。没有把超时放到具体 OpenAI/Anthropic 适配器，因为那会造成实现分叉；也没有在 Host 设置独立 watchdog，因为 Host 无法判断可见输出后是否允许安全重试。

## 10. 验收测试

| 测试编号 | 执行命令 | 场景和通过条件 |
| --- | --- | --- |
| TEST-OPENPAW-CHG0011-001 | `cargo fmt --all -- --check` | Rust 格式检查退出码为 0 |
| TEST-OPENPAW-CHG0011-002 | `cargo test -p openpaw-engine stream -- --nocapture` | 建流超时、逐帧超时、预算耗尽、重试恢复、已有输出不重试和取消优先全部通过 |
| TEST-OPENPAW-CHG0011-003 | `cargo test -p openpaw-engine` | 引擎全部单元和场景测试通过 |
| TEST-OPENPAW-CHG0011-004 | `cargo test -p openpaw-engine-host --features engine-host` | Host 对引擎失败的清理和终态回归测试通过 |
| TEST-OPENPAW-CHG0011-005 | `CARGO_TARGET_DIR=target-windows cargo build -p openpaw-cli --features engine-host --locked` | Windows Host 二进制构建退出码为 0 |

## 11. 追踪、兼容和回滚

| 需求编号 | 设计章节 | 接口编号 | 代码文件或符号 | 测试编号 | 验收证据 |
| --- | --- | --- | --- | --- | --- |
| REQ-B-HOST-004 | `DES-OPENPAW-B-2026-09-15` 第3、6节 | 无 | `agent_loop.rs::stream_with_retry` | TEST-OPENPAW-CHG0011-002、TEST-OPENPAW-CHG0011-003 | 2026-09-16 历史引擎测试记录；正式 PR 证据待补 |
| REQ-B-HOST-002 | `DES-OPENPAW-B-2026-09-15` 第4、6节 | API-HOST-SESSION-006 | `agent_loop.rs` 到 Host 失败清理 | TEST-OPENPAW-CHG0011-004 | `docs/b-line/testing.md` 历史记录 |

兼容性：默认超时时长仍为 120 秒，成功流和公共字段不变。回滚只恢复旧 `stream_with_retry` 等待逻辑和对应测试，不迁移 session 数据；回滚会恢复 Provider 建流或无帧时永久 busy 的已知风险。

## 12. 验证和审批记录

| 时间 | 人员或系统 | 结论 | 证据或备注 |
| --- | --- | --- | --- |
| 2026-09-16 | 历史 Windows 验证 | 历史引擎、Host 和构建记为通过 | 旧文档记录 stream 8 项、Engine 共 50 项、Host 29 个单元和 14 个 HTTP 测试及 Windows build 通过；本次迁移未重跑 |
| 2026-09-16 | 历史 native demo | 服务健康通过，真实 Provider 延迟未实测 | 旧记录只读接口为毫秒级且未发送真实 Prompt，不能证明上游首帧性能 |
| 2026-09-16 | 历史治理尝试 | 未通过 | 旧记录三项治理命令退出 2，不能计为通过 |
| 2026-09-17 | Codex 文档迁移 | 结构门禁通过 | `python governance/check.py validate --profile governance/project-profile.toml` 退出码 0；不等同 L4 人工批准、PR 或功能复测 |
| 待审批 | Git CODEOWNER | 待审核 | 当前没有可核验的平台审核记录 |
