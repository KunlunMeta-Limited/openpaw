# Spike S1：agent_loop.rs（8,099 行）精读结论

- **日期**：2026-08-30 · **状态**：已归档（方案 §15.7 S1）
- **目的**：绘制 run_turn 调用树，确定 E1/E2 切分线（R1 主缓解），修正方案 §3.2 分级。

## 1. 文件实测结构（2026-08-30 母仓快照）

| 区段 | 行号 | 规模 | 内容 |
|------|------|------|------|
| 类型与事件 | 49–325 | ~280 | `AgentEvent`、`ClassifierStatus`、`ToolCall`、`ToolBatchResult`、`SkillAutoLoadConfig` |
| `AgentLoop` 构造/装配 | 325–1150 | ~830 | 40+ 个 `with_*` builder、cancel/budget/checkpoint 管理器 |
| **`run_turn`** | **1151–2354** | **~1,200** | 主循环（见 §2） |
| `stream_with_retry` ×2 | 2354–2900 | ~550 | 流式 + 重试（重载：带/不带 StreamingToolExecutor） |
| 插件钩子 | 2900–2968 | ~70 | before/after tool hook |
| `execute_tools` | 2968–3068 | ~100 | **三路径分发器**（见 §3） |
| ctx_inspect / snip / 历史修补 | 3068–3568 | ~500 | 企业件（剥离） |
| `execute_tools_sequential` | 3568–4864 | ~1,300 | 串行路径（含编排工具拦截） |
| `group_safe_calls_for_execution` | 4864–4923 | ~60 | 并发分组 |
| `execute_tools_phased` | 4923–5510 | ~590 | **三阶段路径**（见 §4） |
| `execute_tools_parallel` | 5510–5859 | ~350 | 纯并行路径 |
| 入口封装 | 5859–6098 | ~240 | `run_turn_channel` / `run_turn_shared` / `simple_prompt` |
| 内联测试 | 6098–8099 | ~2,000 | goal/压缩/frozen-context 等 |

## 2. run_turn 主循环骨架（E1 移植目标）

```
reset_cancel_token → hooks(UserPromptSubmit/Setup) → 技能自动加载[剥离→memory 直查,M3]
loop {
  ① cancel check                    → EndTurn
  ② goal pre_turn_check[改写→goal-ledger 留痕]   (E2,先留挂点)
  ③ budget consume/expand           → 预算耗尽 EndTurn
  ④ 压缩检查: microcompact×3(首轮一次) + pre-turn estimate→threshold→trim→compact
  ⑤ build CompletionRequest         (system + messages + tools + cache_edits[G])
  ⑥ stream_with_retry(req)          → (resp, stream_exec)
  ⑦ stop_reason != ToolUse          → usage 回填 → EndTurn / MaxTokens escalate
  ⑧ ToolUse: 收集 tool_use 块 → execute_tools(...) → 回填 ToolResult 块
  ⑨ continue
}
```

关键语义（E1 必须保持）：每轮重置 cancel token；压缩检查每迭代执行（microcompact 扫描
仅首轮一次）；`Final` 事件必达；usage 按 `saturating_add` 累计。

## 3. execute_tools 分发器语义

```
partition ctx_inspect        [剥离: E1 无此工具]
partition orchestration      [剥离: delegate_task 等 E1 无]
若 parallel_tools && len>1 → execute_tools_phased (parallel_phased 语义,默认特性)
否则                        → execute_tools_sequential
```

## 4. 三阶段执行语义（方案 B1/A1 的物证，execute_tools_phased）

- **Phase 1 分类**：pre-hook（deny 即报错）→ guardrail `check_with_context`
  （缓存 `GuardrailCache` 复用流式期判定）→ 三分：
  - `Deny(reason)` → 立即产出 `ToolResult{is_error: "blocked by guardrail: …"}`（**报错路径**）
  - `Allow` → `safe_calls`
  - `Confirm(reason)` → `confirm_calls`
- **Phase 2**：safe_calls 并行执行（`group_safe_calls_for_execution` 按
  `ToolConcurrency` 分组，ParallelSafe 并行 / PathScoped 按路径互斥 / NeverParallel 串行）
- **Phase 3**：confirm_calls 走 ApprovalManager 串行审批执行

## 5. stream_with_retry 骨架

外层 `'outer: loop`：`provider.stream()` 失败 → `classify_api_error` →
`RetryableTransient|RetryableOverloaded|FallbackCandidate` 且 attempts < max_retries →
延迟（`delay_for_attempt_with_jitter`，RateLimited 尊重 Retry-After）→ 重试；
否则返回错误。内层：cancel 检查 → 逐事件转发（TextDelta 等）→ `Final` 必达返回；
无 Final = Provider 错误（含 pause 缓冲路径[剥离 pause → E2]）。

## 6. E1/E2 切分线（修正 §3.2）

**E1（本轮落地）**：run_turn 骨架（①③⑤⑥⑦⑧⑨）+ `execute_tools` 分发器 +
三阶段 phased 语义 + sequential/parallel 简版 + `stream_with_retry`（重试三类 + Retry-After）
+ `AgentEvent` 最小集 + `IterationBudget` 简版。
**E2（下轮）**：压缩子系统重写（token 估算 + 阈值触发 + 单次摘要 ~1,000 行）、
fallback_provider（context-length 回退）、token_budget(.parser)、审批纵深
（SmartApproval/whitelist）、hook_event、streaming_tool_executor 评估。
**拉伸（依序延后）**：checkpoint → nudge → stop_hook。
**剥离确认（黑名单命中）**：ctx_inspect、snip/历史修补（~500 行）、编排工具拦截、
pause_token（E2 再评估）、skill_discovery（M3 改 memory 直查）。

## 7. core 侧最小执行面（engine 依赖清单）

`GuardrailDecision{Allow|Confirm|Deny}` + `ToolGuardrail` trait + `GuardrailChain`（S7 同源件）、
`ToolCallOutcome`、`ToolExecutor` trait（list_tools/call）、`ToolHost` 聚合 trait（E1 只需
Executor+Validator 两个子 trait）、`NullToolHost`、`ApprovalManager`/`ApprovalResponse`
（E1 提供 AutoApprove/AutoDeny 两个内建实现）、`IterationBudget`。
