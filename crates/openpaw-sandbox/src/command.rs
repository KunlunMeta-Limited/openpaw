//! Sandboxed command construction and execution helpers.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::audit::AuditLog;
use crate::error::SandboxError;
use crate::policy::SandboxPolicy;
use crate::SandboxResult;

/// Builder for constructing sandboxed commands with specific options.
pub struct SandboxedCommand {
    /// The command to execute.
    command: String,
    /// Arguments to the command.
    args: Vec<String>,
    /// Working directory.
    cwd: Option<PathBuf>,
    /// Environment variables to set (in addition to inherited ones).
    env: Vec<(String, String)>,
    /// Execution timeout.
    timeout: Option<Duration>,
    /// The sandbox policy to apply.
    policy: SandboxPolicy,
}

impl SandboxedCommand {
    /// Create a new sandboxed command builder.
    pub fn new(command: &str, policy: SandboxPolicy) -> Self {
        Self {
            command: command.to_string(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            timeout: None,
            policy,
        }
    }

    /// Add an argument to the command.
    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.to_string());
        self
    }

    /// Add multiple arguments to the command.
    pub fn args(mut self, args: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_string()));
        self
    }

    /// Set the working directory.
    pub fn cwd(mut self, cwd: &Path) -> Self {
        self.cwd = Some(cwd.to_path_buf());
        self
    }

    /// Set an environment variable.
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// Set the execution timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Get the full command string (command + args).
    pub fn full_command(&self) -> String {
        let mut cmd = self.command.clone();
        for arg in &self.args {
            cmd.push(' ');
            if arg.contains(' ') || arg.contains('"') {
                cmd.push('"');
                cmd.push_str(&arg.replace('"', "\\\""));
                cmd.push('"');
            } else {
                cmd.push_str(arg);
            }
        }
        cmd
    }

    /// Get a reference to the policy.
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Execute the command in the sandbox.
    pub async fn exec(self) -> Result<SandboxResult, SandboxError> {
        let full_command = self.full_command();
        let cwd = self.cwd.as_deref();
        let timeout = self.timeout;

        // Enforce timeout during execution, not after.
        // Previously, the command ran to completion and the timeout was
        // checked post-hoc, making it ineffective for limiting runtime.
        let exec_fut = crate::platform::exec_sandboxed(&self.policy, &full_command, cwd);

        match timeout {
            Some(t) => match tokio::time::timeout(t, exec_fut).await {
                Ok(result) => result,
                Err(_) => Err(SandboxError::Timeout(t.as_millis() as u64)),
            },
            None => exec_fut.await,
        }
    }
}

/// Execute a command without any sandbox (fallback).
///
/// This is used when the platform doesn't support native sandboxing
/// or when the policy is `None`/`FullAccess`.
pub(crate) async fn exec_unrestricted(
    command: &str,
    cwd: Option<&Path>,
) -> Result<SandboxResult, SandboxError> {
    let start = std::time::Instant::now();

    let mut cmd = tokio::process::Command::new(if cfg!(windows) { "cmd" } else { "sh" });
    if cfg!(windows) {
        cmd.args(["/C", command]);
    } else {
        cmd.args(["-c", command]);
    }

    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| SandboxError::ExecutionFailed(format!("failed to execute command: {e}")))?;

    let duration_ms = start.elapsed().as_millis() as u64;

    let mut audit = AuditLog::new(command, cwd, "None", crate::platform::backend_name());
    audit.duration_ms = duration_ms;

    Ok(SandboxResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
        audit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_command_simple() {
        let cmd = SandboxedCommand::new("echo", SandboxPolicy::None);
        assert_eq!(cmd.full_command(), "echo");
    }

    #[test]
    fn full_command_with_args() {
        let cmd = SandboxedCommand::new("echo", SandboxPolicy::None)
            .arg("hello")
            .arg("world");
        assert_eq!(cmd.full_command(), "echo hello world");
    }

    #[test]
    fn full_command_with_spaces() {
        let cmd = SandboxedCommand::new("echo", SandboxPolicy::None).arg("hello world");
        assert_eq!(cmd.full_command(), "echo \"hello world\"");
    }

    #[test]
    fn command_builder_timeout() {
        let cmd = SandboxedCommand::new("sleep", SandboxPolicy::ReadOnly)
            .arg("10")
            .timeout(Duration::from_secs(5));
        assert_eq!(cmd.timeout, Some(Duration::from_secs(5)));
    }

    #[test]
    fn command_builder_cwd() {
        let cmd = SandboxedCommand::new("ls", SandboxPolicy::ReadOnly).cwd(Path::new("/tmp"));
        assert_eq!(cmd.cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn command_builder_policy() {
        let cmd = SandboxedCommand::new("test", SandboxPolicy::read_only());
        assert!(cmd.policy().is_restricted());
    }
}
