# M5 Exit Report — 入口全集里程碑出口自检

> 日期：2026-08-30 · 覆盖会话 17–18 的 M5 交付物。
> 对照方案出口标准（§3 里程碑表 M5 行）逐项核验。里程碑标准原文：
> 「`--sidecar` 经 OpenWork/Claude Code/Cursor 三客户端接线教程实测；
> engine-host 以 Phase 2 第 1 周验证关卡结论为准（§4.4）；doctor 同名告警可复现
> （ACP 移 Phase 3，§15.2）」。

## 1. 出口标准核对

| # | M5 出口标准 | 状态 | 证据 |
|---|-------------|------|------|
| 1 | `--sidecar` MCP server | ✅（server 侧就绪） | 五工具（ask / memory_get·set·search / evolve_status）+ stdio/HTTP 双传输 + JSON-RPC 全链路 e2e（sidecar 单测 9 + e2e 2）；847/2,000 红线 |
| 2 | 三客户端接线教程实测（OpenWork/Claude Code/Cursor） | ⏳ **Phase 2 人工项** | server 侧就绪待实测；HTTP 壳与 SSE 客户端互操作验证列 M5b（D-12：无 SSE/session，MCP 规范允许 405）；教程模板随 M6 文档批次 |
| 3 | engine-host 验证关卡结论 | ✅ **不通过→归档** | [ADR-001](boundary-decisions/ADR-001-engine-host-archived.md)：OpenCode 公开扩展面（SDK/plugin/ACP）为客户端接口，无宿主侧引擎替换规范；V2 重构未冻结（R4 成立）→ engine-host 归档社区实验路线（§608 预设路径），主线无阻塞 |
| 4 | doctor 同名告警可复现 | ✅ | 5 检查项 PASS/WARN + 网络审计报告（静态扫描 network call sites 清单——零遥测物证）；doctor 自身即首个审计命中（可复现） |
| 5 | ACP 移 Phase 3 | ✅ 按方案执行 | 未实现，§15.2 口径 |

**结论：server 侧与 doctor 达成；三客户端实测为 Phase 2 人工项（非代码阻塞），
engine-host 按预设路径归档——M5 出口以"代码面完成 + 人工实测项移交"关闭。**

## 2. 交付物清单（会话 17–18）

| Crate/组件 | 落物 | 测试 |
|---|---|---|
| openpaw-cli `--sidecar` | 五工具 MCP server（协议层全走 rmcp 1.8；HTTP 手写薄壳 + 自实现 Transport——D-12） | 单测 9 + JSON-RPC e2e 2 |
| openpaw-cli 轻子命令 | doctor / goal new·status·resume / config show·doctor / model list·default / tool list / completions（M5b 占位）/ init | 运行冒烟全过 |
| openpaw-cli help 快照 | 12 子命令 --help 子串断言（规避 clap 版本漂移） | 4 例 |
| xtask `telemetry` 门（Q3） | Cargo.lock 274 依赖特征扫描 + 188 源文件符号扫描 + docs/golden 豁免 + inline marker | 4 例 |
| xtask `smoke-offline` 门（Q4） | build → hello（离线 echo）→ doctor 网络审计在场 → evolve status 装载，四步 PASS | 双次运行全绿 |
| CI 矩阵 | ci.yml + Q3 telemetry 步骤；nightly.yml + three-fives（Q8 冷构建计时 <5min 断言）+ smoke-offline job；release.yml M6b 占位注释 | 结构审阅 |
| **性能修复** | **scrub.rs 每行循环重建正则的 O(n·rules) 缺陷修复**（规则提升循环外）——扫描从十几分钟级降至秒级 | 四门无回归 |

## 3. 关键裁决

| # | 裁决 | 理由 |
|---|------|------|
| **ADR-001** | engine-host 归档社区实验路线（E8 不通过） | 引擎替换面公开规范不存在（证据见 ADR）；§608 预设路径，重启条件记录在案 |
| D-12 | sidecar HTTP 手写薄壳（协议层全走 rmcp） | axum/hyper 不在依赖 + rmcp SinkStreamTransport 与 tokio sender 不兼容；R9 薄封装精神 |
| — | rmcp 工具宏→手写 ServerHandler | 宏需 schemars 直依赖；tool schema 静态 JSON（协议语义不变） |
| — | `sidecar status/stop` 为信息性输出 | 方案未定守护形态；Lite 口径无 daemon（红线注释） |

## 4. 工程纪律（质量门终态，2026-08-30 会话 18 时点）

| 门 | 结果 |
|----|------|
| cargo test --workspace | **53 个测试目标全绿**（cli 双 feature 面：default + sidecar 各自全绿） |
| clippy `-D warnings`（双面）/ fmt / Q6 golden / Q7 snapshot(11 traits, 58 types) | ✅ 全绿 |
| Q10 scrub | ✅ **235 files clean（秒级）** |
| Q1 行数 | 业务 **24,850** / 50,000（sidecar 单列 847/2,000） |
| **Q3 telemetry 门（新）** | ✅ 274 deps + 188 files clean |
| **Q4 smoke-offline 门（新）** | ✅ 四步 PASS |

## 5. 移交（M6）

1. **三客户端接线教程实测**：Phase 2 人工项——server 侧就绪，教程模板随 M6 文档
   批次；HTTP 壳 SSE 互操作实测待验证；
2. **M6 剩余**：cold-build 三平台计时 CI 实跑（nightly 已就位）、上手成功率脚本、
   release.yml 实现（Sigstore + 可复现构建）、1.0 冻结三件套（设计方案 §7.8）；
3. **风险观察项**：openpaw-memory 接近 cap；embed feature 空壳；HTTP 壳 SSE 互操作。
