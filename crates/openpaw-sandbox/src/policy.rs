//! Sandbox policy definitions.
//!
//! Policies control what a sandboxed command can access:
//! - File system: read-only, workspace-writable, or full access
//! - Network: no access, domain-restricted, or full access

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level sandbox policy combining file system and network restrictions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SandboxPolicy {
    /// No sandboxing — the command runs unrestricted.
    #[default]
    None,
    /// Read-only file system + no network access.
    ReadOnly,
    /// Workspace directories are writable, specific domains are accessible.
    WorkspaceWrite {
        /// Paths that the sandboxed process may write to.
        writable_roots: Vec<PathBuf>,
        /// Domains the sandboxed process may connect to.
        allowed_domains: Vec<String>,
    },
    /// Full file system and network access (equivalent to None but with audit logging).
    FullAccess,
}

impl SandboxPolicy {
    /// Convenience constructor for a read-only policy.
    pub fn read_only() -> Self {
        Self::ReadOnly
    }

    /// Convenience constructor for a workspace-write policy.
    pub fn workspace_write(writable_roots: Vec<&str>, allowed_domains: Vec<&str>) -> Self {
        Self::WorkspaceWrite {
            writable_roots: writable_roots.iter().map(|s| PathBuf::from(*s)).collect(),
            allowed_domains: allowed_domains.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// Get the file system component of this policy.
    pub fn fs_policy(&self) -> FileSystemPolicy {
        match self {
            Self::None | Self::FullAccess => FileSystemPolicy::FullAccess,
            Self::ReadOnly => FileSystemPolicy::ReadOnly,
            Self::WorkspaceWrite { writable_roots, .. } => FileSystemPolicy::WorkspaceWrite {
                roots: writable_roots.clone(),
            },
        }
    }

    /// Get the network component of this policy.
    pub fn net_policy(&self) -> NetworkPolicy {
        match self {
            Self::None | Self::FullAccess => NetworkPolicy::FullAccess,
            Self::ReadOnly => NetworkPolicy::NoAccess,
            Self::WorkspaceWrite {
                allowed_domains, ..
            } => {
                if allowed_domains.is_empty() {
                    NetworkPolicy::NoAccess
                } else {
                    NetworkPolicy::AllowDomains(allowed_domains.clone())
                }
            }
        }
    }

    /// Whether this policy provides any restriction at all.
    pub fn is_restricted(&self) -> bool {
        !matches!(self, Self::None | Self::FullAccess)
    }
}

/// File system access policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FileSystemPolicy {
    /// No write access to any path.
    ReadOnly,
    /// Specific roots are writable; everything else is read-only.
    WorkspaceWrite { roots: Vec<PathBuf> },
    /// Full file system access.
    FullAccess,
}

/// Network access policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// No network access at all.
    NoAccess,
    /// Only specific domains are accessible.
    AllowDomains(Vec<String>),
    /// Full network access.
    FullAccess,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_read_only_is_restricted() {
        assert!(SandboxPolicy::read_only().is_restricted());
    }

    #[test]
    fn policy_none_is_not_restricted() {
        assert!(!SandboxPolicy::None.is_restricted());
    }

    #[test]
    fn policy_workspace_write_is_restricted() {
        let policy = SandboxPolicy::workspace_write(vec!["."], vec![]);
        assert!(policy.is_restricted());
    }

    #[test]
    fn fs_policy_from_read_only() {
        assert_eq!(
            SandboxPolicy::read_only().fs_policy(),
            FileSystemPolicy::ReadOnly
        );
    }

    #[test]
    fn net_policy_from_read_only() {
        assert_eq!(
            SandboxPolicy::read_only().net_policy(),
            NetworkPolicy::NoAccess
        );
    }

    #[test]
    fn net_policy_from_workspace_write_with_domains() {
        let policy = SandboxPolicy::workspace_write(vec!["."], vec!["api.example.com"]);
        assert_eq!(
            policy.net_policy(),
            NetworkPolicy::AllowDomains(vec!["api.example.com".to_string()])
        );
    }

    #[test]
    fn net_policy_from_workspace_write_no_domains() {
        let policy = SandboxPolicy::workspace_write(vec!["."], vec![]);
        assert_eq!(policy.net_policy(), NetworkPolicy::NoAccess);
    }

    #[test]
    fn serialization_roundtrip() {
        let policy = SandboxPolicy::workspace_write(vec!["/tmp"], vec!["example.com"]);
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: SandboxPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, deserialized);
    }

    #[test]
    fn default_is_none() {
        assert_eq!(SandboxPolicy::default(), SandboxPolicy::None);
    }
}
