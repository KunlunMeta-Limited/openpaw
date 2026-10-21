//! Environment abstraction for command execution.
//!
//! An [`Environment`] is the single execution seam behind the exec tools:
//! [`LocalEnvironment`] runs commands directly on the host, and
//! [`SandboxedEnvironment`] routes restricted/isolated policies through the
//! local sandbox crate (`openpaw-sandbox`). The bash tool judges command
//! safety first, then hands the command to one of these backends.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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
/// 用途：原工具的可注入执行环境
/// 字段：命令执行、文件复制、健康与shell类型
/// 来源去向：原bash工具到本地或受限后端
/// 约束：取消后等待实际执行结束；默认扩展兼容旧实现
/// 稳定性：新增协议无关接口
/// 关联：需求/设计 DES-OP-004；REQ-OP-005。
/// Stability: unstable.
pub trait Environment: Send + Sync {
    async fn exec(&self, command: &str, cwd: Option<&Path>) -> EnvironmentResult;
    /// 功能：可取消执行的兼容入口
    /// 参数：command/cwd为输入，cancel为调用token
    /// 返回：实际执行结果
    /// 错误：通过结果stderr/非零退出码报告
    /// 状态：默认兼容旧exec，不提前丢弃副作用
    /// 并发与取消：旧实现等待原exec；受控后端重载以杀树并等待
    /// 权限：不因取消或审批降低环境策略
    /// 关联：需求/设计 DES-OP-005；REQ-OP-006。
    /// Stability: unstable.
    async fn exec_cancellable(
        &self,
        command: &str,
        cwd: Option<&Path>,
        cancel: CancellationToken,
    ) -> EnvironmentResult {
        if cancel.is_cancelled() {
            return EnvironmentResult {
                stdout: String::new(),
                stderr: "command cancelled before execution".into(),
                exit_code: -1,
            };
        }
        self.exec(command, cwd).await
    }
    /// 功能：声明执行时实际使用的shell
    /// 参数：无
    /// 返回：明确shell类型
    /// 错误：无
    /// 状态：只读
    /// 并发与取消：无等待
    /// 权限：注入其他launcher的环境必须重载，不能按文本猜测
    /// 关联：需求/设计 DES-OP-004；REQ-OP-005。
    /// Stability: unstable.
    fn shell_type(&self) -> crate::bashast::ShellType {
        if cfg!(windows) {
            crate::bashast::ShellType::Cmd
        } else {
            crate::bashast::ShellType::Bash
        }
    }
    async fn push_file(&self, local: &Path, remote: &Path) -> Result<()>;
    async fn pull_file(&self, remote: &Path, local: &Path) -> Result<()>;
    async fn health_check(&self) -> bool;
    fn name(&self) -> &str;
}

/// Runs commands directly on the host — the historical bash tool path
/// (`cmd /C` on Windows, `sh -c` elsewhere) with the workspace as cwd, null
/// stdin and a managed process group / Job Object; cancellation waits for actual cleanup.
/// 用途：原普通本地执行环境
/// 字段：无
/// 来源去向：工具命令到原Sandbox普通托管执行
/// 约束：管理进程树不表示文件/网络隔离
/// 稳定性：保留原构造，延展取消行为
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
pub struct LocalEnvironment;

#[async_trait]
impl Environment for LocalEnvironment {
    async fn exec(&self, command: &str, cwd: Option<&Path>) -> EnvironmentResult {
        self.exec_cancellable(command, cwd, CancellationToken::new())
            .await
    }
    async fn exec_cancellable(
        &self,
        command: &str,
        cwd: Option<&Path>,
        cancel: CancellationToken,
    ) -> EnvironmentResult {
        sandbox_result(
            openpaw_sandbox::Sandbox::new(openpaw_sandbox::SandboxPolicy::None)
                .expect("sandbox construction is infallible")
                .exec_cancellable(command, cwd, cancel)
                .await,
        )
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

fn sandbox_result(
    result: std::result::Result<openpaw_sandbox::SandboxResult, openpaw_sandbox::SandboxError>,
) -> EnvironmentResult {
    match result {
        Ok(result) => EnvironmentResult {
            stdout: result.stdout,
            stderr: result.stderr,
            exit_code: result.exit_code,
        },
        Err(e) => EnvironmentResult {
            stdout: String::new(),
            stderr: format!("command execution failed: {e}"),
            exit_code: -1,
        },
    }
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
        self.exec_cancellable(command, cwd, CancellationToken::new())
            .await
    }
    async fn exec_cancellable(
        &self,
        command: &str,
        cwd: Option<&Path>,
        cancel: CancellationToken,
    ) -> EnvironmentResult {
        let cwd = cwd.unwrap_or(&self.workspace);
        match openpaw_sandbox::Sandbox::new(self.sandbox_policy.clone()) {
            Ok(sandbox) => {
                sandbox_result(sandbox.exec_cancellable(command, Some(cwd), cancel).await)
            }
            Err(e) => EnvironmentResult {
                stdout: String::new(),
                stderr: format!("sandbox init failed: {e}"),
                exit_code: -1,
            },
        }
    }

    async fn push_file(&self, local: &Path, remote: &Path) -> Result<()> {
        if self.policy == SandboxPolicy::Isolated {
            return Err(Error::ToolHost(
                "isolated environment forbids file writes".into(),
            ));
        }
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
        if self.policy == SandboxPolicy::Isolated {
            return Err(Error::ToolHost(
                "isolated environment forbids file writes".into(),
            ));
        }
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

/// 功能：直接构造指定环境，不嵌套阻塞runtime也不降级
/// 参数：kind为请求策略，workspace为根目录
/// 返回：对应环境；执行时验证实际后端
/// 错误：构造不探测运行能力，执行失败明确返回
/// 状态：无命令、目录或权限副作用
/// 并发与取消：适用current-thread和multi-thread runtime
/// 权限：受限请求永远不返回LocalEnvironment
/// 关联：需求/设计 DES-OP-005；REQ-OP-006。
/// Stability: unstable.
pub fn create_environment(kind: EnvironmentKind, workspace: PathBuf) -> Arc<dyn Environment> {
    match kind {
        EnvironmentKind::Local => Arc::new(LocalEnvironment),
        EnvironmentKind::Sandbox(policy) => Arc::new(SandboxedEnvironment::new(policy, workspace)),
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
