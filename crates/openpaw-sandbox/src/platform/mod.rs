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
/// 功能：探测当前平台受限后端的基本可用性
/// 参数：无
/// 返回：Linux具备bwrap时为真；Windows受限网络尚不可验证返回假
/// 错误：探测不可用返回假
/// 状态：只读探测
/// 并发与取消：同步，不提供取消
/// 权限：可用不代表任意策略已落实，执行时仍严格验证
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
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
        // 现有token/ACL不能落实策略要求的网络隔离；本地Job执行不等于受限能力。
        false
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
    exec_sandboxed_cancellable(
        policy,
        command,
        cwd,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
}

/// 功能：把取消与隔离策略分派到原平台
/// 参数：policy/command/cwd为输入，cancel为token
/// 返回：实际执行结果
/// 错误：缺后端或无法落实限制时拒绝
/// 状态：不创建额外执行器
/// 并发与取消：下游持有token并等待清理
/// 权限：不以环境提示代替隔离
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
pub(crate) async fn exec_sandboxed_cancellable(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<SandboxResult, SandboxError> {
    if !policy.is_restricted() {
        return crate::command::exec_unrestricted_cancellable(command, cwd, cancel).await;
    }
    #[cfg(target_os = "linux")]
    {
        linux::exec_bwrap_cancellable(policy, command, cwd, cancel).await
    }
    #[cfg(windows)]
    {
        windows::exec_restricted_cancellable(policy, command, cwd, cancel).await
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (policy, command, cwd, cancel);
        Err(SandboxError::PlatformUnavailable(
            "restricted managed backend unavailable on this platform".into(),
        ))
    }
}

// ---- Platform modules ----

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
/// 职责：复用Windows token、ACL与Job执行机制。
/// 输入输出：命令/策略到实际进程结果。
/// 依赖：系统句柄与Job/管道API。
/// 禁止职责：不把环境提示当隔离，不降级普通执行。
pub(crate) mod windows;
