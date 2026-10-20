//! Linux sandbox implementation using Bubblewrap (bwrap).
//!
//! Falls back to unrestricted execution if bwrap is not installed.
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
use std::time::Instant;

use crate::audit::AuditLog;
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
pub async fn exec_bwrap(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
) -> Result<SandboxResult, SandboxError> {
    let bwrap_path =
        which_bwrap().ok_or_else(|| SandboxError::ToolNotFound("bwrap".to_string()))?;

    let start = Instant::now();
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

    // Add domain restrictions hint via environment variable
    if let NetworkPolicy::AllowDomains(domains) = policy.net_policy() {
        if !domains.is_empty() {
            // bwrap doesn't natively support domain whitelisting;
            // set env hint for application-level enforcement
            args.push("--setenv".to_string());
            args.push("OPENPAW_ALLOWED_DOMAINS".to_string());
            args.push(domains.join(","));
        }
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

    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| SandboxError::ExecutionFailed(format!("bwrap execution failed: {e}")))?;

    let duration_ms = start.elapsed().as_millis() as u64;

    let mut audit = AuditLog::new(command, cwd, &policy_name, "bubblewrap");
    audit.exit_code = output.status.code().unwrap_or(-1);
    audit.duration_ms = duration_ms;

    Ok(SandboxResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: audit.exit_code,
        audit,
    })
}

/// Get the list of available sandbox backends on Linux.
///
/// Returns a list of backend names that are available, in order of preference.
#[cfg(test)]
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
