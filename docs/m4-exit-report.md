# M4 Exit Report — 进化开箱里程碑出口自检

> 日期：2026-08-30 · 覆盖会话 15–16 的 M4 全部交付物。
> 对照方案出口标准（§3 里程碑表 M4 行）逐项核验。里程碑标准原文：
> 「"对话→反思→知识文件变化（带 unverified 标记）"全程演示脚本入库；
> evolve status/reset 可用；goal 存折"建档→记账→续跑→留痕"四件套 e2e」。

## 1. 出口标准核对

| # | M4 出口标准 | 状态 | 证据 |
|---|-------------|------|------|
| 1 | "对话→反思→知识文件变化（带 unverified 标记）"全程演示脚本入库 | ✅ | `openpaw-reflect-lite/examples/demo_reflect_loop.rs`（283 行）：[1/4] trigger（3 轮假对话含错误轮）→ [2/4] reflect（mock provider 固定 schema 输出）→ [3/4] persist（产物 frontmatter 打印 `unverified: true`）→ [4/4] stats（两域 100% 占比）；核心路径抽为 `run_demo()` 可测函数，`tests/demo_assertions.rs` 断言两域 100% unverified |
| 2 | evolve status/reset 可用 | ✅ | `openpaw evolve status`：unverified 占比表（user/project 双域）+ goal 档案概览（id/status/progress 从 run_summary 台账解析）；`openpaw evolve reset --yes`：经 frontmatter 原子读写移除 unverified 标记（只重写永不删除；坏文件跳过计数）；无 --yes 三段式拒绝 |
| 3 | goal 存折"建档→记账→续跑→留痕"四件套 e2e | ✅ | `goal_ledger_e2e`：建档→读回 / 三轮账本（PROGRESS×2 + BLOCKED_ON_USER 标记解析，每写 fsync）/ resume（末条 next_action→恢复决策）/ 重开账本全量回放（7 事件序列 + 时间戳单调） |
| 4 | 双向读写黄金用例（OpenPaw 记的账 FreeAgent 直接接手） | ✅ | 上游 schema JSONL 行可解析 + openpaw 写入行键集合 `{"type","timestamp","goal_id","payload"}` 与上游一致 + 上游风格 .md 档案可载入 |

**结论：M4 出口标准全部达成，M4 关闭。**

## 2. 同源件收官：S1–S7 全部落地

| 同源件 | 落地里程碑 | 状态 |
|---|---|---|
| S1 frontmatter codec | M1 | ✅ |
| S4 trait 签名（Evolution/LongHorizon/PlatformAdapter） | M1 authored | ✅ |
| S5 requires_human_alignment 底线 | M1 authored（Q9） | ✅ |
| S7 GuardrailDecision | M2 | ✅ |
| S6 HookPoint 24 变体 | M3（+M3 engine 触发接线） | ✅ |
| **S2 ReflectionOutput + unverified** | **M4** | ✅ **本会话收官** |
| **S3 Goal 存折 schema** | **M4** | ✅ **本会话收官** |

## 3. 交付物清单（会话 15–16）

| Crate | 落物 | 测试 |
|---|---|---|
| openpaw-reflect-lite | ReflectionOutput schema（S2 verbatim）+ trigger/reflect/persist/stats/config；**代差红线四处注释**（无质量门控/无策展合并/无相位信号/单一阈值） | 16 例（含 100% unverified 双通道断言） |
| openpaw-goal-ledger | GoalEventLedger（verbatim：fsync + 注入防护）+ models 子集（四标记 + InteractionMode 11→UserGate）+ FsGoalStore + resume 决策 | 46 例（43 单元 + 3 e2e） |
| openpaw-cli | `evolve status/reset`（stats 数据源 + goal 概览 + 原子 reset） | evolve_e2e（--yes 门禁/原子重写/坏文件跳过/project 域） |
| examples | demo_reflect_loop（四阶段演示）+ demo_assertions | 1 例 |

## 4. 关键裁决

| # | 裁决 | 理由 |
|---|------|------|
| — | unverified 走 frontmatter flatten-extra 通道 | MemoryFrontmatter/SkillFrontmatter 自带 `#[serde(flatten)] extra` 且 store 保留未知键——零 schema 侵入 |
| — | InteractionMode 11 → UserGate（models 子集） | 方案 §4.3"剥离大部保留 UserGate"；RunSummary.next_action 为 String 无变体桥接 |
| — | `run_summary` 事件类型（母仓无，上游用 turn_completed） | 方案规格定义 resume 种子；对拍时改一个常量 |
| — | GoalQuota/ExecutionHint 保留纯数据结构 | 目标档案与上游文档双向字节兼容；配额逻辑不移植 |
| — | demo 核心路径抽 `run_demo()` + `#[path]` 复用测试 | example 可运行性 + 断言可测性两全 |

## 5. 工程纪律（质量门终态，2026-08-30 会话 16 时点）

| 门 | 结果 |
|----|------|
| cargo test --workspace | **53 个测试目标全绿**（reflect-lite 16 · goal-ledger 46 · cli evolve_e2e 2） |
| clippy `-D warnings` / fmt / Q10 scrub(220 files clean) / Q7 snapshot(11 traits, 58 types) / Q6 golden | ✅ 全绿 |
| Q1 行数 | 业务 **23,227** / 50,000（reflect-lite ~950/3,000 · goal-ledger ~1,240/2,000） |

## 6. 风险与移交（M5 前）

1. **engine-host 前置验证关卡**（§4.4）：OpenWork 引擎替换面公开接口规范未验证——
   M5 启动前须过 Phase 2 第 1 周关卡，不通过则 engine-host 降级社区实验路线
   （不进 12 crate 正式清单）；
2. **`--sidecar`**（§3.9，≤2,000 红线单列预算）：M5 交付；
3. **hooks 深度**（22 个未接线 HookPoint）/ **embed feature 空壳** / **memory cap
   观察项**：按需推进（§15.4 决策点）。
