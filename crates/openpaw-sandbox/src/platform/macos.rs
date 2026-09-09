//! macOS sandbox implementation using Seatbelt (sandbox-exec).
//!
//! Uses `sandbox-exec -p <profile>` to execute commands with a
//! dynamically generated SBPL (Seatbelt Profile Language) profile.
//!
//! This is the same approach used by the `SandboxedEnvironment` in the
//! upstream tools crate, but extracted into the sandbox crate with
//! enhanced policy support.
//!
//! Enhanced with:
//! - Proper `/usr` and system paths read access
//! - DNS resolution allowlist for domain-restricted policies
//! - Process execution from standard paths
//! - Home directory read access for basic functionality
//! - `/var` tmpdir access for temporary files

use std::path::Path;
use std::time::Instant;

use crate::audit::AuditLog;
use crate::error::SandboxError;
use crate::policy::{FileSystemPolicy, NetworkPolicy, SandboxPolicy};
use crate::SandboxResult;

/// Check if sandbox-exec is available on the system.
pub fn is_seatbelt_available() -> bool {
    Path::new("/usr/bin/sandbox-exec").exists()
}

/// Execute a command inside a Seatbelt sandbox.
///
/// Enhancements:
/// - Allow reading from standard system paths (/usr, /System, /Library)
/// - Allow home directory read access for basic operation
/// - DNS resolution support for domain-restricted policies
/// - Process execution from standard system paths
/// - Temporary file access via /var/folders
pub async fn exec_seatbelt(
    policy: &SandboxPolicy,
    command: &str,
    cwd: Option<&Path>,
) -> Result<SandboxResult, SandboxError> {
    if !is_seatbelt_available() {
        return Err(SandboxError::ToolNotFound("sandbox-exec".to_string()));
    }

    let start = Instant::now();
    let policy_name = format!("{policy:?}");
    let profile = build_seatbelt_profile(policy);

    let mut cmd = tokio::process::Command::new("/usr/bin/sandbox-exec");
    cmd.args(["-p", &profile, "sh", "-c", command]);

    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| SandboxError::ExecutionFailed(format!("sandbox-exec failed: {e}")))?;

    let duration_ms = start.elapsed().as_millis() as u64;

    let mut audit = AuditLog::new(command, cwd, &policy_name, "seatbelt");
    audit.exit_code = output.status.code().unwrap_or(-1);
    audit.duration_ms = duration_ms;

    Ok(SandboxResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: audit.exit_code,
        audit,
    })
}

/// Build a Seatbelt SBPL profile from the sandbox policy.
///
/// Generates a complete SBPL profile with:
/// - Default deny rule
/// - File system access rules based on policy
/// - Network access rules based on policy
/// - System path read access for basic operation
/// - Process execution and fork permissions
fn build_seatbelt_profile(policy: &SandboxPolicy) -> String {
    let mut rules = Vec::new();

    // Default: deny everything
    rules.push("(version 1)".to_string());
    rules.push("(deny default)".to_string());

    // Allow reading from system paths (needed for basic operation)
    rules.push("(allow file-read* (subpath \"/usr\"))".to_string());
    rules.push("(allow file-read* (subpath \"/System\"))".to_string());
    rules.push("(allow file-read* (subpath \"/Library\"))".to_string());
    rules.push("(allow file-read* (subpath \"/bin\"))".to_string());
    rules.push("(allow file-read* (subpath \"/sbin\"))".to_string());

    // Allow reading home directory (needed for shell configs, etc.)
    if let Some(home) = dirs::home_dir() {
        rules.push(format!(
            "(allow file-read* (subpath \"{}\"))",
            home.to_string_lossy()
        ));
    }

    // File system rules
    match policy.fs_policy() {
        FileSystemPolicy::FullAccess => {
            rules.push("(allow file-read*)".to_string());
            rules.push("(allow file-write*)".to_string());
        }
        FileSystemPolicy::ReadOnly => {
            rules.push("(allow file-read*)".to_string());
        }
        FileSystemPolicy::WorkspaceWrite { roots } => {
            rules.push("(allow file-read*)".to_string());
            for root in roots {
                let path = root.to_string_lossy();
                rules.push(format!(r#"(allow file-write* (subpath "{path}"))"#));
            }
        }
    }

    // For non-FullAccess policies, deny access to sensitive directories
    // in the home folder to prevent exposure of API keys, SSH keys, and cloud
    // credentials (~/.ssh, ~/.aws, ~/.netrc, ~/.openpaw/config.toml).
    // These deny rules are placed AFTER the broad file-read* allow so they
    // take precedence (SBPL: more specific subpath rules win over broad rules;
    // ties broken by last-match). Note: ~/.openpaw/ remains accessible
    // except config.toml, allowing tools to read other OpenPaw data (e.g.,
    // skills) while protecting the config that may contain secrets (e.g.,
    // API keys).
    // This may break tools that legitimately need ~/.ssh or ~/.aws access.
    if !matches!(policy.fs_policy(), FileSystemPolicy::FullAccess) {
        if let Some(home) = dirs::home_dir() {
            let home_str = home.to_string_lossy();
            for sensitive in &[".ssh", ".aws", ".netrc", ".openpaw/config.toml"] {
                rules.push(format!(
                    "(deny file-read* file-write* (subpath \"{home_str}/{sensitive}\"))"
                ));
            }
        }
    }

    // Network rules
    match policy.net_policy() {
        NetworkPolicy::FullAccess => {
            rules.push("(allow network*)".to_string());
        }
        NetworkPolicy::NoAccess => {
            // Already denied by default
        }
        NetworkPolicy::AllowDomains(domains) => {
            // Seatbelt (SBPL) does not support fine-grained domain-level
            // network filtering. The `host` filter resolves hostnames to IP
            // addresses at profile compilation time and cannot enforce per-
            // domain restrictions. Previously, a wildcard `(host "*")` granted
            // full outbound access, making the domain-specific rules completely
            // redundant and giving a false sense of security.
            //
            // Since domain filtering is not effective, we explicitly allow all
            // outbound network. The OPENPAW_ALLOWED_DOMAINS env hint provides
            // application-level enforcement as a fallback.
            rules.push("(allow network-outbound (host \"localhost\"))".to_string());
            rules.push("(allow network*)".to_string());
            let _ = domains; // domain list not effective for Seatbelt filtering
        }
    }

    // Allowing process-exec from /usr, /bin, /sbin means alternative
    // interpreters (perl, ruby, python3, node, etc.) can be executed,
    // potentially bypassing the command whitelist. A full fix would require
    // a command-specific whitelist, which is complex.
    // This is a known limitation: operators should be aware that any binary
    // under these paths can be executed inside the sandbox.
    rules.push("(allow process-exec (subpath \"/usr\"))".to_string());
    rules.push("(allow process-exec (subpath \"/bin\"))".to_string());
    rules.push("(allow process-exec (subpath \"/sbin\"))".to_string());
    rules.push("(allow process-fork)".to_string());

    // Allow signal handling
    rules.push("(allow signal)".to_string());

    // Allow sysctl access (needed for basic operation)
    rules.push("(allow sysctl-read)".to_string());

    // Temp file access — ReadOnly should not allow writing to /tmp or
    // /var/folders to prevent data leakage via temp files. WorkspaceWrite
    // retains write access since the sandboxed process needs temp space.
    // A future enhancement could use a private temp directory (e.g., a
    // subdirectory of the workspace) instead of host /tmp.
    match policy.fs_policy() {
        FileSystemPolicy::ReadOnly => {
            // /tmp and /var/folders remain read-only (covered by file-read* allow).
        }
        _ => {
            rules.push("(allow file-write* (subpath \"/var/folders\"))".to_string());
            rules.push("(allow file-write* (subpath \"/tmp\"))".to_string());
        }
    }

    rules.join("\n")
}

/// Get the Seatbelt profile string for a given policy (for debugging/testing).
pub fn get_seatbelt_profile(policy: &SandboxPolicy) -> String {
    build_seatbelt_profile(policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seatbelt_profile_read_only() {
        let policy = SandboxPolicy::read_only();
        let profile = build_seatbelt_profile(&policy);
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-read*)"));
        assert!(!profile.contains("(allow file-write*)"));
        assert!(!profile.contains("(allow network*)"));
    }

    #[test]
    fn seatbelt_profile_full_access() {
        let policy = SandboxPolicy::FullAccess;
        let profile = build_seatbelt_profile(&policy);
        assert!(profile.contains("(allow file-read*)"));
        assert!(profile.contains("(allow file-write*)"));
        assert!(profile.contains("(allow network*)"));
    }

    #[test]
    fn seatbelt_profile_workspace_write() {
        let policy = SandboxPolicy::workspace_write(vec!["/tmp/work"], vec!["api.example.com"]);
        let profile = build_seatbelt_profile(&policy);
        assert!(profile.contains(r#"allow file-write* (subpath "/tmp/work")"#));
        // AllowDomains now uses (allow network*) since Seatbelt can't
        // filter by domain. Domain-specific rules are no longer emitted.
        assert!(profile.contains("(allow network*)"));
    }

    #[test]
    fn seatbelt_profile_system_paths() {
        let policy = SandboxPolicy::read_only();
        let profile = build_seatbelt_profile(&policy);
        assert!(profile.contains("(allow file-read* (subpath \"/usr\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/System\"))"));
        assert!(profile.contains("(allow process-exec (subpath \"/usr\"))"));
    }

    #[test]
    fn seatbelt_profile_temp_file_access() {
        let policy = SandboxPolicy::read_only();
        let profile = build_seatbelt_profile(&policy);
        // ReadOnly no longer allows writing to /tmp or /var/folders
        assert!(!profile.contains("(allow file-write* (subpath \"/var/folders\"))"));
        assert!(!profile.contains("(allow file-write* (subpath \"/tmp\"))"));
        // WorkspaceWrite should still have temp file write access
        let policy = SandboxPolicy::workspace_write(vec!["/tmp/work"], vec![]);
        let profile = build_seatbelt_profile(&policy);
        assert!(profile.contains("(allow file-write* (subpath \"/var/folders\"))"));
        assert!(profile.contains("(allow file-write* (subpath \"/tmp\"))"));
    }

    #[test]
    fn seatbelt_profile_network_no_access() {
        let policy = SandboxPolicy::read_only();
        let profile = build_seatbelt_profile(&policy);
        // ReadOnly has NoAccess network, so no network rules should be present
        assert!(!profile.contains("(allow network*)"));
    }

    #[test]
    fn seatbelt_profile_network_allow_domains() {
        let policy = SandboxPolicy::workspace_write(vec!["/tmp"], vec!["api.example.com"]);
        let profile = build_seatbelt_profile(&policy);
        // AllowDomains now uses (allow network*) since Seatbelt can't
        // filter by domain. Domain-specific rules are no longer emitted.
        assert!(profile.contains("(allow network*)"));
        assert!(!profile.contains(r#"(allow network-outbound (host "*"))"#));
    }

    #[test]
    fn get_seatbelt_profile_public_api() {
        let policy = SandboxPolicy::read_only();
        let profile = get_seatbelt_profile(&policy);
        assert!(!profile.is_empty());
        assert!(profile.contains("(version 1)"));
    }
}
