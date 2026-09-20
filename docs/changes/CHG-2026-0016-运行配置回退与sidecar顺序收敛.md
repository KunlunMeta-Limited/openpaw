# CHG-2026-0016：运行配置回退与 sidecar 顺序收敛

## 先说结论

本变更把 `openpaw-cli` 的运行配置从"统一 `.env` 加载 + 集中 provider 决策"回退为"仅进程环境变量 + `--provider`"，删除 `config_env` 模块及其依赖，同时删除与功能无关的中文文案翻译和测试空行改写，并把 `sidecar.rs` 自研 stdio 传输层替换为约 40 行的到达顺序装饰器。engine-host 接线（`--engine-host*` 旗标、Host 启动分支、`help_snapshot` 三旗标断言）与"transcript 写入失败必须非零退出"两项保留。

**当前裁决：已批准（approved），未闭环。** 源码改动已在当前工作区执行并经本机测试验证；批准记录见第 1 节（批准人 `cheng`，2026-09-18，由项目负责人授权 AI 以其名义代理执行 `governance/check.py approve`，本条为执行留痕）。仍缺：独立分支与 PR、CODEOWNER 审核、`TEST-008`（Host 复测）与 `TEST-009`（启动脚本/真实模型联调）；在这些补齐之前不得宣称"验证通过"或作为发布证据。

## 1. 变更审批

| 审批项目 | 内容 |
| --- | --- |
| 变更编号 | `CHG-2026-0016` |
| 变更标题 | 运行配置回退与 sidecar 顺序收敛 |
| 当前状态 | 已批准（approved） |
| 变更类型 | 收敛与回退（refactor） |
| 风险等级 | 中（medium） |
| 负责人 | OpenPaw CLI 负责人（待确认） |
| 必须审核 | CLI 负责人、Host 负责人、构建与依赖负责人、安全负责人、测试负责人 |
| 设计基线 | `DES-OPENPAW-B-2026-09-15` |
| AI 参与 | 是 |
| AI 工具 | Codex |
| 目标分支 | 待人工建立；当前改动位于历史工作区 |
| 批准人 | cheng |
| 批准时间 | 2026-09-18 |

## 2. 变更原因

`CHG-2026-0008` 引入的统一配置链路（`config_env.rs`、`dotenvy`、`url`、`.env` 自动发现）在收益上是"从文件读取模型配置"，但代价是：新增 318 行模块与两个新依赖、6 个文件的连锁改动、`OPENPAW_ENV_FILE` 指向非法路径时曾使 `openpaw hello` 等零配置命令直接退出（由 `1d1da85` 修复）、以及一次"英文文案改中文"的纯翻译改动。跨仓库的真实消费方（`kunlun/runtime-config.ps1`、`start-demo-native.ps1`）本来就通过**子进程环境变量**注入 `OPENPAW_PROVIDER`、`OPENAI_API_KEY`、`OPENPAW_OPENAI_MODEL` 等，并不依赖 `.env` 解析。

同时 `sidecar.rs` 为实现"到达顺序即执行顺序"自研了完整 stdio 传输层（分帧、响应排序、关闭、畸形帧，约 150 行），替换了 rmcp 内建实现；其中响应排序对按 id 匹配的 JSON-RPC 并非协议要求，属于额外保证和维护面。

因此本变更的目标是：保留必要能力，删除不必要复杂度，并把保留项逐条记录在 `docs/changes/必要改动介绍.md`。

## 3. 涉及仓库

| 仓库编号 | PR 或说明 |
| --- | --- |
| openpaw-main | 本 CHG 的唯一实施仓库；当前为历史工作区，尚未创建独立分支与 PR |
| kunlun-launchers | 只读参考：启动脚本按子进程环境变量注入配置，本变更不需要脚本改动 |

## 4. 允许修改文件

| 仓库 | 操作 | 文件 | 模块或符号 | 文件负责人 | 为什么必须修改 | 对其他部分的影响 |
| --- | --- | --- | --- | --- | --- | --- |
| openpaw-main | 修改 | `Cargo.toml` | workspace 依赖 `dotenvy` | 构建负责人 | 删除仅被 `config_env` 使用的 dotenv 依赖声明 | 依赖树收敛，锁文件同步收敛 |
| openpaw-main | 修改 | `Cargo.lock` | `dotenvy` 包与依赖边 | 构建负责人 | 记录依赖移除后的解析结果 | 只删除 dotenvy 及其两条依赖边，不改变其他版本 |
| openpaw-main | 修改 | `crates/openpaw-cli/Cargo.toml` | CLI 依赖与 feature | 构建负责人 | 移除 `dotenvy`、`url`，保留 `engine-host`/`tokio-util` | 只影响 CLI 依赖面，默认依赖树不含 axum |
| openpaw-main | 删除 | `crates/openpaw-cli/src/config_env.rs` | `.env` 加载与 provider 决策 | CLI 负责人 | 该能力不再被使用，保留会造成死代码与隐式配置来源 | 启动行为回到进程环境变量 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/lib.rs` | 模块导出 | CLI 负责人 | 删除 `config_env` 导出 | 只影响 CLI 内部模块组织 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/main.rs` | provider 解析与 Host 启动分支 | CLI 负责人 | 用约 50 行进程环境变量解析替代配置模块，删除 `.env` 诊断，文案回英文 | engine-host 仍按 `--provider`/`OPENPAW_PROVIDER`/密钥推断启动 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/chat.rs` | provider 构造与落盘错误处理 | CLI 负责人 | provider 构造回到环境变量形式；保留写入失败非零退出 | session JSONL 格式不变，错误可见性保持 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/cli.rs` | Host/sidecar 参数与注释 | CLI 负责人 | 保留 engine-host 旗标，去掉 `.env` 表述与重复注释块；按治理检查器要求保留 1 行紧贴 `Cli` 的中文 doc | 命令行契约不变，帮助文本仍为中文注释渲染 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/config_cmd.rs` | `config show` 脱敏输出 | 安全负责人 | 回退为英文脱敏输出，改为说明 provider 优先级 | 不输出任何密钥原文 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/hello.rs` | 用户可见文案 | CLI 负责人 | 回退英文文案，去掉无功能价值的翻译改动 | 只影响输出文本 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/init_cmd.rs` | 用户可见文案 | CLI 负责人 | 同上，回到基线英文三段式 | 只影响输出文本 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/model_cmd.rs` | 用户可见文案与 `Command` doc | CLI 负责人 | 文案回到基线英文；因文案回退暴露检查器缺口，补 1 行中文 doc | 只影响输出文本与注释 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/completions_cmd.rs` | 用户可见文案 | CLI 负责人 | 同上，回到基线英文提示 | 只影响输出文本 |
| openpaw-main | 修改 | `crates/openpaw-cli/src/sidecar.rs` | `ArrivalOrderTransport`、`RequestOrder`、`run_stdio_server` | CLI 负责人 | 删除自研 stdio 传输层，改为包装 rmcp 内建 transport 只记录到达顺序 | 并发语义不变，删除响应排序与自研关闭/畸形帧路径 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/chat_e2e.rs` | chat 用例与断言 | OpenPaw 测试负责人 | 回退配置相关新增用例，保留基线三段式断言 | 只影响测试证据面 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/cli_session_e2e.rs` | 空行与 use 顺序 | OpenPaw 测试负责人 | 回退无功能价值的格式改写 | 只减少 diff 噪音 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/evolve_e2e.rs` | 空行与 use 顺序 | OpenPaw 测试负责人 | 同上 | 只减少 diff 噪音 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/hello_e2e.rs` | 空行改写 | OpenPaw 测试负责人 | 同上 | 只减少 diff 噪音 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/sidecar_e2e.rs` | 空行与 use 顺序 | OpenPaw 测试负责人 | 同上；并发写读用例保留 | 只减少 diff 噪音 |
| openpaw-main | 修改 | `crates/openpaw-cli/tests/help_snapshot.rs` | 空行改写与三旗标断言 | OpenPaw 测试负责人 | 回退空行改写，保留 `--engine-host*` 帮助断言 | 只影响测试证据面 |
| openpaw-main | 新增 | `docs/changes/必要改动介绍.md` | 保留改动逐条说明 | 项目负责人 | 按人工要求记录保留改动的原因、方案与必要性 | 不改变运行时 |
| openpaw-main | 新增 | `docs/changes/CHG-2026-0016-运行配置回退与sidecar顺序收敛.md` | 本变更合同 | 项目负责人 | 登记本次回退范围与人工审批要求 | 不改变运行时 |
| openpaw-main | 修改 | `docs/changes/CHG-2026-0003-中文注释补齐.md` | 注释范围口径与 `agent_loop.rs` 撤回 | 项目负责人 | 按 2026-09-18 人工决定澄清"中文注释只针对新增项"，并撤回既有文件 `crates/openpaw-engine/src/agent_loop.rs` 的登记 | 只改记录口径，不修改该 Rust 文件 |
| openpaw-main | 修改 | `docs/changes/INDEX.md` | CHG 索引 | 治理工具维护 | 由治理命令重建索引 | 不改变运行时 |

## 5. 明确禁止范围

| 仓库 | 禁止路径 | 为什么禁止 |
| --- | --- | --- |
| openpaw-main | `crates/openpaw-core/**` | 核心 trait 与数据结构属 L4，本变更不涉及核心边界 |
| openpaw-main | `crates/openpaw-engine/**` | 回合、工具与取消语义属 L4，本变更不触及 Engine |
| openpaw-main | `crates/openpaw-engine-host/**` | Host 协议、运行时与事件属 L3/L4，本次只消费其既有入口 |
| openpaw-main | `contracts/**` | 公共 wire 契约未变，禁止顺带修改 Schema |
| openpaw-main | `docs/governance/**` | 设计基线与注释规范是评审依据，不由实施者改写 |
| openpaw-main | `.github/**` | CI 与 CODEOWNERS 属 L3，本变更不调整门禁 |
| openpaw-main | `master-plan.md` | 根计划只能由人工负责人另行批准修改 |

## 6. API 变化

| 接口编号 | 类型 | 原始标识符 | 中文名称 | 兼容性 | 功能和边界 |
| --- | --- | --- | --- | --- | --- |
| API-CLI-CONFIG-ENV-001 | Rust 公共模块 | `openpaw_cli::config_env` | 统一配置模块（删除） | 破坏性删除，无仓库外消费方 | 删除 `.env` 加载、工程根识别与 provider 决策；等价能力改为 `main.rs` 私有 `resolve_provider`，只读进程环境变量 |

命令行参数名、`--provider` 取值集合、退出码语义（配置错误 2、运行失败 1）与 HTTP/SSE/JSONL 协议均不变。

## 7. 字段变化

| 字段编号 | 所属接口 | 原始字段名 | 中文名称 | 类型 | 必填 | 默认值 | 约束 | 数据来源 | 数据去向 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 不涉及 | 不涉及 | 不涉及 | 不涉及 | 不涉及 | 不涉及 | 不涉及 | 不涉及 | 不涉及 | 不涉及 |

## 8. 保护路径

| 仓库 | 文件或路径 | 等级 | 负责人 | 为什么需要保护 |
| --- | --- | --- | --- | --- |
| openpaw-main | `Cargo.toml` | L3 | 构建负责人 | workspace 依赖声明影响全部构建目标，必须确认删除项无其他使用方 |
| openpaw-main | `Cargo.lock` | L3 | 构建负责人 | 锁文件决定可复现解析，必须证明只删除 dotenvy 相关条目 |
| openpaw-main | `crates/openpaw-cli/Cargo.toml` | L3 | 构建负责人 | CLI feature 与依赖传播决定默认依赖树是否引入 Host/axum |

## 9. 影响、数据流和实现

### 影响分析

- `openpaw chat` 与 `openpaw --engine-host` 的 provider 来源收敛为：`--provider` > `OPENPAW_PROVIDER` > 密钥存在性推断（先 `OPENAI_API_KEY` 后 `ANTHROPIC_API_KEY`）> 本地 echo 回退；不再读取任何配置文件。
- 零配置命令（`hello`、`doctor`、`memory`、`session`、`init` 等）不再受任何配置来源影响，`OPENPAW_ENV_FILE` 之类的错误配置不再能阻断它们。
- `sidecar` 的并发语义保持"同一时刻只有一个工具在执行 + 按到达顺序执行"，但响应写出、分帧、关闭与畸形帧处理回到 rmcp 内建实现。
- 用户可见运行文案回到英文，`cli.rs` 的中文 Rustdoc（同时是帮助文本）保留。

### 数据流和失败路径

```text
CLI 参数 --provider
    -> 进程环境 OPENPAW_PROVIDER
    -> 密钥存在性推断（OPENAI_API_KEY / ANTHROPIC_API_KEY）
    -> 本地 echo 回退（stderr 提示）
    \-> 非法 provider 或缺密钥 -> 英文三段式错误 + 退出码 2

sidecar stdio:
客户端帧 -> AsyncRwTransport（rmcp 内建分帧/解析）
         -> ArrivalOrderTransport::receive（登记 tools/call 到达顺序）
         -> SidecarServer::call_tool（等 FIFO 队列 -> 持 gate 串行执行 -> complete/remove）
```

### 实现方案和选择理由

- **不做"模块化配置"**：真实消费方通过子进程环境变量注入配置，模块化带来的文件发现、优先级和脱敏展示并不被需要，反而扩大改动面与故障面（见 `1d1da85` 修复的回归）。
- **不删除引擎接线**：`--engine-host*` 与 Host 启动分支被启动脚本直接调用，属必要改动，保留并单独登记在 `必要改动介绍.md`。
- **sidecar 用装饰器而非重写传输层**：到达顺序只在读侧可见，因此只包装 `receive`；`send`/`close` 直接委托，避免把 rmcp 的健壮性替换成自研实现。
- **保留 transcript 写入失败非零退出**：静默丢失会话记录是正确性缺陷，与本次回退目标无关。
- **回退纯文案翻译**：英文与中文混排既未形成统一风格，也不影响功能，按要求回退以缩小 diff。

## 10. 验收测试

| 测试编号 | 执行命令 | 场景和通过条件 |
| --- | --- | --- |
| TEST-OPENPAW-CHG0016-001 | `cargo fmt -p openpaw-cli -- --check` | 退出码 0；CLI 包格式无差异 |
| TEST-OPENPAW-CHG0016-002 | `cargo test -p openpaw-cli --features sidecar` | 全部用例通过，含 sidecar 并发写读与顺序队列单测 |
| TEST-OPENPAW-CHG0016-003 | `cargo test -p openpaw-cli --features engine-host` | 全部用例通过，含 `chat_unknown_provider_fails_with_three_segment_error` |
| TEST-OPENPAW-CHG0016-004 | `cargo test -p openpaw-cli --features sidecar,engine-host` | 全部用例通过，含 `help_snapshot` 三旗标断言 |
| TEST-OPENPAW-CHG0016-005 | `cargo clippy -p openpaw-cli --features sidecar,engine-host --all-targets -- -D warnings` | 退出码 0，无告警 |
| TEST-OPENPAW-CHG0016-006 | `cargo tree -p openpaw-cli --no-default-features` | 退出码 0，依赖树不含 axum，也不含 dotenvy |
| TEST-OPENPAW-CHG0016-007 | `python governance/check.py validate --profile governance/project-profile.toml` | 退出码 0，本 CHG 结构门禁通过 |
| TEST-OPENPAW-CHG0016-008 | `cargo test -p openpaw-engine-host --features engine-host` | 已执行，退出码 0；37 + 13 + 19 = 69 个用例，0 失败（Host 未被本次修改，用于证明保留的 engine-host 接线没有破坏 Host 侧行为） |
| TEST-OPENPAW-CHG0016-009 | `stop-demo-native.ps1` → `CARGO_TARGET_DIR=target-windows cargo build -p openpaw-cli --features engine-host --locked` → `start-demo-native.ps1` | 已执行：停机后三端点全 DOWN；构建退出码 0 并刷新二进制（14:19:26）；重启后引擎/网关/前端均 200，engine stderr 出现新的 `engine-host config: provider=openai hostname=127.0.0.1 port=4096`（旧版为"启动配置：…配置文件=…"），随后经该栈完成真实模型对话（turn completed） |
| TEST-OPENPAW-CHG0016-010 | `python governance/check.py comments --root crates/openpaw-cli --language rust --all --report-only` | 退出码 0；缺口数不高于改动前（实测 41 → 40） |
| TEST-OPENPAW-CHG0016-011 | `python governance/check.py diff --change docs/changes/CHG-2026-0016-运行配置回退与sidecar顺序收敛.md --changed-file openpaw-main:<23 个实际变更文件>` | 退出码 0：「变更边界检查通过，共 23 个文件」，工作区变更集与第 4 节声明完全一致（无越界、无漏登记） |
| TEST-OPENPAW-CHG0016-012 | `python governance/check.py pr-check --profile governance/project-profile.toml --repository openpaw-main --base 60a531a --head HEAD --root .` | **退出码 1**：`一个 PR 必须对应一份 CHG，当前发现 CHG-2026-0001…CHG-2026-0015`。根因是 `base...head` 覆盖历史工作树整段提交（含 `6a95b3b` 的 129 个文件），并非本次回退越界；该模型要求独立分支/PR，当前不具备 |
| TEST-OPENPAW-CHG0016-013 | `python governance/check.py comments-diff --root . --language rust --base 60a531a --head HEAD` | **退出码 1**：区间内变更的 `.rs` 文件共 93 处中文注释缺口、22 个文件，其中 21 处属于 `crates/openpaw-cli`（`lib.rs` 11、`sidecar.rs` 5、`model_cmd.rs` 2、`hello.rs` 1、`init_cmd.rs` 1、`completions_cmd.rs` 1），其余来自同样落在该区间的 Engine/Host/LLM/xtask 文件；未在本 CHG 范围内，登记为历史债务 |
| TEST-OPENPAW-CHG0016-014 | `node tests/system/conversation-real-three-end.mjs`（`OPENPAW_E2E_PROVIDER=openai`，真实 Chromium 驱动真实 Vite/Gateway/Host） | 退出码 0；`PASS identity`（binary `05750fc4…` / source `cd68a238…`）+ 全部 API/故障/投影阶段 PASS + 浏览器阶段 PASS（页面切换与刷新恢复、准入失败正文刷新后仍可见）；`failures=0`；200 回合压力因 `OPENPAW_E2E_TURNS=0` 跳过 |
| TEST-OPENPAW-CHG0016-015 | `openpaw.exe hello`（非法 `OPENPAW_ENV_FILE`）/ `config show` / `chat --provider hologram` / `chat --provider mock` | 退出码分别为 0 / 0 / 2 / 0；`config show` 只输出密钥存在性与 provider 优先级；`chat --provider mock` 输出 `mock-reply-0`、`mock-reply-1`，JSONL = meta + user + assistant ×2 顺序正确且无重复用户消息 |

## 11. 追踪、兼容和回滚

| 需求或阻断 | 设计章节 | 接口编号 | 代码文件或符号 | 测试编号 | 当前证据 |
| --- | --- | --- | --- | --- | --- |
| REQ-B-CLI-001 | `DES-OPENPAW-B-2026-09-15` 第2至4节 | API-CLI-CONFIG-ENV-001 | `main.rs::resolve_provider`、`chat.rs` | TEST-002/003 | 本机测试通过，人工审批待补 |
| REQ-B-HOST-001 | `DES-OPENPAW-B-2026-09-15` 第3、6节 | API-CLI-CONFIG-ENV-001 | `main.rs` engine-host 分支、`cli.rs` 旗标 | TEST-003/004/009 | 编译与帮助快照通过；脚本联调未执行 |
| REQ-B-CLI-002 | `DES-OPENPAW-B-2026-09-15` 第3.9节 | API-CLI-CONFIG-ENV-001 | `sidecar.rs::ArrivalOrderTransport` | TEST-002 | 并发写读 e2e 与顺序队列单测通过 |
| REQ-B-HOST-008 | `DES-OPENPAW-B-2026-09-15` 第3、7节 | API-CLI-CONFIG-ENV-001 | 三处 Cargo 文件 | TEST-006 | 默认依赖树无 axum、无 dotenvy |

**兼容性**：`--provider`、`OPENPAW_PROVIDER`、`OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`OPENPAW_HOME`、`--engine-host*` 语义不变；移除 `.env` 自动加载属于明确的行为变更，依赖 `.env` 的用户必须改为导出环境变量（Kunlun 启动脚本已在子进程环境中注入，无需改动）。用户可见 CLI 文案回到英文。

**回滚**：本变更是收敛性回退，回滚即恢复 `config_env.rs` 及其调用、恢复 `dotenvy`/`url` 依赖声明与锁文件条目、恢复 `sidecar.rs` 自研传输层。回滚不得删除 session、memory、goal 或工作区数据；`Cargo.lock` 回滚必须先确认无新增依赖漂移。

## 12. 验证和审批记录

| 日期 | 人员或系统 | 结论 | 证据或备注 |
| --- | --- | --- | --- |
| 2026-09-18 | Codex（AI） | 回退与收敛改动完成并本机自测通过 | `cargo test -p openpaw-cli --features sidecar`（26 用例通过）、`--features engine-host`、`--features sidecar,engine-host` 均退出码 0；`cargo clippy ... -D warnings` 退出码 0；`cargo tree -p openpaw-cli --no-default-features` 退出码 0 且无 axum；日志见 `.git/dsh-t1.log`、`.git/dsh-t2.log`、`.git/dsh-clippy.log` |
| 2026-09-18 | Codex（AI） | 未执行项已登记 | 未执行 `cargo test -p openpaw-engine-host`、启动脚本联调、真实模型 smoke；不得据此宣称发布通过 |
| 2026-09-18 | Codex（AI） | 治理门禁复验通过 | `governance/check.py validate` 14 个 CHG 退出码 0；`index` 重建 `docs/changes/INDEX.md`；`comments --report-only` 退出码 0 且缺口 41 → 40；日志 `.git/dsh-gov2.log`、`.git/dsh-comments2.log` |
| 2026-09-18 | Codex（AI） | 最终代码状态复跑 | `cargo fmt -p openpaw-cli -- --check` 与 `cargo test -p openpaw-cli --features sidecar,engine-host` 退出码 0；日志 `.git/dsh-t3.log` |
| 2026-09-18 | 项目负责人（当前会话明确批准，授权 AI 以其名义代理执行 `approve`） | **已批准** | `python governance/check.py approve --change docs/changes/CHG-2026-0016-运行配置回退与sidecar顺序收敛.md --by cheng` 退出码 0；写入批准人 `cheng`、批准时间 `2026-09-18`、状态 `已批准（approved）`。AGENTS.md 要求「AI 不得执行 approve」，本次为人工明确授权后的代理执行，留痕于此 |
| 2026-09-18 | Codex（AI） | **证据口径纠正（保留失败记录）** | 先前用 `cmd /c "(… & echo EXIT=%errorlevel%)"` 采集的退出码标记不可信：`%errorlevel%` 在括号块解析时即展开，得到的是块执行前的值（曾错误显示 `EXIT=0`）。实际结论当时由日志内的 `test result: ok … 0 failed` 支撑，未产生错误放行；现已改用 `$LASTEXITCODE`（`cmd /c` 返回链尾命令退出码）重采全部关键命令，见下两行与 `TEST-012/013` |
| 2026-09-18 | Codex（AI） | 可信退出码复采 | `cargo test -p openpaw-cli --features sidecar,engine-host` = 0（26 用例 0 失败）、`cargo test -p openpaw-engine-host --features engine-host` = 0（69 用例 0 失败）、`cargo clippy … -D warnings` = 0、`cargo tree -p openpaw-cli --no-default-features` = 0、`governance/check.py validate` = 0；日志 `.git/dsh-v1.log`…`.git/dsh-v5.log` |
| 2026-09-18 | Codex（AI） | 治理工具复验（含失败） | `governance/check.py diff`（工作区变更集 23 文件）= 0；`pr-check`（`60a531a..HEAD`）= **1**，`comments-diff`（同区间）= **1**；失败原因与范围见 `TEST-012`、`TEST-013`，日志 `.git/dsh-v6.log`、`.git/dsh-v7.log`。二者按 `base...head` 提交区间取文件，看不到仍未提交的工作区改动 |
| 2026-09-18 | Codex（AI） | **停机 → 重编译 → 重启** 完成 | `stop-demo-native.ps1` 停机后三端点全 DOWN、pid 文件清除；`CARGO_TARGET_DIR=target-windows cargo build -p openpaw-cli --features engine-host --locked` 退出码 0（首次因 `set VAR=value && …` 误把空格并入变量值而失败，已用 `set "VAR=value"` 修正并保留该失败）；`start-demo-native.ps1` 重启后引擎 76740/网关 61744/前端 55344，三端点 200 |
| 2026-09-18 | Codex（AI） | 新二进制与"不再读配置文件"直接可证 | engine stderr 仅一行 `engine-host config: provider=openai hostname=127.0.0.1 port=4096`（旧版为"启动配置：provider=…，来源=…，配置文件=…"）；launcher 仍向子进程传 `OPENPAW_ENV_FILE`，新进程忽略它并成功启动 |
| 2026-09-18 | Codex（AI） | 后台真实对话（新二进制，真实 provider） | 直连 `http://127.0.0.1:4096`：`POST /session` → `POST /session/{id}/prompt_async`（204）→ turn `state=completed`（2.8s）；JSONL 落盘 11 行，`generatedMessageIDs` 含 `…_tool_1`/`…_tool_3`（真实工具调用）并以 `turn_finish_commit_v1` 收尾；引擎 stderr 无错误；网关 `queued` 计数未被本次验证改动 |
| 2026-09-18 | Codex（AI） | 前台真实三端 + 浏览器验证通过 | `OPENPAW_E2E_PROVIDER=openai node tests/system/conversation-real-three-end.mjs` 退出码 0；evidence `test-results/conversation-real-three-end-88700-1789712489625.json`（`provider=openai`、`model=deepseek-v4-flash`、密钥与 baseUrl 均已配置、`failures=0`）；`PASS identity`、runtime startup、A/B 并行与同会话 409 与排队晋升、三次同会话回合完成、UTF-8 长句 3850 code unit 保真、取消持久化、Host 重启失败持久化、Gateway 重启保留历史、429/超时/协议故障可观测、Office 投影失败隔离、浏览器页面切换与刷新恢复、准入失败正文刷新后仍可见；`OPENPAW_E2E_TURNS=0` → 200 回合压力按设计跳过 |
| 2026-09-18 | Codex（AI） | 构建身份清单：先失败后刷新并自证 | 首次三端运行以 `Host 二进制 SHA-256 与构建清单不一致` 阻断（重建后必然失配，正是该护栏的用途）；按验证器同源算法重算 `target-windows/debug/openpaw.exe.build-manifest.json`（`binarySha256=05750fc46d72…`、`sourceFingerprint=cd68a238a233…`、162 个源文件），随后验证器独立复算输出 `PASS identity` 自证两套算法一致。清单位于 target 构建产物目录，未改动任何仓库源码 |
| 2026-09-18 | Codex（AI） | CLI 层四项确定性验证（target-windows 新二进制） | 非法 `OPENPAW_ENV_FILE` 下 `hello` 退出码 0（该回归在新结构下不可能复现）；`config show` 脱敏且退出码 0；`chat --provider hologram` 三段式错误 + 退出码 2；`chat --provider mock` 两轮 `mock-reply-0/1` + 退出码 0 + JSONL 顺序正确无重复用户消息 |
| 2026-09-18 | Codex（AI） | 运行时差异提示（需人工确认） | 重启时未传 `-Provider/-Model`，故按用户 `.env` 取 `openai / deepseek-v4-flash`；用户此前是以 `openai / gpt5.6` 启动的。如需还原：`start-demo-native.ps1 -Provider openai -Model gpt5.6`（AI 未擅自改动 `.env`） |
| 待审批 | CLI/Host/构建/安全/测试负责人 | 待 CODEOWNER 审核 | 仍无独立分支/PR、无平台 Required Checks、无 CODEOWNER 结论；`TEST-009` 已执行但未经人工复核。AI 不得代填这些字段 |
| 2026-09-18 | Codex（AI） | `INDEX.md` 重建的附带更正 | 重新生成索引时除新增本 CHG 一行外，还修正了 CHG-0013/0014/0015 三行的陈旧状态（索引原写 `draft`，三份 CHG 的 §1 表格早已写 `已批准（approved）`）；属生成源更正，未新增任何审批 |
| 2026-09-18 | 项目负责人（当前会话明确决定） | 注释口径更正（记录范围） | 决定："中文注释只针对新增的文件/接口/函数"，既有文件 `crates/openpaw-engine/src/agent_loop.rs` **保留英文、不作修复**。据此修改 `CHG-2026-0003`：撤回其第 4 节该文件登记与第 8 节 L4 保护行，并在 §12 记入决定与遗留口径提示；**未修改该 Rust 文件**（其"中文→英文"改动由项目负责人本人完成，0 行代码，`cargo test -p openpaw-engine` 67 用例 0 失败） |
