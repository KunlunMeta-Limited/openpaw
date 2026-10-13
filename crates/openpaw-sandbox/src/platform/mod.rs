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

/// 功能：按平台后端执行命令，受限策略缺少后端时拒绝执行。
/// 参数：`policy` 为隔离策略，`command` 为命令，`cwd` 为可选工作目录。
/// 返回：命令输出、退出码及审计记录。
/// 错误：后端不可用返回 `PlatformUnavailable`；执行错误由对应后端返回。
/// 状态：可启动子进程并按策略访问文件系统。
/// 并发与取消：异步等待后端执行；沿用后端的进程与取消语义。
/// 权限：只有非受限策略可直接执行，受限策略不得静默降级。
/// 关联：`command::exec_unrestricted` 与平台隔离后端。
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
