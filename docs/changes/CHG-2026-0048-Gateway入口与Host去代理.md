# CHG-2026-0048：Gateway 入口与 Host 去代理

status=approved / 已本地实施、功能回归通过，并按用户后续授权于 2026-10-23 切换本机测试预览；本次按用户要求保存提交并推送 feature/adjust-v2，完整发布门禁未全绿。用户要求 Host 不做代理，并在范围及测试方案提出后明确选择“可以，走不鉴权渠道，不需要鉴权”。本授权实施无 Basic 的更小方案：main 不修改，保留 Host 接口、原版 Gateway 和隔离全链路测试入口。L3 CODEOWNER 合并审查另行保留。

## 原因与目标

现有 Host 将 main 页面、静态资源、SSE、UI 反向代理回 Gateway。新目标是浏览器加载 main 前端并同源访问 Gateway，聊天由 Gateway 访问 OpenPaw 后端；Host 只承担业务接口、runtime 和 Office 发布/领取适配。

原版 main 已有无凭据聊天转发。按用户选择，Host 进程不配置 Basic，页面也不新增 Basic 登录；Host 通过已有 offices.initialize.chat 注册地址即可复用原版 Gateway。后台消息原 Bearer 与模型 API key 不属于本次关闭范围，仍保留。无鉴权接口仅映射本机，不开放公网；AI PC 桌面桥接登记继续按既有规则要求认证，因此该模式不提供此能力。

## 允许修改文件

路径相对 office2 根目录；当前授权范围如下。CHG-0047 的未提交修改与用户已有 README 修改独立保留。

| 文件 | 必要性与影响 |
| --- | --- |
| repos/openpaw/crates/openpaw-engine-host/src/http.rs | 删除 main_proxy 及页面/资源/SSE/UI 代理路由，调整现有代理测试为不代理验收；保留聊天、旧创建接口及 CHG-0047 未提交变更。 |
| repos/openpaw/crates/openpaw-engine-host/src/office.rs | 读取服务端聊天地址，在初始化时注册；首轮及 Gateway 状态丢失后恢复配置，保留既有创建互斥、动作投影及全部持久格式。 |
| compose.yaml | 15180 从 Host 移到 ai-office-main；移除 Host Basic 环境变量并以 env -u 防止镜像继承，配置 Gateway 可达的聊天地址；保留 18789 本机管理入口及其他挂载。 |
| deploy/engine-health.sh | 同时兼容无 Basic 和成对 Basic，避免 set -u 读取缺失变量导致健康检查失败。 |
| tests/gateway-topology.test.mjs | 新增隔离进程跨仓库验收，通过实际 Gateway/Host 接口验证新链路，无生产数据挂载。 |
| README.md、docs/preview-environment.md | 更新候选启动入口与配置说明，明确源码配置变化尚未部署，不改写历史验收。 |
| build/ai-office-main/Dockerfile（2026-10-23 重启授权补充） | 当前原版 main 引用 contracts 等新增 workspace 包，运行镜像必须复制完整 packages；去除失效旧聊天配置并加入真实 Gateway 导入检查。不改 main 源码。 |
| artifacts/gateway-entry-20261023/preview-restart/（构建/部署证据） | 保存候选及回滚镜像配置、数据备份和部署验证脚本/结果；无秘密的镜像 override 用于重启时精确选择制品。 |
| .scratch/gateway-entry/spec.md | 记录用户请求、确认范围与测试入口、实际结果。 |
| repos/openpaw/docs/changes/CHG-2026-0048-Gateway入口与Host去代理.md、repos/openpaw/docs/changes/INDEX.md | 登记授权、设计、实际验证、回滚和索引。 |

## 接口与实现

- 删除 Host `/office`、`/office/*`、`/assets/*`、`/farm-assets/*`、`/brand/*`、`/characters/*`、`/pawoffice-config.js`、`/api/pawoffice/events`、`/api/pawoffice/reply`、`/api/pawoffice/ui` 代理职责。鉴权通过后这些路径不再由 Host 处理；未经认证的请求仍按原认证层拒绝。
- 保留 Host `/api/pawoffice/chat`、旧 `/api/chat`、`/api/pawoffice/workspaces` 及全部 session/OpenWork 接口，沿用现有可选 Basic 机制；当前 Compose 不配置 Basic。
- Gateway 浏览器入口沿用原 `/api/pawoffice/chat`、UI、reply、events 及静态文件合同，不加 Basic；后端 `/api/pawoffice/messages` 继续要求 Bearer，main 工作树保持远端原版。
- `OPENPAW_MAIN_CHAT_URL` 为 Gateway 可达的 Host 业务地址，Compose 配置为 `http://engine-host:8788/api/pawoffice/chat`；Host 初始化时通过原 chat.url 字段注册，不接收浏览器上游配置。地址仅允许无身份信息及片段的 HTTP(S)，错误时拒绝周期目录初始化并发出不含地址的 tracing 告警；原投影和模型回合继续。未设置/空白时不覆盖 Gateway 原配置。
- 创建、只读员工详情及审批保持原领取/回复消息合同，不改为 Host 代理浏览器请求；会话隔离、执行/取消、历史和重启恢复不变。

## 禁止修改

不改 Core、Engine、Store、runtime、protocol、events、approval、Cargo/lock、CI 或任何 main 源码/测试：新问题是入口与传输适配，不需要改变模型算法、持久格式、权限决策和前端展示。Host 原鉴权代码保留，仍允许其他部署选择成对 Basic。Office feature/OpenWork 相关客户端配置与独立认证保持；即便继续发送旧 Basic，匿名 Host 也不依赖该头。

保留用户已修改的 Host `src/README.md`（SHA256 `FBF1CCBF2AEE82713916D08FA5B257E0B159779C22EE5D2DE6D423E868056925`）。不改 `.env`，不输出/提交秘密，不新建长期代理进程，不重启或替换当前预览/调试服务，不提交、不推送、不部署。

## 替代方案

仅换端口但保留 Host Basic 会导致聊天失败；给 main 加 Basic 适配会改变用户要求保留的原版源码。用户明确选择无鉴权，接受仅本机/容器内网可达与 AI PC 登记不可用的取舍；因此选择原版 Gateway + Host 既有匿名模式，不新增认证功能或长期反向代理。

## 验证与回滚

测试入口详见工作区 `.scratch/gateway-entry/spec.md`，已按 tdd 完成去代理及聊天登记红绿。Host 包和 CLI/Host parity、Gateway 既有测试、跨进程隔离验收、fmt/严格 Clippy/默认依赖树/xtask 和 Compose 无秘密配置校验按实际结果记录；现有全仓 Clippy、lines、scrub 基线失败不得冒充通过。

回滚仅撤销本次逐文件差异和端口/配置变更，不覆盖 CHG-0047 或用户已有修改，不删除办公室、工作目录、会话历史或镜像。本次不部署，因此不执行线上流量切换。

## 验证记录

验证日期为 2026-10-23；Host 基于 `aa267bb55b0f9b66bb9834c9dda029dca3ba4615`，保留 CHG-0047 未提交差异。main HEAD 与 origin/main 均为 `d150b96ff0fff59ece869da7e3dc6067c1b066e3`，工作树干净。用户 Host README 的 SHA256 仍为上述值。当前源码及 Compose 尚未部署；本次没有提交、推送、改 `.env`、重启服务或修改用户数据。

以下证据均在 office2 根目录 `artifacts/gateway-entry-20261023/`。Rust 测试使用已有 `office2-engine-host:baseline-1d711e7`，检查使用已有 `mcr.microsoft.com/devcontainers/rust:1-1-bookworm`，Rust 1.88.0，`--pull never --network none`；源码只读挂载 `/src`，离线 registry 与 target 复用既有卷，`OPENPAW_ENGINE_PERMISSIONS=auto`。工作区测试将 parity fixtures 和 tools target 输出挂载为临时内存目录，避免向只读源码写夹具。脚本 `rust-tests.sh`、`checks.sh` 保留完整 Cargo 参数和退出码。

| 检查 | 退出码与结果 | 日志 |
| --- | --- | --- |
| Host HTTP 去代理用例 `main_gateway_browser_routes_are_not_proxied_and_business_can_be_anonymous` | 红 101：旧 `/office` 返回 200；绿 0：列出的页面/资源/UI/SSE 路径均 404、不转发，匿名健康 200 | red-no-proxy.log、green-no-proxy.log |
| 实际 Gateway → 实际 Host 聊天登记 | 红 1：未登记时 `503 CHAT_NOT_CONFIGURED`；实现后绿 0 | red-registration.log、green-integration-final.log |
| 根目录 `tests/gateway-topology.test.mjs` 最终矩阵 | 0；7 passed、0 failed、0 skipped。匿名创建真实目录且无模型调用；实际受控 OpenAI SSE 回复中文；历史、完整/活动恢复、显式停止、只读详情；Gateway 重启后登记恢复；未设/空白配置保留旧聊天地址；身份信息/片段/非 HTTP 地址拒绝初始化但 Echo 业务仍执行 | integration-verified.log |
| 原版 main 既有 `officeChatServer`、`officeCreateIntegration`、`officeIntegrationServer`、`officeProtocolRecovery` | 0；39 passed、0 failed、0 skipped；无凭据聊天及跨源校验等沿用原实现 | gateway-original-tests-final.log |
| `cargo build -p openpaw-cli --features engine-host --offline --locked` | 0；冻结当前 Host 二进制后执行最终集成测试 | build-final.log |
| `cargo test --workspace --features engine-host --no-fail-fast --offline --locked -- --test-threads=1` | 0；1183 passed、0 failed、3 项既有 ignored；含 Host 197 项、CLI/Host parity 3 项 | workspace-host.log、test-results.tsv |
| `cargo test --workspace --no-fail-fast --offline --locked -- --test-threads=1` | 0；982 passed、0 failed、2 项既有 ignored | workspace-default.log、test-results.tsv |
| Host 严格 Clippy，`--all-targets --no-deps -- -D warnings` | 0；Host 自身通过，不代表所有依赖通过 | clippy-host.log |
| 两种工作区严格 Clippy，默认与 `--features engine-host` | 均 101；未修改的 sandbox/memory 既有格式及常量断言告警，未越界修复 | clippy-workspace-default.log、clippy-workspace-host.log |
| `cargo run -p xtask -- lines` | 1；Host 12916/7000、aux 12051/11500、Host tests 4780/4000。此前已超限；相对 CHG-0047 Host 业务计数减少 50，未放宽预算 | lines.log |
| `cargo run -p xtask -- scrub --check` | 1；仍为 15 处既有匹配，含用户 README，未新增匹配 | scrub.log |
| `snapshot --check`、`golden`、`telemetry` | 均 0 | snapshot.log、golden.log、telemetry.log |
| `cargo tree -p openpaw-cli --no-default-features` 及无 Axum 检查 | 均 0；默认 CLI feature 为空，Cargo/lock 未修改 | default-tree.log、check-results.tsv |
| `cargo fmt --all -- --check`、`git diff --check` | 均 0；最终差异人工核对在授权范围内，新增/修改 Rustdoc 为中文 | 本轮终端结果 |
| `docker compose config --quiet` 及仅返回 PASS 的解析断言 | 0；15180/25180 → main:3000，18789 → Host；均绑定回环，Host 无 Basic 注入且入口 env -u，聊天地址正确；未输出解析后的凭据 | 本轮终端结果 |
| 健康脚本 POSIX `sh -n` | 0 | 本轮终端结果 |
| 健康脚本匿名/成对 Basic/空 Basic 真实进程补充矩阵 | 未执行：自动审批服务返回 503（审核通道不可用），动作未获执行；不将其记为通过 | health-review-blocked.md、check-health.sh |
| cargo-deny | 未执行：当前环境无 cargo-deny 命令，本次 Cargo/lock 未改变；不代替 CI 依赖审计 | 本轮终端结果 |

Gateway 测试环境：本机原 main 的 workspace 包链接不可用且缺少 undici，故仅在 `main-copy/` 建立源码副本及隔离依赖链接；main 源码和本机 node_modules 均未修改。使用锁定的 undici 7.30.1 下载包（仅依赖准备允许外网，验收进程无外网），packages 与 server 构建成功。最终原版测试在 Windows Node 24.19.0 运行；实际 Linux Gateway→Host 测试复用已有 main 镜像的依赖（Fastify 5.12.7、@fastify/static 10.1.6、Zod 4.6.7）并装入本次原版编译输出、undici 7.30.1。`run-integration.ps1`/`.sh` 记录启动方式；临时容器不挂载预览/调试数据，静态页面验证只检查 HTTP 响应，不冒充浏览器交互验收。

冻结验收二进制 `openpaw-candidate` SHA256 为 `CB9BA3772C82DFB1794473960D1D0BEE48923B57426DA36FCC78349FE31C6EF7`。测试过程中曾被默认 feature 工作区构建覆盖共享缓存中的 CLI，导致一轮 Host 启动失败；已重新显式构建并冻结到独立文件，最终矩阵使用冻结文件通过。其他夹具修正包括补齐只读源码副本的 integration 目录、按 Host 原 snapshot 流合同断言、重启后重建连接。原始失败日志保留；CLI 未初始化 tracing subscriber，故最终从公开 HTTP 行为验证非法地址拒绝，而不将 stderr 告警作为前提。

结论：所确认入口的本地实现与自动化回归完成。main 不改源码，旧创建接口、聊天、多会话、取消/审批和员工只读详情保留；本次只改变入口与传输适配。未验浏览器点击、真实模型效果、Windows 原生 Host、健康脚本三配置补充矩阵及正式发布；全仓门禁未全绿，不能声明可以发布，CODEOWNER 合并审查另行进行。

## 用户授权本机预览切换（2026-10-23）

用户在上述本地结果交付后要求“可以重启测试服务给我看吗”，明确授权候选构建和本机 15180 预览切换。此后续授权取代先前“不重启、不部署”的本轮限制；保留其他范围约束及全仓门禁未通过的事实，不提交、不推送、不改 `.env`。核对运行目标为根 Compose 的 office2-ai-office-main-1 与 office2-engine-host-1；feature/OpenWork 继续运行，38380 调试服务不启动。

先保留旧镜像与容器身份，检查无活动回合，停止 Host 后备份其持久数据及工作文件，再以独立候选镜像及无秘密 override 启动 main/Host。部署后验页面、匿名健康、真实端口归属、聊天登记、原会话目录及静态资源；不以浏览器访问旧页代替候选制品验证。回滚恢复旧镜像、旧端口及 Basic 配置，但不覆盖更新后的数据；原配置/数据备份仅用于故障恢复。

### 本机切换结果

16:23 main/Host 均健康：main 镜像 `office2-ai-office-main:gateway-entry-d150b96-20261023`，镜像 ID `sha256:d05b9d134c727f7d5ff3d4ea2c14c62ebb3e24dbbe7ea472aff6b9941d30326d`；Host 镜像 `office2-engine-host:gateway-entry-aa267bb-20261023`，镜像 ID `sha256:fb2ae591a397e8b678d39d436415237e1894be3ca39883fc12f51a01859d08b0`，运行二进制 SHA256 `8954382d613c44d86bba5b277357c83b9f3a5d4e6cfcf7820d21f533b35866f2`。

构建、切换与只读验证脚本均退出 0。16:24 验证匿名 Host/Gateway 健康、15180 Office 页面和 24 项资源、6 个既有办公室、16 次 Gateway 历史读取、旧主/附加会话绑定、端口归属及 Host 哈希。feature/OpenWork 身份和启动时间未变；`.env` 和用户 README 未改，main 工作树干净。浏览器刷新后场景渲染完成、业务数据和消息均已连接；本次未调用模型或创建测试工作区。

停止前无活动回合，备份旧镜像、Host 数据及工作文件。精确启动与私密回退命令见根 `artifacts/gateway-entry-20261023/preview-restart/README.md`，日常启动须带 images.yaml；默认根 Compose 标签未覆盖。原始“未部署”段落为切换前验证历史。全仓门禁/真实模型等限制仍保留。

随后按用户询问只读查询远端 main，得到 `76d774a5e9039731dc04e982ea656073f2c7ac6c`；运行 main 仍为当天 13:48 拉取的 `d150b96`，未自动更新新提交。

## 用户授权提交和推送（2026-10-23）

用户在要求审查当前 Docker 运行版本后，补充“现在的版本先提交到本地，然后 push 到 github 云端”。此授权取代上文历史“不提交、不推送”限制；本次仅保存 OpenPaw 当前分支快照并推送同名分支，不创建或合并 PR，不修改服务及运行数据。提交前核对 http.rs、office.rs 和用户原有 src/README.md 的 SHA256，均与 preview-restart/host-source.json 的实际构建输入一致。

快照包括 CHG-0047/0048 创建兼容与 Gateway 去代理实现、测试、变更说明/索引，以及未改写的用户原有 Host src/README.md 调查记录。根集成工作区的 Compose、Dockerfile、部署脚本和私密备份不属于本 Git 仓库，不随本次 OpenPaw 分支推送；新拓扑的运行参数以本 CHG“接口与实现”及本机预览记录为准。沿用已有功能和门禁结果，不以保存快照代表全仓检查通过；后续继续针对本次快照审查。
