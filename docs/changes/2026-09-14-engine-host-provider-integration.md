# Engine Host Provider 复用与 OpenWork 模型展示计划

## 记录信息

- 日期：2026-09-14
- 来源任务 ID：T2、T3、T4、T7、T9
- 变更 ID：`2026-09-14-engine-host-provider-integration`
- 开发依据：`docs/engine-host/master.md` §2.3、§2.4、§3.1、§3.2、§8.1、§8.2
- 当前状态：Engine Host 范围内实现、Docker 验证和真实 OpenAI 兼容 Provider 调用已完成
- 基线说明：当前 Engine Host `/provider` 只返回简单 `mock` 数组，`prompt_async` 固定创建 `MockProvider`；现有 OpenWork 测试属于基础接口 smoke，尚未满足正式 A 线验收。

## 目标

让 Engine Host 复用 OpenPaw 已有 `LlmProvider` 能力，对 OpenWork 提供可识别的 Provider/Model catalog，并按 `prompt_async` 中的 `providerID/modelID` 选择 mock、echo 或已配置的真实 Provider。完成后，OpenWork 能展示可用模型，并通过 attached 链路完成真实 assistant 文本、SSE、工具、审批与终止验证。

## 责任边界

- `openpaw-core` 保留 `LlmProvider` trait 与公共模型请求类型。
- `openpaw-llm` 保留 mock、echo、OpenAI、Anthropic 的现有具体实现，本次不修改。
- `openpaw-cli` 保持现状，本次不修改 `chat.rs`、`cli.rs` 或 `main.rs`。
- `openpaw-engine-host` 在正式模块边界内负责 OpenCode v1 HTTP/SSE 适配、Provider catalog、请求校验和运行时装配；它只调用 `openpaw-llm` 的公开类型，不重新实现供应商协议。
- `openwork` 负责 attached 地址、Provider/Model 展示、模型选择、SDK 请求和 UI 状态，不保存或转发模型 API Key。

## 当前差异

| 能力 | 正式目标 | 当前实现 | 判定 |
| --- | --- | --- | --- |
| Provider 响应 | `{ all, connected }`，Provider 含 `models` | `[{ id, name }]`，只有 mock 名称 | 未完成 |
| 模型选择 | `prompt_async.model.providerID/modelID` 生效 | Prompt 只有 `parts` | 未完成 |
| Provider 构造 | CLI 与 Host 复用同一入口 | CLI 内联 match；Host 硬编码 MockProvider | 未完成 |
| 真实配置 | 已配置 Provider 才进入 `connected` | Host 不读取真实 Provider 配置 | 未完成 |
| OpenWork 展示 | 显示 Host 返回的可用模型 | 默认仍可能是 `opencode/big-pickle` | 未完成 |
| T9 验收 | 通过 OpenWork proxy/SDK 等到 assistant message | 直连 Host，只检查路由和状态码 | 未完成 |

## 配置设计

Provider 构造使用 Host 内部的结构化运行时配置，API Key 只从环境变量、Docker secret 或未进入 Git 的本机配置读取。

```text
ProviderConfig
  provider_id
  model_id
  base_url（可选）
  api_key（运行时 secret）
        ↓
runtime.rs Host 装配逻辑
        ↓
Arc<dyn LlmProvider>
        ↓
AgentLoop
```

建议配置入口：

- `OPENAI_API_KEY`：OpenAI 兼容服务凭据。
- `OPENAI_BASE_URL`：OpenAI 兼容服务的 `/v1` 根地址。
- `OPENAI_MODEL`：默认模型名称。
- `ANTHROPIC_API_KEY`：Anthropic 凭据。
- `OPENPAW_ENGINE_PROVIDER`：在本次仅限 Engine Host 文件的边界内，作为请求未指定模型时的默认 Provider。
- mock、echo 不依赖网络；OpenAI、Anthropic 只有在配置完整且可构造时进入 `connected`。

任何密钥不得写入本文件、Compose 默认值、镜像层、测试输出或 Git 历史。

## 拟影响文件

### OpenPaw（代码范围严格限制在 Engine Host）

- `crates/openpaw-engine-host/src/protocol.rs`：补齐 ProviderList、ProviderModel、ModelRef 和 Prompt model 类型与 fixture。
- `crates/openpaw-engine-host/src/runtime.rs`：保存 Provider catalog、默认选择、session AgentLoop、工具 registry 和 SessionWriter。
- `crates/openpaw-engine-host/src/http.rs`：返回正式 `/provider` 结构；解析并校验 `prompt_async.model`。
- `crates/openpaw-engine-host/src/events.rs`：保证真实 Provider 输出仍转换为正式 message/part/SSE 事件。
- `crates/openpaw-engine-host/src/lib.rs`：把 HostConfig 和 Provider 配置传入共享运行时。

明确不新增 Host 模块，不修改 Engine Host 目录外的代码。`crates/openpaw-llm/src/openai.rs` 已支持 `OpenAiProvider::new(api_key, model).with_base_url(base_url)`；`crates/openpaw-cli/src/chat.rs` 保持原状。若兼容性测试证明必须扩大范围，则停止实现并先更新本记录，不直接越界修改。

### OpenWork

- `apps/app/src/app/constants.ts`：失效默认模型改为 Host default 或首个 connected model。
- `apps/app/src/react-app/infra/provider-list-query.ts`：保持按 `connected` 和 `models` 展示，补充 attached 场景测试。
- 设置页相关组件：显示 `Engine: OpenPaw (attached)` 和不支持能力状态。
- `evals/worlds/openpaw-engine.ts`：启动真实 Host 与 OpenWork Server。
- `evals/specs/openpaw-engine-attach.test.ts`：通过 OpenWork proxy/SDK 完成正式断言。

扩大文件范围或改变 Provider 配置语义前，先更新本记录。

## 实施步骤

1. 固化正式 JSON fixture，使当前错误 Provider 数组和缺失 Prompt model 的实现先产生失败测试。
2. 在 `runtime.rs` 内使用 `openpaw-llm` 公开类型完成 Host 装配，让 CLI 和供应商实现保持不变。
3. 为 Host 建立 Provider catalog；`all` 表示支持项，`connected` 表示当前可调用项；请求未指定模型时使用 Host 的运行时默认 Provider。
4. 修正 `/provider` 响应和 `prompt_async` 请求解析；未知或未配置的 Provider 返回 400，不静默回退到 mock。
5. 复用 `arc_registry(workspace)`、AgentLoop 和 SessionWriter，确保真实 Provider 与 mock 走同一运行时。
6. 在 OpenWork 清理失效默认模型，并使用 Host catalog 展示和选择模型。
7. 将 T9 改为启动真实 Host 与 OpenWork Server，通过 `/workspace/:id/opencode/*` 和 SDK 等待 assistant message。
8. 完成 Docker 中的 mock、tools、stream、人工审批、abort 和真实 OpenAI 兼容服务验证。

## 验证方法

所有构建和测试优先在 JYZS Docker Compose 与已有镜像中运行；无可用镜像时才现场 build，并记录镜像与下载情况。

### OpenPaw

```powershell
cargo test -p openpaw-llm
cargo test -p openpaw-engine-host --features engine-host
cargo test -p openpaw-cli --features engine-host
cargo run -p xtask -- lines
```

检查项：

- mock/echo 无密钥时出现在 `connected`。
- 真实 Provider 配置完整时，其模型出现在 `connected` 和 `models`。
- 未配置或不支持的 Provider 返回明确错误。
- `prompt_async` 的选择实际决定 AgentLoop 使用的 Provider 和 model。
- 日志、HTTP 响应和测试输出不包含 API Key。

### OpenWork

```powershell
pnpm --dir apps/server test -- opencode-connection.test.ts
pnpm --dir evals exec vitest run specs/openpaw-engine-attach.test.ts
```

检查项：

- 测试请求经过 OpenWork Server proxy，而不是只直连 Host。
- Provider 选择器显示 `mock/mock-1`；配置真实服务后显示相应真实模型。
- 发送 prompt 后，message history 出现 assistant 文本。
- SSE 顺序、tool part、manual permission 和 abort 满足 `master.md`。
- 正式 T9 输出通过且无 skip。

## 验收标准

- OpenWork 设置页显示 `Engine: OpenPaw (attached)`。
- `/provider` 结构符合 OpenCode v1 fixture，模型选择器不再显示失效的默认模型。
- mock 文本、tools、stream 三种模式完成桌面链路验证。
- 已配置的 OpenAI 兼容服务完成至少一次真实流式文本验证。
- 人工审批完成 asked、once/always/reject、replied 和 abort 清理。
- abort 后不再产生该 session 的文本 delta，状态最终为 idle。
- Docker 运行阶段不下载依赖，API Key 不进入仓库、镜像和日志证据。

## 未完成项与下一步

- 未完成：常驻 Compose 未注入真实 Provider 密钥，因此本轮没有向外部 OpenAI/Anthropic 服务发出真实请求；OpenWork 旧 smoke 仍按数组断言 `/provider`，受本次严格文件边界限制未修改；现有 CLI 没有把 `--provider` 传入 `openpaw_engine_host::run`，因此本次严格边界内的 Host 默认值通过 `OPENPAW_ENGINE_PROVIDER` 配置。
- 下一步：在不落盘的运行时环境中注入 Provider 配置后执行一次真实流式对话；OpenWork 协议测试需在单独授权的 OpenWork 变更中更新为 `{ all, connected }`。
- 待确认事项：团队大纲当前写明 `connected` 固定列出四个 Provider；本实现遵循已确认的运行语义，只列当前可调用项，避免前端展示未配置且调用必然返回 400 的模型。

## 实际实现

- 实际代码文件严格限定为：
  - `crates/openpaw-engine-host/src/protocol.rs`
  - `crates/openpaw-engine-host/src/runtime.rs`
  - `crates/openpaw-engine-host/src/http.rs`
- 未新增 `provider.rs` 或其他模块；未修改 `openpaw-llm`、`openpaw-cli`、OpenWork 与 Compose。
- `protocol.rs` 增加 `ModelRef`、带 `model` 的 `Prompt`、`ProviderModel`、`Provider` 和 `ProviderList`，并增加 Prompt/Provider JSON 往返夹具。
- `runtime.rs` 从环境读取 Host Provider 配置，直接复用 `MockProvider`、`EchoProvider`、`OpenAiProvider::with_base_url` 和 `AnthropicProvider::with_base_url`；没有复制供应商 HTTP 实现。
- `/provider` 返回 `{ all, connected }`。`all` 登记 mock、echo、anthropic、openai 及模型；`connected` 默认含 mock/echo，真实 Provider 仅在对应 API Key 存在时加入。
- `prompt_async` 按 `model.providerID/modelID` 解析实际 Provider；未知 Provider、未配置真实 Provider和 catalog 外模型返回 400，不静默降级。

## 实际验证

- Docker 单测：`cargo test -p openpaw-engine-host --features engine-host`，11 passed、0 failed。
- Docker CLI 回归：`cargo test -p openpaw-cli --features engine-host`，全部通过。
- Docker 行数闸门：`cargo run -p xtask -- lines`，Engine Host 760/5000，`lines: all budgets respected`。
- 可迁移镜像：`docker compose --profile dev build openpaw-engine-host` 成功，生成 `jyzs-openpaw-engine-host:latest`；构建阶段重新下载并编译 Cargo 依赖，运行镜像只包含 release 二进制。
- 常驻容器：`docker compose --profile dev up -d --wait --force-recreate openpaw-engine-host` 后状态 healthy；OpenWork Web 重启成功。
- 宿主访问：`GET http://127.0.0.1:8789/provider` 返回四项 catalog，`connected` 为 `mock`、`echo`。
- Compose 网络：OpenWork 开发容器访问 `http://openpaw-engine-host:8788/provider` 返回 200 且结构一致。
- Prompt 冒烟：指定 `mock/mock-1` 返回 204 并写入 7 条回合事件；未知 Provider 返回 400。
- 工具链说明：首次复验因仓库 `stable` 声明触发工具链下载，已中止；随后固定 `RUSTUP_TOOLCHAIN=1.88.0` 完成测试。行数闸门使用独立临时 target 重新编译 xtask，以避免复用含旧编译路径的缓存二进制。
- Git 状态：本轮尚未提交；仓库中 `sidecar.rs`、sandbox、`.cargo/`、`Dockerfile` 为本轮前既有工作树内容，不属于本变更。

## 真实 Provider 补充修复计划

- 触发：本机 `.env` 注入 OpenAI 兼容配置后，`/provider` 已把 `openai/gpt-5.6-sol` 列为 connected；相同配置直连供应商返回预期文本，但通过 Host 的 Prompt 在 30 秒内只有用户记录，没有 assistant 事件。
- 根因：`http.rs` 创建新的 `EngineSession` 后没有把本次用户文本加入 EngineSession；同时 `AgentLoop::new()` 的默认模型为 `openpaw-default`，Host 解析出的 `modelID` 只写入 Session 元数据，没有通过 `with_model` 传给实际 Provider 请求。mock 固定输出不检查 messages/model，因而掩盖了两个问题。
- 范围：只修改已经获准的 `crates/openpaw-engine-host/src/http.rs`，在后台 turn 启动前写入 `openpaw_core::Message::user_text`，并把解析后的模型传给 `AgentLoop::with_model`；不新增文件，不修改 Host 外代码。
- 验证：重跑 Docker Host/CLI 测试，重建 Engine Host 镜像，通过 Host 请求 `openai/gpt-5.6-sol` 并确认产生 assistant 文本；密钥只保存在 Git 忽略的本机 `.env`，不得进入日志或文档。

## 真实 Provider 最终结果

- 本机配置保存于 Git 忽略的 `openpaw/.env`；根 Compose 使用同样被 `.git/info/exclude` 忽略的 `docker-compose.override.yml` 自动加载该文件。普通 `docker compose --profile dev up` 即可恢复配置。
- Provider 发现：Host 和 OpenWork 容器均返回 connected 包含 `openai`，OpenAI models 包含 `gpt-5.6-sol`。
- 供应商直连：非流式 `/chat/completions` 返回预期文本；Docker 网络中的流式请求返回 HTTP 200 和 `text/event-stream`。
- Host 修复：`http.rs` 把用户文本写入 `EngineSession`，并使用 `AgentLoop::with_model` 传递解析后的 modelID。
- 源码容器真实请求返回 `OPENPAW_PROVIDER_OK`；最终 release 镜像真实请求返回 `OPENPAW_RELEASE_OK`；普通 Compose 自动加载本机环境后再次返回 `AUTO_ENV_OK`。
- 结论：Base URL、凭据、模型、Provider 构造、AgentLoop 模型选择、流式解析和 Host HTTP 链路已经闭环。密钥未写入本记录、镜像层或 Git 跟踪文件。
- 更正：前文“未向真实外部模型发出请求”仅描述修复前状态，现已完成真实调用验证；剩余事项只有 OpenWork 旧 smoke 数组断言，需在允许修改 OpenWork 时单独处理。
