# Sidecar 容器监听与 Linux Sandbox 借用修复

## 记录信息

- 日期：2026-09-16
- 来源任务 ID：T5、T6
- 来源补充：用户明确批准提交工作树中既有的 `sidecar.rs` 与 `linux.rs` 修改。
- 变更 ID：`2026-09-16-sidecar-container-sandbox-fix`
- 开发依据：飞书《OpenPaw 引擎宿主》§1.4、§6、§8.1；本日 Sidecar 历史审计记录。
- 基线 commit：`3b85244e7206bc4cf6c6b98344d3b03d3e26e59e`
- 当前分支：`dev`
- 当前状态：文档准备完成，验证与提交待执行。

## 目标与范围

本次只整理和提交两组已经存在于工作树中的独立修改：

1. Linux Sandbox 借用修复：
   - `crates/openpaw-sandbox/src/platform/linux.rs`
   - `create_dir_all(root)` 改为 `create_dir_all(&root)`，避免移动可变引用后继续在错误信息中使用 `root`。
   - 不改变创建目录的运行语义。
2. Sidecar 容器监听：
   - `crates/openpaw-cli/src/sidecar.rs`
   - 根 `Dockerfile`
   - HTTP transport 从固定 `127.0.0.1:0` 改为读取 `OPENPAW_SIDECAR_HOST`、`OPENPAW_SIDECAR_PORT`，默认值保持不变。
   - Docker 运行时设置 `0.0.0.0:8788`，使端口能够从容器映射到宿主机。

明确排除：

- `.cargo/config.toml`：这是本机 rsproxy 和网络重试策略，不与功能代码提交混合。
- Sidecar stdio 并发语义与 E2E 测试：本轮不修改；初始版本并发用例超时属于已确认的基线问题。
- Engine Host：已在独立提交中完成，本轮不再修改。

## 实施与提交步骤

1. 使用 Docker `rust:1.88-bookworm` 验证 Sandbox 包。
2. 编译 Sidecar feature，运行 Sidecar 单元测试和两个不涉及并发顺序的 E2E 用例。
3. 对已知并发 E2E 使用超时保护，保留真实失败证据，不宣称全套 Sidecar 测试通过。
4. 构建根 Sidecar Dockerfile，并验证容器固定监听端口和 MCP initialize。
5. 分别提交 Sandbox 与 Sidecar，避免混合提交。
6. 回填实际命令、退出码、镜像与 commit。

## 验证方法

```powershell
cargo test -p openpaw-sandbox
cargo test -p openpaw-cli --features sidecar --lib --bin openpaw
cargo test -p openpaw-cli --features sidecar --test sidecar_e2e sidecar_stdio_initialize_tools_list_and_tool_roundtrips -- --exact
cargo test -p openpaw-cli --features sidecar --test sidecar_e2e sidecar_unknown_transport_fails_with_three_segment_error -- --exact
timeout 20s cargo test -p openpaw-cli --features sidecar --test sidecar_e2e sidecar_concurrent_calls_serialize_set_before_get -- --exact --nocapture
docker build -f Dockerfile -t jyzs-openpaw-sidecar:validation .
git diff --cached --check
```

通过标准：Sandbox 和 Sidecar 非并发验证退出码为 0；Docker 镜像构建成功并能通过固定端口响应 MCP；已知并发用例的超时必须记录为未通过，不能记为通过或 skip。

## 风险

- `OPENPAW_SIDECAR_HOST=0.0.0.0` 会扩大监听范围；部署时需限制端口暴露范围或增加认证边界。
- 根 Dockerfile 依赖构建阶段可访问 Rust 依赖源；运行层不应下载依赖。
- Sidecar 并发测试缺陷仍存在，本次提交不代表 Sidecar 全套验收完成。

## 待确认事项

无。用户已授权本次独立提交；没有授权推送远端。
