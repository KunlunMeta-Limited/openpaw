//! Sandbox error types.

use thiserror::Error;

/// Errors that can occur during sandbox operations.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// The sandbox backend is unavailable on this platform.
    #[error("sandbox backend unavailable: {0}")]
    PlatformUnavailable(String),

    /// Failed to create a restricted token (Windows).
    #[error("failed to create restricted token: {0}")]
    TokenCreation(String),

    /// Failed to generate a capability SID (Windows).
    #[error("failed to generate capability SID: {0}")]
    SidGeneration(String),

    /// Failed to create or configure a Job Object (Windows).
    #[error("job object error: {0}")]
    JobObject(String),

    /// Failed to modify ACL on a path (Windows).
    #[error("ACL operation failed on {path}: {reason}")]
    AclOperation { path: String, reason: String },

    /// Failed to configure firewall rules (Windows).
    #[error("firewall rule error: {0}")]
    FirewallRule(String),

    /// The required sandbox tool was not found on the system.
    #[error("sandbox tool not found: {0}")]
    ToolNotFound(String),

    /// The command execution failed.
    #[error("command execution failed: {0}")]
    ExecutionFailed(String),

    /// The command timed out.
    #[error("command timed out after {0}ms")]
    Timeout(u64),

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Audit log storage error.
    #[error("audit log error: {0}")]
    AuditLog(String),

    /// Config signature verification failed.
    #[error("signature invalid: {0}")]
    SignatureInvalid(String),
}
