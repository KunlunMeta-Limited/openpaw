//! Platform-specific sandbox implementations.
//!
//! This module dispatches to the appropriate backend based on the
//! current operating system. Each platform module implements the
//! `exec_sandboxed` function with the same signature.

use std::path::Path;

use crate::error::SandboxError;
use crate::policy::SandboxPolicy;
use crate::SandboxResult;

/// Check if native sandboxing is available on this platform.
pub fn is_sandbox_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::is_bwrap_available()
    }
    #[cfg(target_os = "macos")]
    {
        macos::is_seatbelt_available()
    }
    #[cfg(target_os = "windows")]
    {
        // Windows sandbox is always available (uses built-in APIs)
        true
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

/// Get the name of the sandbox backend for the current platform.
pub fn backend_name() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "bubblewrap"
    }
    #[cfg(target_os = "macos")]
    {
        "seatbelt"
    }
    #[cfg(target_os = "windows")]
    {
        "restricted_token"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "none"
    }
}

/// Execute a command with the given sandbox policy.
///
/// Dispatches to the platform-specific implementation. If native
/// sandboxing is unavailable for a restricted policy, returns
/// `SandboxError::PlatformUnavailable` (fail-closed).
/// 功能：将命令分派到当前平台；受限后端缺失返回失败。
/// 参数：policy为策略；command为命令；cwd为可选工作目录。
/// 返回：命令输出、退出码和审计。
/// 错误：平台不可用或后端执行错误。
/// 状态：非受限策略调用普通执行，受限策略进入对应后端。
/// 并发与取消：异步执行；各后端清理/取消能力仍须按阶段完善。
/// 权限：必须先授权；不据统一入口推断所有策略均已落实。
/// 关联：现有沙箱分派契约与执行能力矩阵。
pub async fn exec_sandboxed(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
) -> Result<SandboxResult, SandboxError> {
    // If the policy is unrestricted, run without sandbox
    if !policy.is_restricted() {
        return crate::command::exec_unrestricted(command, cwd).await;
    }

    // Try platform-specific sandboxing
    #[cfg(target_os = "linux")]
    {
        if linux::is_bwrap_available() {
            return linux::exec_bwrap(policy, command, cwd).await;
        }
        // Fail closed: cannot enforce a restricted policy without bwrap
        Err(SandboxError::PlatformUnavailable(
            "bwrap not found on Linux; cannot enforce restricted sandbox policy".to_string(),
        ))
    }

    #[cfg(target_os = "macos")]
    {
        if macos::is_seatbelt_available() {
            return macos::exec_seatbelt(policy, command, cwd).await;
        }
        // Fail closed: cannot enforce a restricted policy without sandbox-exec
        Err(SandboxError::PlatformUnavailable(
            "sandbox-exec not found on macOS; cannot enforce restricted sandbox policy".to_string(),
        ))
    }

    #[cfg(target_os = "windows")]
    {
        windows::exec_restricted(policy, command, cwd).await
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        // Fail closed: no sandbox support on this platform
        Err(SandboxError::PlatformUnavailable(
            "no sandbox support on this platform; cannot enforce restricted sandbox policy"
                .to_string(),
        ))
    }
}

// ---- Platform modules ----

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;
