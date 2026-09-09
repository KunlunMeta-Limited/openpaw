# M3 Exit Report — 知识闭环里程碑出口自检

> 日期：2026-08-30 · 覆盖会话 10–14 的 M3 全部交付物。
> 对照方案出口标准（§3 里程碑表 M3 行）逐项核验。里程碑标准原文：
> 「会话落盘 sessions/*.jsonl + index.db FTS5 检索；记忆/技能增删改查；
> 24 HookPoint 注册与触发示例 ×3」。

## 1. 出口标准核对

| # | M3 出口标准 | 状态 | 证据 |
|---|-------------|------|------|
| 1 | 会话落盘 sessions/*.jsonl | ✅ | chat SessionWriter（会话 5）+ 每条消息 `index_event` 入 index.db（会话 12 接线，best-effort） |
| 2 | index.db FTS5 检索 | ✅ | `openpaw-store/src/sqlite.rs`：messages_fts / audit_log_fts 两虚表 + 迁移链 v1–v8；`session search`（FTS5 MATCH + snippet）全链路 e2e：chat 一轮 → list → search 命中（tests/cli_session_e2e.rs） |
| 3 | 记忆增删改查 | ✅ | FsMemoryStore 双域（会话 10）+ relevance/sm2/injection（会话 11）+ cli `memory list/search/show/delete` + manage `domain:"memory"`（会话 14） |
| 4 | 技能增删改查 | ✅ | skills 13 文件（会话 13）：五态 lifecycle + FsSkillStore + SkillManager + tool 12 动作（fail-closed 经 ApprovalManager） |
| 5 | 24 HookPoint 注册与触发示例 ×3 | ✅ | plugins：24 钩子逐点注册/触发/解除单测 + 3 examples（before_tool_call / after_reflect / on_approval，可运行）（会话 10）；**engine 侧真实触发接线**（会话 14）：AgentLoop.execute_with_hooks 在全部 4 个执行点触发 BeforeToolCall/AfterToolCall（阻断/替换/观察语义） |

**结论：M3 出口标准 5/5 达成，M3 关闭。**

## 2. 交付物清单（会话 10–14）

| Crate | 落物 | 测试 |
|---|---|---|
| openpaw-plugins | 24 HookPoint + HookChain + manifest/discovery/context/security | 52 例 + 3 examples |
| openpaw-memory（memory 侧） | 九类 MemoryType + FsMemoryStore 双域 + stats/sm2/relevance/injection/palace/index/export + core memory_provider 底座 | 110 例（SM-2 边界 / 九类型快照 / 双域隔离） |
| openpaw-memory（skills 侧） | 五态 lifecycle + FsSkillStore + manager/dag/conditional/hot_reload/relevance + tool 12 动作 | 五态全迁移（8 合法边 + 9 非法拒绝）|
| openpaw-memory（工具面） | memory_tool + skills::tool + **manage 合并入口**（D-11 闭合） | 域路由 + fail-closed |
| openpaw-store | sqlite 子集（sessions/messages + 两 FTS5 虚表 + 迁移链）+ global_db | FTS 命中/持久性/迁移/audit 9 例 |
| openpaw-cli | `session list/show/search` + `memory list/search/show/delete` + chat→index.db 接线 | 全链路 e2e |
| openpaw-core | memory_provider 底座 + **InjectionRecord 回填**（Session.injection_records，serde 向后兼容） | 往返 + 旧格式兼容 |
| openpaw-engine | **HookPoint 触发接线**（execute_with_hooks，4 执行点统一，零开销短路） | 阻断/观察/无钩子回归 |

## 3. 关键裁决（影响 M3 口径的）

| # | 裁决 | 理由 |
|---|------|------|
| D-11 | manage 合并入口：memory tool + skills tool 路由封装（`domain: "memory"\|"skill"`），两独立入口保留 | 方案合并动机为预算；路由封装达成单一入口语义且不破坏既有调用 |
| — | telemetry 裁剪保留（方案"剥离 593"） | relevance/provenance/manager 依赖其类型；hub 面随 hub 三件剥离 |
| — | 空查询"按 category 浏览"分支保留（母仓为空结果） | 既有 e2e 契约；有意偏差已注释 |
| — | InjectionRecord 挂 Session 结构而非 SessionEvent | 母仓同款；SessionEvent 枚举不动保证 store/sqlite 兼容 |

## 4. 工程纪律（质量门终态，2026-08-30 会话 14 时点）

| 门 | 结果 |
|----|------|
| cargo test --workspace | **50 个测试目标全绿**（详见 devlog 会话 14） |
| clippy `-D warnings` / fmt / Q10 scrub(203 files clean) / Q7 snapshot(11 traits, 58 types) / Q6 golden | ✅ 全绿 |
| Q1 行数 | 业务 **21,056** / 50,000（memory ~8,000/7,400——**接近 cap，见 §5**） |

## 5. 风险与移交

1. **openpaw-memory 接近 cap（~8,000/7,400）**：skills 并入是方案 §3.7 口径，超出量
   随 M4（reflect-lite / goal-ledger）推进时观察；若持续超限走 §15.4 决策点（ADR）；
2. **embed feature 为空壳**（memory 与 skills 两侧）：向量检索（fastembed + ann_index）
   按方案 default 关闭，1.0 前评估是否交付；
3. **hooks 深度**：当前接线 BeforeToolCall/AfterToolCall 两点；其余 22 HookPoint
   （含 5 个 Peer*/Collaboration*——内置触发源为空）按需在 M4+ 触发面扩展。
