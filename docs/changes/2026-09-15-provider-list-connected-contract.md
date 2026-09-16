# Engine Host ProviderList connected 契约校正

- 来源任务 ID：T2、T3、T7、T9
- 变更 ID：`2026-09-15-provider-list-connected-contract`
- 开发依据：飞书《OpenPaw 引擎宿主》§3.2 的 `provider.list` 最小形状和 §8.1 的 M4 验收
- OpenPaw 基线 commit：`12a44e5172ca89aed2498171efa3da526f958f57`
- 当前状态：文档准备完成，代码待修改

## 问题与证据

OpenWork T9 在修正旧数组断言后按团队协议校验四个 Provider。Docker 首轮结果为 2 passed、1 failed：Host 返回 `connected = ["mock", "echo", "openai"]`，缺少 `anthropic`。团队大纲明确要求 `all` 包含四项，`connected` 也列出这四个 ID。

当前 `ProviderConfig::catalog` 按 API Key 是否存在动态生成 `connected`，`protocol.rs` fixture 只包含 `mock` 和 `echo`，两者均与当前团队协议不一致。

## 目标与范围

只校正 Engine Host 的 ProviderList catalog、fixture 和对应单元测试：

- `crates/openpaw-engine-host/src/runtime.rs`
- `crates/openpaw-engine-host/src/protocol.rs`
- `docs/changes/2026-09-15-provider-list-connected-contract.md`

`all` 和 `connected` 均覆盖 `mock`、`echo`、`anthropic`、`openai`。Provider 真正执行时仍由现有 `resolve` 检查 API Key；缺少真实凭据时继续返回明确的 provider 未配置错误。本变更不复制 Provider 实现，也不修改 LLM、CLI、工具、审批或 OpenWork 生产代码。

## 实施步骤

1. 将 `ProviderConfig::catalog` 的 `connected` 固定为团队协议规定的四个 ID。
2. 更新 catalog 单元测试，验证无网络 Provider 凭据时列表仍符合协议。
3. 更新 `ProviderList` 序列化往返 fixture 的 `connected`。
4. 在 Docker 中运行 Engine Host 定向测试和完整测试。
5. 重建现有 Engine Host 服务，重新运行 OpenWork T9 eval。

## 验证方法

```powershell
docker compose --profile dev build openpaw-engine-host
docker compose --profile dev up -d --wait --force-recreate openpaw-engine-host
docker compose --profile dev exec -T openpaw-engine-host cargo test -p openpaw-engine-host --features engine-host
docker compose --profile dev exec -T -e OPENPAW_ENGINE_HOST_URL=http://openpaw-engine-host:8788 openwork-dev pnpm --dir evals exec vitest run specs/openpaw-engine-attach.test.ts
```

通过标准：Host 测试退出码 0；`/provider` 的 `all` 与 `connected` 包含四个规定 ID；T9 为 3 passed、0 failed、无 skip。

## 工作树边界

`runtime.rs` 和 `protocol.rs` 已包含当前 Engine Host 会话、Provider 与工具闭环的未提交开发成果。本变更仅修改 catalog、相应测试及 fixture 的局部行，不覆盖其他改动。CLI、sandbox、`.cargo/`、Dockerfile 和其他既有文件不纳入本变更。

## 待确认事项

无。

## 调研更正：不修改 Host Provider 语义

继续读取既有 `docs/changes/2026-09-14-engine-host-provider-integration.md` 后，确认本文件最初拟定的“固定四项 connected”与已确认实现冲突：

- `all` 是 Host 支持目录。
- `connected` 是当前实际可调用目录；mock/echo 固定可用，网络 Provider 只有配置凭据后可用。
- 网络 Provider 的模型 ID 来自运行配置，不能固定成示例模型。
- `resolve` 对未配置 Provider 返回 400，前端不应把这种 Provider 显示为 connected。

因此，本文件对应的 Host 代码方案撤回，不保留对 `runtime.rs`、`protocol.rs` 的修改。团队大纲 §3.2 的静态 `connected` 描述与已确认运行语义存在文档差异，应走团队协议维护流程处理，不能通过引入错误运行行为解决。

当前状态：调研关闭，无 Host 代码交付。

待确认事项：团队大纲 ProviderList 文字更正。

## 验证结果

- 恢复动态 Provider 源码后运行 `cargo test --locked -p openpaw-engine-host --features engine-host`：19 passed、0 failed。
- 最终镜像：`sha256:c39025e081508a911e581f272c9933b0785b4766549bb666185f7a0d4dc93c15`；Engine Host healthy。
- `/provider`：all 为四个支持项；connected 为 mock、echo 和已配置的 openai；OpenAI 目录显示实际配置模型；未配置 Anthropic 不进入 connected。
- OpenWork T9：正式 `pnpm evals:pr specs/openpaw-engine-attach.test.ts` 退出码 0，3 tests passed、无 skip。
- 实际文件：本记录；没有保留 `runtime.rs` 或 `protocol.rs` 的本任务代码差异。
- Git：尚未提交。
