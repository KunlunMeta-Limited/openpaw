//! 职责：承载Office与固定OpenCode适配器共用的运行服务。
//! 输入输出：可信Rust作用域到Session、Run、审批、查询及就绪监听。
//! 依赖：可选核心引擎、工具、原存储和执行环境。
//! 禁止职责：不复制AgentLoop、会话账本或工具主体；外部协议留后续适配器。
#[cfg(feature = "engine-host")]
mod approval;
#[cfg(feature = "engine-host")]
mod config;
#[cfg(feature = "engine-host")]
mod query;
#[cfg(feature = "engine-host")]
mod resource;
#[cfg(feature = "engine-host")]
mod server;
#[cfg(feature = "engine-host")]
mod service;
#[cfg(feature = "engine-host")]
mod store;
#[cfg(feature = "engine-host")]
mod tool_host;
#[cfg(feature = "engine-host")]
mod types;
#[cfg(feature = "engine-host")]
pub use {config::HostConfig, server::HostServer, service::Host, types::*};
