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
mod process;

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

    /// 功能：保留旧审批回调配置入口
    /// 参数：callback为旧回调实现
    /// 返回：更新配置后的Sandbox
    /// 错误：无
    /// 状态：仅保存引用，当前exec不调用它来改变策略
    /// 并发与取消：线程安全引用，不等待
    /// 权限：工具批准通过核心上下文；回调不能解除请求的隔离
    /// 关联：需求/设计 DES-OP-005；REQ-OP-006。
    /// Stability: unstable.
    pub fn with_approval_callback(
        mut self,
        callback: std::sync::Arc<dyn approval::SandboxApprovalCallback>,
    ) -> Self {
        self.approval_callback = Some(callback);
        self
    }

    /// 功能：使用原后端执行命令，限制不可用时拒绝
    /// 参数：command/cwd为命令和工作目录
    /// 返回：真实输出与审计
    /// 错误：后端不可用或执行失败
    /// 状态：委托可取消入口
    /// 并发与取消：无外部token时仍托管进程并等待回收
    /// 权限：审批回调/白名单不能解除调用者请求的隔离
    /// 关联：需求/设计 DES-OP-005；REQ-OP-006。
    /// Stability: unstable.
    pub async fn exec(
        &self,
        command: &str,
        cwd: Option<&std::path::Path>,
    ) -> Result<SandboxResult, SandboxError> {
        platform::exec_sandboxed(&self.policy, command, cwd).await
    }
    /// 功能：贯通调用取消到原平台后端
    /// 参数：command/cwd为输入，cancel为调用token
    /// 返回：清理完成的真实结果
    /// 错误：限制不可落实、取消或系统错误
    /// 状态：不读取白名单以降级受限请求
    /// 并发与取消：取消杀进程树且等待实际回收
    /// 权限：人工批准不能变更请求的隔离级别
    /// 关联：需求/设计 DES-OP-005；REQ-OP-006。
    /// Stability: unstable.
    pub async fn exec_cancellable(
        &self,
        command: &str,
        cwd: Option<&std::path::Path>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<SandboxResult, SandboxError> {
        platform::exec_sandboxed_cancellable(&self.policy, command, cwd, cancel).await
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
