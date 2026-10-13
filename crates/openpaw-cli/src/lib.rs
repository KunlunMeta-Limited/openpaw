//! openpaw-cli library surface.
//!
//! Subcommand implementations live here so the binary and the integration
//! tests share the same entry points (each `run_*` function is a testable
//! pure function returning an exit code). The `Cli`/`Command` definitions
//! live here too, so tests can render help text via `Cli::command()`.

/// 功能：导出共享 approval 能力。
/// 参数：无。
/// 返回：模块接口。
/// 错误：由模块接口说明。
/// 状态：无独立状态。
/// 并发与取消：由模块接口说明。
/// 权限：保持既有边界。
/// 关联：CHG-2026-0023。
pub mod approval;
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
