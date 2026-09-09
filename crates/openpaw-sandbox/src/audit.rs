//! Audit logging for sandbox operations.
//!
//! Every sandboxed command produces an `AuditLog` that records what
//! happened during execution. Logs can be stored via `AuditStore`
//! implementations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::error::SandboxError;

/// A record of a single sandboxed command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    /// When the command was started.
    pub timestamp: DateTime<Utc>,
    /// The command that was executed.
    pub command: String,
    /// Working directory for the command.
    pub cwd: Option<PathBuf>,
    /// Name of the sandbox policy applied.
    pub policy: String,
    /// Name of the platform backend used.
    pub platform: String,
    /// Exit code of the command.
    pub exit_code: i32,
    /// Wall-clock duration of the command in milliseconds.
    pub duration_ms: u64,
    /// Files that were written to during execution.
    pub files_written: Vec<PathBuf>,
    /// Files that the sandbox denied read access to.
    pub files_read_denied: Vec<PathBuf>,
    /// Network connection attempts during execution.
    pub network_attempts: Vec<NetworkAttempt>,
}

/// A network connection attempt made by a sandboxed command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkAttempt {
    /// The domain that was accessed.
    pub domain: String,
    /// The port that was used.
    pub port: u16,
    /// Whether the connection was allowed by the policy.
    pub allowed: bool,
}

impl AuditLog {
    /// Create a new audit log with the given command and policy info.
    pub fn new(command: &str, cwd: Option<&Path>, policy: &str, platform: &str) -> Self {
        Self {
            timestamp: Utc::now(),
            command: command.to_string(),
            cwd: cwd.map(PathBuf::from),
            policy: policy.to_string(),
            platform: platform.to_string(),
            exit_code: -1,
            duration_ms: 0,
            files_written: Vec::new(),
            files_read_denied: Vec::new(),
            network_attempts: Vec::new(),
        }
    }
}

/// Storage backend for audit logs.
pub trait AuditStore: Send + Sync {
    /// Store an audit log entry.
    fn store(&self, log: &AuditLog) -> Result<(), SandboxError>;
}

/// File-based audit log storage.
///
/// Appends JSON-lines to a file, one entry per line.
pub struct FileAuditStore {
    path: PathBuf,
}

impl FileAuditStore {
    /// Create a new file-based audit store that writes to the given path.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Create a store in the default location (`~/.openpaw/sandbox-audit.jsonl`).
    pub fn default_location() -> Result<Self, SandboxError> {
        let home = dirs::home_dir()
            .ok_or_else(|| SandboxError::AuditLog("cannot determine home directory".to_string()))?;
        let dir = home.join(".openpaw");
        std::fs::create_dir_all(&dir).map_err(|e| SandboxError::AuditLog(e.to_string()))?;
        Ok(Self::new(dir.join("sandbox-audit.jsonl")))
    }
}

impl AuditStore for FileAuditStore {
    fn store(&self, log: &AuditLog) -> Result<(), SandboxError> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| SandboxError::AuditLog(format!("failed to open audit file: {e}")))?;
        let line = serde_json::to_string(log)
            .map_err(|e| SandboxError::AuditLog(format!("failed to serialize audit log: {e}")))?;
        writeln!(file, "{line}")
            .map_err(|e| SandboxError::AuditLog(format!("failed to write audit log: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn audit_log_new() {
        let log = AuditLog::new("ls -la", None, "ReadOnly", "windows");
        assert_eq!(log.command, "ls -la");
        assert_eq!(log.policy, "ReadOnly");
        assert_eq!(log.platform, "windows");
        assert_eq!(log.exit_code, -1);
        assert!(log.files_written.is_empty());
    }

    #[test]
    fn file_audit_store_writes_jsonl() {
        let dir = TempDir::new().unwrap();
        let store = FileAuditStore::new(dir.path().join("audit.jsonl"));

        let log1 = AuditLog::new("cmd1", None, "ReadOnly", "test");
        let log2 = AuditLog::new("cmd2", None, "FullAccess", "test");

        store.store(&log1).unwrap();
        store.store(&log2).unwrap();

        let content = std::fs::read_to_string(dir.path().join("audit.jsonl")).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        let parsed1: AuditLog = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed1.command, "cmd1");
        let parsed2: AuditLog = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(parsed2.command, "cmd2");
    }

    #[test]
    fn audit_log_serialization_roundtrip() {
        let mut log = AuditLog::new("test", Some(Path::new("/tmp")), "WorkspaceWrite", "linux");
        log.exit_code = 0;
        log.duration_ms = 42;
        log.files_written.push(PathBuf::from("/tmp/out.txt"));
        log.network_attempts.push(NetworkAttempt {
            domain: "example.com".to_string(),
            port: 443,
            allowed: true,
        });

        let json = serde_json::to_string(&log).unwrap();
        let parsed: AuditLog = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.command, "test");
        assert_eq!(parsed.exit_code, 0);
        assert_eq!(parsed.files_written.len(), 1);
        assert_eq!(parsed.network_attempts.len(), 1);
    }
}
