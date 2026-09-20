# Engine Host 测试

`http.rs` 在不打开 TCP 端口的情况下测试公共 router。覆盖 health 和 provider
契约、session 线格式、message 与 prompt 状态、Basic Authentication、格式错误的
JSON、全部必需/降级/v2 路由、global SSE、工作区优先级，以及活动回合的原子预留。

fake OpenAI server 使用随机回环端口，验证请求实际命中配置的 `/v1/chat/completions`
并携带 prompt 的 `modelID`；不访问外部网络。更底层的 delete/load、workspace 重启、
approval/abort、stream cancel、Office owner 和多 response completion 位于对应源码
模块的 `#[cfg(test)]` 中。

运行 `cargo test -p openpaw-engine-host --features engine-host`。
