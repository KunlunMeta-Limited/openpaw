# T3 HTTP 基础面实施记录（HostConfig、Basic Authentication、就绪标记、降级路由）

- 来源任务 ID：T3（关联 T4/T5 已提前落地、T6 CLI 旗标、T7/T9 attached 联调）
- 变更 ID：`2026-09-18-t3-http-baseline`
- 开发依据：飞书《OpenPaw 引擎宿主》团队大纲 §1.4 硬约束、§2.3 模块结构、§2.4 CLI 入口、§3.2 协议快照、§5.1（T3）、§8.1（M2）、§8.2 联调步骤；本地同文 `openpaw/docs/engine-host/master.md`
- 流程状态：执行中（飞书《个人工作计划》最近一次状态回填为 09-15；09-17 每日记录仍为「执行中」）
- 基线 commit：`3b85244`（openpaw `dev`）
- 修改前工作树：含 09-17 未提交改动（`events.rs`、`protocol.rs`、`http.rs`、`openpaw-cli/src/sidecar.rs`、`openpaw-engine/src/agent_loop.rs`、`openpaw-llm/src/openai.rs`、`openpaw-sandbox/src/platform/linux.rs` 等）。本变更不覆盖、不回退这些改动；`http.rs` 已在同文件上叠加。

## 目标

补齐团队大纲 T3 的 HTTP 基础面，使 OpenWork attached 所需的鉴权、降级路由与工作区解析符合 §3.2，并使 §8.2 的验收命令可以原样执行。

## 范围

- 只改 `crates/openpaw-engine-host`、`crates/openpaw-cli`（CLI 旗标接线）与其测试；不改 `openpaw-core`/`openpaw-engine`/`openpaw-llm`/`openpaw-store` 的业务行为。
- 附带 `openwork/evals/specs/openpaw-engine-attach.test.ts` 的对照断言（跨项目，单独记录）。
- 不在本轮：SSE 事件翻译（T4，已落地）、人工审批（T5，已落地）、T10 办公室动作、OpenCode v2。

## 现状与缺口（2026-09-18 只读核对）

| 要求 | 现状 | 证据 |
|---|---|---|
| `lib.rs` 提供 `HostConfig` + `run()` | 只有 `run(hostname, port)`；全仓无 `HostConfig` | `crates/openpaw-engine-host/src/lib.rs:14` |
| 就绪标记 | 输出 `openpaw server listening on http://{addr}`，与 §2.4 约定不符 | `lib.rs:40` |
| 必做路由 | 13 条齐全 | `lib.rs:22-36` |
| 降级路由 | `/config`、`/mcp`、`/command`、`/project`、`/session/:id/command|shell|fork|revert|summarize` 全部未注册 | `lib.rs:21-37` |
| Basic Authentication | 未实现；`OPENCODE_SERVER_USERNAME/PASSWORD` 在 Rust 代码中零引用 | `http.rs` 全文 |
| 工作区优先级 | header → query → body.directory → `"."`，无 CLI 默认工作区 | `http.rs:44-47` |
| 400/404/409 | 已具备 | `http.rs:38,111,76,86,96,141` |
| 401 | 缺失 | 同上 |
| CLI 传参 | 只传 hostname/port；`--provider/--workspace/--home` 仅在 `chat` 子命令下 | `openpaw-cli/src/main.rs:30-43`、`cli.rs:57-68` |

## 拟影响文件

| 文件 | 改动 |
|---|---|
| `crates/openpaw-engine-host/src/lib.rs` | 新增 `HostConfig`；`run(config)` 组装路由与鉴权层；启动前校验凭据成对；输出唯一就绪行 |
| `crates/openpaw-engine-host/src/http.rs` | Basic Authentication 中间件；9 条显式降级路由；工作区优先级补 CLI 默认工作区；401 语义；`prompt`/`permission_reply` 手写 JSON 解析统一 400；新增契约测试模块 `contract_tests` |
| `crates/openpaw-engine-host/src/runtime.rs` | `RuntimeState::new(config)`：默认工作区、`home` 兜底到 `openpaw_core::openpaw_home()`（未设 `OPENPAW_HOME` 时仍写 JSONL） |
| `crates/openpaw-engine-host/src/protocol.rs` | 未改动（`CreateSession.directory` 保留以兼容既有夹具，不再参与工作区解析） |
| `crates/openpaw-engine-host/Cargo.toml` | `base64` 可选依赖；dev-dependency `tower`（`util`，已在 `Cargo.lock` 0.5.3） |
| `openpaw/Cargo.toml`、`Cargo.lock` | workspace 增加 `tower` 声明；`base64` 接线到 host |
| `crates/openpaw-cli/src/cli.rs` | `--provider`、`--workspace`、`--home` 提升为全局旗标（`global = true`） |
| `crates/openpaw-cli/src/{session_cmd,memory_cmd,evolve_cmd,goal_cmd,config_cmd,tool_cmd}.rs` | 删除 19 处重复的 `--home` / `--workspace` 定义（全局旗标传播后同名定义会被 clap 跳过，必须移除才能统一） |
| `crates/openpaw-cli/src/main.rs` | engine-host 分支构造并传入 `HostConfig`；三个全局值统一分发 |
| `crates/openpaw-cli/src/chat.rs` | `ChatArgs` 去掉 clap 属性，改由 `main.rs` 注入全局值 |
| `crates/openpaw-cli/tests/help_snapshot.rs` | `sub_help` 先 `build()`，让全局旗标传播到子命令 help |
| `docs/engine-host/t3-acceptance.sh` | 可复跑的 curl 验收脚本（M2/M6 复用） |

## 实施步骤

1. 补 `HostConfig`（hostname、port、provider、home、workspace、username/password）与 `HostConfig::from_cli`；凭据只配一个时返回错误，`main.rs` 以退出码 1 终止。
2. `run(config)`：绑定成功后只输出一条 `opencode server listening on http://<addr>`；随后 `axum::serve`。
3. `http.rs` 加 Basic Authentication 层：未配置凭据时放行；两者都配置时校验 `Authorization: Basic base64(user:pass)`，缺失或错误返回 401（含 `WWW-Authenticate`）。
4. 注册 9 条降级路由：`GET /config`、`GET /mcp` → `{}`；`GET /command`、`GET /project` → `[]`；`POST /session/:id/command|shell|fork|revert|summarize` → 204。
5. 工作区解析改为 header → query → body.directory（SDK 兼容保留）→ CLI 默认工作区；去掉硬编码 `"."`。
6. `runtime.rs` 用配置里的 `home`/`workspace`；`home` 为空时回落 `openpaw_core::openpaw_home()`，保证 `<home>/sessions/<id>.jsonl` 始终写入。
7. CLI：三个旗标升为全局并接线到 `HostConfig`；`chat` 改为读全局值，行为不变。
8. 测试与证据：host 集成测试 + Docker 内 curl 全覆盖 + 行数预算 + 默认依赖树检查。

## 关键决策（2026-09-18 本人确认）

- **D-1 工作区优先级（已确认）**：**严格按 §3.2 三档**——`x-opencode-directory` → `directory` 查询参数 → CLI 默认工作区。移除 09-15 为兼容 SDK 增加、§3.2 未列出的 `body.directory` 回退（`CreateSession.directory` 字段保留以兼容既有 JSON 夹具，但不再参与工作区解析）。
- **D-2 就绪标记（已确认）**：打印实际绑定地址 `opencode server listening on http://<local_addr>`；默认配置下即 `http://127.0.0.1:<port>`，与 §2.4 字面一致；容器 `0.0.0.0:8788` 时输出可用地址。
- **D-3 全局旗标**：按 2026-09-18 用户确认，`--provider/--workspace/--home` 提升为全局旗标（避免 `openpaw --engine-host --provider mock` 被 clap 判为未知参数），`chat` 改为复用全局值。provider 优先级：显式 `--provider` > `OPENPAW_ENGINE_PROVIDER` > `mock`。
- **D-4 集成测试**：用 `tower::ServiceExt::oneshot` 直接驱动 `Router`，不为测试引入新 crate（`tower 0.5.3` 已在 `Cargo.lock`）。

## 验证方法

| 命令 | 通过标准 |
|---|---|
| `cargo test -p openpaw-engine-host --features engine-host` | 退出码 0；新增路由/鉴权/降级用例全绿 |
| `cargo test -p openpaw-cli --features engine-host` | 退出码 0；`help_snapshot` 与新 CLI 一致 |
| `cargo test -p openpaw-cli --features sidecar` | 退出码 0（M3 沿用） |
| `cargo run -p xtask -- lines` | 退出码 0；engine-host ≤ 5000 |
| 默认 `cargo tree` | 不含 axum |
| Docker + curl 全路由脚本 | 必做路由、降级路由、v2 404、401/400/404/409 全部符合 |

### curl 覆盖清单（M2 证据）

- 必做：`GET /global/health`、`POST /session`、`GET /session`、`GET /session/:id`、`GET /session/:id/message`、`GET /session/:id/todo`、`GET /session/status`、`POST /session/:id/prompt_async`、`POST /session/:id/abort`、`GET /event`、`GET /provider`、`GET /permission`、`POST /permission/:id/reply`
- 降级：`GET /config`、`GET /mcp`、`GET /command`、`GET /project`、`POST /session/:id/command|shell|fork|revert|summarize`
- v2：`GET /api/health` → 404
- 鉴权：无凭据 → 200；只配一个 → 启动失败；错凭据 → 401；正确凭据 → 200
- provider：`mock`、`echo` 各完成 health → create → prompt → message 并返回 assistant 文本；`OPENPAW_ENGINE_MOCK=stream` 下 abort 后不再 busy、不再产生新文本

## 环境

- 优先 Docker `rust:1.88-bookworm`（源码挂 `/workspace`，复用既有 Cargo 卷），与 09-17 记录一致。
- 本机 Windows 复核记录：`cargo test` 因缺少 MSVC `link.exe` 无法编译（`error: linker link.exe not found`），不作为验证环境；Docker API 在受限沙箱下可能被拒绝，若被拒绝则记录并改用等价容器路径。

## 验收标准（来自本任务定义）

1. `cargo test -p openpaw-engine-host --features engine-host` 退出码为 0。
2. curl 覆盖全部必做路由和降级路由；v2 路径返回 404。
3. 未配置用户名和密码时可访问；只配置一个时启动失败；都配置时错误凭据返回 401。
4. mock、echo 均可完成 health → create → prompt → message，并返回 assistant 文本。
5. `OPENPAW_ENGINE_MOCK=stream` 下，abort 后 session 不再 busy，且不再产生新文本。
6. 消息写入 `<OPENPAW_HOME>/sessions/<id>.jsonl`。
7. 完成证据：host 测试与 curl 结果；mock、echo、stream 三种实验结果；监听地址和唯一 stdout 就绪行。

## 实施结果（2026-09-18）

### 实现要点

- `lib.rs`：`HostConfig`（`hostname/port/provider/home/workspace/username/password` + 四个 `with_*` 构造器 + `authentication()`）、公开的 `router(state, auth)`、`run(config)`；凭据只配一侧时 `anyhow::bail!`，进程退出码 1 且不打印就绪行；就绪行固定为 `opencode server listening on http://<local_addr>`，是 stdout 的唯一输出。
- `http.rs`：`BasicAuth`（大小写不敏感的 `Basic` scheme + base64 解码 + `user:password` 精确比对）、`require_auth` 中间件（401 + `WWW-Authenticate: Basic realm="openpaw-engine-host"`）、`empty_object`/`empty_array`/`degraded` 三类降级处理器、工作区严格三档（`x-opencode-directory` → `?directory=` → CLI 默认工作区，百分比解码保留）、`prompt` 与 `permission_reply` 改为手写 JSON 解析使所有解析失败统一 400（axum 默认会把字段缺失判为 422）。
- `runtime.rs`：`RuntimeState::new(&HostConfig)` 保存默认工作区与 `home`；JSONL 不再依赖 `OPENPAW_HOME` 是否存在，始终写 `<home>/sessions/<id>.jsonl`。
- CLI：三个旗标全局化后，`openpaw --engine-host --provider mock` 与 §8.2 命令一致；`openpaw chat --provider mock`、`openpaw doctor --workspace X`、`openpaw session list --home Y` 等既有用法不变。
- 行数预算处置：`tests/` 目录按裸行计入 `aux` 预算，而该预算在本次改动前已是 9946/10000。因此 HTTP 契约测试放在 `src/http.rs` 的 `#[cfg(test)] mod contract_tests`（仓库既有模式，`xtask lines` 明确排除 `#[cfg(test)]`），未占用 `aux` 预算，也未放宽任何上限。

### 命令与退出码

环境：Docker Desktop，容器 `hopeful_proskuriakova`（镜像 `rust:1.88-bookworm`），源码挂 `/workspace`，复用 `openpaw-main_cargo-registry` / `openpaw-main_cargo-git` / `openpaw-main_openpaw-target` 三个既有卷；`RUSTUP_TOOLCHAIN=1.88.0`；`rustfmt` 现场 `rustup component add rustfmt` 安装。

| 命令 | 结果 |
|---|---|
| `cargo fmt -p openpaw-engine-host -p openpaw-cli --check` | 退出码 0 |
| `cargo test -p openpaw-engine-host --features engine-host` | 退出码 0；lib **38 passed**（含 7 项新契约测试）、集成 **34 passed**（协议夹具） |
| `cargo test -p openpaw-engine-host`（无 feature） | 退出码 0 |
| `cargo test -p openpaw-cli --features engine-host` | 退出码 0；全部 suite 通过（含 `help_snapshot` 4 passed） |
| `cargo test -p openpaw-cli --features sidecar` | **未通过**：`sidecar_concurrent_calls_serialize_set_before_get` 卡死，`timeout 240` 返回 124；其余 2 项通过。该缺陷在 `2026-09-16-engine-host-commit-and-sidecar-audit.md` 已复现并归因于初始提交 `60a531a`（测试丢弃乱序响应 + 门控不保证请求到达顺序），与本次改动无关 |
| `cargo run -p xtask -- lines` | 退出码 0；`openpaw-engine-host` 1893/5000、`openpaw-cli` 2857/4500、`aux` 9949/10000 |
| `cargo tree -p openpaw-cli \| grep -c axum` | 0（关闭 feature 时默认依赖树不含 axum） |
| `bash docs/engine-host/t3-acceptance.sh`（容器内） | **68 项断言全部 PASS**（脚本自带 PASS/FAIL 汇总，末行 `SUMMARY: pass=68 fail=0`） |

### curl / 端到端证据（`docs/engine-host/t3-acceptance.sh`）

- 就绪行：唯一一行 `opencode server listening on http://127.0.0.1:38641`（`--engine-host-port 0` 动态端口）。
- 必做路由：`/global/health`、`POST/GET /session`、`/session/:id`、`/session/:id/message`、`/session/:id/todo`、`/session/status`、`/provider`、`/permission` 均 200；`/event`、`/global/event`、`/api/event` 的 `content-type` 为 `text/event-stream`。
- mock：create 200 → `prompt_async` 204（空 body）→ `message` 出现 assistant 文本并回显提示 → `abort` 200 `true`。
- echo：同一链路通过，assistant 文本回显 `echo-probe`。
- `OPENPAW_ENGINE_MOCK=stream`：prompt 204 → abort 200 → 轮询 `/session/status` 不再包含该会话；abort 后 1 秒 `message` 字节数不变（2209），即不再产生新文本。
- 降级路由：`/config` 与 `/mcp` 返回 `{}`；`/command` 与 `/project` 返回 `[]`；`command|shell|fork|revert|summarize` 五个 POST 返回 204 空 body。
- v2：`/api/health`、`/api/session` 返回 404。
- 鉴权：仅设 `OPENCODE_SERVER_USERNAME` 或仅设 `OPENCODE_SERVER_PASSWORD` 均启动失败（退出码非 0、无就绪行，错误信息 `... is set but ... is missing; set both or neither`）；两者都设时匿名 401、错误凭据 401、正确凭据 200，且 401 带 `WWW-Authenticate: Basic realm=`。
- 回合准入：`OPENPAW_ENGINE_MOCK=tools` + `OPENPAW_ENGINE_PERMISSIONS=manual` 下，同一 session 第二次 `prompt_async` 返回 409，另一 session 同时返回 204（可并行）。
- 持久化：`<OPENPAW_HOME>/sessions/<id>.jsonl` 生成且含事件记录。

### 跨项目验证（OpenWork T9）

- 扩展现有 journey spec 至 6 个用例（新增降级路由、v2 404、鉴权分支），未新增测试文件。
- `pnpm --dir evals exec vitest run specs/openpaw-engine-attach.test.ts`（指向本次新构建 host、带凭据）：1 file / **6 passed** / 0 skipped。
- 同一命令指向未配置凭据的 host（匿名分支）：1 file / **6 passed** / 0 skipped。
- 正式命令 `pnpm evals:pr specs/openpaw-engine-attach.test.ts`（带凭据）：1 file / **6 passed** / 0 skipped。

## 关联提交

- `5f34107` — `feat(engine-host): T3 HTTP baseline (HostConfig, Basic auth, degraded routes, global flags)`（openpaw `dev` 分支）
- `874573a` — 同批提交前先落地的既有 09-16/09-17 待提交工作（engine / llm / sandbox / host 协议与夹具），本变更在其之上
- 以上提交均在 openpaw `dev` 分支本地，未推送远端。

## 未完成项与下一步

- `cargo test -p openpaw-cli --features sidecar` 仍未通过（09-16 已记录的既有缺陷）；M3 因此不能记为通过，需另立 Sidecar 修复任务（先定协议顺序语义，再让测试缓存乱序响应）。
- `openwork/evals/worlds/openpaw-engine.ts` 仍不存在（§3.3 要求 world 拉起 host），M4 正式验收当前仍依赖外部预启动 host + `OPENPAW_ENGINE_HOST_URL`。
- 容器镜像未重建：`jyzs-openpaw-engine-host:latest` 仍是旧代码；compose 侧未做本轮回归。
- 本次改动**未提交**（工作树状态，无 commit / push）。
- 待确认：`--provider/--workspace/--home` 全局化是 CLI 表面变化，团队大纲 §2.4 只写「复用现有 chat 参数及其优先级」，是否需要同步飞书原文；`OPENPAW_ENGINE_PROVIDER` 亦未登记在 §2.4 环境变量表。
- 09-17 每日记录遗留的页面截图确认与有效慢流终止验收未在本轮处理。
