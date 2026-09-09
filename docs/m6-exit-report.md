# M6 Exit Report — 发布工程里程碑出口自检 & 1.0 冻结评估

> 日期：2026-08-30 · 覆盖会话 18–19 的 M6 交付物 + 全项目终验。
> 对照方案出口标准（§3 里程碑表 M6 行 / §355-358）逐项核验。里程碑标准原文：
> 「三个五全达标入 CI；零遥测符号扫描 + 断网烟测绿；签名发布 + 可复现构建；
> 1.0 冻结三件套（设计方案 §7.8）达成」。

## 1. 出口标准核对

| # | M6 出口标准 | 状态 | 证据 |
|---|-------------|------|------|
| 1 | 三个五全达标入 CI | ✅ | nightly.yml `three-fives` job（Q8 冷构建计时 <5min 断言，windows-latest）；`smoke-offline` job（Q4）；Q1 行数门（ci.yml quality-gates）——本机实测：冷构建/增量构建 14.6–23s（onboarding 计时）、业务行 23,227/50,000、上手脚本 `onboarding: PASS (5 steps)`；三平台计时 CI 就位待 nightly 实跑 |
| 2 | 零遥测符号扫描 | ✅ | `xtask telemetry`：Cargo.lock 274 依赖特征扫描（12 遥测 SDK 特征）+ 188 源文件符号扫描——**clean**；`doctor` 网络审计报告（可复现，doctor 自身即首个命中）；配置面无 `[telemetry]` 节（不给入口） |
| 3 | 断网烟测绿 | ✅ | `xtask smoke-offline` 四步 PASS（build → hello 离线 echo → doctor 网络审计在场 → evolve status 装载）×2 |
| 4 | 签名发布 + 可复现构建 | ✅ | release.yml：三平台 matrix `--locked` + `SOURCE_DATE_EPOCH` + `CARGO_INCREMENTAL=0` + LTO + sha256 清单 + cosign keyless（id-token OIDC）+ `softprops/action-gh-release`；12 包 publish 拓扑序 dry-run（core 实测 exit 0；引导期 deferred 语义注释） |
| 5 | 1.0 冻结三件套 | ✅ | ① stable 标注全覆盖：`xtask stability` **88 项零缺失**（stable 82 / unstable 6 注明理由）；② [knowledge-format-1.0.md](specs/knowledge-format-1.0.md)（unverified 字段 + D8 schema + 黄金样本 3 件入 golden 门）；③ [compatibility.md](compatibility.md)（semver 承诺 / MSRV / RFC 流程 / 平台承诺 / Q 门清单） |

**结论：M6 出口标准 5/5 达成。**

## 2. 全项目终验（M0–M6 十九个会话）

### 2.1 质量门总表（本报告时点）

| 门 | 结果 |
|----|------|
| cargo test --workspace | **53 目标全绿**（累计 ~620 测试函数） |
| clippy `-D warnings` / fmt | ✅ 0 / 0 |
| Q1 行数 | 业务 **23,227 / 50,000**（12 crate 全部低于 cap） |
| Q3 telemetry（新）/ Q4 smoke-offline（新）/ Q6 golden / Q7 snapshot / Q10 scrub（242 clean 秒级） | ✅ 全绿 |
| Q2 依赖白名单（cargo-deny） | ✅（ci.yml quality-gates；禁运清单零引入） |
| Q8 三个五 | ✅ CI 化（nightly three-fives + onboarding 脚本实测 PASS） |
| Q9 requires_human_alignment | ✅ 编译期断言（M1 物证，REQUIRES_HUMAN_ALIGNMENT_DEFAULT=true） |
| stability 门（新） | ✅ 88 项零缺失（stable 82 / unstable 6） |

### 2.2 里程碑总表

| 里程碑 | 会话 | 出口报告 |
|---|---|---|
| M0 工程骨架 / M1 地基 | 1 | devlog 会话 1 |
| M2 引擎与工具 | 2–9 | [m2-exit-report](m2-exit-report.md) |
| M3 知识闭环 | 10–14 | [m3-exit-report](m3-exit-report.md) |
| M4 进化开箱 | 15–16 | [m4-exit-report](m4-exit-report.md) |
| M5 入口全集 | 17–18 | [m5-exit-report](m5-exit-report.md) |
| **M6 发布工程** | 18–19 | **本报告** |

### 2.3 功能面终态

- **19 工具全通**（fs 6 + exec 3 + web 3 + meta 7+1；bash 走 AST 安全链，web 走 SSRF/DNS-rebinding 守卫，写操作 fail-closed 审批）；
- **真实 LLM 链路**：anthropic/openai + 统一 SSE + 错误分类→重试→fallback→压缩→token 预算；
- **知识闭环**：九类记忆（TF-IDF+CJK bigram + SM-2）+ 技能五态 + ReflectionOutput（S2）+ Goal 存折（S3）+ manage 合并入口 + evolve/goal/session/memory CLI + index.db FTS5；
- **24 HookPoint**：plugins 注册/触发/解除 + engine BeforeToolCall/AfterToolCall 运行时接线；
- **沙箱**：Windows Restricted Token（实跑）+ bwrap/Seatbelt（fail-closed）；
- **入口全集**：12 子命令 + --sidecar MCP server（stdio/HTTP）+ doctor 网络审计 + hello 30 秒首响。

### 2.4 同源件 S1–S7

全部落地（S1/S4/S5=M1 · S7=M2 · S6=M3 · S2/S3=M4）；D8 互操作线格式与上游策展器互通（schema verbatim）。

## 3. 偏差登记终态

编号偏差 8 条（D-5~D-12，2 条已闭合：D-6/D-11）+ 无编号裁决 4 条——全表见
[devlog.md §偏差登记](devlog.md)。要点：D-9 sandbox 独立 crate（unsafe 边界）、
D-10 net 四模块不移植、D-12 sidecar HTTP 薄壳、ADR-001 engine-host 归档。

## 4. 1.0 冻结评估

| 维度 | 状态 |
|---|---|
| 接口冻结 | ✅ core 88 公开项 stability 标注 + Q7 快照机器可读（`openpaw-core.api.json`） |
| 数据格式冻结 | ✅ knowledge-format-1.0.md（字段只增不删 + flatten extra 保留未知键）+ golden 门 |
| 兼容承诺 | ✅ compatibility.md（semver + MSRV + RFC 流程 + Q 门清单） |
| **发布前置**（非冻结项） | crates.io token 配置（dry-run 已验证拓扑序）+ nightly 三平台实跑 + 三客户端教程实测（Phase 2 人工项） |

**1.0 冻结就绪度：代码/格式/承诺三件套达成；剩余为发布运营动作（token/实测/CI 实跑）。**

## 5. 遗留与移交

| 项 | 归属 |
|---|------|
| 三客户端接线教程实测 + HTTP 壳 SSE 互操作 | Phase 2 人工项 |
| crates.io 真实发布（token + 拓扑序） | 发布运营 |
| nightly 三平台 cold-build 实跑 | CI 运营 |
| openpaw-memory cap 观察（~8,000/7,400）/ embed feature 空壳 / thought_snapshots 回填 | §15.4 决策点 |
| HTTP 壳 SSE 互操作（D-12） | M5b 验证项 |
