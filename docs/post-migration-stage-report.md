# Post-Migration Stage Report — 阶段验收报告（会话 21–31）

> 日期：2026-08-31 · 覆盖会话 21–31 的"迁移后批次"全部交付物。
> 背景：工作区整体迁移（D:\MAKOTO\Desktop → E:\Desktop）后，OpenPaw 完成
> Gitee 开源仓初始化（`kunlun-meta/openpaw`），并进入 1.0 冻结就绪后的
> 回归验证、缺陷修复与性能优化阶段。
> 口径：以《OpenPaw 开发方案》的承诺面（19 工具清单、十门质量门、
> compatibility.md 兼容性承诺）为对照基准；逐项证据均可在本仓库复现。
> 详细过程记录见 [devlog.md](devlog.md)（阶段小结 + 会话 21–31 逐节）。

## 1. 阶段总判定

**通过。** 十门全量复跑全绿（会话 31，P-86 后）；E2E 矩阵三面闭合
（happy / error / concurrent）；本阶段发现并修复真实缺陷与缺口各一
（P-83 transcript 重复、P-85 工具声明面空缺）、补全方案承诺工具一项
（P-86 `session_search`）；零未决缺陷、零偏差新增。

## 2. 交付物与核验

### 2.1 迁移与仓库运营

| # | 交付物 | 状态 | 证据 |
|---|--------|------|------|
| 1 | 工作区迁移（两项目 → E:\Desktop） | ✅ | 项目完整；rust-free-agent 仓库随迁（origin/master 同步） |
| 2 | OpenPaw Gitee 仓库初始化 | ✅ | `git init -b main` + 首推 256 文件 / 56,749 行（commit `5272cf6`）；.gitignore 排除 target/ 与临时验证文件 |
| 3 | 迁移回归处置 | ✅ | **P-76**：`env!("CARGO_MANIFEST_DIR")` 烘焙于迁移前测试二进制，cargo 指纹不变不重编 → 全量 `cargo clean` 重编后全绿；4 处 env! 使用点复查均属标准用法 |

### 2.2 缺陷与缺口修复链（P-83 / P-85 / P-86）

| # | 问题 | 根因 | 修复 | 验证 |
|---|------|------|------|------|
| P-83 | 单轮 chat 在 transcript 写入**两条相同 user 消息** | chat.rs 轮末持久化循环漏跳 user 的 append（仅跳过了 SQLite 索引） | `continue` 前移至 append 之前（单点移动）；伴生：`session list` 列头 `messages` → `turns`（该列为轮计数） | cli 13 测试全绿 + 实跑复验（JSONL = meta + 2×message，无重复） |
| P-85 | `chat` 工具链接线只完成一半：ToolHost（执行侧）挂载，但 `run_turn` 的 `tools: &[ToolSpec]`（**LLM 声明面**）传空——模型永不可见 20 工具，ToolHost 空转 | 声明/执行两侧接线不完整；离线 provider（mock/echo）从不产生 tool_use，缺口被测试面完全遮蔽 26 个会话 | `ToolHost::list_tools()` 取 specs 传入 run_turn（单次取用）；sidecar `ask` 维持无工具（有意最小形状，NullToolHost fail-safe） | clippy 0 + cli 13 测试 + mock 实跑；**全库同类排查**（14 调用点 × 15 挂载点交叉核对）零残留（会话 28） |
| P-86 | 方案 §3.4 十九工具清单中的 **`session_search`（meta）自 M3 起缺失**——M2 报告移交 M3 后，M3 报告以 CLI `session search` 子命令的达成遮蔽了 meta 工具交付 | 里程碑出口报告以"数量口径"对照，被 todo 拆分/合并的命名差异遮蔽 | 补实现 `meta/session_search.rs`（~130 行：复用 `SessionDb::search` FTS5，ReadOnly/ParallelSafe，输出与 CLI 逐字段一致，错误三段式四态）+ host 接线 + 断言 21 | tools 256 测试全绿（含新 2 例）；注册 21 名与文档一致 |

**根因预防（入验收纪律）**：里程碑出口报告的工具清单对照必须列**工具名
映射表**，而非数量口径——数量会被拆分/合并的命名差异遮蔽。

### 2.3 性能优化

| # | 点位 | 之前 | 之后 |
|---|------|------|------|
| P-77 | ddg 适配器 | 每次搜索请求 `Regex::new` ×3（热路径） | 三个固定正则 OnceLock 静态化（与 web_extract 风格一致） |
| P-77 | grep 工具 | 同 pattern 编译两次（验证 + 使用） | 编译一次 move 进 spawn_blocking；删除 `.expect` panic 面 |
| P-79 | 搜索双 adapter | 每次 `web_search` 重建 `reqwest::Client`（TLS 连接池/DNS 缓存丢弃） | `shared_search_client()` 进程级 OnceLock 单例，adapter clone 复用 |
| — | Q8 冷构建基线 | — | **158.2s**（clean 16.2s + 构建 + 全测；红线 5min 的 53%，E 盘新基线） |

全库反模式复查结论：`Regex::new` 与 `Client::builder` 调用点逐一定性，
web_extract/security/approval 原已优化，无热路径残留。

### 2.4 质量门自证与强化

| 项 | 结果 |
|----|------|
| **Q10 拦截自产代码** | ✅ P-81：P-79 修复注释中的任务 ID 编号（P-79/P-77）被 scrub 规则 `\b[A-Z]-?\d{2,}\b` 如设计拦截 → 删编号保留语义（不加豁免）；门有效性自证 |
| **MSRV 从声明到强制** | ✅ compatibility.md §2 声明 MSRV 1.80 但从未强制（本地 1.80 安装受镜像限制，D-5 再现）→ **ci.yml 新增 `msrv` job**（1.80.0 全 workspace check）——锁定的依赖或改动需更新 rustc 即红灯，迫使声明走 RFC 复议 |
| **RustSec 口径澄清** | ✅ Q2 的 cargo-deny action 默认含 advisories 检查——无需引入 cargo-audit（避免双重口径漂移） |

### 2.5 运行层 E2E 矩阵（三面闭合）

| 面 | 覆盖 | 结果 |
|----|------|------|
| **happy** | 12 子命令逐一实跑（init/hello/chat/session list·show·search/memory list·show·search·delete/evolve status·reset/doctor/goal new·status·resume/config/model/tool/completions）+ sidecar 双传输（stdio 五工具全调 / HTTP：动态端口解析 → initialize → GET 405（D-12 预设语义实测复现）→ tools/call）+ 四类持久化闭环（sidecar 写 → CLI 读；chat → session 落盘；goal 落盘决策；evolve reset 保留文件清标记） | ✅ |
| **error** | 无效 provider / 缺 id ×5 / `evolve reset` 无 `--yes` 拒绝（人审门语义）/ 空索引降级——错误三段式（what/why/next）逐命令验证 | ✅ 零缺陷 |
| **concurrent** | 8 个 sidecar 进程并行写同一 home：**8/8 落盘无丢失/损坏/交错**（file_lock 原子性实测）；同 id upsert ×3 幂等（最终版生效、单文件） | ✅ |

### 2.6 稳定性物证

| 项 | 结果 |
|----|------|
| bash-ast fuzz 冒烟 | ✅ 30s **156,715 次迭代零 panic**（种子语料 32 → 最终 2,429） |
| 打包面 | ✅ 14 包 `cargo package --list` 全过、无异常文件（crates.io 仅剩 token） |
| 文档健康 | ✅ docs 16 文件 30 个相对链接 **0 断链**；cargo doc（core）exit 0 |
| chat 工具声明 | ✅ 21 工具 specs 进 LLM 请求不破坏离线链路（mock 实跑） |

## 3. 十门终态（会话 31 全量复跑，P-86 后）

| 门 | 结果 |
|----|------|
| `cargo test --workspace` | ✅ exit 0（全目标） |
| clippy `-D warnings` / fmt | ✅ 0 / 0 |
| Q1 行数 | ✅ 业务 **28,336** / 50,000（tools 6,600/14,000；aux 9,287/10,000） |
| Q2 依赖白名单 | ✅ CI 侧（cargo-deny action；advisories 覆盖 RustSec） |
| Q3 零遥测 | ✅ 274 deps + 190 symbols clean |
| Q4 断网烟测 | ✅ 4/4 PASS |
| Q6 golden | ✅ all green（9 frontmatter + session + ReflectionOutput） |
| Q7 trait 快照 | ✅ 11 traits / 58 types 无漂移 |
| Q8 三个五 | ✅ 冷构建 158.2s 基线 + onboarding PASS（会话 19 实测） |
| Q9 人机对齐 | ✅ 编译期断言 + CLI 边界（evolve reset 拒绝语义）运行层验证 |
| Q10 移植卫生 | ✅ 245 files clean（P-81 自证后） |
| stability（1.0 附加） | ✅ 88 项零缺失（stable 82 / unstable 6） |
| msrv（1.0 附加） | ✅ ci.yml msrv job 就位（1.80.0 全 workspace check） |

## 4. 工具面口径（P-86 后统一）

- **注册 21 名** = fs 6（read/write/edit/grep/glob/notebook_edit）+ exec 3
  （bash/clipboard/sleep）+ web 3（fetch/search/extract）+ meta 9
  （think / todo_add / todo_update / todo_list / clarify / enter_plan_mode /
  exit_plan_mode / structured_output / session_search）；
- 方案 §3.4 口径 19 = 实现 21 − todo 拆分多 2 + session_search 补 1（等价
  换算，方案文档保持历史原貌）；
- `chat` = 全工具 agent loop；sidecar `ask` = 单轮无工具（有意对称）。

## 5. 问题登记汇总（本阶段 P-76 ~ P-86）

| # | 一句话 | 定性 |
|---|--------|------|
| P-76 | 迁移后 golden 门红——CARGO_MANIFEST_DIR 随 target 陈旧 | 环境缓存缺陷 → clean 重编 |
| P-77 | ddg/grep 热路径正则重建 | 性能 → 修复 |
| P-78 | sidecar 并发请求响应乱序 | 观察（协议允许，id 配对即正确） |
| P-79 | 搜索 adapter 每次重建 HTTP client | 性能 → 修复 |
| P-80 | 终端集成输出失效 + PS 执行策略 | 环境工具链 → Bypass 落盘模式固化 |
| P-81 | Q10 拦截自产注释中的任务 ID | 门自证 → 修注释 |
| P-82 | feature 门控二进制的测试时序陷阱 | 流程 → build/assert/执行串行规范 |
| P-83 | chat transcript 重复 user 消息 | **功能缺陷 → 修复** |
| P-84 | 脚本 BOM + PS 5.1 数组拼接撕裂 | 环境工具链 → 无 BOM + 独立赋值规范 |
| P-85 | chat 工具声明面空缺 | **功能缺口 → 修复 + 全库排查** |
| P-86 | session_search meta 工具自 M3 缺失 | **承诺面缺口 → 补实现 + 验收纪律修订** |

## 6. 移交清单（全部为外部依赖项）

| 项 | 归属 | 现状 |
|---|------|------|
| 三客户端接线实测（Claude Code / Cursor / OpenWork） | Phase 2 人工 | 教程就绪（[tutorials/sidecar-clients.md](tutorials/sidecar-clients.md)）+ 双传输脚本化实测佐证；**会话 37 推进**：三客户端经探测均未安装（npm 缺失），按用户决策改以 **Trae** 实测——stdio 协议栈五工具探针全过并修复并发调用缺陷（P-90）；Trae GUI 两步（建 `.trae/mcp.json` + 开启项目级 MCP 开关）移交用户 |
| HTTP 壳 SSE 互操作（D-12 / M5b） | M5b | GET 405 已运行层实测复现（预设语义） |
| crates.io 真实发布 | 发布运营 | token 配置即可（拓扑序 dry-run 已验证、14 包打包面干净） |
| nightly 三平台 cold-build 实跑 → D-5 硬版本 pin 定版 | CI 运营 | workflow 就位待实跑；msrv job 就位（1.80 实编在 CI 侧强制） |
| GOVERNANCE.md / SECURITY.md / 数据政策页（方案 §7.11/§7.12 承诺 M6） | 社区批次 | **本阶段核验时发现未交付且未登记**（P-88）——CONTRIBUTING 的 Security 段已改为现状指引，正式治理文档随首批公开发布交付 |

## 7. 结论

迁移后批次（会话 21–31）在 1.0 冻结纪律内完成：**回归全绿、三个真实
缺陷/缺口修复、两轮性能热点清零、E2E 矩阵三面闭合、MSRV 承诺落地强制**。
方案承诺面（19 工具、十门、兼容性条目）与实现的对齐度经本阶段逐项核对
后达到可发布状态；剩余工作全部依赖外部凭证与环境，无未决代码事项。
