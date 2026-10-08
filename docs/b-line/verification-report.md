# B 线验证报告

> 历史验证快照：本文件记录早期脚本和缺陷证据，不代表当前工作树的最终验收。当前候选证据以 `../../ai-office-react-main/docs/04-验收记录.md` 为准；旧失败结论仅用于追溯缺陷来源。

> 当前审计日期：2026-09-18
> 当前裁决：**历史报告，不作为当前候选发布裁决。** 下列旧验收路径和历史绿测只作背景；当前候选状态以 AI Office `docs/04-验收记录.md` 为准。本文保留早期 SSE、回合/工具安全和可靠动画审查结论，不能用其中的“当前候选”措辞覆盖 2026-09-18 工作树事实。

本报告由 `chenningbo/kunlun/run-acceptance.sh` 和 `run-acceptance.ps1` 生成或
更新，记录实际日期、源码路径、命令输出和退出码。由于所提供源码目录不包含
Git 元数据，报告不宣称任何 branch 或 commit。

报告必须包括失败项、因外部 key 跳过的检查，以及 OpenWork T7/T9/M4 明确的
范围外状态。

## 当前证据

旧的跨项目报告维护在 `chenningbo/kunlun` 验收目录中。路径、源码和测试覆盖已经变化，这些报告不是当前候选的权威通过证据：

- `../../../chenningbo/kunlun/verification-report.md`：最终 Ubuntu/WSL 运行记录；
  所有记录的命令退出码均为 `0`，包括 Rust 格式检查、engine 与 host 测试、
  sidecar 测试、engine-host 构建与 Clippy、5000 行预算、无 `axum` 的 feature-off
  依赖树、npm/Vitest/Playwright，以及 OpenPaw 到 AI Office 的联调探针。
- `../../../chenningbo/kunlun/verification-report-windows.md`：最终 Windows
  PowerShell Core 运行记录；所有已执行命令退出码均为 `0`。由于 MSVC toolchain
  缺少对应 component，Windows Clippy 被明确记录为未执行；由于 Playwright 使用
  已安装的系统 Chrome，Chromium 下载被明确跳过。

历史联调探针记录了四个通过场景：echo/session/office 降级行为、manual approval 与
tools、stream abort（包括同一 session 返回 `409` 和跨 session 并行），以及
Basic Authentication。外部 Anthropic/OpenAI 调用有意不执行。OpenWork
T7/T9/M4、managed spawn 和 OpenCode v2 仍不在范围内，不宣称已通过。源码目录
不含 Git 元数据，因此不虚构 branch、commit 或 PR 标识。

## 当前增量证据

- AI Office 对话中心首次进入时会自动选中最新会话，并通过网关读取用户与王明的正文，
  不再把已有会话误显示为“暂无消息”。
- 快捷工具的语音入口会明确显示“语音输入：不支持”；浏览器弹窗被阻止时会显示中文
  状态，避免入口静默失败。
- 旧记录中的前端 Vitest 29 项、Playwright 2 项和四组联调只覆盖早期范围，不能替代当前多会话、长回复、恢复、压力和动画因果测试。
- 所有新增验证报告、演示日志和错误提示均使用中文；仅保留 HTTP 路径、命令、包名和
  provider/model 标识等必要技术原文。
- CHG-2026-0019 的 Session mutation 增量使用隔离容器验证，未替换或重启现有服务。
  Host HTTP 合同矩阵为 32/32；OpenWork direct Host 与 `bootAttachedWorld` 代理 journey
  为 8/8、零 skip。`pnpm typecheck` 退出 0；`pnpm evals:typecheck` 因缓存镜像中的既有
  Bun/enterprise schema/NodeNext/Node 22 类型环境问题退出 2，且没有错误指向本次测试文件。
- Rust 格式、默认依赖树和 engine-host CLI 构建退出 0；Host 全量 92/92。`xtask lines`
  退出 1：Host 6156/5700、Host tests 2707/1900，aux 10008/10100。严格 Clippy
  退出 101，原因是 `openpaw-sandbox` 11 条既有 warning。五项治理命令均因缺少固定版本
  `AI_GOVERNANCE_FRAMEWORK` 退出 1。
- CHG-2026-0020 将 REST 历史中的纯 ToolResult user 消息折叠回 assistant tool part，
  修复 Revert 最终 assistant 后 OpenWork 误报无结果。Windows 定向测试因缺少 MSVC `link.exe`
  未执行；Docker Linux 定向测试 1/1、Host 全量 92/92 通过，格式检查退出 0。
- CHG-2026-0021 将可由 `TurnOutcome` 可靠关联的 assistant Revert 边界规范化为同回合 user
  游标，使输入、输出和尾部按完整回合隐藏。Windows 测试仍因缺少 MSVC `link.exe` 未执行；
  Docker Linux 定向测试 3/3、HTTP 33/33、Host 全包 93/93 通过，格式检查退出 0。
  OpenWork 使用干净 `main` `3e58d95`，与 `origin/main` 为 0/0；从两仓源码重建并强制重建后，
  `engine-host` 和 `openwork` 均健康。容器内 REST 实测 assistant 请求返回 user 游标，页面点击
  Revert 后该轮输入与输出同时隐藏，Restore 无需刷新即恢复。最终边界复核确认 staged REST
  snapshot 始终保留原始消息、已隐藏消息再次 Revert 仍返回 400，Restore 只清除游标；最新
  `engine-host` 镜像已再次构建并健康启动，隔离 smoke 会话已删除。后续低风险去重统一了
  staged 可见前缀和 committed turn 状态裁剪，并移除冷启动第三份临时消息克隆；同源 Linux
  Docker 全量仍为 93/93。`xtask lines` 显示 Host 从 6278 降至 6267、Host tests 从 2862
  降至 2859；门禁仍因 6267/5700 与 2859/1900 退出 1，本轮未调整预算。五项治理命令
  在 `general-tools:latest` 中复跑仍全部退出 1，镜像内缺少 `/workspace/tools/pipeline.py`。

## 当前强证据与失败证据

| 类别 | 当前证据 | 判定 |
| --- | --- | --- |
| LLM mock contract | `mock_server_contract` 为 `8/8` | 局部通过；不覆盖 UTF-8 字节边界、截断流、终止 sentinel 和缓冲上限 |
| LLM SSE 单元 | `sse::tests` 为 `3/3` | 局部通过；ASCII 拆分不能替代中文/emoji 和畸形流矩阵 |
| Host reliability/runtime/HTTP | `7/7`、`10/10`、`15/15` | 局部通过；不覆盖冷加载隔离、全部孤儿恢复和真实 listener shutdown |
| Session archive/fork/revert | PATCH/fork/revert/unrevert、Basic Auth、SSE 包络、并发、ToolResult 折叠、assistant 回合规范化、完整 staged snapshot、已隐藏边界 400 和重启 HTTP 33/33；Host 全包 93/93；OpenWork 既有 8/8 | Host 自动化通过；CHG-0021 使用未修改 OpenWork 主线完成 REST 与页面 Revert/Restore 无刷新复验；全仓 `evals:typecheck` 仍受既有环境基线阻断 |
| 50K 助手正文 | provider 长度 50000；Host/JSONL 长度 50004，哈希不同并出现 `U+FFFD` | 失败 |
| 180K 助手正文 | JSONL assistant 长度 180004，替换字符 7 个 | 失败 |
| 动画可靠性 | v1 动作无 session/turn/eventId/sequence，MPSC `try_send` 和 HTTP POST 可丢 | 失败/未实现 |
| T2/T6 | 固定 JSON fixtures 缺失；help snapshots 未覆盖三个 Host 旗标 | 未通过 |
| T10 | 第 5 号角色终态语义与根计划冲突 | 待人工决策 |
| 治理结构 | 2026-09-17 index/validate 和 2 个现有 Schema contracts 通过；索引共 13 个 CHG | 机器结构通过，不等于功能或人工审批通过 |
| 公共中文注释 | `crates` 报告 902 个源码缺口；关键范围 10 个缺口均在 `openpaw-llm/src/sse.rs` | 未通过 |

50K/180K 根因定位到 `crates/openpaw-llm/src/sse.rs` 对每个 HTTP byte chunk 独立执行 `String::from_utf8_lossy`。严格终止缺口还包括 `[DONE]` 被转换为 `null`、EOF 未完成 decoder、Anthropic 缺 `message_stop` 仍可 finalize 和无界缓冲。修复设计见 CHG-2026-0013 B10/B11；可靠动画生产端设计见 CHG-2026-0014；Host 冷加载和关闭设计见 CHG-2026-0015。

## G01-G08 当前验证结论

本轮只完成 draft 设计归档和追踪更新，没有运行重型功能、压力或故障注入测试。下表所有项目统一为 **待批准 / 待实施 / 阻断**；局部 `8/8`、`3/3`、`7/7`、`10/10`、`15/15` 不能替代对应门禁。

| 项目 | 已确认缺口或风险 | 归属与必须完成的证据 | 当前状态 |
| --- | --- | --- | --- |
| G01 请求幂等 | `prompt_async` 重试可创建第二 turn，并可能重复 user、Provider、权限和工具副作用 | CHG-2026-0013 B12/§13.1；TEST-020 首次、重放、冲突、跨 session、响应丢失和重启矩阵 | 待批准 / 待实施 / 阻断 |
| G02 SSE gap/reconnect | broadcast lag 当前可被静默过滤，客户端无法判断本地投影已缺事件 | CHG-2026-0013 B13/§13.2；TEST-021 session/global gap、主动断开、2 秒 REST 恢复和资源回落 | 待批准 / 待实施 / 阻断 |
| G03 load 错误分类 | 损坏、无权限和暂时不可用可能被折叠为普通 404，半构造和诊断边界不完整 | CHG-2026-0015 §13.1；TEST-012 验证四类错误、原文件保护、脱敏和其它 session 隔离 | 待批准 / 待实施 / 阻断 |
| G04 `taskkill /F` 恢复 | 当前丢弃 Router/AppState 的测试不等价于真实进程在持久化临界点被强杀 | CHG-2026-0015 §13.2；TEST-013 五阶段各至少 20 次，并与 CHG-2026-0013 B04/G01 联合 | 待批准 / 待实施 / 阻断 |
| G05 shutdown 覆盖 | 无证据证明 SSE、同步 `/message`、半上传、approval/tool 和首次/二次 Ctrl+C 都能按 5 秒边界收敛 | CHG-2026-0015 §13.3；TEST-014 验证 listener、连接、任务、终态、端口重绑和强制退出 | 待批准 / 待实施 / 阻断 |
| G06 并发与公平性 | 基础 4/32/FIFO 路径不能证明取消晋升唯一性、30 分钟稳定性或关闭时 permit/registry 全归零 | CHG-2026-0013 B14/§13.3 与 CHG-2026-0015 §13.4；TEST-022/016 同候选联合执行 | 待批准 / 待实施 / 阻断 |
| G07 组合长回复 | 既有 fixture 不覆盖复杂 Markdown、代码、多 block、组合字符、reasoning/text 和 tool arguments 组合 | CHG-2026-0013 B15/§13.4；TEST-023 两种 Provider、确定性切块、50K/180K 逐层哈希和唯一 terminal | 待批准 / 待实施 / 阻断 |
| G08 资源预算 | 请求、累计 Provider/Host 流、事件数以及 session 文件/行/消息/冷加载内存缺完整上限与超限收敛证据 | CHG-2026-0013 B16/§13.5 与 CHG-2026-0015 §13.5；TEST-024/015 低于、等于、超过边界矩阵 | 待批准 / 待实施 / 阻断 |

G01/G02 可能新增公共请求字段、响应头、JSONL 幂等事实和 SSE 控制事件；G03/G05/G08 可能新增稳定错误码。对应 Schema 当前尚未实施或批准。若实现必须修改公共 wire、`openpaw-core`、`openpaw-store` 或 CHG 主允许表外文件，必须先扩展 draft、补负责人和兼容/回滚设计并重新人工审批。

## 当前未完成项

- CHG-2026-0013 的 B01-B16 均未获得人工批准和源码实施；B12-B16 对应 G01、G02、G06 稳态、G07 和 G08 流预算，全部保持阻断。
- CHG-2026-0015 的主合同及 G03/G04/G05/G06 关闭边界/G08 session 历史预算附录均未获人工批准和实施。
- 协调动画变更 `XCHG-2026-ANIMATION-001` 已收敛为 OpenPaw CHG-2026-0014 单一主合同，但仍是 draft；没有 v2 Schema、outbox、journal、ACK、cursor、snapshot 或自动化证据。
- 固定 fixtures、Host help 旗标、永不返回 future 取消、并行工具最终输入校验、fsync 临界故障和 1000 次竞态压力未执行。
- OpenWork attached Session mutation journey 已完成；完整三方联调、受保护分支、真实 CODEOWNER、PR/Required Checks 和回滚演练仍未完成。
- 全仓公共中文注释迁移未完成；B10 修改 `sse.rs` 时必须按框架栏目补齐其公共声明 Rustdoc。

最终裁决：**未通过，不建议发布。** 任何历史“所有命令退出 0”只适用于当时列出的命令和代码范围，不能覆盖当前确认缺陷，也不能把 T10-T12/M5 或整个 master-plan 标记完成。
