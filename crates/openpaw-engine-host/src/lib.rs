//! 功能：提供 OpenCode 兼容的 HTTP/SSE Host 适配层。
//! 输入输出边界：从 CLI 接收 HostConfig，向 OpenWork/AI Office 提供 REST、SSE 和动作投影。
//! 依赖约束：本 crate 只在 `engine-host` feature 下启用；默认核心构建不得引入 axum。
//! 禁止承担：不修改 `openpaw-core` 公共 trait，不绕过 runtime 直接写 session。
//! 关联：`DES-OPENPAW-B-2026-09-15`、`REQ-B-HOST-008`。 <!-- openpaw-scrub-allow：本仓设计/接口/测试编号，非上游任务 -->

#[cfg(feature = "engine-host")]
mod approval;
#[cfg(feature = "engine-host")]
mod context;
#[cfg(feature = "engine-host")]
mod delegation;
#[cfg(feature = "engine-host")]
mod events;
#[cfg(feature = "engine-host")]
mod goal;
#[cfg(feature = "engine-host")]
mod http;
#[cfg(feature = "engine-host")]
mod office;
#[cfg(feature = "engine-host")]
mod protocol;
#[cfg(feature = "engine-host")]
mod runtime;

#[cfg(feature = "engine-host")]
pub use http::{build_router, run, run_blocking, run_with_shutdown};
#[cfg(feature = "engine-host")]
pub use protocol::{HostConfig, OpenCodeEvent};
#[cfg(feature = "engine-host")]
pub use runtime::{AppState, SessionHandle};

/// 功能：标记当前构建是否启用了 `engine-host` feature。
/// 返回：启用 feature 时为 true，否则为 false。
/// 稳定性：供 CLI 和集成检查读取，不代表 Host 已经启动。
/// 关联：`REQ-B-HOST-001`、`REQ-B-HOST-008`。
pub const ENGINE_HOST_AVAILABLE: bool = cfg!(feature = "engine-host");
