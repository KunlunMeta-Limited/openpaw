# T0 验证与交接记录

日期：2026-10-20。OpenPaw 基线 `60a531a51172d14fc3db1a8e7eb6762c2c8369d8`；开发分支 `docs/engine-host-t0`；PR **仅指向 `merge/ai-ofice-react-overagent`**。本记录随 T0 提交，准确审核提交以 PR head 为准。

状态：本地验证完成，远端 CI 初轮已启动，第二轮 Windows 原生测试通过，最新依赖修复仍需远端复核；尚未获得非作者批准或合入，T0 未关闭，T1 不开始。R1 设计待单独批准，Office 未修改。

## 已交付入口

| 产物 | 定位及性质 |
|---|---|
| 九阶段计划、运行/审批/锁/恢复图 | master.md；替换旧任务分配，不实现后续能力 |
| 复用和缺陷/Office 修改台账 | audit.md；D01—D11 留 T1/T2，D12—D15、D17—D23 基线修正（含 YAML 类型兼容调整和待审许可证政策），D16 待查 |
| 契约冻结及来源哈希 | contracts/README.md、sources.json；Office 固定 21 份来源，SDK 精确版本/integrity |
| 真实类型/Schema/SDK fixtures | ../../tests/engine-host-contracts（从仓库根定位）；14 个测试，无跳过 |
| PR 交接规范 | handoff.md、仓库 PR 模板 |
| R1 具体设计 | r1-chat.md；无 Office 源码改动 |
| CI | 原 ci.yml 扩展默认/全 feature、MSRV、sidecar 共存和 Linux/Windows 契约检查 |
| 行数与 feature | 原 xtask AST 统计复用，取消数值门槛；CLI opt-in 依赖连线，Host 仍无运行入口 |

复用：原 CI、AST 计数、既有引擎/存储/sidecar 测试。修改：必要基线缺陷与门禁/贡献说明。新增：协议测试包、原始测试 Schema 快照、交接文档；原 Rust 仓没有对应真实协议验证能力。Node/SDK/Zod 都只在测试图内，不是新的生产 Bridge 或核心副本。

## Linux 实测

系统 Linux x86_64。Rust stable `rustc 1.99.1`（rustfmt/clippy），MSRV `1.88.0`，Node `24.19.0`，npm `11.6.2`。官方工具准备到用户缓存，未替换系统 Node 18 或修改 shell 启动配置。

| 命令（仓库根，除另注） | 最终退出码 | 实际结果 |
|---|---:|---|
| `cargo fmt --all -- --check` | 0 | 格式通过 |
| `cargo test --locked --workspace` | 0 | 891 passed，2 个既有文档示例 ignored；见下方间歇失败记录 |
| `cargo test --locked --workspace --all-features` | 0 | 903 passed，2 个既有文档示例 ignored |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | 0 | 默认配置 |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | 0 | 包含 Host/sidecar |
| `cargo build --locked -p openpaw-cli --features engine-host,sidecar` | 0 | 两 feature 共存构建，不代表 Host 服务实现 |
| `cargo +1.88.0 check --locked --workspace --all-targets --all-features` | 0 | MSRV 编译检查，不是 MSRV 全量运行测试 |
| `cargo tree --locked -p openpaw-cli --edges normal --prefix none` | 0 | 默认 CLI 无 axum；T0 本身尚未新增 HTTP 框架 |
| `cargo run --locked -p xtask -- lines` | 0 | AST 统计，无数值限制，文件读取/解析失败仍报错 |
| `cargo run --locked -p xtask -- scrub --check` | 0 | 清洁；新增注释触发一次编号扫描，修正后通过 |
| `cargo run --locked -p xtask -- snapshot --check` | 0 | core trait 无漂移 |
| `cargo run --locked -p xtask -- golden` | 0 | 原数据 golden |
| `cargo run --locked -p xtask -- telemetry` | 0 | 原扫描 |
| `cargo run --locked -p xtask -- stability` | 0 | 原公开接口标注 |
| `cargo run --locked -p xtask -- smoke-offline` | 0 | 原离线旅程 |
| 契约包 `npm ci --ignore-scripts --no-audit --no-fund` | 0 | 干净安装 11 包，lockfile 固定 |
| `node prepare-office.mjs --office-source <Office checkout>` | 0 | 固定提交全部 21 份来源哈希匹配，未读取用户未提交修改 |
| 契约包 `npm run check` | 0 | SDK v1 satisfies + 负类型用例，未用强转掩盖缺字段 |
| 契约包 `npm test` | 0 | 14 passed / 0 failed / 0 skipped；真实 SDK SSE 迭代器读取 loopback fixture HTTP |
| `git diff --check` | 0 | 无空白错误 |

现有两个 ignored 是 core/sandbox 文档例子，本 PR 未新增 ignored/skip。实际 Office 浏览器、Host 服务、SDK→Host、真实 Provider、Windows 运行及发布均未在 T0 验收；分别属于后续阶段，不能把 fixtures 称为端到端交付。

## 首次失败与处理

- D12：Linux 编译 E0382，借用 writable root 保留错误上下文；独立提交。
- D13：全 feature MCP 断连测试偶发未断开。fixture 现在等待真正 RunningService.cancel 完成，六个 MCP E2E 及全量通过；不改生产 MCP。
- D14：原平台 cfg/lint 导致 clippy 失败，最小 cfg/尾表达式等修正；保持 -D warnings。
- D15：sidecar 测试丢弃其他 ID 的先到响应并永久等待；还错误假设并发 RPC 遵循发送顺序。缓存响应、30 秒失败诊断、失败时回收子进程，依赖读取等写入确认；不改生产执行行为，不删除并发覆盖。
- D16：默认测试一次在释放锁后立即重获失败。首次默认/全 feature、单项复查、最终默认复跑均通过；原因尚未确认，未改生产锁或放宽断言。T3 采用锁前必须调查并提供跨进程证据，不能标记“已修复”。

本地原始日志保存在 `/tmp/openpaw-t0-*.log`，供本机复核；临时日志不进入 Git，远端审核以本表、独立提交与 CI 输出相互核对。初轮失败日志保留，不能以最终成功抹去初轮问题。

工作区挂载为 ntfs3：Cargo 最初创建临时 target 目录时出现阻塞/进程异常，依赖重新解包同样出现阻塞。Rust 构建使用 ext4 `CARGO_TARGET_DIR`；干净 npm 安装在 ext4 测试副本执行，测试源/lockfile 与本 PR 一致。异常 Cargo 临时目录移入本地忽略的 target，没有提交或删除用户文件。此环境问题不算源码通过证据，后续直接在原生 CI 复核。

Linux 推荐（目录自行选择，不能复用系统环境变量承载业务配置）：

```bash
export CARGO_TARGET_DIR=/absolute/ext4-cache/openpaw-target
cargo fmt --all -- --check
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo build --locked -p openpaw-cli --features engine-host,sidecar
cargo +1.88.0 check --locked --workspace --all-targets --all-features
```

## Windows 原生 CI 和门禁

本机没有 Windows 执行环境，不声称已通过。已有 ci.yml 的 windows-latest 原生矩阵执行默认/全 feature 测试、clippy、共存构建和默认依赖树；新契约任务在 Windows 原生 Node 24.19.0 上执行同一锁文件/类型/Schema/SDK 测试。macOS 原测试覆盖保留。PR 创建后记录远端结果；未运行或红灯时保持 Draft，不宣称双平台验收完成。

PowerShell 验证命令与上述 cargo 相同。契约包使用 `Set-Location tests/engine-host-contracts` 后执行 README 的四条 npm 命令，无 WSL/Docker/sh 依赖。T2/T3/T8 另外验证真实 Job Object、cmd、路径和进程回收，不能由 T0 feature 编译替代。

## 交给 T1 与回退

T1 输入：固定基线、audit.md D01—D04 的源码复现点、contracts 的正反例、默认 feature/旧接口兼容要求，以及同一 CI 和交接模板。T1 应补真实 Provider HTTP 请求捕获、各阶段取消、工具配对/压缩/损坏数据/写入失败回归；目前均未实现。

T0 不改变数据格式、公开 core trait 或默认 CLI 命令，无数据迁移。D21 更换 YAML 后端，公开 Mapping/Error 类型身份变化，需要 Rust 下游同步依赖；详见 dependency-baseline.md 和 CHANGELOG.md，不能声明所有公开类型完全兼容。回退为撤销本 Tx 的明确提交；不会删除会话/Office 数据。作者不自批、不合入。用户/指定审核人批准当前提交并合入集成分支后，才能开始 T1；R1 仍独立待审。

## 远端初轮记录

PR #4 / Actions run 39359489541：Linux 契约通过；Windows 在 prepare:office 报原始 Schema 哈希不同。Git Windows checkout 的换行转换改变固定快照字节，新增 .gitattributes 对测试快照禁用转换，保留哈希校验严格度；待新提交复跑。其他平台状态继续核对，不能将本轮计为通过。

同轮 macOS 默认测试两项 safety 断言失败（D18）：生产路径已 canonicalize，测试仍与未解析 /var 符号链接的 temp 根比较。仅修测试比较根的规范化，不放宽范围校验；Windows 也适用。CI test/clippy matrix 禁用 fail-fast，以便一平台失败时其他平台仍产出原生结果。

同轮 fmt、MSRV 和 Linux/Windows clippy 均通过；Linux/Windows workspace 测试因 macOS 失败被原 matrix fail-fast 取消，不能计作通过。quality-gates 的 RustSec 数据库含 CVSS 4.0，旧 cargo-deny-action v1 不能解析（D19）；升级官方 v2 固定提交（cargo-deny 0.20.2），完整保留 deny.toml 检查，等待新轮。

本地补充验证：D18 四个 safety 单测通过；D17 在 ext4 临时 checkout 设置 core.autocrlf=true，全部 10 个原始快照（9 个 Schema 及 LICENSE）哈希匹配，未对验证输入做换行归一化。原生 Windows 结果仍以新 CI 为准。

## 第二轮与依赖修复复验

Actions run [39360307473](https://github.com/KunlunMeta-Limited/openpaw/actions/runs/39360307473)，对应 head 79335d3：Windows 原生 default/all-feature workspace tests、共存 build、默认依赖树均成功（12m14s）；macOS 测试、两平台 clippy/契约、fmt 和 MSRV 通过。Linux 默认测试通过，全 feature parity 的并发共享文件读到空内容（D23）；已隔离 replay 输出目录，保留原断言。quality 扫描工具已能正常读取公告库，发现原 lru/rustls/serde_yml 公告及既有许可证政策冲突，逐项独立提交修复（D20—D22）。本轮整体失败，不作为最终成功。

依赖修复后 Linux 本地复验（Rust 1.99.1/1.88.0，ext4 target）：默认测试 891 passed，全 feature 903 passed，均 exit 0；既有 2 个 ignored 不变。全 feature clippy 初次因 D23 新 helper 的多余借用失败，修正后 exit 0；MSRV all-targets/all-features check exit 0。YAML 相关 Store/Memory/Goal/Reflect 和原 golden 均通过，未改 golden。原 core trait 无变化，YAML 公共类型迁移见专项说明。

官方 cargo-deny 0.20.2 本地完整 `--workspace check` exit 0：advisories/bans/licenses/sources 全部 ok；保留重复依赖等原 warn 政策，未忽略公告。许可证成功基于 deny.toml 中显式精确版本例外，必须作为政策变化审核，不能当成原政策自动允许。随包许可证原文保留原字节及 SHA-256，T8 需验证分发。

最新提交仍须完整远端 CI，最终检查以 PR 当前 head 为准；不能用上一轮 Windows 结果替代依赖更新后的原生验证。详细本地日志为 `/tmp/openpaw-t0-deps-*.log`、`openpaw-t0-yaml-regression.log`、`openpaw-t0-parity-final.log`、`openpaw-t0-deny-final.log`。
