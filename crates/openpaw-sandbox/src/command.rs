//! Sandboxed command construction and execution helpers.

use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// 功能：将builder参数安全表示为shell字面值
    /// 参数：self包含已授权命令文本和字面参数
    /// 返回：拼好的命令文本；Windows不支持的参数由exec拒绝
    /// 错误：无，执行前另作支持性检查
    /// 状态：不执行任何进程
    /// 并发与取消：同步，无取消
    /// 权限：command本身是有意脚本文本；args不能注入新命令
    /// 关联：需求/设计 DES-OP-005；REQ-OP-006。
    /// Stability: unstable.
    pub fn full_command(&self) -> String {
        let mut command = self.command.clone();
        for arg in &self.args {
            command.push(' ');
            if !arg.is_empty()
                && arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_./:-".contains(c))
            {
                command.push_str(arg);
            } else if cfg!(windows) {
                command.push('"');
                command.push_str(arg);
                command.push('"');
            } else {
                command.push('\'');
                command.push_str(&arg.replace('\'', "'\"'\"'"));
                command.push('\'');
            }
        }
        command
    }

    /// Get a reference to the policy.
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Execute the command in the sandbox.
    /// 功能：运行构建好的命令并落实超时清理
    /// 参数：self为命令配置
    /// 返回：真实输出与审计
    /// 错误：执行失败或超时；超时也等实际清理
    /// 状态：启动原平台执行
    /// 并发与取消：超时发送token后等待，而不drop副作用future
    /// 权限：请求隔离不可绕过
    /// 关联：需求/设计 DES-OP-005；REQ-OP-006。
    /// Stability: unstable.
    pub async fn exec(self) -> Result<SandboxResult, SandboxError> {
        if !self.env.is_empty() {
            return Err(SandboxError::ExecutionFailed(
                "builder env overrides are not supported; refusing to ignore them".into(),
            ));
        }
        #[cfg(windows)]
        if self
            .args
            .iter()
            .any(|arg| arg.chars().any(|c| "&|<>^%!\"\r\n\0".contains(c)))
        {
            return Err(SandboxError::ExecutionFailed(
                "cmd builder cannot safely represent these literal arguments".into(),
            ));
        }
        let command = self.full_command();
        let cancel = tokio_util::sync::CancellationToken::new();
        let execution = crate::platform::exec_sandboxed_cancellable(
            &self.policy,
            &command,
            self.cwd.as_deref(),
            cancel.clone(),
        );
        tokio::pin!(execution);
        if let Some(timeout) = self.timeout {
            tokio::select! { biased;
                _=tokio::time::sleep(timeout)=> {cancel.cancel(); let _=execution.await; Err(SandboxError::Timeout(timeout.as_millis().try_into().unwrap_or(u64::MAX)))},
                result=&mut execution=>result,
            }
        } else {
            execution.await
        }
    }
}

/// 功能：复用托管进程实现普通本地命令
/// 参数：command/cwd为命令和根目录，cancel为token
/// 返回：真实输出和审计
/// 错误：创建、取消或回收错误
/// 状态：仅普通策略使用
/// 并发与取消：进程组或Job托管，取消等待清理
/// 权限：普通执行不声称有文件/网络隔离
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
pub(crate) async fn exec_unrestricted_cancellable(
    command: &str,
    cwd: Option<&Path>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<SandboxResult, SandboxError> {
    #[cfg(unix)]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", command]);
        crate::process::run_command(cmd, command, cwd, "None", "process_group", cancel).await
    }
    #[cfg(windows)]
    {
        crate::platform::windows::exec_local(command, cwd, cancel).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (command, cwd, cancel);
        Err(SandboxError::PlatformUnavailable(
            "managed execution unavailable".into(),
        ))
    }
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
        assert_eq!(
            cmd.full_command(),
            if cfg!(windows) {
                "echo \"hello world\""
            } else {
                "echo 'hello world'"
            }
        );
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
