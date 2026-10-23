# OpenPaw A/B 合并与双前端联通验收报告

日期：2026-09-21。交付目录：`C:\Users\JingJia\Desktop\gitee_openpaw`。

## 结论与边界

本轮以 B 线运行时为主体，整合 A 线交付与 sidecar 兼容修复，部署了新的 `gitee-openpaw` 四服务栈。OpenWork 和 AI Office 经各自服务端连接同一个 OpenPaw Host，共享会话历史；原 JYZS 目录及旧数据卷保留。使用 DeepSeek 官方地址进行真实中文对话和文件工具调用，审批由 OpenWork 页面操作。验收不是只检查 HTTP 200，也不以 mock 代替页面。

当前发布门禁仍未全绿：严格 clippy 返回 101；两仓五类治理命令因外部框架缺失返回 2；没有代签 CODEOWNER、没有推送或 PR。可靠动画 v2 尚未实现，不能将 v1 动画测试写成 v2 交付。

最终页面、重启及故障验收结论在本报告末尾的“最终收口记录”汇总；前文保留每类检查的证据范围。

## 版本和补丁身份

| 部分 | 基线与身份 | 本轮补丁 |
| --- | --- | --- |
| 共同祖先 | main `60a531a` | 无 |
| A 线 | feature/openwork-attached-engine_host `26c741c` | 交付与兼容能力来源 |
| B 线 | feature/ai-office-attached-engine_host `f085218` | Host 运行时、Office 投影及可靠性来源 |
| 合并 Host | bugfix/openwork-session-branch-revert-archive `e1ded8a`（基于合并 Host） | SessionSummary 复用 SessionWire；真实 PATCH/fork/revert/unrevert；追加式恢复；鉴权与 SSE 契约测试 |
| OpenWork | 独立 openwork-ui `7ce6381` | 产品代码不变；attached eval 覆盖 archive、分支、回退、重启与代理路径 |
| AI Office | Gitee SSH 独立获取，detached `37a76fdf36cf44f2bb2978b286d6752c467fad59` | Gateway 监听地址配置；Docker/静态服务；E2E 画布截图断言 |
| 根编排 | compose.yaml、compose.verify.yaml、deploy、workspace | 不在三个源码仓提交中，必须随部署一起交付 |

本轮修改未提交。`artifacts/versions.json` 保存完整 HEAD、tracked diff SHA256、未跟踪补丁文件哈希清单及四个镜像 ID；三个 `.patch` 保存 tracked diff，新增文件在工作区副本中。清单排除本报告本身，避免自引用。可运行 `node deploy/capture-versions.mjs` 重新生成；根 `.env` 不参与任何身份清单。

## 合并取舍矩阵

| 模块/行为 | 裁决 | 来源和原因 |
| --- | --- | --- |
| 单一 engine-host / AgentLoop | 保留 B、整合 A 接口 | 避免运行两套引擎；Host 独占 session JSONL 写入，Gateway 不写 session |
| 回合状态、turns、接收/完成事务、孤儿恢复 | 保留 B | 保留 completed/failed/cancelled、稳定回合身份、持久重放与中断恢复 |
| requestID 幂等、同键冲突 | 保留并兼容 OpenWork | OpenWork messageID 作为用户消息 ID；缺 requestID 时复用作幂等键；重启可确认同一接收事实 |
| 多会话和生命周期 | 保留 B | 每 session 独立资源；全局并发与排队、关闭和取消可靠性不退回 A 的较小实现 |
| 工具审批/拒绝/取消 | 保留 B，修复 `always` | pending 清理、审批映射与回合终态保留；`always` 记住当前 session 的同名工具授权，不跨 session/重启；OpenWork 可处理 Office 发起的请求 |
| SSE 与 UTF-8 decoder | 保留 B 可靠性、采用统一全局路由 | `/event`、`/global/event`、`/api/event` 均全局；query 会话过滤已在 ef1a8f8 去除，属于真实兼容取舍 |
| Office action 投影 | 保留 B | A 无该模块；保留角色状态、一次 desk_visit、脱敏动作与失败隔离 |
| 回合结束角色状态 | 明确保留 B | 按用户选择全部回 idle；不同于旧团队大纲中角色保持整理的描述 |
| 动画可靠投递 v2 | 未实现 | 不具备可靠 outbox、重放去重/多消费因果保障；不是合并丢失 |
| Office 任务/侧栏/预览 | 保留独立 Office 37a76fd | 含五个摘要 API、会话中心、技能/文件预览及任务创建/取消/重试；goals 独立可写 |
| Docker / Linux 构建 | 整合 A | 采用容器交付资产及 `create_dir_all(&root)` 所有权修复；本轮新 Compose 四服务 |
| CLI provider 优先级 | 整合 A | 显式参数 > OPENPAW_ENGINE_PROVIDER > 默认；本栈显式 openai，镜像不默认强制 mock |
| sidecar Host/port 与顺序 | 整合 A | 保留环境绑定、到达顺序 FIFO、共享会话互斥与缓冲乱序响应的 E2E 夹具 |
| 会话创建与错误优先级 | 整合 A 兼容 | POST session 为 200；不存在 permission 先返回 404，保持 SDK 消费行为 |
| 会话列表字段 | 本轮修复 | 列表补 slug/time.created/time.updated；保留 createdAt/updatedAt/provider/model/active；只读扫描不创建 AgentLoop |
| 消息接收/时间投影 | 本轮修复 | GET 单消息与 SSE 使用已接受 ID；历史时间用已有消息/回合元数据，旧无元数据以 session 时间兜底；不改 Core/存储 schema |
| archive / fork / revert | 本轮补齐 | PATCH 支持 archive/unarchive；fork 返回新根会话并支持边界；revert/unrevert 暂存或恢复尾部，替换 prompt 原子提交逻辑裁剪 |
| shell / command / summarize | 保留降级 | session 范围占位 204，未实现真实行为；不能当作完整 OpenCode 能力 |
| 根路径重复写操作 | 删除 R1 | `/shell /fork /revert /summarize` 根路径 404；POST `/command` 为 405；会话变更只保留登记的 session 路由 |
| 重复审批等待封装 | 删除/整合 R2 | 删除多余异步包装，保留同步互斥、取消和 pending 清理，不复制新的权限队列 |
| provider 默认值重复 | 整合 R3 | 统一配置来源与常量，保留协议消费者所需的模型列表 |
| A 的 28 份旧类型夹具 | 未移植 | 合并采用 B 协议与测试；不能声称覆盖等价。本轮发现的列表/客户端 ID 缺口证明历史验收覆盖不完整 |

仍存在的历史重复主要是 CLI、sidecar、Host 分别构建 provider 的工厂逻辑，早于 A/B 合并；本轮不扩为大范围重构。sidecar 的 FIFO 保证进入顺序，mutex 保证共享状态串行，二者目的不同，不属于冗余。同步 message 和异步 prompt 共用调度器，是兼容接口。SessionSummary 的新旧字段也是必要兼容。`harness/verify.mjs` 是仓库 canonical 脚本的部署副本，内容一致；它是分发重复，不是第二套业务实现。

## 本轮发现并修复的问题

1. 旧 workspace 链接 `ws_2801c08e0351` 不适用于新的 OpenWork 状态卷。新卷实际 ID 为 `ws_c52ddf65534b`。更关键的是历史列表缺少 OpenWork 使用的 slug/time；此前只验证“数组/200”的测试没有发现页面问题。
2. 首次进入 OpenWork 默认选择 opencode/big-pickle，本 Host 无此 provider。增加两个可选 VITE 默认配置，部署使用 openai/deepseek-v4-pro；用户已有浏览器偏好仍优先。
3. Host 忽略客户端 messageID，OpenWork 无法对账乐观消息和接收事实。补保留 ID、SSE ID、只读单消息查询及重启幂等回归；不同 requestID 不能覆盖既有消息身份。
4. 消息 time 原为 0，刷新后显示 1970。改为读取已有消息/回合时间；历史无明确信息的消息只能使用 session 创建时间，不能伪造精确发送时刻。
5. revert 的旧 204/原 Session 响应会让页面静默 no-op。现已实现 staged revert/unrevert、替换 prompt 提交，以及完整和边界 Branch；Session 事件统一为 `{sessionID,info}`。
6. Gateway 默认只监听回环；容器显式 0.0.0.0，默认行为不变。sessions/skills 只读，任务 goals 单独可写，避免任务创建被只读挂载阻断。

取消后的旧 user 请求仍保留于历史，这是 B 线行为。真实模型曾在收到故事请求后选择补做刚取消的 read_file；Host 接收的 messageID 和正文已核对正确，并非偷偷重发。测试在同一会话明确“放弃旧任务”后完成真实流式取消。OpenWork 对没有 assistant 结果的取消回合可能显示恢复提示；引擎 idle 与 pending 清理需以状态/SSE 断言为准，不能仅凭该提示推断仍在执行。

## 部署和鉴权

| 服务 | 宿主回环端口 | 容器/职责 |
| --- | --- | --- |
| engine-host | 8789 | gitee-openpaw-engine-host-1，唯一模型和 HTTP/SSE 引擎 |
| openwork | 5178、8778 | gitee-openpaw-openwork-1，页面及 attached server |
| office-gateway | 8765 | gitee-openpaw-office-gateway-1，Office API、任务、动作队列 |
| ai-office | 5173 | gitee-openpaw-ai-office-1，37a76fd 构建的新前端 |

OpenWork：`http://localhost:5178/workspace/ws_c52ddf65534b/session`；AI Office：`http://localhost:5173`。

OpenWork 实际请求经过 `/workspace/:id/opencode/*`，server 使用容器内 Host 地址和 Basic Auth。运行进程核对未启动独立 OpenCode。Host 持久 sessions，Gateway 只读共享；任务和 OpenWork 状态各自持久。所有容器 cwd/工作区统一 `/workspace`，映射新 workspace 目录。四服务具有重启策略、健康检查；最终重启验证见收口记录。

官方 endpoint 为 `https://api.deepseek.com/v1`，provider=openai。模型预检实际仅返回 deepseek-flash/deepseek-v4-pro，未返回计划默认 deepseek-chat；已明确改用可用的 deepseek-v4-pro，没有 mock 回退。该结论来自请求结果，不以模型自述推断身份。

密钥仅在忽略的 `.env`，只注入 Host，不进入前端、报告、源码 diff 或镜像构建层。服务 token/Basic password 与模型 key 分开。Playwright trace 可能含本地服务授权头，交付前用 `deploy/sanitize-evidence.py` 脱敏；原始 `.env` 不进入证据。

构建可复现限制：OpenWork Dockerfile 基于缓存 `jyzs-openwork-dev:latest`，使用离线 pnpm。新的容器/网络/业务卷是独立的，但全新机器需先准备或导入该基础镜像及依赖缓存。现有 Node 22 环境对部分声明 Node>=24 的包发出 engine 警告，当前 Bun 启动及 attached eval 成功，不宣称通用全平台构建通过。

## 验收命令、退出码和证据范围

所有证据路径以工作区根目录为基准。业务使用真实模型；mock 栈为独立 `gitee-openpaw-verify`，无业务卷/密钥/宿主端口。Rust 命令在本轮新建 `gitee-openpaw-check-final` 容器，源码 `/src`，复用的只有 cargo 下载和构建缓存。

| 检查 | 命令/入口 | 当前结果与证据 |
| --- | --- | --- |
| Rust 格式 | cargo fmt --all -- --check | 0，artifacts/fmt.log |
| 代码行数 | cargo run -p xtask -- lines | 1；Host 6156/5700、Host tests 2707/1900 超限，aux 10008/10100 通过 |
| Host | cargo test -p openpaw-engine-host --features engine-host | 0；37 单元 +13 reliability +19 HTTP =69；host.log |
| CLI Host | cargo test -p openpaw-cli --features engine-host | 0；13，cli-host.log |
| CLI sidecar | cargo test -p openpaw-cli --features sidecar | 0；27（含3个E2E），sidecar.log |
| LLM | cargo test -p openpaw-llm | 0；26+14=40，llm.log |
| 默认 feature | cargo tree -p openpaw-cli --no-default-features | 0，tree.log 无 axum |
| release | cargo build --release --locked --features engine-host -p openpaw-cli | 0，release.log |
| 严格 clippy | cargo clippy -p openpaw-engine-host --features engine-host --all-targets -- -D warnings | 101，sandbox 11项告警，clippy.log |
| Host clippy | 上项增加 --no-deps | 101，office.rs 既有 collapsible_else_if；源码该处未改，clippy-no-deps.log |
| Office lint/build/test | deploy/check-office.sh | 0/0/0，114个测试通过；office-{lint,build,test}.log |
| Office E2E | Chromium + 原仓 Playwright | 初轮8/9；修正失效 readPixels 断言后9/9、退出0；office-e2e-final.log |
| OpenWork official attached | pnpm evals:pr specs/openpaw-engine-attach.test.ts | 0；8/8、0 skip，覆盖 direct Host 与 spawned attached server 代理 |
| mock tools/stream | node harness/verify.mjs tools / stream | 最新镜像结果见收口记录；分开运行、清空隔离动作队列 |
| 双页面真实对话/工具/取消 | deploy/stack-e2e.mjs、stack-journey.mjs | journey 11项通过、0 pageerror；stack-journey.json；取消前截图与跨端工具结果截图 |
| Office 真实任务 API | node deploy/verify-live-api.mjs | 0；五接口、创建/取消/重试完成、列表新旧字段；live-api.json |
| 面板/降级/新会话 | deploy/stack-panels.mjs | 最新结果见收口记录；不以 API 代替页面 |
| 故障与重启页面 | deploy/stack-resilience.mjs | 最新结果见收口记录 |
| 治理两仓各5项 | validate / contracts / comments / pr-check / comments-diff | 每项2；缺 AI_GOVERNANCE_FRAMEWORK 固定版本目录；governance-final.log 和逐项日志 |

此前测试失败不隐去：`stack-journey-before-identity-fix.json`、`stack-journey-model-tool-choice.json` 保留协议缺陷及模型选择旧工具的红记录；任务停止初轮未处理 confirm 对话框，`stack-panels-before-confirm.json` 保留原记录。Office 最初 readPixels 测试在合成后读取已清空的 WebGL buffer，截图能看到正常场景；修复的是断言，不是修改产品渲染。反复生成的 synthetic session/task 均是本轮验收数据，未删除。

页面证据使用独立 Docker Chromium（含中文字体），不是宿主桌面手工浏览。桌面工具返回 Codex auth token unavailable，未绕过控制用户桌面。截图包括 openwork-real-reply、openwork-manual-approval、office-tool-result、office-skill-preview、office-task-completed、openwork-streaming-before-stop；以对应 JSON 断言为通过依据。

## 团队大纲差异与剩余限制

标准只读取 JYZS `.feishu-oauth/inbox/1-工作流规范.md`、`2-团队大纲.md`，未访问飞书。CHG 先登记范围、接口、测试和回滚；用户已批准实施，不等于代签 CODEOWNER。

- B 线全部 idle 是用户明确选择；可靠动画 v2 独立后续。v1 `/actions` 读后清空、多页面消费者可能竞争，不能承诺可靠重放、多消费者零丢失或跨会话因果动画。
- aux 从10000放宽到10100是此前合并中已有例外，本轮没有再放宽；Session mutation 按批准的 CHG 将 Host/Host tests 上限分别调整为 5700/1900。
- 没有移植 A 全部旧类型夹具；现有测试不能等同于所有原分支验收。完整发布压力矩阵、v2动画万次事件与全套OpenCode能力不属于已通过项。
- Office 页面有会话和工具展示，但本版本未提供独立人工审批按钮；真实人工批准/拒绝在 OpenWork 页面操作，共享 Host pending。
- AI Office 与 OpenWork 共享 Host/session 是当前合并部署的设计；Compose 使用 `manual` 审批时，AI Office 不会绕过 Host 自动批准。AI Office 只保留审批 API 代理，当前前端未渲染独立审批控件；因此需要在 OpenWork 批准，不能把“自动分派智能体”理解成“自动批准工具”。
- shell/command/summarize、browser/calendar 等仍是明确降级/未开放；不要把双前端联通解释为 OpenCode 全功能兼容。
- 代码中的 clippy 问题未扩范围修复。治理框架缺失和人工审批未执行仍是正式发布限制。

## 停止清单和回滚

本轮启动前实际停止：jyzs-openpaw-engine-host-1、jyzs-openwork-web-1、jyzs-ai-office-1、jyzs-openwork-1、jyzs-openpaw-1。权威清单 `artifacts/old-running-containers.json`。其他旧验收/构建容器在 Docker 重启后本来就已停止，不冒充本轮主动停止。GitNexus 未被操作（当时已停止），考公容器保持运行。

新卷：gitee-openpaw_sessions、gitee-openpaw_goals、gitee-openpaw_openwork-state；旧卷没有删除、迁移或覆盖。执行根目录 `deploy/rollback.ps1` 会停止新栈并按保存清单启动旧容器。恢复新部署用 `deploy/start.ps1 -SkipBuild`；完整重建去掉 SkipBuild。不要执行 down -v。回滚过程未实际执行，以免打断交付的新栈；脚本内容已核对。

## 最终收口记录

本次后续修复已完成源码和格式检查：Host 空 body session 回归已加入，`always` session 授权回归已加入，AI Office Gateway 注释已同步。运行中的旧 Host 镜像仍可复现原始 400；由于本轮 Docker Desktop API 的自动审批通道连续返回 503，未能重建/替换容器，因此新镜像、页面点击和重启复验暂不记为通过。恢复 Docker 通道后执行 `docker compose build engine-host openwork`、`docker compose up -d --no-build --wait --wait-timeout 180`，再补录空 POST、OpenWork 新建会话和同工具二次回合的页面证据。

2026-09-24 的 Session mutation 修复在独立临时容器中验证，没有重建、停止或重启现有四服务。
Host 全量 92/92、HTTP 32/32；OpenWork attached journey 8/8、零 skip；`pnpm typecheck` 退出 0。
`pnpm evals:typecheck` 因缓存镜像既有类型环境问题退出 2，没有错误指向改动文件；该项保留为阻断，
不能写成通过。行数门禁也因 6156/5700、2707/1900 退出 1；预算必须精简或重新审批，不能写成通过。
写入 committed revert 后只能回滚到仍识别 `session_revert_committed_v1` 的 Host。
