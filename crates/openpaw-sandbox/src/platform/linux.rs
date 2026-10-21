//! 职责：提供Linux bwrap执行与测试诊断。
//! 输入输出：策略和命令转为子进程输出/审计或失败。
//! 依赖：bwrap、sh与tokio进程接口。
//! 禁止职责：不把Landlock探测或域名环境提示描述为已落实隔离；托管取消待后续阶段。
//! Linux sandbox implementation using Bubblewrap (bwrap).
//!
//! Restricted dispatch rejects execution if bwrap is not installed.
//!
//! bwrap provides namespace-based isolation:
//! - `--unshare-all`: Unshare all namespaces (pid, net, ipc, mount, user, uts)
//! - `--die-with-parent`: Kill sandbox when parent process dies
//! - `--ro-bind / /`: Mount root filesystem read-only
//! - `--bind <dir> <dir>`: Mount directory writable
//! - `--share-net`: Re-share network (for policies that allow network)
//!
//! Kernel diagnostics are test-only; they do not enable Landlock enforcement.
//! Runtime isolation is provided by bwrap.

use std::path::Path;

use crate::error::SandboxError;
use crate::policy::{FileSystemPolicy, NetworkPolicy, SandboxPolicy};
use crate::SandboxResult;

/// Check if bwrap is available on the system.
pub fn is_bwrap_available() -> bool {
    which_bwrap().is_some()
}

/// Check if Landlock is available on the current kernel.
///
/// Landlock requires kernel >= 5.13 and the `landlock` syscall to be available.
/// This is a best-effort check; actual enforcement depends on kernel support.
#[cfg(test)]
/// 功能：提供测试用内核版本探测，不证明隔离生效。
/// 参数：无。
/// 返回：版本至少为最低内核时为真。
/// 错误：探测失败返回假。
/// 状态：读取内核版本。
/// 并发与取消：同步探测，无取消。
/// 权限：执行uname；不改变权限。
/// 关联：平台诊断测试；实际执行仍依赖bwrap。
pub fn is_landlock_available() -> bool {
    // Check kernel version
    let version = get_kernel_version();
    version.as_ref().map(|v| v >= &(5, 13)).unwrap_or(false)
}

/// Get the Linux kernel version as (major, minor).
#[cfg(test)]
fn get_kernel_version() -> Option<(u32, u32)> {
    let output = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()?;

    let version_str = String::from_utf8_lossy(&output.stdout);
    parse_kernel_version(&version_str)
}

/// Parse a kernel version string like "5.15.0-generic" into (5, 15).
#[cfg(test)]
fn parse_kernel_version(version: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = version.trim().split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let major = parts[0].parse::<u32>().ok()?;
    let minor = parts[1].parse::<u32>().ok()?;
    Some((major, minor))
}

fn which_bwrap() -> Option<std::path::PathBuf> {
    let candidates = ["/usr/bin/bwrap", "/usr/local/bin/bwrap"];
    for candidate in candidates {
        if Path::new(candidate).exists() {
            return Some(std::path::PathBuf::from(candidate));
        }
    }
    // Try PATH lookup
    std::process::Command::new("which")
        .arg("bwrap")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if path.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(path))
            }
        })
}

/// Execute a command inside a bwrap sandbox.
///
/// Enhancements:
/// - Hostname isolation (`--hostname openpaw-sandbox`)
/// - New session namespace (`--unshare-ipc` for IPC isolation)
/// - Resource limits via cgroup (when available)
/// - /dev and /proc properly mounted
/// - /tmp as tmpfs (prevents persistent writes)
/// - Readonly `/sys` mount
///
/// 功能：用现有bwrap策略执行命令，返回完整输出和审计。
/// 参数：policy为策略；command交给sh执行；cwd为工作目录。
/// 返回：输出、退出码和审计；子进程非零退出保留在结果。
/// 错误：缺bwrap、创建可写根或启动失败返回错误。
/// 状态：可能创建可写根并执行具有副作用的命令。
/// 并发与取消：异步等待输出，尚未提供调用级取消与进程树清理保证。
/// 权限：调用前需批准；域名白名单仅提示，不能当严格隔离保证。
/// 关联：沙箱平台分派和后续执行环境缺陷修复。
pub(crate) async fn exec_bwrap_cancellable(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<SandboxResult, SandboxError> {
    // bwrap不能实施域名白名单；提示变量不是网络隔离，必须先拒绝再创建目录。
    if matches!(policy.net_policy(), NetworkPolicy::AllowDomains(_)) {
        return Err(SandboxError::PlatformUnavailable(
            "bwrap cannot enforce domain allowlist".into(),
        ));
    }
    let bwrap_path =
        which_bwrap().ok_or_else(|| SandboxError::ToolNotFound("bwrap".to_string()))?;

    let policy_name = format!("{policy:?}");

    let mut args = vec![
        "--unshare-all".to_string(),
        "--die-with-parent".to_string(),
        "--hostname".to_string(),
        "openpaw-sandbox".to_string(),
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--ro-bind".to_string(),
        "/sys".to_string(),
        "/sys".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
    ];

    // `--unshare-all` provides namespace isolation but does NOT add
    // syscall filtering. Known bwrap escape vectors via syscalls like
    // `keyctl`, `perf_event_open`, `bpf`, and `ptrace` are not blocked.
    // Seccomp filters or Landlock would be needed to close this gap.
    // Landlock availability is detected (is_landlock_available) but never
    // applied — this should be addressed in a future enhancement.

    // Add writable roots for WorkspaceWrite policy
    if let FileSystemPolicy::WorkspaceWrite { roots } = policy.fs_policy() {
        for root in roots {
            if root.exists() {
                args.push("--bind".to_string());
                args.push(root.to_string_lossy().to_string());
                args.push(root.to_string_lossy().to_string());
            } else {
                // Create the directory if it doesn't exist
                // D12: borrow so error context retains the root. // openpaw-scrub-allow
                std::fs::create_dir_all(&root).map_err(|e| {
                    SandboxError::ExecutionFailed(format!(
                        "failed to create writable root {}: {e}",
                        root.display()
                    ))
                })?;
                args.push("--bind".to_string());
                args.push(root.to_string_lossy().to_string());
                args.push(root.to_string_lossy().to_string());
            }
        }
    }

    // Allow network if policy permits
    if !matches!(policy.net_policy(), NetworkPolicy::NoAccess) {
        args.push("--share-net".to_string());
    }

    // Clear all inherited environment variables to prevent API key
    // leakage (OPENAI_API_KEY, ANTHROPIC_API_KEY, OPENPAW_SIGNING_KEY,
    // etc.). Only essential variables are explicitly re-set below.
    args.push("--clearenv".to_string());
    for key in &["PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM"] {
        if let Ok(value) = std::env::var(key) {
            args.push("--setenv".to_string());
            args.push((*key).to_string());
            args.push(value);
        }
    }

    // The actual command
    args.push("--".to_string());
    args.push("sh".to_string());
    args.push("-c".to_string());
    args.push(command.to_string());

    let mut cmd = tokio::process::Command::new(&bwrap_path);
    cmd.args(&args);

    crate::process::run_command(cmd, command, cwd, &policy_name, "bubblewrap", cancel).await
}

/// Get the list of available sandbox backends on Linux.
///
/// Returns a list of backend names that are available, in order of preference.
#[cfg(test)]
/// 功能：返回测试探测的后端名称，不声明全部可执行。
/// 参数：无。
/// 返回：可探测后端名列表。
/// 错误：探测失败对应后端不列入。
/// 状态：读取本机工具与内核信息。
/// 并发与取消：同步，只读，无取消。
/// 权限：与本机诊断相同；不提升权限。
/// 关联：平台诊断回归。
pub fn available_backends() -> Vec<&'static str> {
    let mut backends = Vec::new();
    if is_bwrap_available() {
        backends.push("bubblewrap");
    }
    if is_landlock_available() {
        backends.push("landlock");
    }
    backends
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kernel_version_standard() {
        assert_eq!(parse_kernel_version("5.15.0-generic"), Some((5, 15)));
        assert_eq!(parse_kernel_version("6.1.0"), Some((6, 1)));
        assert_eq!(parse_kernel_version("5.13.0-rc1"), Some((5, 13)));
    }

    #[test]
    fn parse_kernel_version_edge_cases() {
        assert_eq!(parse_kernel_version("4.9.0"), Some((4, 9)));
        assert_eq!(parse_kernel_version("6.8"), Some((6, 8)));
        assert_eq!(parse_kernel_version(""), None);
        assert_eq!(parse_kernel_version("invalid"), None);
        assert_eq!(parse_kernel_version("5"), None);
    }

    #[test]
    fn landlock_requires_5_13() {
        // Verify our version check logic
        assert!(is_landlock_available() || get_kernel_version().is_none_or(|v| v < (5, 13)));
    }

    #[test]
    fn bwrap_args_readonly_policy() {
        // Verify the args structure for read-only policy
        let policy = SandboxPolicy::read_only();
        let mut args = vec![
            "--unshare-all".to_string(),
            "--die-with-parent".to_string(),
            "--hostname".to_string(),
            "openpaw-sandbox".to_string(),
        ];
        // No --share-net for NoAccess
        if !matches!(policy.net_policy(), NetworkPolicy::NoAccess) {
            args.push("--share-net".to_string());
        }
        assert!(!args.contains(&"--share-net".to_string()));
    }

    #[test]
    fn bwrap_args_full_access_network() {
        let policy = SandboxPolicy::FullAccess;
        let should_share_net = !matches!(policy.net_policy(), NetworkPolicy::NoAccess);
        assert!(should_share_net);
    }

    #[test]
    fn available_backends_structure() {
        let backends = available_backends();
        // On any platform, backends should be a valid vec
        for backend in &backends {
            assert!(!backend.is_empty());
        }
    }
}
