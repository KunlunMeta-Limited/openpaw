# openpaw-engine-host

`openpaw-engine-host` 是 OpenPaw 的 B 线 OpenCode v1 适配器，仅监听回环地址。
它是由 Cargo feature `engine-host` 保护的实验功能，不属于 1.0 兼容性承诺。

## 模块说明

- `protocol.rs`：线协议对象、请求解码和 host 配置。
- `runtime.rs`：每 session 独立状态、AgentLoop 所有权、JSONL 持久化，以及
  provider/tool 构造。
- `http.rs`：Axum 路由、鉴权、状态码和 SSE 传输。
- `events.rs`：将 AgentEvent 转换为 OpenCode 事件。
- `approval.rs`：每 session 临时 permission 的请求和回复。
- `office.rs`：有序、非阻塞的 AI Office 动作 POST 队列。

依赖方向为 `http -> runtime/events/approval -> core/engine`；`office` 是可选
观察器，绝不改变 AgentLoop 结果。每个 session 独占 AgentLoop、取消状态、
approval manager 和 broadcast channel。host 默认只绑定 `127.0.0.1`。

## 运行

```text
openpaw --engine-host --engine-host-port 0 --provider mock
```

设置 `OPENPAW_ENGINE_MOCK=stream` 可启用延迟 chunk；设置
`OPENPAW_ENGINE_PERMISSIONS=manual` 可启用审批路径。`OPENPAW_HOME`、
`OPENPAW_OFFICE_ACTIONS_URL`、`OPENCODE_SERVER_USERNAME` 和
`OPENCODE_SERVER_PASSWORD` 分别配置持久化、Office 投递和 Basic Authentication。

prompt body 的 `model.providerID/modelID` 是本回合真实执行选择。外部 provider 使用
`ANTHROPIC_API_KEY`/`OPENAI_API_KEY`；可选 base URL 必须是无 query/fragment 的
HTTP(S) URL。创建 session 时解析出的绝对 workspace 写入 `Metadata.cwd`，重启后
不会退回到另一个 CLI 工作区。

## 验证

```text
cargo test -p openpaw-engine-host --features engine-host
cargo build -p openpaw-cli --features engine-host
cargo clippy -p openpaw-engine-host --features engine-host --all-targets -- -D warnings
```

跨项目命令位于 `chenningbo/kunlun`。
