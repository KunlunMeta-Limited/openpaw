//! openpaw-cli library surface.
//!
//! Subcommand implementations live here so the binary and the integration
//! tests share the same entry points (each `run_*` function is a testable
//! pure function returning an exit code). The `Cli`/`Command` definitions
//! live here too, so tests can render help text via `Cli::command()`.

pub mod cli;
pub mod completions_cmd;
pub mod config_cmd;
pub mod doctor;
pub mod evolve_cmd;
pub mod goal_cmd;
pub mod init_cmd;
pub mod memory_cmd;
pub mod model_cmd;
pub mod session_cmd;
pub mod tool_cmd;

pub use cli::{Cli, Command};

/// 职责：可选Host命令装配与停止。
/// 输入输出：本机配置到退出码。
/// 依赖：共享Host服务。
/// 禁止职责：不处理Office协议或复写核心。
#[cfg(feature = "engine-host")]
pub mod host_cmd;
