# OpenPaw 开发方案（Engineering Plan）

> **版本**：1.4 · **日期**：2026-08-30 · **状态**：工程实施事实源（随实施演进，口径变更走 ADR；修订史与可执行性评估见 §15–§16）
>
> **上位文档**：《OpenPaw 开源项目设计方案》v1.6（下称"设计方案"）。设计方案回答"做什么、为什么、边界在哪"；本方案回答"**怎么拆、怎么写、怎么验**"。两者冲突时以设计方案为准，本方案修订须同步登记。
>
> **代码基线**：FreeAgent 仓库 `d:\MAKOTO\Desktop\rust-free-agent` 2026-08-30 快照（40 个 crate 目录 / 678 个 .rs / 297,677 行含内联测试 / 6,067 个测试函数，均已实测）。
>
> **实现现状**：本方案为规划口径（历史原貌）。实施现状（M0–M6 关闭、
> 工具注册 21 名、偏差登记 D-1~D-12、迁移后阶段验收）以
> [post-migration-stage-report.md](post-migration-stage-report.md) 与
> [devlog.md](devlog.md)（含 §偏差登记）为准。

---

## 1. 工程总则

### 1.1 拆卸三原则

| 原则 | 含义 | 适用 |
|------|------|------|
| **同源优先** | 能文件级拷贝就不改写：只做 `freeagent_*` → `openpaw_*` 命名替换与依赖重排 | store / memory / skills / plugins / llm / mcp / bash-ast |
| **骨架移植** | 只搬主循环与管线的骨架，企业级模块列入黑名单不搬（§3.3 剥离清单） | engine / cli / sandbox |
| **新增最小** | 三个新 crate 按红线写：reflect-lite ≤3 千、goal-ledger ≤2 千、engine-host ≤2 千，行数断言入 CI | 新增件 |

### 1.2 全局替换与禁运

- **机械替换**：`freeagent_` → `openpaw_`、`freeagent-` → `openpaw-`、`FreeAgent` → `OpenPaw`、`~/.freeagent` → `~/.openpaw`、`./.freeagent` → `./.openpaw`；
- **禁运依赖**（cargo-deny 白名单强制）：`deno_core` / `v8` / `tokio-tungstenite`（浏览器栈五件套整体不进）、`aws-*`（bedrock 走可选 feature 时才引入，且不进 default）、workspace 内任何 `freeagent-*` 路径；
- **default 关闭的重依赖**：`fastembed` 仅作为 `openpaw-memory` 的 optional 依赖（FreeAgent 现状即 `default = []`，`embed = ["fastembed"]`，照搬）；
- **移植卫生（禁词 scrub）**：移植代码含母仓内部痕迹——插件注释引用内部文档（如 `09-deep-integration.md`）、内部代号（`SimonConfig` 等）、任务 ID（`D35` / `A-11` / `M29` 等）、内部域名（`hub.freeagent.dev`）——入库前逐文件 scrub，CI 禁词扫描（Q10）兜底；
- **许可证**：Apache-2.0 + DCO，从 M0 起即定稿，杜绝许可证追溯。

### 1.3 行数口径与预算（单一事实源）

各 crate 上限为**业务代码**（不含内联 `#[cfg(test)]`、`tests/` 目录与生成代码）；全部测试计入"测试与辅助 ≤1 万"口径。CI 脚本按此口径分行统计（剥离 `#[cfg(test)]` 块后计数）。

| crate | 来源（实测行数，含内联测试） | 预算上限 | 预算依据 |
|-------|------------------------------|----------|----------|
| openpaw-core | freeagent-core（19,622 src；config.rs 1,909 / guardrail.rs 1,546 / provider.rs 954） | **≤5,000** | trait 子集 + 类型层；config/schema **重建**而非搬运（v1.2 数据回测上调） |
| openpaw-engine | freeagent-engine（29,666 src；agent_loop.rs 8,099 / compaction.rs 4,353）+ context（1,690） | **≤8,200** | 骨架重写 + 分级移植 E1/E2（§3.2）；compaction 重写 ~1,000；拉伸目标可依序延后 |
| openpaw-llm | freeagent-llm（10,833 src；双 transport 合计 2,452） | **≤5,200** | transport 统一层重写 ~900；4 提供者 + 9 模板 + fallback |
| openpaw-tools | fs 3,446 + exec 2,469 + web 3,935 + meta 5,317 + tools 5,147（host.rs 3,610）+ bash-ast 4,205 + sandbox 5,702 + net 3,244 | **≤14,000** | 19 工具 + host 重写 ~1,000 + bash-ast bash 链先行 + 沙箱三平台原样 + net 四模块（范围决议见 §3.4 / §15.2） |
| openpaw-mcp | freeagent-mcp（4,786 src；host.rs 1,683） | **≤2,400** | host 精简 + 熔断器原样 + 插值/资源最小集 |
| openpaw-store | freeagent-store（4,926 src；sqlite.rs 2,768 / file_migration 1,146） | **≤2,000** | sqlite 子集 ~1,300 + frontmatter/session；迁移剥离 |
| openpaw-memory | memory（6,747）+ skills（7,295，去 Hub/telemetry 约 1,700） | **≤7,400** | 双 tool.rs（784+926）合并 ~400、palace 简化；embed 转 feature |
| openpaw-plugins | freeagent-plugins（2,178 src） | **≤1,300** | 24 HookPoint 原样；security 精简 |
| openpaw-cli | freeagent-cli（15,461 src；chat.rs 2,123 / main.rs 1,171） | **≤4,500** | 14 子命令轻量化 + hello/evolve；**ACP 移 Phase 3**（§15.2）；sidecar 另列 |
| **九项合计** | | **= 50,000** | 恰等"三个五"核心口径（sidecar 不占核心） |
| openpaw-reflect-lite | 新增（参照 reflect 7,830 src 的机制子集） | **≤3,000** | 三步最小闭环（§4.2） |
| sidecar（宿主 openpaw-cli `sidecar.rs`） | 新增（E6） | **≤2,000** | MCP 服务器封装，单列红线同 Lite 纪律（设计方案 §4.5 E6 / §7.5） |
| openpaw-engine-host | 新增 | **≤2,000** | OpenCode 公开引擎接口替身（§4.4，[待验证]） |
| openpaw-goal-ledger | 新增（参照 goal 2,930 src 的四件套子集） | **≤2,000** | 档案/账本/续跑/留痕（§4.3） |
| 测试与辅助 | 全 crate 测试 + 内部 test-utils | **≤10,000** | 黄金用例、trait 快照、e2e、示例 |
| **总计** | | **= 69,000** | 与设计方案 §3.3 六项相加恰等（5 万核心 + 1 万测试 + 3 千 Lite + 2 千 Sidecar + 2 千引擎位 + 2 千存折） |

> 预算纪律：任一 crate 超限即触发设计方案 §4.4.3 纪律复核，回滚或走 ADR 调整；禁止"先写超再补手续"。

### 1.4 目标仓库结构

```
openpaw/
├── Cargo.toml                # workspace：12 members（resolver 2，rust-version 1.80，unsafe_code=forbid 沿用）
├── rust-toolchain.toml       # 钉死工具链（可复现构建前提，§12）
├── crates/
│   ├── openpaw-core/         ├── openpaw-engine/        ├── openpaw-llm/
│   ├── openpaw-tools/        ├── openpaw-mcp/           ├── openpaw-store/
│   ├── openpaw-memory/       ├── openpaw-plugins/       ├── openpaw-cli/
│   ├── openpaw-reflect-lite/ ├── openpaw-goal-ledger/   └── openpaw-engine-host/
├── docs/
│   ├── ARCHITECTURE.md           # 60 分钟导读（设计方案 §6.4.3）
│   ├── walkthrough/              # 三条代码导读路径
│   ├── specs/                    # knowledge-format-1.0.md、数据政策.md（M6）
│   ├── boundary-decisions/       # ADR 归档（设计方案 §7.6 防线一）
│   ├── strategymeta.md           # 双语口径单一事实源（设计方案 §9.2）
│   └── same-source-registry.md   # 同源件清单（本方案 §6.1，M0 建档）
├── examples/                 # 每扩展点最小示例（入 CI 编译矩阵，设计方案 §6.2 DX 规范）
├── tests/golden/             # 双轨黄金样本（memories / skills / sessions / goal 账本，母仓导出，§13 F-2）
├── xtask/                    # 行数统计 / trait 快照 / 发布辅助（非发布 crate，行数计入"测试与辅助"）
└── .github/workflows/        # ci.yml / nightly.yml / release.yml（§12）
```

### 1.5 依赖分层（禁环规则与发布顺序）

```
L0  openpaw-core                                    （地基，零内部依赖）
L1  openpaw-store · openpaw-llm · openpaw-mcp · openpaw-plugins        （→ core）
L2  openpaw-memory（→store）· openpaw-goal-ledger（→store）· openpaw-tools（→mcp）
L3  openpaw-engine（→llm/tools/memory/plugins/store）
L3' openpaw-reflect-lite（→ core + memory + store）· openpaw-engine-host（→engine）
L4  openpaw-cli（→ 全部）
```

- **禁环**：只允许上层依赖下层；`cargo modules`/依赖图检查入 CI；
- **策略与引擎解耦**：`openpaw-reflect-lite` 不依赖 engine/cli（锦标赛要求策略可独立编译，设计方案 §6.3 路线 1）；依赖 core + memory + store——LLM 调用经注入的 `dyn LlmProvider`（core trait），知识写入经 `MemoryProvider` 与 memory crate 的技能写入面，具体实现由 cli 装配；
- **发布顺序 = 拓扑序**：core → store/llm/mcp/plugins → memory/goal-ledger/tools → engine → reflect-lite/engine-host → cli（§12 版本策略）。

---

## 2. 里程碑总览（对应设计方案 §8 路线图）

| 里程碑 | 对应 Phase | 主题 | 出口条件（全部 CI 可验证） |
|--------|-----------|------|----------------------------|
| M0 | Phase 0 后半 | 工程骨架与纪律基建 | workspace 可编译空壳 ×12；CI 骨架 + 行数断言脚本 + cargo-deny 白名单 + 命名替换脚本；LICENSE/DCO/README 骨架；同源件清单建档（§6.1） |
| M1 | Phase 1 前段 | 地基：core / store / llm | `openpaw hello`（echo 模型）跑通一轮对话；frontmatter 黄金用例双向读写绿；trait 快照测试首次生成 |
| M2 | Phase 1 中段 | 引擎与工具：engine / tools / mcp | 真实 LLM 首轮对话 + 三阶段工具执行 + 19 工具全通 + bash-ast 只读/破坏性判定用例全绿 + 本地沙箱三平台冒烟 |
| M3 | Phase 1 中段 | 知识闭环：memory / plugins / cli | 会话落盘 sessions/*.jsonl + index.db FTS5 检索；记忆/技能增删改查；24 HookPoint 注册与触发示例 ×3 |
| M4 | Phase 1 后段 | 进化开箱：reflect-lite / goal-ledger | "对话→反思→知识文件变化（带 `unverified` 标记）"全程演示脚本入库；`evolve status/reset` 可用；goal 存折"建档→记账→续跑→留痕"四件套 e2e |
| M5 | Phase 2 | 入口全集：sidecar / engine-host / doctor | `--sidecar` 经 OpenWork/Claude Code/Cursor 三客户端接线教程实测；engine-host 以 Phase 2 第 1 周验证关卡结论为准（§4.4）；doctor 同名告警可复现（**ACP 移 Phase 3**，§15.2） |
| M6 | Phase 2 出口 | 发布工程 | 三个五全达标入 CI；零遥测符号扫描 + 断网烟测绿；签名发布 + 可复现构建；1.0 冻结三件套（设计方案 §7.8）达成 |

排期遵从设计方案 §8（Phase 0：2 周 / Phase 1：6–8 周 / Phase 2：4 周），本方案不另设时间口径。

---

## 3. crate 级拆卸映射（12 张卡片）

> 每张卡片 = 移植白名单 + 剥离黑名单 + 改写点 + 验收。源文件清单以 2026-08-30 快照为准。

### 3.1 openpaw-core（≤5,000）

- **移植（trait/类型层，原样或近原样）**：`LlmProvider`、`ProviderTransport`、`TokenCounter`、`Message`、`Session`（类型）、`ToolHost` / `ToolExecutor` / `ToolRegistrar` / `ToolValidator` / `ToolOutputTruncator` / `ToolContextWindow`、`ToolSpec` / `ToolCall` / `ToolOutput`、`ToolGuardrail`（Allow/Confirm/Deny 三级）+ `GuardrailChain`、`ApprovalManager` / `ApprovalHook` / `ApprovalRequest`、`HookEventCallback`、`MemoryProvider`、`CommandClassifier`（权限分类，保留规则版）、`Error` / `workspace` / `session_mode` / `i18n`（薄化，仅 en/zh）、`str_util` / `lock_util`；
- **重建（不搬运）**：`config.rs`（源 1,909 行，按 §11 schema 骨架重建 ~600 行）与 `hook_config.rs`（源 1,265 行，收敛为最小钩子配置）；
- **新增**：`EvolutionPolicy` / `LongHorizonPolicy` / `PlatformAdapter`（自 gateway `adapter.rs` 抽出 trait 定义）+ `NullPolicy` + `requires_human_alignment()`（§4.1）；
- **剥离**：`AiClassifier`+`llm_classifier` 的 LLM 权限分类纵深、`ToolCacheManager`、`ToolAliasResolver`、`InstallHandler`、`ConcurrencyChecker`、`TideScheduler`（reflect 专用，Lite 自带）、`McpResourceReader`（下沉 mcp crate）、`pricing`（成本估算下沉 cli 观测）、`observability` / `metrics` / `audit_log` 完整审计流水（保留最小 audit trait）、`redaction` / `data_retention`（企业件）、`prompts/` 大部；
- **验收**：`cargo doc` 全公开项稳定级标注（§7.8）；trait 快照测试生成并通过。

### 3.2 openpaw-engine（≤8,200）——最大风险项

- **移植（骨架重写）**：`agent_loop.rs` 只取 `run_turn` 主循环（取消检查→压缩检查→流式调用→工具执行→回填）与 `execute_tools` 三路径分发（phased/parallel/sequential；保留 `parallel_phased` 的 Deny→报错 / Allow→并行 / Confirm→串行审批三阶段语义）、`stream_with_retry` 重试骨架、`compaction.rs`（token 估算 + 阈值触发 + 摘要压缩）、`fallback_provider.rs`（context-length 回退）、`token_budget(.parser)`、`stop_hook.rs`、`nudge.rs`（简化）、`interrupt(.behavior)` / `execute_tools_interrupt`（基础取消分组）、`pause_token.rs`、`permission_retry.rs`（基础）、`checkpoint.rs`（会话恢复基础）、`hook_event.rs`；并入 freeagent-context 的 `load_config` / `load_tool_host` / `ContextSources`；
- **分级移植（控制 R1 风险的节奏器）**：
  - **E1 骨架**（M2 前半）：`run_turn` + `execute_tools` 三路径 + `stream_with_retry` + 事件流——跑通对拍最小集；
  - **E2 纵深**（M2 后半）：压缩精简版（源 compaction.rs 4,353 行按"token 估算 + 阈值触发 + 单次摘要"重写 ~1,000 行）、fallback、token 预算、审批分支；
  - **拉伸目标（超预算依序延后，§15.4）**：`checkpoint` → `nudge` → `stop_hook`——延后不损伤 B1–A4 承诺；
- **剥离黑名单（不搬）**：`auto_continue`、`browser_pool(.md)` / `browser_route`、`consolidation`、`elastic_semaphore`、`idle_detector`、`intent_normalize`、`mailbox`、`markdown_agent`、`mcp_auth`、`otel`、`provider_limiter`、`scaling_policy`、`session_registry`、`skill_discovery_state` / `skill_learning` / `skill_prefetch`、`steer`、`streaming_tool_executor`、`sub_agent_result` / `sub_agent_trait`、`task_budget`、`transitions`、`uds_transport`、`context_modifier`（评估后定）；
- **改写点**：goal 挂点改为调用 `openpaw-goal-ledger` 的留痕接口（pre_turn_check 简化）；技能发现挂点改为 memory 直查；
- **验收**：与 FreeAgent 的**对拍 e2e**——同一输入脚本下，轮次结构、tool_use 序列、事件类型一致（允许文本差异）；压缩触发用例覆盖 3 档阈值。

### 3.3 openpaw-llm（≤5,200）

- **移植（近全量）**：`anthropic` / `openai` / `gemini`（feature）/ `bedrock`（feature，aws-* 依赖仅随 feature）、`fallback.rs`（主→备降级链 + 独立重试预算）、`sse.rs`、`http_error.rs`、`template.rs`（9 模板原样：deepseek/openrouter/groq/mistral/xai/nvidia/huggingface/ollama/siliconflow）；
- **重写**：`anthropic_transport.rs`（1,131）+ `openai_transport.rs`（1,321）合并为**统一 SSE/重试传输层** ~900 行（两者 80% 结构同构）；`config.rs`（源 2,106 行）按 §11 schema 重建 ~700 行；
- **剥离**：`openai_responses.rs`（上游注释自陈"完整集成 planned"，非完整提供者）；
- **验收**：mock-server 合同测试覆盖 4 提供者流式/非流式/tool_use 三矩阵；模板快照测试（防上游漂移）。

### 3.4 openpaw-tools（≤14,000）

- **19 个核心工具初选清单（M2 评审定稿）**：
  - fs（6）：`read_file` / `write_file` / `edit_file` / `grep` / `glob` / `notebook_edit`
  - exec（3）：`bash` / `clipboard` / `sleep`
  - web（3）：`web_fetch` / `web_search` / `web_extract`
  - meta（7）：`think` / `clarify` / `todo_write` / `enter_plan_mode` / `exit_plan_mode` / `session_search` / `structured_output`
- **ToolHost 聚合层重写**：源 tools/host.rs 3,610 行（含缓存/别名/校验纵深）重写为 ~1,000 行注册-分发骨架——本 crate 最大的单点重写；
- **bash-ast 并入（阶段化）**：M2 移植 bash 链（parser 947 / shadow 546 / semantics 503 / readonly 475 / lib 341 / ps 之外 ≈ 2,300 业务行）；PowerShell 链（ps_parser 483 / ps_semantics 303）**Phase 3 补齐（1.0 前）**——B6 最终态承诺不变，时点调整记入 §15.4 决策点；
- **本地沙箱并入（三平台原样）**：`platform/{windows 1,153, macos 314, linux 294, mod}` + `command` / `policy` / `behavior`（584）/ `approval`（577）/ `file_lock` ≈ 4,000 业务行；**剥离** `credential`（536）、`whitelist_learner`（489）、`snapshot`（634）、`audit`（完整版）；
- **剥离工具**：`grep_legacy` / `glob_legacy` 变体、`vault_http_fetch`、`push_notification`、`team_*`（6）、`task_*` 相关、`delegate_task`、`execute_extra_tool`、`tool_search`（工具面小无需搜索）、`browser_*`（4）、`palace_*`（3）——宫殿索引不暴露为工具，经 CLI `memory` 子命令与注入链消费；`mcp_read/list_resources`（由 openpaw-mcp 挂载，不重复注册）；
- **web 栈**：`web_fetch`（570）/ `web_search`（mod 521 + adapters，**收敛为 bing + duckduckgo 双适配**）/ `web_extract`（337）+ `html_to_md`（665）；freeagent-net 的 `client` / `cookies` / `robots` / `blocklist` 四模块并入本 crate（约 1,200 行计入预算）；**剥离** `wreq_client`（stealth 指纹）、`interceptor` 完整链（保留 Block 判定）、`vault_http`、`content_summarizer`（下沉 LLM）；
- **验收**：每工具至少 1 合同测试 + 1 失败路径测试；bash-ast 既有 `readonly_regression` / `fuzz_baseline` 用例全绿；沙箱三平台冒烟（R6）。

### 3.5 openpaw-mcp（≤2,400）

- **移植**：`host.rs`（1,683，精简至 ~1,000）/ `server.rs`（stdio + Streamable HTTP，ServerSpec 收敛为 Stdio/Http 两种）、`circuit_breaker.rs`（Closed/Open/HalfOpen 原样）、`config.rs`（精简）/ `interpolator.rs`（`${VAR}` 插值）、`resource.rs`（列出/读取最小集）；
- **剥离**：`schema_normalize` 深度归一（保留基础校验）、`prompt.rs` 远端 Prompt 缓存管理、SSE/WebSocket 两种 ServerSpec、keepalive 纵深；
- **验收**：对 rmcp 官方 test-server 的连接/熔断/重连 e2e。

### 3.6 openpaw-store（≤2,000）

- **移植**：`frontmatter.rs`（**同源件**）、`session.rs`（545）/ `session_persist.rs`（JSONL 逐条 fsync + 原子重写）、`sqlite.rs` 子集（源 2,768 行 → ~1,300：messages_fts / audit_log_fts 两虚表 + 基础表）、`global_db.rs`；
- **剥离**：`file_migration.rs`（1,146，只保留 sessions/memories 两类迁移的最小实现）、sub_agent_sessions_fts 等企业表；
- **验收**：**黄金用例双向读写**（与 FreeAgent 互拷 sessions/memories 文件可解析，§6.2）。

### 3.7 openpaw-memory（≤7,400，含 skills）

- **移植（memory 侧）**：`memory.rs`（九类 MemoryType + 衰减半衰期）、`store.rs`（1,128，User/Project 双域）、`sm2.rs`、`relevance.rs`（701，TF-IDF + CJK bigram + 效能重排）、`injection.rs`（452，含防漂移段）、`palace.rs`（五 zone 索引，简化）、`export.rs`、`stats.rs`、`index.rs`；
- **移植（skills 侧）**：`lifecycle.rs`（五态状态机）、`manager.rs`（568）、`dag.rs`（426）、`conditional.rs`、`hot_reload.rs`、`manifest.rs`、`permissions.rs`、`provenance.rs`、`relevance.rs`（563）；
- **合并精简**：memory `tool.rs`（784）与 skills `tool.rs`（926）合并为单一 `manage` 工具入口 ~400 行；`telemetry.rs`（593）剥离；
- **feature 化**：`embed.rs`（967）+ `ann_index.rs`（459）（`embed = ["fastembed"]`，default 关闭，FreeAgent 现状照搬）；
- **剥离**：`hub_client` / `hub_service`（533）/ `hub_store`（600）（Skill Hub 市场）、`browser_memory`、`gap_record`（评估）、`consolidation_lock`（简化为进程内 Mutex）；
- **验收**：九类型 × frontmatter 字段快照测试；SM-2 间隔边界用例；技能五态全迁移路径用例。

### 3.8 openpaw-plugins（≤1,300）

- **移植**：`hooks.rs`（340，**24 个 HookPoint 枚举原样**——含 5 个 Peer*/Collaboration*，见风险 R2）、`manifest.rs`（397）、`discovery.rs`（448）、`context.rs`（221）、`security.rs`（369，精简为信任分级 L0–L3 的静态校验部分）；
- **剥离**：`workflow_bridge.rs`（349，workflow 不存在）；
- **改写点**：Peer*/Collaboration* 钩子保留可注册语义，内置触发源为空（由未来社区 workflow 插件提供）；
- **验收**：24 钩子注册/触发/解除单测 + 3 个 minimal example（BeforeToolCall / AfterReflect / OnApproval）。

### 3.9 openpaw-cli（≤4,500；sidecar 另列 ≤2,000；ACP 移 Phase 3）

- **子命令精选（~14）**：`chat` / `ask` / `hello`（新）/ `session` / `config`（doctor 合并）/ `memory` / `skills` / `evolve`（新：status/reset）/ `goal`（新：new/status/resume，对接 goal-ledger）/ `mcp` / `model` / `tool` / `completions` / `init`；全局旗标 `--sidecar` / `--engine-host`；
- **剥离子命令**：`cron` / `task` / `workflow` / `gateway(.auth)` / `hub` / `data` / `context` / `tide` / `cdp` / `image_input` / `editor` / `reflect` / `reflect_stats`（并入 evolve）；
- **移植设施**：`readline.rs`、`banner.rs`、`exit_codes.rs`（错误三段式"发生了什么/为什么/下一步"在此落地）；`chat.rs`（源 2,123）精简至 ~1,200（流式渲染最小集）；
- **新增模块**：`hello.rs`（内置 echo provider + 演示知识读写 + reflect-lite 首秀，无 key 30 秒）、`sidecar.rs`（**≤2,000 行红线**单列预算，feature `sidecar`：rmcp server 模式，Streamable HTTP 默认绑 127.0.0.1 + stdio 两种；暴露对话/记忆读写/进化状态三类能力；`sidecar status/stop`）、`import_skill.rs`（E7 占位 stub，Phase 2 good first issue）；
- **ACP（Phase 3 排期，能力承诺 ◐ 不变）**：实测最小集估算 ~3,400 行（agent_turn 2,027 + server 1,228 + session_service 793 + transport 399 的子集），Phase 2 内强推将挤爆本 crate 预算（§15.2）——作为独立增量在 Phase 3 以 `--features acp` 交付，预算届时走 ADR；
- **门面语义**：freeagent-agent 不整体移植；`autonomous_loop`（946）/ `review_gate`（256）的**人工评审语义**由 goal-ledger 的 UserGate（`[GOAL_BLOCKED_ON_USER:]`）与 evolve 审批承接；
- **验收**：`openpaw hello` 无 key 30 秒内首响（CI 计时断言）；14 子命令 `--help` 快照测试。

### 3.10 openpaw-reflect-lite（≤3,000，新增）——见 §4.2

### 3.11 openpaw-goal-ledger（≤2,000，新增）——见 §4.3

### 3.12 openpaw-engine-host（≤2,000，新增，[待验证]）——见 §4.4

---

## 4. 新增代码设计

### 4.1 openpaw-core：三个新 trait（OpenPaw 侧新增抽象，FreeAgent 以实现形态对齐）

```rust
#[async_trait]
pub trait EvolutionPolicy: Send + Sync {
    fn name(&self) -> &str;
    /// 每轮对话后：本轮是否值得反思？（频率由策略自定）
    async fn should_reflect(&self, ctx: &TurnContext) -> bool;
    /// 从会话转录中提取候选知识（记忆/技能/思维路径）
    async fn extract(&self, transcript: &Transcript) -> Vec<KnowledgeCandidate>;
    /// 候选处置：合并、淘汰、去重、路由到用户审批
    async fn resolve(&self, candidates: Vec<KnowledgeCandidate>) -> Vec<KnowledgeCandidate>;
    /// 不可协商底线：写入必须经用户审批。默认 true；覆写后仍须保持 true。
    fn requires_human_alignment(&self) -> bool { true }
}

#[async_trait]
pub trait LongHorizonPolicy: Send + Sync {
    fn name(&self) -> &str;
    /// 下一轮执行前：是否继续 / 暂停 / 移交用户（Lite 内置策略仅返回 Continue|Pause|AskUser）
    async fn next_action(&self, ledger: &GoalLedgerView) -> LongHorizonDecision;
}

pub trait PlatformAdapter: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self, sink: InboundSink) -> Result<()>;
    async fn stop(&self) -> Result<()>;
}
```

- `NullPolicy`：`should_reflect` 恒 false 的默认策略，M1 随 core 落地；
- **CI 断言**：`requires_human_alignment` 默认值必须为 true（设计方案 §7.6 防线二物证）；
- 签名冻结走 RFC（设计方案 §7.8 稳定级 `#[stable]` 首批名单）。

**支撑类型契约（M1 定稿，草案）**：

```rust
pub struct TurnContext {
    pub session_id: SessionId,
    pub turn_index: u64,
    pub error_count: u32,
    pub tool_calls_this_turn: u32,
    pub transcript_tail: Vec<Message>,
}
pub struct Transcript { pub messages: Vec<Message> }

pub enum CandidateKind { Memory(MemoryType), Skill, ThinkingPath }
pub enum Confidence { Low, Medium, High }
pub struct KnowledgeCandidate {
    pub kind: CandidateKind,
    pub title: String,
    pub body_markdown: String,          // Markdown 正文，frontmatter 由 persist 侧合成
    pub confidence: Confidence,
    pub frontmatter_extra: serde_json::Value,
}

pub enum LongHorizonDecision { Continue, Pause { reason: String }, AskUser { question: String }, Stop { reason: String } }
/// core 只定义最小视图；goal-ledger 负责同源富类型（GoalFrontmatter/RunSummary，§4.3）↔ 视图的映射
pub struct GoalLedgerView {
    pub goal_id: String,
    pub status: GoalStatusView,
    pub last_run: Option<RunSummaryView>,
    pub total_events: u64,
}
pub struct InboundSink { /* PlatformAdapter 入站投递句柄（消息 → 会话），M5 定稿 */ }
```

> 分层约束：core 不依赖 goal-ledger/memory 等上层 crate——视图类型（`GoalStatusView`/`RunSummaryView`）在 core 定义最小字段集，goal-ledger（L2）负责同源 schema 到视图的单向映射，避免 trait 依赖倒置。
>
> **trait 迭代窗口（防过早冻结）**：M1 交付"草案 v0"→ M2 末按 engine/tools 移植反馈定稿 v1（移植者是 trait 的第一批真实用户）→ 此后才进入设计方案 §7.8 稳定级管理（`#[stable]` 冻结推迟到 M6 的 1.0 三件套）。M1–M2 期间签名允许破坏性迭代，但每次变更须同步 trait 快照（Q7）与 §15.8 打分表复核。

### 4.2 openpaw-reflect-lite（≤3,000）

三步最小闭环，模块规划：

| 模块 | 职责 | 来源参照 |
|------|------|----------|
| `trigger.rs` | 规则触发：`turns_since_last >= N`（默认 3）或本轮出现错误 | freeagent-reflect `micro.rs` 的单一信号简化版 |
| `reflect.rs` | 单次 LLM 反思（max_tokens=1024），输出 **ReflectionOutput 同源 schema**（summary + memory_candidates + skill_candidates + conflicts） | `output.rs` schema 原样（D8 同源件）+ `prompt.rs` 精简（**不含** Quality Gate 八条——代差红线） |
| `persist.rs` | 候选直接落盘：memory/skills 文件 frontmatter 强制写 `unverified: true`；不合并、不去重 | `deferred.rs` 的暂存思路 + store 写入 |
| `stats.rs` | `unverified` 占比统计（evolve status 数据源） | 新增 |
| `config.rs` | N 值与开关 | 新增 |

- **依赖边界**：`→ core + memory + store`（§1.5）——LLM 调用经注入的 `dyn LlmProvider`，记忆写入经 `MemoryProvider`，技能落盘复用 memory crate 的写入面；**不依赖 engine/cli**（锦标赛独立编译保持）；
- **代差红线落实**：无质量门控、无策展合并、无相位信号、触发为单一固定阈值——四件不进 Lite 写入模块注释与 CI 行数断言旁注；
- **验收（设计方案 §8 幼脑出口）**："对话→反思→知识文件变化"演示脚本入库；产物 100% 带 `unverified`（CI 断言）；D8 互操作测试——FreeAgent 策展器 `Curator::curate` 能读取 Lite 产物（跨仓测试入黄金用例）。

### 4.3 openpaw-goal-ledger（≤2,000）

| 模块 | 职责 | 来源 |
|------|------|------|
| `store.rs` | 目标档案：`.md` + YAML frontmatter 原子写、扁平目录扫描 | 移植 freeagent-goal `store.rs` 的 `FsGoalStore`（剥离 `std::sync::Mutex` 跨进程锁，仅进程内锁） |
| `ledger.rs` | 进度账本：append-only JSONL、每写 fsync、id 校验防路径注入 | 移植 `event.rs` 的 `GoalEventLedger` 原样（**同源件**） |
| `models.rs` | `GoalFrontmatter` / `GoalStatus` / `RunSummary` / `[GOAL_COMPLETE|FAILED|PROGRESS:x|BLOCKED_ON_USER:]` 标记解析 | 移植 `models.rs` 子集（**同源件**；剥离 11 种 InteractionMode 大部，保留 UserGate） |
| `resume.rs` | 断点续跑：读账本末条 `RunSummary.next_action` → 恢复执行**一次**（用户显式 `openpaw goal resume` 触发） | 新增（对应 CLI `run_auto_resume` 的单步版，剥离常驻 `--interval` 守护） |
| `error.rs` | 错误类型 | 精简移植 |

- **四件自动驾驶智能不进 Lite**（硬红线）：`quota.rs`（配额管理）、`stall.rs`（停滞重规划注入）、`lock.rs`（跨进程并发保护）、CLI `auto-resume` 守护进程——全部不移植；
- **验收**：建档→三轮执行（含一次 `[GOAL_BLOCKED_ON_USER:]`）→resume 续跑→账本完整回放 e2e；账本 schema 与 FreeAgent `freeagent-goal` 双向读写黄金用例（OpenPaw 记的账 FreeAgent 直接接手）。

### 4.4 openpaw-engine-host（≤2,000，[待验证]）

- **前置**：Phase 2 第 1 周验证关卡（设计方案 §8）——OpenWork 引擎替换面存在公开接口规范或上游 RFC 接受；不通过则本 crate 降级为社区实验路线，**不进入 12 crate 正式清单发布**（占位包撤回）；
- **设计**：`protocol.rs`（OpenCode 公开引擎接口消息结构，仅依赖其公开签名）+ `transport.rs`（stdio JSON-RPC）+ `bridge.rs`（协议方法 → AgentLoop 会话）；
- **纪律**：feature `engine-host` 隔离，未启用零激活；依赖单向——核心 9 crate 零 OpenWork/OpenCode 符号（cargo-deny 白名单断言）；OpenWork 私有 API 适配列入永不清单。

---

## 5. 里程碑任务分解（WBS）

### M0 工程骨架
- [ ] workspace `Cargo.toml`（12 members + resolver 2 + `unsafe_code = "forbid"` 沿用）+ `rust-toolchain.toml`
- [ ] **xtask 工程脚本四件**（规格见 §12，吞吐量检验 §15.6 的前提）：`lines`（syn AST 口径行数 + cap 断言）/ `port`（半自动搬运：白名单拷贝 + 机械替换 + Q10 禁词预检报告）/ `scrub --check`（Q10 全仓扫描）/ `snapshot`（trait 快照生成）
- [ ] cargo-deny 白名单（禁运清单 §1.2）+ `rustfmt`/`clippy` 配置沿用上游
- [ ] 同源件清单建档 `docs/same-source-registry.md`（§6.1）
- [ ] **去风险 Spike S1–S6 执行并归档结论**（§15.7）——S1 结论直接修正 §3.2 分级切分线
- [ ] 仓库就绪：issue/PR 模板（含 §6.4.4 自检清单）、分支保护、标签体系（good-first-issue 等）、security@ 占位与披露政策页
- [ ] LICENSE / NOTICE / CONTRIBUTING(DCO) / README 骨架（设计方案 §9.1 英文首屏）
- **出口**：`xtask lines/scrub/snapshot` 三命令可用且入 CI；S1–S6 结论归档；空 workspace 三平台 CI 绿。

### M1 地基（core / store / llm）
- [ ] core trait 层移植 + 三个新 trait + NullPolicy
- [ ] store 移植 + frontmatter 黄金用例（FreeAgent 样本文件入库 `tests/golden/`）
- [ ] llm 移植 + mock 合同测试
- [ ] `openpaw hello` 最小版（echo provider）
- **出口**：`openpaw hello` 跑通 echo 对话；frontmatter 黄金用例双向读写绿；首个 trait 快照落盘（草案 v0，§4.1 迭代窗口）。

### M2 引擎与工具（engine / tools / mcp）
- [ ] AgentLoop 骨架（E1/E2 分级，§3.2）+ 三阶段执行 + 压缩精简版 + 降级
- [ ] 19 工具 + ToolHost 重写 + bash-ast（bash 链先行）+ 本地沙箱三平台 + net 四模块并入
- [ ] mcp 客户端 + 熔断器
- **出口**：真实 LLM 多轮对话 e2e；对拍测试（R1）首版；工具矩阵全绿。

### M3 知识闭环（memory / plugins / cli）
- [ ] memory + skills 移植（embed feature 化）
- [ ] plugins 24 钩子 + 3 example
- [ ] cli 14 子命令 + 错误三段式 + config doctor
- **出口**：会话/记忆/技能全链路落盘可查；钩子触发示例可跑。

### M4 进化开箱（reflect-lite / goal-ledger）
- [ ] reflect-lite 五模块 + `unverified` 断言
- [ ] goal-ledger 五模块 + goal 子命令
- [ ] EvolutionPolicy/LongHorizonPolicy 装载器（plugins 侧）+ `openpaw evolve status/reset`
- **出口**：设计方案 §8 幼脑出口 + 存折四件套 e2e；Lite/存折行数断言生效。

### M5 入口全集
- [ ] M5a sidecar（rmcp server + 三客户端接线教程实测）
- [ ] M5b engine-host（验证关卡通过后启动；不通过即归档）
- [ ] M5c doctor（官方命名空间核对 + 网络审计）
- **出口**：设计方案 Phase 2 E6/E8 出口条件。（ACP 移 Phase 3 交付，§15.2）

### M6 发布工程
- [ ] 三个五 CI 化（冷构建三平台计时、上手脚本、hello 首响）
- [ ] 零遥测符号扫描 + 断网烟测 + `openpaw doctor` 网络审计
- [ ] 签名发布（Sigstore）+ 可复现构建 + 12 占位包 `cargo publish`
- [ ] 1.0 冻结三件套：stable 标注全覆盖 / 知识格式 1.0 规范（含 unverified 字段与 D8 schema）/ 兼容性承诺
- **出口**：设计方案 Phase 2 全部出口条件 + §9.4 T0 清单就绪。

**出口演示剧本（每里程碑 5 分钟内可复演，演示脚本入库 `examples/demos/`）**：

| 里程碑 | 演示 |
|--------|------|
| M0 | `xtask lines` 输出 12 crate 预算表；`xtask scrub --check` 演示禁词命中即红 |
| M1 | `openpaw hello` 无 key 键入两轮对话；golden 双向读写测试绿 |
| M2 | 让 Agent 用 `grep` + `bash` 完成真实小任务（如"找出含 TODO 的行数"）；对拍最小集 3 场景报告 |
| M3 | 写入一条记忆 → 重启会话仍记得；技能五态流转演示；钩子 example 触发日志 |
| M4 | 对话触发反思 → `evolve status` 显示 `unverified` 产物 → `evolve reset` 一键清空；goal 建档→续跑→账本回放 |
| M5 | OpenWork 接线 `--sidecar` 成功对话；`doctor` 网络审计报告 |
| M6 | 三平台 `cargo install` + 断网全流程 + doctor 全绿 + 签名校验 |

---

## 6. 双轨同步工程

### 6.1 同源件清单（OpenPaw 为 source of truth，FreeAgent 侧改为引用）

| # | 同源件 | OpenPaw 侧位置 | FreeAgent 侧对齐物 |
|---|--------|----------------|--------------------|
| S1 | 知识文件 frontmatter 编解码 | openpaw-store `frontmatter.rs` | freeagent-store `frontmatter.rs` |
| S2 | ReflectionOutput schema（+`unverified` 字段） | openpaw-reflect-lite `reflect.rs` | freeagent-reflect `output.rs` |
| S3 | Goal 档案/账本 schema（`[GOAL_*]` 标记） | openpaw-goal-ledger `models.rs`/`ledger.rs` | freeagent-goal `models.rs`/`event.rs` |
| S4 | `EvolutionPolicy` / `LongHorizonPolicy` / `PlatformAdapter` 签名 | openpaw-core（新增，OpenPaw 主笔） | FreeAgent 后续实现对齐 |
| S5 | `requires_human_alignment` 默认 true | openpaw-core | 同左 |
| S6 | HookPoint 24 枚举 | openpaw-plugins `hooks.rs` | freeagent-plugins `hooks.rs` |
| S7 | `GuardrailDecision` 三级语义 | openpaw-core | freeagent-core `guardrail.rs` |

### 6.2 同步机制落地

- **黄金用例双向读写**：`tests/golden/` 存放双侧样本文件（memories/skills/sessions/goal 账本各 ≥3 件），双侧 CI 每日互读对方样本——一侧改格式未同步即红灯（设计方案 §7.6 防线二）；
- **trait 快照测试**：`cargo openpaw snapshot` 生成 `openpaw-core` 全公开 trait 签名快照，随 release 发布，FreeAgent 侧引用编译；
- **对拍测试**（M2 起持续）：同一对话脚本在 FreeAgent/OpenPaw 双侧运行，断言轮次结构与工具调用序列等价——骨架重写正确性的主证据，M6 后归档为回归用例；**运行位置为内部 CI**（公有仓零 FreeAgent 依赖，规避闭源代码外泄），证据以 JSON 转录 fixture 形式导出入库；建议 ≥30 场景（工具矩阵 × 取消 × 压缩触发 × 审批分支）。
- **对拍 harness 规格**：① 场景脚本为 JSONL（每行：user 消息 / 工具结果注入 / 断言点）；② **对比键仅限结构**：轮次数与终止原因、tool_use 名称序列、事件类型序列、压缩触发时刻——模型文本内容不比对；③ **双侧注入同一 mock provider**（各自经 `LlmProvider` trait 注入同一确定性实现），使输出确定、检验与模型解耦；真实 LLM 仅做每日冒烟对比（不进断言）；④ 首个最小集 3 场景（纯对话 / 单工具串行 / 双工具并行）即 §3.2 E1 出口线。

---

## 7. CI 质量门总表

| # | 质量门 | 断言内容 | 依据 |
|---|--------|----------|------|
| Q1 | 行数断言 | per-crate cap（§1.3 表）；Lite/存折/引擎位单列 | 设计方案 §3.3 / §4.3 |
| Q2 | 依赖白名单 | 禁运清单（deno_core/v8/工作流系）；engine-host 依赖单向；核心 9 crate 零 OpenWork 符号 | §7.5 / §7.7 归属注记 |
| Q3 | 零遥测 | 二进制符号扫描无遥测 SDK；`doctor` 网络审计列出全部外联 | §7.12 |
| Q4 | 断网烟测 | 断网跑通 hello→对话→策略装载全流程 | §7.12 |
| Q5 | `unverified` 断言 | reflect-lite 产物 frontmatter 必含 `unverified: true` | §4.3 决策四 |
| Q6 | 黄金用例 | S1–S3 双向读写每日互检 | §7.6 防线二 |
| Q7 | trait 快照 | 公开签名漂移即红 | §7.7 规则三 |
| Q8 | 三个五 | 冷构建 <5min / 上手成功率 / 核心行数 ≤5 万 | §3.3 / §6.4.6 |
| Q9 | requires_human_alignment | 默认值恒 true 的编译期/测试期断言 | §4.4.2 硬边界物证 |
| Q10 | 移植卫生（禁词扫描） | 入库内容零命中：`Simon` 等母仓代号、`freeagent` 残留标识符、内部文档名（`*-roadmap.md` / `*-integration.md` 等）、任务 ID 模式（`\b[A-Z]-?\d{2,}\b`）、内部域名（`*.freeagent.dev` / `*.freeagent.cn`） | §15.3 P4 |

---

## 8. 风险清单与对策

| # | 风险 | 影响 | 对策 |
|---|------|------|------|
| R1 | `agent_loop.rs` 单文件约 8 千行，骨架重写行为漂移 | 高 | 对拍测试（§6.2）为主证据；重写时逐函数对照上游单元测试迁移；压缩/重试两子系统先行独立移植 |
| R2 | 24 HookPoint 中 5 个 Peer*/Collaboration* 无内置触发源 | 中 | 枚举与注册语义保留，文档标注"触发源由 workflow 社区插件提供"；不算残缺（Q3 断网烟测不含协作） |
| R3 | ACP 最小集 ~3,400 行 vs Phase 2 预算 ≤1,500（缺口 2.3×；实测 agent_turn.rs 2,027 / server.rs 1,228） | 中 | **已决议（§15.2）：ACP 排期移 Phase 3**，能力承诺 ◐ 不变；届时预算走 ADR，仍超限则收缩为 chat-only 会话面或转社区仓 |
| R4 | OpenCode 引擎接口未冻结（v0.1.9 快照） | 中 | engine-host 排在验证关卡后（M5b）；占位包可撤回；sidecar 通道不受影响 |
| R5 | 双轨格式漂移 | 高 | 黄金用例 + trait 快照（Q6/Q7）；同源件禁双仓手改 |
| R6 | 沙箱三平台差异（Windows Restricted Token 复杂度最高） | 中 | M2 先 Linux/macOS 冒烟，Windows 用例对齐 freeagent-sandbox 既有 e2e_sb02_sb04_windows 用例集 |
| R7 | 移植期间 FreeAgent 主干继续演进 | 中 | 以 2026-08-30 快照为拆卸基线打 tag（`disassembly-baseline-20260830`）；之后 FreeAgent 变更按同源件清单选择性同步 |
| R8 | 行数预算挤爆（尤其 engine/cli） | 中 | 六项上限相加恰等 6.9 万、无冗余余量：超限先减 feature 再走 ADR，禁止 quietly 超；per-crate cap 由 Q1 质量门守卫 |
| R9 | 单点押注 rmcp 上游（client 与 sidecar 的 server 双面） | 中 | 传输层以 trait 薄封装（自建 `McpTransport` 抽象），上游破坏性变更只改适配层；Spike S2 预研 rmcp server feature 成熟度（M0） |
| R10 | 内部 CI 建设成本被低估（对拍环境 / 模型 key 池 / Windows runner 沙箱权限） | 中 | P3 前提显式化（§15.3）；对拍 harness 先本地单机跑通再上 CI；key 池限额与用例分桶（mock 优先、真实 LLM 用例走夜间桶） |

---

## 9. 附录：FreeAgent 40 crate 处置总表（与设计方案 §4.2 对齐，含实测体积）

| FreeAgent crate | 实测行数 | 处置 | 去向 |
|-----------------|----------|------|------|
| freeagent-core | 20,108 | 保留（子集） | openpaw-core |
| freeagent-engine | 31,994 | 保留（骨架） | openpaw-engine |
| freeagent-context | 1,690 | 合并 | openpaw-engine |
| freeagent-llm | 11,373 | 保留（近全量） | openpaw-llm |
| freeagent-tools / -fs / -exec / -web / -meta | 6,976 / 3,765 / 2,469 / 4,213 / 5,317 | 简化（精选 19 工具） | openpaw-tools |
| freeagent-tools-media | 1,237 | 剥离 | 社区 feature |
| freeagent-bash-ast | 4,642 | 并入 | openpaw-tools |
| freeagent-sandbox | 8,426 | 简化并入（本地部分） | openpaw-tools |
| freeagent-net（四模块） | 3,244 | 部分并入（client/cookies/robots/blocklist） | openpaw-tools |
| freeagent-mcp | 5,311 | 保留（精简） | openpaw-mcp |
| freeagent-store | 5,155 | 保留（子集） | openpaw-store |
| freeagent-memory | 6,986 | 简化合并 | openpaw-memory |
| freeagent-skills | 7,792 | 简化合并（去 Hub） | openpaw-memory |
| freeagent-plugins | 2,178 | 保留 | openpaw-plugins |
| freeagent-cli | 15,792 | 保留（子集） | openpaw-cli |
| freeagent-agent | 7,827 | 拆解（门面语义 / 评审语义） | openpaw-cli / goal-ledger UserGate |
| freeagent-reflect | 9,070 | 开放接口 + 机制参照 | EvolutionPolicy trait + openpaw-reflect-lite |
| freeagent-goal | 2,930 | 三层拆解（ADR-003） | openpaw-goal-ledger（子集）/ 格式同源 |
| freeagent-acp + acp-schema | 12,108 + 1,331 | 简化 feature | openpaw-cli `--features acp`（最小集） |
| freeagent-lsp | 1,141 | 剥离 | 社区 feature |
| freeagent-installer | 5,028 | 剥离 | 分发走 cargo/brew/Releases（F6 精简版口径） |
| freeagent-gateway | 15,070 | 开放接口 | `PlatformAdapter` trait 入 core；五平台网关闭源 |
| freeagent-sync | 2,603 | 剥离 | FreeAgent 专有 |
| freeagent-integrations | 775 | 剥离 | FreeAgent 专有 |
| freeagent-integration-tests | 5,317 | 简化 | 测试与辅助预算 |
| freeagent-test-utils | 475 | 参照重写 | 内部 test-utils（测试预算） |
| freeagent-workflow / task / cron / graph | 40,475 / 6,718 / 3,353 / 1,086 | 剥离 | §6.3 实验路线（社区） |
| freeagent-browser / cdp / dom / js | 1,753 / 5,085 / 3,992 / 3,797 | 剥离 | MCP 浏览器工具服务器（社区参考实现） |
| freeagent-tui | 19,075 | 剥离 | 视社区需求以 plugin 回归 |

> 口径注：实测行数含内联测试；预算上限（§1.3）为业务代码口径，两者不可直接相减推"削减率"。

---

## 10. 测试移植矩阵

FreeAgent 既有测试资产（6,067 个测试函数）的处置——移植/改造/不移植三分，起步规模约 190+ 用例，全部计入"测试与辅助 ≤1 万"预算：

| FreeAgent 测试资产 | 规模 | 处置 | 去向 |
|--------------------|------|------|------|
| bash-ast：`readonly_regression` + `fuzz_baseline` | 14+1 | **原样移植** | openpaw-tools |
| mcp：`e2e_backoff` / `e2e_interpolation` / `e2e_keepalive` / `e2e_audit_fixes` | 5+8+4+12 | **近原样移植** | openpaw-mcp |
| llm：`e2e_provider_transport` | 21 | **原样移植**（mock 服务器合同测试） | openpaw-llm |
| sandbox：`e2e_sb01` / `e2e_sb02_sb04_windows` / `e2e_sb09_approval` / `e2e_sb10_behavior` | 20+17+21+27 | **子集移植**（剥离件断言删除） | openpaw-tools |
| engine：`phase2_zero_overhead` / `e2e_tool_system` / `cancel_orphan_e2e` | 7+12+2 | **移植** | openpaw-engine |
| memory：`task_e2e` / `e2e_memory_provider` | 4+6 | **改造移植** | openpaw-memory |
| skills：`e2e_hub` | 28 | **改造**（去网络 Hub → 本地生命周期/安装审计测试） | openpaw-memory |
| store：`migration_concurrency` | 2 | **简化移植** | openpaw-store |
| integration-tests：`goal_lifetime_e2e` | 7 | **改造移植** | openpaw-goal-ledger |
| integration-tests：`reflect_simon_*`（phase1/phase2/phase3/comp) | 14+10+12+10 | **改造**为 Lite 三步闭环测试（去策展/质量门断言） | openpaw-reflect-lite |
| integration-tests：`storage_migration_e2e` | 27 | **子集移植** | openpaw-store |
| llm：`e2e_live_multimodal`；sandbox：`sb08_signing` / `sb11_whitelist` / `sb12_snapshot` / `sb13_credential`；tui/gateway/workflow/cli-cdp 全部测试 | —— | **不移植**（对应剥离件或真实凭证依赖） | —— |
| 对拍测试（FreeAgent ↔ OpenPaw 同脚本断言） | 新增 | **新建** | tests/对拍 harness（§6.2） |

## 11. 配置体系（config.toml schema 骨架，M3 定稿）

```toml
schema_version = 1                     # 迁移与 doctor 校验锚点

[provider]
default = "anthropic"                  # anthropic | openai | template:<name> | gemini | bedrock
request_timeout_secs = 120

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"      # 约定：密钥只经环境变量，永不落盘

[model]                                # A4 降级链
primary = "claude-sonnet-4"
fallbacks = ["template:ollama/qwen3:32b"]

[mcp.servers.<name>]
transport = "stdio"                    # stdio | http（ServerSpec 收敛为两种，§3.5）
command = ["npx", "-y", "<server>"]    # transport = "http" 时为 url = "..."

[sandbox]
mode = "local"                         # local | off（无 remote——B5 口径）

[evolve]
policy = "reflect-lite"                # reflect-lite | null | <社区策略插件名>
reflect_turn_threshold = 3             # Lite 触发阈值（四件智能不进 Lite 的边界刻度）

[goal]
ledger = true                          # goal-ledger 开关（false = 不启用存折）
```

- **零遥测的配置面表达**：不存在 `[telemetry]` 节——不给入口，而非默认关闭（设计方案 §7.12 军规一的延伸）；
- `config doctor`（§3.9）校验规则：未知键告警、缺失键输出**可复制的修复命令**、`schema_version` 迁移提示（设计方案 §6.4.2"配置即代码/失败即文档"）。

## 12. 开发环境与构建发布

- **工具链**：Rust 1.80（沿用上游 workspace `rust-version`）；rustfmt / clippy / `unsafe_code = "forbid"` 配置沿用；`rust-toolchain.toml` 钉死版本；
- **与母仓构建体系的关键差异**：FreeAgent 为**离线 vendored 构建**（vendor/ + cc/ 目录，根 Cargo.toml D57 注释自证），OpenPaw **不 vendor**——直接 crates.io + Cargo.lock；可复现构建 = `--locked` + 工具链钉死 + 容器化 release 构建（设计方案 §7.10.3 第 3 条）；
- **原生依赖注意**：bash-ast 的 `tree-sitter-bash` / tree-sitter-pwsh 语法 crate 在 M2 锁版本并入白名单；rusqlite bundled（store）沿用；
- **CI 矩阵**：`ci.yml`（PR：fmt + clippy + test + Q1 行数门 + Q2 依赖白名单 + Q7 快照）/ `nightly.yml`（三平台冷构建计时 Q8 + Q6 黄金用例 + Q4 断网烟测）/ `release.yml`（Sigstore 签名 + 可复现构建 + 拓扑序 publish）；
- **版本策略**：M0–M5 全线统一 0.x（workspace version）；M6 起拆独立版本 + 独立 changelog（设计方案 §7.8）；`openpaw-engine-host` 保持 0.x 预发布直至 E8 验证关卡通过；
- **发布顺序**：按 §1.5 拓扑序；T0 完成 12 占位包矩阵（设计方案 §1.4 第 2 条）。

**工程脚本（xtask）规格**（M0 交付，§15.6 吞吐量检验的前提）：

| 命令 | 规格 |
|------|------|
| `xtask lines` | **syn AST 解析**统计业务行（剥离 `#[cfg(test)]` item、`tests/` 目录、生成代码）——禁用正则/花括号计数（字符串字面量会误判）；输出 per-crate 业务行 vs cap 差额，超限非零退出（Q1） |
| `xtask port --crate <name>` | 半自动搬运：按 §3 白名单拷贝 → `freeagent`→`openpaw` 机械替换（标识符/路径/宏）→ Q10 禁词预检报告（**只报告不自动改语义**，人工裁决） |
| `xtask scrub --check` | Q10 禁词全仓扫描（注释/字符串/标识符三分报告），CI 门 |
| `xtask snapshot` | syn 遍历 openpaw-core 公开项生成签名快照 `openpaw-core.api.json`（Q7，FreeAgent 侧消费，§13 F-5） |
| `xtask golden` | 黄金用例双向读写运行器（Q6） |

## 13. FreeAgent 母仓配套动作（双轨的另一半）

双轨同步不是 OpenPaw 单侧工程——以下动作落在母仓，纳入母仓排期并向边界裁决会报备：

| # | 动作 | 时点 | 依据 |
|---|------|------|------|
| F-1 | 打拆卸基线 tag：`disassembly-baseline-20260830` | M0 前 | R7 |
| F-2 | 导出脱敏黄金样本（memories / skills / sessions / goal 账本各 ≥3 件）→ OpenPaw `tests/golden/` | M1 前 | Q6 |
| F-3 | 同源件 S1–S7 改为引用 OpenPaw（git subtree / cargo git dep），废除双仓手改副本 | M3 起 | 设计方案 §7.7 规则一 |
| F-4 | Curator 读取 reflect-lite 产物（`unverified` 清洗）互操作测试，入 freeagent-integration-tests | M4 | D8 黄金用例的另一半 |
| F-5 | CI 消费 openpaw-core trait 快照编译（快照测试的 FreeAgent 侧） | M6 | Q7 / §7.7 规则三 |
| F-6 | 白皮书更新数字（297,677 行 / 6,067 测试 / 39+1 crate 口径）并回钉"信任竞赛论"原文 | M6 | 设计方案 v1.5 观察项① |
| F-7 | 指定 RunLoop 账本 schema 兼容责任人（ADR-003 格式同源承诺的母仓侧 owner） | M4 | 设计方案 §4.3 决策五 |
| F-8 | 提供对拍运行环境：内部 CI runner（含 Windows）+ 真实 LLM key 池（限额分桶） | M1 前 | P3 / R10 |

## 14. 文档交付物清单（挂历里程碑）

| 交付物 | 里程碑 | 对应设计方案 |
|--------|--------|--------------|
| `docs/strategymeta.md` + `docs/same-source-registry.md` | M0 | §9.2 / §7.7 |
| `docs/boundary-decisions/` ADR 目录与模板 | M0 | §7.6 防线一 |
| 错误码表（三段式：发生了什么 / 为什么 / 下一步） | M3 | §6.4.2 |
| `ARCHITECTURE.md` + 三条 walkthrough | M3–M4 | §6.4.3 |
| 接线教程 ×4（OpenWork / Claude Code / Cursor / engine-host） | M5 | §6.4.2 |
| `GOVERNANCE.md`（含弃养条款）+ `docs/specs/数据政策.md` | M6 | §7.11 / §7.12 |
| `docs/specs/knowledge-format-1.0.md`（含 `unverified` 字段与 D8 schema 黄金样本库） | M6 | §7.8 冻结三件套 |
| 兼容性承诺 / API 稳定级标注全覆盖 | M6 | §7.8 |

## 15. 可执行性评估（v1.2 数据回测）

### 15.1 评估方法

以 2026-08-30 快照的**文件级实测行数**为输入，按移植比假设（同源搬运 ≈ 0.9、骨架/合并重写按目标功能估算）对 §1.3 预算逐 crate 回测，暴露缺口并以"范围决议"闭合——**不提高"三个五"上限**。

### 15.2 回测结果与修正决议

| crate | v1.1 预算 | 数据回测（业务行估算） | 缺口 | v1.2 决议 |
|-------|-----------|------------------------|------|-----------|
| tools | 9,000 | **~15,700**（fs 2,900 + exec 1,350 + web 2,400 + net 1,200 + meta 1,500 + host 3,610 + bash-ast 3,600 + sandbox 4,000 的搬运口径） | ~1.7× | 预算上调 **14,000** + 三项范围决议：host 重写 ~1,000、bash-ast bash 链先行（PowerShell 转 Phase 3）、web_search 收敛双 adapter；差额由多 crate 腾挪 |
| memory+skills | 6,700 | **~10,500**（memory 6,747 + skills 去 Hub/telemetry 后 ~5,000） | ~1.6× | 预算上调 **7,400** + 双 tool.rs 合并（1,710→400）、telemetry 剥离、palace 简化 |
| acp | 1,500 | **~3,400** 最小集（agent_turn 2,027 + server 1,228 + session_service 793 + transport 399 子集） | ~2.3× | **排期移 Phase 3**（设计方案 Phase 2 出口未含 ACP，能力承诺 ◐ 不变），预算届时走 ADR |
| core | 4,500 | **~6,300**（config 1,909 + guardrail 1,546 + provider 954 + approval 851 + error 877 + 类型层） | ~1.4× | 预算上调 **5,000** + config/hook_config 重建而非搬运 |
| engine | 9,500 | 骨架估算 ~9,000（agent_loop 8,099 重写 ~4,000 + compaction 简化 ~1,000 + 其余 ~2,600 + context 装配 ~1,200） | 持平偏紧 | 预算 **8,200** + 分级移植 E1/E2 + 拉伸目标清单（checkpoint/nudge/stop_hook 可依序延后） |
| llm | 5,500 | ~6,400（双 transport 2,452 未合并口径） | ~1.2× | 预算 **5,200** + transport 统一层重写 ~900 |
| mcp / store / plugins / cli | 3,000 / 3,500 / 1,800 / 6,500 | 2,600 / 2,400 / 1,400 / ~5,400 | 基本持平 | **2,400 / 2,000 / 1,300 / 4,500**（ACP 移出后 cli 回落） |
| reflect-lite / goal-ledger / sidecar / engine-host | 3,000 / 2,000 / 2,000 / 2,000 | 2,500 / 1,800 / 1,500 / 1,500（新增件按设计直写） | 无缺口 | 维持 |

**收口结果**：核心九项 5,000+8,200+5,200+14,000+2,400+2,000+7,400+1,300+4,500 = **50,000**；总计 69,000 恰等。全部缺口以范围决议闭合，"三个五"承诺不动。

### 15.3 前提条件（不满足即阻塞，非技术风险）

| # | 前提 | 阻塞点 |
|---|------|--------|
| P1 | 人力：设计方案 §7.11 承诺 ≥2 FTE Maintainer（含 DX Owner）+ 轮值安全响应人在 M0 就位 | M0 全部任务 |
| P2 | 母仓配合：F-1（基线 tag）/ F-2（golden 样本导出）完成 | M1 起的 Q6 黄金用例门无法建立 |
| P3 | 内部 CI 就绪：对拍测试运行于内部环境（公有仓禁依赖闭源 FreeAgent），JSON 转录 fixture 导出通道打通 | R1 缓解失效 → engine 骨架正确性无主证据 |
| P4 | 法务与移植卫生：Apache-2.0 合规审查 + 禁词表（Q10）在 M0 定稿——移植代码实测含内部文档引用（`09-deep-integration.md` 等）、内部代号（`SimonConfig`）、任务 ID、内部域名 | 任何代码入库；泄密与"虚伪开源"风险 |

### 15.4 Go/No-Go 决策点（预算超限的预设降级路径）

| 决策点 | 触发条件 | 预设动作 |
|--------|----------|----------|
| M2 出口 | engine 骨架 > 8,200 行 | 依序延后拉伸目标：checkpoint → nudge → stop_hook（不损伤 A1–A4 承诺） |
| M2 出口 | tools > 14,000 行 | bash-ast PowerShell 链正式转 Phase 3（本方案 §3.4 已预置），必要时走 ADR |
| M4 出口 | reflect-lite > 3,000 行 | 触发设计方案 §4.3 决策四规模红线：回滚重构，不得 ADR 豁免 |
| M5b 前 | E8 验证关卡不通过 | engine-host 归档、占位包撤回（主线无阻塞，E6 sidecar 不受影响） |
| Phase 3 | ACP 最小集 > 2,000 行 | ADR：收缩为 chat-only 会话面（session + turn + approval）或转社区仓 |
| 任一 | 单 crate 超限且拉伸目标已用尽 | ADR 调整预算分配（crate 间挪移，总量不破 69,000） |

### 15.5 总体判定

- **M0–M4：高可行**——预算 v2 经数据回测收口；同源搬运占比高；风险集中于 R1（agent_loop 行为等价性），由对拍测试兜底；
- **M5：中**——sidecar 可行（rmcp server 能力成熟、范围收敛）；engine-host 挂验证关卡，不通过即归档，主线无阻塞；
- **ACP：延 Phase 3**——最小集与 Phase 2 预算缺口 2.3×，强推将牺牲 cli 质量；能力承诺保留、时点让路；
- **最大剩余不确定性**：对拍覆盖度决定 agent_loop 重写信心——建议 ≥30 场景先行，M2 期间按失败案例滚动扩充；
- **结论**：方案在"三个五"与 12-crate 冻结口径内**可执行**，前提条件 P1–P4 满足后即可按 M0 清单动工。

### 15.6 关键路径与吞吐量检验

**关键链**：core → store → engine E1 → tools → 对拍最小集 → chat（M2 出口）；并行支线：llm/mcp/plugins 侧与 store→memory→Lite 链可全程并行。最长杆是 engine，对拍（P3）是其唯一正确性证据源——故 P3 排在 M1 前就绪。

**吞吐量检验**（锚定设计方案 §8 既有时长 Phase 0–2 共 12–14 周，2 FTE）：

| 类别 | 体量（v1.2 预算分解） | 节奏要求 | 判定 |
|------|------------------------|----------|------|
| 同源搬运 + 改名 + scrub | ~43,000 行 | ~3,200 行/周 | 脚本化改名 + Q10 自动 scrub 后为机械劳动——可行 |
| 新写 / 重写（含测试） | ~25,500 行（engine ~5,000 / 测试 ~6,000 / core ~2,500 / Lite 2,500 / cli ~1,800 / llm ~1,600 / 新增件 ~7,300） | ~1,900 行/周 ≈ 400–500 行/日（双人） | 含测试与调试——偏紧但可行；真实瓶颈在"修编译 + 移植测试"而非打字 |

前提：改名与 scrub 脚本在 M0 交付（否则搬运吞吐不成立）；本检验为可行性量级核对，不构成新排期口径。

### 15.7 去风险 Spike 清单（M0 内执行，每项 ≤2 天）

| # | Spike | 验证问题 | 产出 |
|---|-------|----------|------|
| S1 | agent_loop.rs（8,099 行）精读 | run_turn 调用树与耦合面 | 骨架提纲 + E1/E2 切分线（R1 主缓解） |
| S2 | rmcp server feature 验证 | server 端 stdio + Streamable HTTP 成熟度 | sidecar 技术选型确认（R9） |
| S3 | Windows Restricted Token 在 CI runner | 权限可用性 | 沙箱 CI 策略（R6/R10） |
| S4 | rusqlite FTS5 CJK 查询行为 | 分词与 store 裁剪边界 | sqlite 子集裁剪线 |
| S5 | tree-sitter-bash / pwsh crate | crates.io 可用性与版本 | bash-ast 依赖锁 |
| S6 | OpenCode 引擎接口初探 | 公开面证据收集 | M5b 验证关卡检查单细化（§4.4） |

### 15.8 12-crate 可行度打分

| crate | 来源完备性 | 重写占比 | 测试资产 | 残余风险 | 可行度 |
|-------|-----------|----------|----------|----------|--------|
| core | 高（25 trait 实测） | ~30%（config/hook 重建） | 中 | 中 | **高** |
| engine | 高（agent_loop 8,099 实测） | ~60% | 强（e2e 矩阵可移植） | 高 | **中**（对拍兜底） |
| llm | 高 | ~25%（transport 合并） | 强 | 低 | **高** |
| tools | 高 | ~25%（host 重写） | 强 | 中 | **高** |
| mcp | 高 | ~15% | 强 | 低 | **高** |
| store | 高 | ~20%（sqlite 裁剪） | 中 | 低 | **高** |
| memory | 高 | ~10% | 中 | 低 | **高** |
| plugins | 高 | ~10% | 中 | 低（R2 已定性） | **高** |
| cli | 高 | ~40%（chat 精简 + 新命令） | 中 | 中 | **中高** |
| reflect-lite | 新写（参照 7,830 src） | — | 改造 | 低 | **高** |
| goal-ledger | 新写（参照 2,930 src） | — | 改造 | 低 | **高** |
| engine-host | 无源（外部接口未冻结） | — | 新写 | 高 | **待验证**（M5b 关卡） |

### 15.9 平台与依赖验证矩阵

| 组件 | Linux | macOS | Windows | 备注 |
|------|-------|-------|---------|------|
| 沙箱后端 | Bubblewrap ✓ | Seatbelt ✓ | Restricted Token（S3 验证） | R6 |
| FTS5（rusqlite bundled） | ✓ | ✓ | ✓ | S4 |
| readline / crossterm | ✓ | ✓ | ✓ | 沿用上游实现 |
| 断网烟测（Q4） | ✓ | ✓ | ✓ | nightly |
| 冷构建计时（Q8） | ✓ | ✓ | ✓ | nightly 三平台 |

依赖锁：rmcp 1.6（S2）/ rusqlite 0.33 bundled / fastembed 5（optional）/ tree-sitter 系（S5）/ tokio 1.x 沿用上游。

### 15.10 里程碑入口检查单与人力分工

**入口条件**（未满足不进入该里程碑）：

| 里程碑 | 入口条件 |
|--------|----------|
| M1 | §4.1 trait 契约定稿草案 + F-2 golden 样本到位（P2） |
| M2 | llm mock 合同测试全绿 + 19 工具清单评审定稿 + P3 对拍环境就绪 |
| M3 | store 黄金用例绿（Q6）+ M1/M2 出口达成 |
| M4 | M3 出口达成（memory/cli 就绪） |
| M5 | sidecar 依赖（engine/mcp）稳定 + E8 关卡结论 |
| M6 | Q1–Q10 全绿 |

**分工（2 FTE + 轮值，对应设计方案 §7.11 承诺）**：
- **A · 内核线**：core / engine / llm / tools（R1 主战场，Spike S1 后认领）；
- **B · 知识与应用线**：store / memory / plugins / cli，兼 **DX Owner**（设计方案 §6.4.6 周检制度）；
- **安全响应人（轮值）**：M5/M6 安全出口主责（签名/可复现构建/doctor）；
- 社区协作自 M5 起开闸（good first issue 流水线，设计方案 §6.4.4）。

### 15.11 与设计方案的偏差登记（Deviation Register）

| # | 偏差 | 相对设计方案条款 | 定性与处置 |
|---|------|------------------|------------|
| D-1 | ACP 交付时点：Phase 2 内 → Phase 3 | §4.2 E4（◐ 能力承诺）、§8（Phase 出口未点名 ACP） | **排期调整非口径变更**，能力承诺不变；若边界裁决会判定为口径变更，走设计方案 ADR 并同步 §9.2 strategymeta |
| D-2 | bash-ast PowerShell 解析延至 Phase 3（1.0 前） | §4.5 B6 ●（tree-sitter bash + PowerShell 双解析，最终态） | 分阶段交付；1.0 前补齐，届时未达成即走 ADR 降级 B6 口径 |
| D-3 | sidecar 宿主 = openpaw-cli 内模块 | 设计方案未指定宿主 crate | 决议登记，非偏差 |
| D-4 | reflect-lite 依赖含 memory/store | §4.3 未指定依赖面 | 分层决议（§1.5），非偏差 |

执行纪律：本表随 §16 版本登记同步维护；边界裁决会月度过目，判定为口径变更的条目即转入设计方案 ADR 流程——**本方案对上位文档的所有偏离必须显性留痕，不允许沉默漂移**。

## 16. 变更登记

| 版本 | 日期 | 变更 |
|------|------|------|
| 1.0 | 2026-08-30 | 首版：基于设计方案 v1.6 与 FreeAgent 2026-08-30 快照全量实测编写 |
| 1.1 | 2026-08-30 | 增补：目标仓库结构与依赖分层（§1.4–1.5）、支撑类型契约（§4.1）、测试移植矩阵（§10）、配置体系 schema 骨架（§11）、开发环境与构建发布（§12）、FreeAgent 母仓配套动作（§13）、文档交付物清单（§14）；修正 §3.4 宫殿工具表述残留 |
| 1.2 | 2026-08-30 | **可执行性评估与再平衡**：文件级实测回测暴露预算缺口（tools 1.7× / memory 1.6× / ACP 2.3× / core 1.4×），预算全面重定（§1.3 v2，核心九项 50,000 收口）并以范围决议闭合——host 重写、bash-ast 阶段化（PowerShell 转 Phase 3）、transport 统一层重写、双 tool.rs 合并、**ACP 排期移 Phase 3**；新增 engine 分级移植 E1/E2 与拉伸目标清单（§3.2）、对拍内部 CI 化（§6.2）、移植卫生禁词门 Q10（§1.2/§7）、本章评估（§15：回测/前提/Go-NoGo/判定） |
| 1.3 | 2026-08-30 | **可执行性深化**：新增关键路径与吞吐量检验（§15.6，搬运 ~43k/新写 ~25.5k 的双人节奏核对）、去风险 Spike 清单 S1–S6（§15.7，M0 内执行）、12-crate 可行度打分（§15.8）、平台与依赖验证矩阵（§15.9）、里程碑入口检查单与人力分工（§15.10）；修正 reflect-lite 分层（"仅 → core"不可行——技能落盘需 memory 写入面，改为 core+memory+store，锦标赛独立性=不依赖 engine/cli）；新增风险 R9（rmcp 单点押注）/R10（内部 CI 成本）与母仓动作 F-8（对拍环境） |
| 1.4 | 2026-08-30 | **可执行性收尾**：新增 xtask 工程脚本规格（§12：syn AST 行数统计/半自动搬运/禁词扫描/快照/golden 五命令——吞吐检验与 Q1/Q7/Q10 的执行载体）、对拍 harness 规格（§6.2：JSONL 场景 + mock 双侧对齐 + 结构对比键 + E1 最小集 3 场景）、trait 迭代窗口（§4.1：M1 草案→M2 定稿→M6 冻结，防过早 stable）、偏差登记表（§15.11：D-1 ACP 时点 / D-2 PowerShell 时点显性留痕）、M0 扩容（Spike 落项 + 仓建清单）与 M0–M6 出口演示剧本（§5）；修正 R3 对策过时引用 |
