//! # openpaw-sandbox
//!
//! Cross-platform sandbox for secure command execution.
//!
//! ## Platform Support
//!
//! | Platform | Backend | Status |
//! |----------|---------|--------|
//! | Windows | Restricted Token + Capability SID + Job Object | native |
//! | Linux | Bubblewrap (bwrap) | native |
//! | macOS | Seatbelt (sandbox-exec) | native |
//!
//! ## Quick Start
//!
//! ```ignore
//! use openpaw_sandbox::{Sandbox, SandboxPolicy};
//!
//! let policy = SandboxPolicy::workspace_write(vec!["./workspace"], vec!["api.example.com"]);
//! let sandbox = Sandbox::new(policy)?;
//! let result = sandbox.exec("ls", None).await?;
//! println!("exit_code: {}, stdout: {}", result.exit_code, result.stdout);
//! ```

pub mod approval;
pub mod audit;
pub mod behavior;
pub mod command;
pub mod error;
pub mod file_lock;
mod platform;
pub mod policy;

pub use approval::{
    AllowAlwaysSandboxCallback, AllowOnceSandboxCallback, DenySandboxCallback, FnSandboxCallback,
    SandboxApprovalCallback, SandboxApprovalContext, SandboxApprovalDecision, SandboxWhitelist,
    SandboxWhitelistRule,
};
pub use audit::{AuditLog, AuditStore, FileAuditStore};
pub use behavior::{
    AnomalyKind, AnomalySeverity, BehaviorAnomaly, BehaviorMonitor, BehaviorMonitorConfig,
    BehaviorStats, CommandCategory,
};
pub use command::SandboxedCommand;
pub use error::SandboxError;
pub use file_lock::{try_lock_exclusive, try_lock_shared, FileLockGuard};
pub use policy::{FileSystemPolicy, NetworkPolicy, SandboxPolicy};

/// Execution result from a sandboxed command.
#[derive(Debug, Clone)]
pub struct SandboxResult {
    /// Standard output captured from the command.
    pub stdout: String,
    /// Standard error captured from the command.
    pub stderr: String,
    /// Exit code of the command (0 = success).
    pub exit_code: i32,
    /// Audit log recording what happened during execution.
    pub audit: AuditLog,
}

/// The main sandbox interface.
///
/// Creates a platform-appropriate sandbox based on the given policy
/// and provides a unified `exec` method for running commands.
pub struct Sandbox {
    policy: policy::SandboxPolicy,
    approval_callback: Option<std::sync::Arc<dyn approval::SandboxApprovalCallback>>,
    whitelist: std::sync::Arc<std::sync::Mutex<approval::SandboxWhitelist>>,
}

impl Sandbox {
    /// Create a new sandbox with the given policy.
    pub fn new(policy: policy::SandboxPolicy) -> Result<Self, SandboxError> {
        Ok(Self {
            policy,
            approval_callback: None,
            whitelist: std::sync::Arc::new(
                std::sync::Mutex::new(approval::SandboxWhitelist::new()),
            ),
        })
    }

    /// Set an interactive approval callback.
    ///
    /// When set, commands that would be blocked by sandbox policy trigger
    /// the callback, allowing the user to AllowOnce, AllowAlways, or Deny.
    pub fn with_approval_callback(
        mut self,
        callback: std::sync::Arc<dyn approval::SandboxApprovalCallback>,
    ) -> Self {
        self.approval_callback = Some(callback);
        self
    }

    /// Execute a command inside the sandbox.
    ///
    /// The command runs with the restrictions defined by the sandbox policy.
    /// If sandbox execution fails and no approval callback is set, the
    /// command is blocked (fail-closed) and an error is returned.
    ///
    /// If an approval callback is set and the sandbox would block the command,
    /// the callback is invoked first to give the user a choice.
    pub async fn exec(
        &self,
        command: &str,
        cwd: Option<&std::path::Path>,
    ) -> Result<SandboxResult, SandboxError> {
        // Check whitelist first (AllowAlways rules)
        let is_whitelisted = {
            let wl = self.whitelist.lock().unwrap_or_else(|e| e.into_inner());
            wl.is_allowed(command)
        };
        if is_whitelisted {
            return self.exec_unrestricted(command, cwd).await;
        }

        // Try sandboxed execution
        let result = platform::exec_sandboxed(&self.policy, command, cwd).await;

        match result {
            Ok(sandbox_result) => Ok(sandbox_result),
            Err(SandboxError::ExecutionFailed(_)) | Err(SandboxError::PlatformUnavailable(_)) => {
                // Sandbox blocked or unavailable — ask for approval if callback is set
                if let Some(ref callback) = self.approval_callback {
                    let ctx = approval::SandboxApprovalContext {
                        command: command.to_string(),
                        policy: format!("{:?}", self.policy),
                        cwd: cwd.map(|p| p.to_path_buf()),
                        reason: "sandbox policy would block this command".to_string(),
                    };

                    match callback.on_sandbox_block(ctx).await {
                        approval::SandboxApprovalDecision::AllowOnce => {
                            self.exec_unrestricted(command, cwd).await
                        }
                        approval::SandboxApprovalDecision::AllowAlways { pattern } => {
                            {
                                let mut wl =
                                    self.whitelist.lock().unwrap_or_else(|e| e.into_inner());
                                wl.add_rule(&pattern);
                            }
                            self.exec_unrestricted(command, cwd).await
                        }
                        approval::SandboxApprovalDecision::Deny { reason } => {
                            Err(SandboxError::ExecutionFailed(format!(
                                "denied by interactive approval: {reason}"
                            )))
                        }
                    }
                } else {
                    // No callback set — fail closed: do not bypass the sandbox
                    Err(SandboxError::ExecutionFailed(format!(
                        "sandbox blocked command '{command}' and no approval callback is configured"
                    )))
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Execute a command without sandbox restrictions.
    async fn exec_unrestricted(
        &self,
        command: &str,
        cwd: Option<&std::path::Path>,
    ) -> Result<SandboxResult, SandboxError> {
        // Use a None-policy sandbox which just runs the command locally
        let none_sandbox =
            platform::exec_sandboxed(&policy::SandboxPolicy::None, command, cwd).await;
        none_sandbox
    }

    /// Get a reference to the current sandbox policy.
    pub fn policy(&self) -> &policy::SandboxPolicy {
        &self.policy
    }

    /// Get a reference to the whitelist.
    pub fn whitelist(&self) -> &std::sync::Mutex<approval::SandboxWhitelist> {
        &self.whitelist
    }

    /// Check if native sandboxing is available on the current platform.
    pub fn is_available() -> bool {
        platform::is_sandbox_available()
    }

    /// Get the name of the sandbox backend for the current platform.
    pub fn backend_name() -> &'static str {
        platform::backend_name()
    }
}
