//! Environment abstraction for command execution.
//!
//! An [`Environment`] is the single execution seam behind the exec tools:
//! [`LocalEnvironment`] runs commands directly on the host, and
//! [`SandboxedEnvironment`] routes restricted/isolated policies through the
//! local sandbox crate (`openpaw-sandbox`). The bash tool judges command
//! safety first, then hands the command to one of these backends.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use openpaw_core::{Error, Result};

/// Raw result of a single command execution inside an [`Environment`].
pub struct EnvironmentResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Extract exit code from process status, distinguishing signal termination.
/// On Unix, when a process is killed by a signal, returns -signal_number
/// (e.g. -9 for SIGKILL) instead of conflating with exit code -1.
pub fn exit_code_from(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -signal;
        }
    }
    -1
}

#[async_trait]
pub trait Environment: Send + Sync {
    async fn exec(&self, command: &str, cwd: Option<&Path>) -> EnvironmentResult;
    async fn push_file(&self, local: &Path, remote: &Path) -> Result<()>;
    async fn pull_file(&self, remote: &Path, local: &Path) -> Result<()>;
    async fn health_check(&self) -> bool;
    fn name(&self) -> &str;
}

/// Runs commands directly on the host — the historical bash tool path
/// (`cmd /C` on Windows, `sh -c` elsewhere) with the workspace as cwd, null
/// stdin and kill-on-drop, so a timed-out command cannot outlive the
/// caller's timeout.
pub struct LocalEnvironment;

#[async_trait]
impl Environment for LocalEnvironment {
    async fn exec(&self, command: &str, cwd: Option<&Path>) -> EnvironmentResult {
        let mut cmd = tokio::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" });
        cmd.arg(if cfg!(windows) { "/C" } else { "-c" })
            .arg(command)
            // Null stdin so the command cannot inherit and block on the parent's.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Killed when the caller's timeout drops the child.
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        match cmd.output().await {
            Ok(output) => EnvironmentResult {
                stdout: decode_output(&output.stdout),
                stderr: decode_output(&output.stderr),
                exit_code: exit_code_from(&output.status),
            },
            Err(e) => EnvironmentResult {
                stdout: String::new(),
                stderr: format!("bash exec error: {e}"),
                exit_code: -1,
            },
        }
    }

    async fn push_file(&self, local: &Path, remote: &Path) -> Result<()> {
        tokio::fs::copy(local, remote)
            .await
            .map_err(|e| Error::ToolHost(format!("copy file: {e}")))?;
        Ok(())
    }

    async fn pull_file(&self, remote: &Path, local: &Path) -> Result<()> {
        tokio::fs::copy(remote, local)
            .await
            .map_err(|e| Error::ToolHost(format!("copy file: {e}")))?;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "local"
    }
}

/// Decode command output bytes as UTF-8 with lossy fallback.
fn decode_output(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Execution isolation level requested for an environment.
///
/// This is the coarse, caller-facing policy. [`SandboxedEnvironment`] maps it
/// onto the local sandbox crate's fine-grained policy when delegating:
/// `Restricted` becomes a workspace-writable sandbox, `Isolated` becomes a
/// read-only sandbox without network access.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPolicy {
    /// No sandboxing — commands run directly on the host.
    #[default]
    None,
    /// Workspace-writable sandbox; host reads and network are allowed.
    Restricted,
    /// Read-only sandbox without network access.
    Isolated,
}

/// Stub environment that always refuses to execute commands. Used when an
/// isolated policy is requested but the sandbox backend is unavailable —
/// fail closed rather than silently executing unsandboxed.
struct FailClosedEnvironment {
    reason: String,
}

#[async_trait]
impl Environment for FailClosedEnvironment {
    async fn exec(&self, _command: &str, _cwd: Option<&Path>) -> EnvironmentResult {
        EnvironmentResult {
            stdout: String::new(),
            stderr: self.reason.clone(),
            exit_code: -1,
        }
    }

    async fn push_file(&self, _local: &Path, _remote: &Path) -> Result<()> {
        Err(Error::ToolHost(self.reason.clone()))
    }

    async fn pull_file(&self, _remote: &Path, _local: &Path) -> Result<()> {
        Err(Error::ToolHost(self.reason.clone()))
    }

    async fn health_check(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        "fail-closed"
    }
}

/// Environment that delegates restricted/isolated execution to the local
/// sandbox crate. The `None` policy executes locally, exactly like
/// [`LocalEnvironment`].
pub struct SandboxedEnvironment {
    policy: SandboxPolicy,
    workspace: PathBuf,
    /// Fine-grained policy handed to the sandbox crate.
    sandbox_policy: openpaw_sandbox::SandboxPolicy,
}

impl SandboxedEnvironment {
    pub fn new(policy: SandboxPolicy, workspace: PathBuf) -> Self {
        let sandbox_policy = match &policy {
            SandboxPolicy::None => openpaw_sandbox::SandboxPolicy::None,
            SandboxPolicy::Restricted => openpaw_sandbox::SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![workspace.clone()],
                allowed_domains: vec![],
            },
            SandboxPolicy::Isolated => openpaw_sandbox::SandboxPolicy::ReadOnly,
        };
        Self {
            policy,
            workspace,
            sandbox_policy,
        }
    }

    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Validates that `path` resolves to a location inside the sandbox's
    /// writable area (the workspace). Returns the canonicalized path on
    /// success, or an error if the path escapes the allowed area — this keeps
    /// push_file/pull_file from reading or writing arbitrary files.
    async fn validate_writable_path(&self, path: &Path) -> Result<PathBuf> {
        // No sandbox restriction when the policy is None.
        if matches!(self.policy, SandboxPolicy::None) {
            return Ok(path.to_path_buf());
        }

        // Canonicalize: if the path already exists use it directly; otherwise
        // canonicalize the parent directory and re-attach the file name, so
        // destination paths that do not exist yet are handled too.
        let canonical = match tokio::fs::canonicalize(path).await {
            Ok(c) => c,
            Err(_) => {
                let parent = path.parent().ok_or_else(|| {
                    Error::ToolHost("sandboxed copy: path has no parent".to_string())
                })?;
                let file_name = path.file_name().ok_or_else(|| {
                    Error::ToolHost("sandboxed copy: path has no file name".to_string())
                })?;
                let parent_canonical = tokio::fs::canonicalize(parent).await.map_err(|e| {
                    Error::ToolHost(format!("sandboxed copy: cannot canonicalize parent: {e}"))
                })?;
                parent_canonical.join(file_name)
            }
        };

        let root_canonical = tokio::fs::canonicalize(&self.workspace)
            .await
            .map_err(|e| {
                Error::ToolHost(format!(
                    "sandboxed copy: cannot canonicalize workspace: {e}"
                ))
            })?;
        if canonical.starts_with(&root_canonical) {
            return Ok(canonical);
        }

        Err(Error::ToolHost(format!(
            "sandboxed copy: path {} is outside the allowed writable area",
            path.display()
        )))
    }

    /// Reject symlinks at `path` before copying: `symlink_metadata` does not
    /// follow the final component, so a swap-attack symlink is caught before
    /// the copy can follow it out of the sandbox.
    async fn check_no_symlink(&self, path: &Path) -> Result<()> {
        if matches!(self.policy, SandboxPolicy::None) {
            return Ok(());
        }
        if let Ok(meta) = tokio::fs::symlink_metadata(path).await {
            if meta.is_symlink() {
                return Err(Error::ToolHost(format!(
                    "sandboxed copy: refusing to copy via symlink {} (TOCTOU protection)",
                    path.display()
                )));
            }
        }
        // Path doesn't exist yet (common for copy destinations) — skip.
        Ok(())
    }
}

#[async_trait]
impl Environment for SandboxedEnvironment {
    async fn exec(&self, command: &str, cwd: Option<&Path>) -> EnvironmentResult {
        let effective_cwd = cwd.unwrap_or(&self.workspace);

        match self.policy {
            SandboxPolicy::None => LocalEnvironment.exec(command, Some(effective_cwd)).await,
            SandboxPolicy::Restricted | SandboxPolicy::Isolated => {
                // The sandbox crate itself fails closed: if the command is
                // blocked by policy or the backend is unavailable, `exec`
                // returns an error instead of running the command unsandboxed.
                match openpaw_sandbox::Sandbox::new(self.sandbox_policy.clone()) {
                    Ok(sandbox) => match sandbox.exec(command, Some(effective_cwd)).await {
                        Ok(result) => EnvironmentResult {
                            stdout: result.stdout,
                            stderr: result.stderr,
                            exit_code: result.exit_code,
                        },
                        Err(e) => EnvironmentResult {
                            stdout: String::new(),
                            stderr: format!("sandbox exec failed: {e}"),
                            exit_code: -1,
                        },
                    },
                    Err(e) => EnvironmentResult {
                        stdout: String::new(),
                        stderr: format!("sandbox init failed: {e}"),
                        exit_code: -1,
                    },
                }
            }
        }
    }

    async fn push_file(&self, local: &Path, remote: &Path) -> Result<()> {
        self.validate_writable_path(local).await?;
        self.validate_writable_path(remote).await?;
        self.check_no_symlink(local).await?;
        self.check_no_symlink(remote).await?;
        tokio::fs::copy(local, remote)
            .await
            .map_err(|e| Error::ToolHost(format!("sandboxed copy file: {e}")))?;
        // Post-copy verification: re-check the destination in case it was
        // swapped to point outside the sandbox during the copy window.
        self.validate_writable_path(remote).await?;
        Ok(())
    }

    async fn pull_file(&self, remote: &Path, local: &Path) -> Result<()> {
        self.validate_writable_path(remote).await?;
        self.validate_writable_path(local).await?;
        self.check_no_symlink(remote).await?;
        self.check_no_symlink(local).await?;
        tokio::fs::copy(remote, local)
            .await
            .map_err(|e| Error::ToolHost(format!("sandboxed copy file: {e}")))?;
        self.validate_writable_path(local).await?;
        Ok(())
    }

    async fn health_check(&self) -> bool {
        match self.policy {
            SandboxPolicy::None => true,
            SandboxPolicy::Restricted | SandboxPolicy::Isolated => {
                openpaw_sandbox::Sandbox::is_available()
            }
        }
    }

    fn name(&self) -> &str {
        match self.policy {
            SandboxPolicy::None => "local",
            SandboxPolicy::Restricted => "sandboxed-restricted",
            SandboxPolicy::Isolated => "sandboxed-isolated",
        }
    }
}

/// Selects which [`Environment`] backend `create_environment` should build.
pub enum EnvironmentKind {
    /// No sandboxing — commands run directly on the host.
    Local,
    /// Sandbox via [`SandboxPolicy`] (restricted token on Windows, bwrap on
    /// Linux, Seatbelt on macOS — per the local sandbox crate's backend).
    Sandbox(SandboxPolicy),
}

pub fn create_environment(kind: EnvironmentKind, workspace: PathBuf) -> Arc<dyn Environment> {
    match kind {
        EnvironmentKind::Local => Arc::new(LocalEnvironment),
        EnvironmentKind::Sandbox(policy) => {
            let sandboxed = SandboxedEnvironment::new(policy.clone(), workspace);
            // Handle the case where we might already be inside a tokio
            // runtime: `block_in_place` avoids "cannot start a runtime from
            // within a runtime" panics.
            let health_ok = match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    tokio::task::block_in_place(|| handle.block_on(sandboxed.health_check()))
                }
                Err(_) => match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt.block_on(sandboxed.health_check()),
                    Err(_) => false,
                },
            };
            if health_ok {
                Arc::new(sandboxed)
            } else {
                // Fail closed for Isolated: do not silently downgrade to
                // unsandboxed local execution.
                match policy {
                    SandboxPolicy::Isolated => Arc::new(FailClosedEnvironment {
                        reason: "Isolated sandbox unavailable: health check failed".to_string(),
                    }),
                    _ => Arc::new(LocalEnvironment),
                }
            }
        }
    }
}

/// Backward-compatible helper: creates an environment from a sandbox policy.
pub fn create_environment_from_policy(
    policy: &SandboxPolicy,
    workspace: PathBuf,
) -> Arc<dyn Environment> {
    match policy {
        SandboxPolicy::None => create_environment(EnvironmentKind::Local, workspace),
        SandboxPolicy::Restricted | SandboxPolicy::Isolated => {
            create_environment(EnvironmentKind::Sandbox(policy.clone()), workspace)
        }
    }
}
