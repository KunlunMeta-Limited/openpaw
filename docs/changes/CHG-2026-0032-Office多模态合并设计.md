# CHG-2026-0032：Office 与多模态合并设计

status=approved（实施已批准，验收进行中）。2026-10-19 用户对 Q11 回复“是”，确认按 Q1–Q10 及本文件范围实施；不代表已通过代码审查或发布。

## 用户目标与已确认事实

2026-10-19，用户调用 grill-with-docs，要求设计以下分支的合并，并将 Host 中五个独立文件收敛到其他 Host 文件。第一轮已明确内部模块、实施阶段、桌面与父子授权原则；更细的运行语义仍在访谈中。

| 来源 | 固定提交 | 主要能力 |
| --- | --- | --- |
| `feature/ai-office-attached-engine_host` | `6199a7e9f1858e3f451171e86a03115db268e854` | 子 Agent 委派、多空间动画、产物登记、预算耗尽最终总结 |
| `feature/add-multimodal-capabilities` | `e125364c066b255f6bec2775d4ca639932efb757` | 图片/音频、桌面工具、共享执行/持久化、Fork/Revert/Archive |

本轮只读查询远端确认以上提交仍为当前分支头。共同基点为 `5bafcb5adb5f39cc077ea4cdb184ca69edb4b5dd`。此前 merge-tree 预演发现 approval.rs、runtime.rs、CHG INDEX 共 3 文件、16 处文本冲突；本轮未实际合并。

技能项目初始化的四个配置及 CHG-0031 仍是工作区外待审草稿，本变更不代替该确认，也不应用初始化草稿。

## 当前允许修改范围

用户已确认下述逐文件范围及实现方式。允许完整合入固定 Office 提交的 19 个来源文件，并在下表文件内实施兼容修复和迁移；包括本 CHG 与 ADR-006。来源历史文档只保留历史语义，不覆盖多模态分支的同号记录。现有未跟踪容器构建文件不纳入本次提交。未经登记的接口、依赖或业务路径不扩大修改。

## 五个文件的调查与候选归宿

| 源文件 | 当前总行数 | 候选处理及理由 |
| --- | ---: | --- |
| `crates/openpaw-engine-host/src/message_projection.rs` | 369 | 归入 `protocol.rs` 的内部 projection 模块：消息 wire、错误过滤、附件投影共同服务 REST/SSE。放进 events.rs 是另一可行方案，但需拓展其现有 SSE 职责。 |
| `crates/openpaw-engine-host/src/input_presentation.rs` | 96 | 请求准备归 `MessageRequest` 方法，展示属性还原归同一 projection 模块；两者集中到 protocol.rs，保持 HTTP 接收、请求指纹、历史展示共用实现。 |
| `crates/openpaw-engine-host/src/session_mutation.rs` | 151 | 生产部分只是 Store re-export；删除转发层，runtime.rs 直接导入 Store。五个 Store 语义测试迁到已有 Store 测试模块，不将历史算法搬回 Host。 |
| `crates/openpaw-engine-host/src/ai_pc.rs` | 1096 | 若五个文件均必须并入现有文件，候选为 runtime.rs 内部 desktop 模块，保留登记/验证/工具执行的内部接口。 |
| `crates/openpaw-engine-host/src/ai_pc_visual.rs` | 371 | 与上项合入同一 desktop 模块，保留回合代次、取消、ACK 与媒体交付测试及内部隔离。 |

上述行数包含测试，并非 xtask 业务行计数。runtime.rs 当前 3819 行，protocol.rs 661 行；全部桌面实现迁入 runtime 后，该文件粗略会超过 5000 行。Q9 已确认采用现有文件中的私有模块方案，不新增 desktop.rs；接受这一物理文件长度代价，不放宽业务行门禁。

桌面实现不放入 office.rs：前者执行受权限控制的真实操作，后者是失败不得影响引擎结果的尽力投影。也不把带 token/client/活动状态的登记对象放进可序列化协议类型。合并物理文件不等于压平内部模块，更不会自动减少 crate 业务行预算。

## 合并策略候选

建议以多模态为基础，在独立整合分支先迁入 Office 行为并完成验证，再以独立提交实施文件收敛。Engine 保有共享执行/调度，Store 保有历史变换/事务；遵循既有 ADR-003、ADR-004、ADR-005。

父 Agent 工具候选组合：桌面工具包装 → Office 委派/产物登记 → 共享写锁的工作区工具 → ToolRegistry。新建、恢复、Fork 统一构造，避免修一处漏两处。

## 必须处理的兼容点

- delegation.rs 五处 ToolCallOutcome 构造需适配 media；LimitedProvider 必须转发逐模型能力；两层工具包装需透传 set_media_context/remember_media。
- 预算耗尽的最终总结必须取得稳定消息身份并进入完整 generated_messages 收集器，不能只追加压缩后的 session.messages。
- Fork 必须创建独立委派上下文，补齐 SessionHandleInit；桌面登记按既有规则不继承、不持久恢复。
- Office 子 Agent 当前共享父审批管理器，且只有工作区文件/命令工具。审批 always 在两个分支中的含义相反，选择记忆行为时须同时明确父子授权范围。
- 子任务完成缓存按任务指纹复用、独立持久化，当前没有主回合边界；主会话 Revert 后旧结果可能继续命中。这是静态发现，未运行复现，需设计失效策略。
- 按 Q8 保留 Office 显式 URL 开关与故障隔离，不另行解耦委派工具与 Office 开关。
- 展示属性不进入模型正文，REST/SSE 附件共用转换；回退/Fork 后只读取当前有效历史的附件。

## 允许修改文件范围（Q11 批准）

| 区域 | 候选文件及影响 |
| --- | --- |
| Host 功能整合 | `src/runtime.rs`（工具与执行组装、生命周期）、`src/delegation.rs`（Office 新增、缓存持久化及媒体适配）、`src/approval.rs`（独立子 Gate 与父子授权）、`src/office.rs`（动画/产物）、`src/events.rs`（新增引擎事件与媒体投影）、`src/protocol.rs`（Office 开关、请求/展示）、`src/lib.rs`（模块声明） |
| Host 文件迁移 | 上表五个源文件删除；`src/http.rs` 调整类型与函数路径；`src/README.md` 更新模块地图；接收文件保留原测试覆盖 |
| Engine | `crates/openpaw-engine/src/agent_loop.rs`（预算总结及生成收集）、`src/events.rs`（真实工具执行事件） |
| Store | `crates/openpaw-store/src/transcript.rs`（接收已有历史语义测试，具体落点待核查） |
| 验证 | Host `tests/http.rs`、`tests/conversation_reliability.rs`；Engine `tests/e2_wrapup.rs`、`tests/engine_e1.rs` 及两个 parity fixtures；必要的 CLI/Host parity 补充点待确定 |
| 文档 | 本 CHG、`docs/changes/INDEX.md` 的来源重号澄清与必要的既有 ADR/词汇更新；来源 Office 文档作为历史合并记录保留 |

本表 Host 的 src 路径相对 crates/openpaw-engine-host/，其余路径已注明 crate。Office 来源增量另含 .gitattributes 与历史文档，这些应随完整合并保留/审阅，不能把仅列出的手工修复文件误认为全部分支增量。

Core 公共 trait、Cargo 依赖、外部 AI Office 前端、Everypaw 原生桥、CI 暂无修改必要；不以文件收敛为由修改这些受保护范围。测试发现确有必要时，先解释影响并更新最终范围。

## 已确认的设计决定（第一轮）

- Q1：用户回复“允许”。五个独立源文件移除，接收文件允许保留私有模块与原测试分组。具体接收位置以上表为候选，尚未最终逐项确认。
- Q2：用户回复“可以”。以多模态为基线，在独立整合分支先完成并验证功能合并，再以单独提交收敛文件。
- Q3：用户回复“可以”。子 Agent 保持后台工作区文件/命令任务，桌面仅主 Agent 使用；子 Agent 不继承父桌面登记、凭据或活动回合身份。
- Q4：用户回复“是”。主会话保留 always 同工具授权记忆，但不自动扩展给子 Agent；子任务需要明确审批。子审批 Gate 的路由与自身授权寿命仍待下一轮明确。

Q3/Q4 属于会影响后续兼容与权限语义的真实权衡，已记录在 `docs/boundary-decisions/ADR-006-office-delegation-authority.md`，其中 accepted 指设计决定已确认，不代表实现或合并已批准。尚无已确定的新领域术语，因此不往 GLOSSARY.md 写入实现细节。

## 已确认的设计决定（第二轮）

用户依次回复“Q5可以 Q6可以 Q7可以 Q8可以”：

- Q5：成功暂存 Revert 时即清除本会话全部已完成的子任务结果缓存，保留逻辑子空间身份；unrevert 不恢复缓存，需要时重新执行。既有工作区文件不随历史回退删除。
- Q6：本轮保持任务文字委派，子 Agent 可以读取工作区文件，但不自动继承主会话上传的图片/音频。不新增 attachmentIDs 参数或附件委派流程。子任务通过工作区工具自行读取媒体仍遵循当前模型能力、路径与权限规则。
- Q7：每次实际子任务执行拥有独立审批范围；子任务的 always 仅在这次任务内复用，不随同名空间复用到下次委派。父审批不授权子任务，子审批也不反向授权父会话。这里的独立范围不意味着 auto 模式下无条件弹窗，仍沿用既有 manual/guardrail 策略。
- Q8：保留显式 Office URL 同时启用委派、产物登记和动画的现有行为，本轮不新增独立开关。

## 实现调查补充（服务于已确认语义）

HTTP 当前仅聚合主 session.approvals。子任务独立 Gate 需在 Host 维护活动子审批集合，列表与回复按唯一请求 ID 路由；SSE 保留父 sessionID，metadata 标注子空间名称及任务作用域。正常结束、取消、回合回滚、future drop、删除都需结束子 pending，并防止父回合取消后迟到登记。可只改 Host，无须改 Core trait 或 Engine Gate。

当前磁盘委派 v1 只保存 completed 非空记录，单纯清内存会在重启后复活旧结果；简单删除记录又会丢失空间身份。因此 Q5 的实现必须同时保证结果失效、身份保留和崩溃/重启安全。采用 Host 私有 v2 格式拆分身份与可选结果；旧 v1 结果保守视为不可复用。在既有空闲/回合锁内先验证 Revert，再可靠持久化无结果的缓存，清理内存后才追加并应用 RevertStaged。缓存失效失败则返回持久化错误，不宣告 Revert 成功；后续 journal 写失败最多损失可重算的缓存，不改变历史。启动及恢复不得通过备份回退复活已失效结果。实际原子替换及故障注入验证需在实施中落实，不能把可靠失效实现为只记录 warning。

暂存 Revert 成功后立即失效是用户已经选择的行为；不能擅自改成“仅提交替换输入时清理”或“unrevert 恢复缓存”。缓存失效本身不回滚工作区工具副作用。

## 已确认的设计决定（第三轮）

- Q9：用户回复“是”。五个文件按候选表分别归入 protocol.rs、runtime.rs，删除 Store 转发文件，测试迁至 Store，并保留私有模块；不新增 desktop.rs。
- Q10：用户回复“是应该有容器可以使用 可以看看”。本轮验收包含实际 AI Office 页面与真实桌面桥联合验证，以及 Rust 编译/回归/门禁和 Host HTTP 契约检查。真实联调不再推迟为后续交付；缺少模型或桥接条件时明确列为未完成，不把 mock 等同真实端到端验证。

Q11 已确认共享理解并批准实施。既有 HTTP 路由和请求形状保持兼容；permission 列表/回复扩展至活动子任务，SSE metadata 标注来源，sessionID 仍为父会话。缓存 v2 仅为 Host 私有持久格式。当前门禁上限保持原值；如果合并后的实测超限，报告实际差额与可行精简，不自动放宽或修改 AST 计数规则。

## 容器条件调查（2026-10-19，只读）

Docker context 为 desktop-linux；compose 项目 office-feature 的三个现有服务均运行且健康，以下 HTTP 检查均返回 200：

| 服务 | 本地入口 |
| --- | --- |
| engine-host | `http://127.0.0.1:8788/global/health` |
| ai-office | 页面 `http://127.0.0.1:5173/`；Gateway `http://127.0.0.1:8765/health` |
| openwork | `http://127.0.0.1:8787/health` |

当前 Host 使用 mock provider、manual 权限策略，已配置 Office URL；容器及本进程环境未配置 OpenAI/Anthropic API key。构建来源记录为多模态 e125364，属于合并前基线。三个服务均无源码 bind mount，修改源码不会更新其内置二进制。健康检查不代表真实模型、委派或桌面操作通过验收。

现有 named volumes 含会话、产物和共享工作区。后续建议复用构建配置及镜像层，以独立 compose project、端口和全新 volumes 验证合并结果，保留现有运行实例。现有 builder 最终 stage 是产物导出环境，不能视为已具备 Cargo 测试工具链；可使用 Dockerfile 的 Rust toolchain stage 执行完整门禁。Linux 服务容器不能替代 Windows 原生桌面桥；桌面桥可用性另行只读核查。本轮未重启、重建服务或新建测试会话，也未输出凭据。

原生桥补充调查：当前工作区及父 openpaw 目录未见 Everypaw 仓库；Desktop、Downloads 和 AppData 的已检查位置未发现 Everypaw 源码、可执行包或配对 manifest，8810 未监听。历史 CHG-0028/0029 的相邻工作区和配对验收记录不能证明本机已有可用桥。真实联调仍需提供或定位 Everypaw 可启动包/源码，并接入真实模型配置；目前不宣称这两项条件齐备。

## 验证与回滚规划

功能阶段先验证编译兼容、预算总结持久化、父子权限、桌面登记隔离、附件回看和 Fork/Revert/取消；迁移阶段针对 REST/SSE/HTTP 保持行为等价，并验证旧路径已移除。最终运行仓库要求的 fmt、严格 Clippy、workspace/Host 测试、parity 与 xtask 门禁。

xtask 上限：Host 7000、Engine 8200、Host tests 4000、aux 11500，按既有 AST 规则测量；不把合文件当作预算优化，也不默许提高上限。

本轮只有只读调查与此草案记录，无业务代码、合并提交、部署或运行测试。保留原容器构建并行工作。正式实现建议通过独立分支与分阶段提交隔离，回滚代码时保留用户会话及产物；持久化兼容仍遵守原 ADR/CHG。

## 实施记录：功能合并阶段

已创建 `integration/office-multimodal`，多模态 e125364 为代码基线。完整合入 Office 6199a7e；approval 采用共享 Engine Gate 并在 Host adapter 内聚合独立子审批；runtime 保留共享 turn_execution、完整生成消息提交与桌面外层包装。新建、恢复、Fork 共用 session_tool_host；子任务不包含桌面包装或父附件表。ToolCallOutcome / AgentEvent 媒体字段、逐模型能力及两层工具媒体上下文已适配。预算总结取得稳定消息身份并进入生成收集器。

独立 Docker Rust 1.88 验证容器已建立，使用新 target volume，不改现有三服务。初次 Docker Hub token 超时，经当前命令的本机代理重试成功；未改全局配置。`cargo check --workspace --all-targets --features engine-host --locked` 退出 0；`cargo test --workspace --features engine-host --locked -- --test-threads=1` 退出 0，包含 Host 单元 97、reliability 16、HTTP 37。原有 ignored 不作功能证据。日志在仓库外 `artifacts/office-multimodal/functional-tests.log`。

行数初测 Host 8842/7000、aux 11651/11500，退出 1；保留原预算和 AST 规则，不宣称门禁通过。严格 workspace Clippy（Rust 1.88）因未修改的 sandbox 等文件已有 lint 失败，日志另存；新增预算条件的布尔表达式按原副作用顺序规范化。最终迁移后继续验证并记录准确结果。
