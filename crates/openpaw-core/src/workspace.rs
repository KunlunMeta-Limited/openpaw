//! Workspace home resolution and directory layout.
//!
//! Stability: unstable — the on-disk directory conventions (subdirectory
//! names, zones, plugin layout) are still evolving; the layout is pinned
//! only after the 1.0 storage-layout review.

use std::path::PathBuf;

use crate::Result;

/// Resolve the OpenPaw home directory.
///
/// Precedence: `OPENPAW_HOME` env override, else `~/.openpaw`.
///
/// Stability: unstable — the default location may gain a platform-specific
/// redirect (XDG / Known Folders) before 1.0.
pub fn openpaw_home() -> PathBuf {
    if let Some(home) = std::env::var_os("OPENPAW_HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    let base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(".openpaw")
}

/// Standard subdirectories of the home directory.
///
/// Stability: unstable — new zones/skill layout may add subdirectories;
/// existing names will not be renamed before 1.0.
pub const SUBDIRS: &[&str] = &["sessions", "memories", "skills", "goals", "plugins"];

/// Create the home directory layout if missing. Returns the home path.
///
/// Stability: unstable — tracks [`SUBDIRS`]; see the module-level note.
pub fn init_workspace_dirs() -> Result<PathBuf> {
    let home = openpaw_home();
    for sub in SUBDIRS {
        let dir = home.join(sub);
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
        }
    }
    Ok(home)
}

/// Validate a user-visible workspace name: non-empty, no path separators,
/// no control characters, length cap 64.
///
/// Stability: unstable — the accepted character set may widen once the
/// multi-workspace UX settles.
pub fn validate_workspace_name(name: &str) -> Result<()> {
    const MAX: usize = 64;
    if name.is_empty() {
        return Err(crate::Error::InvalidInput(
            "workspace name must not be empty".into(),
        ));
    }
    if name.len() > MAX {
        return Err(crate::Error::InvalidInput(format!(
            "workspace name too long (max {MAX} bytes)"
        )));
    }
    if name.contains(['/', '\\']) {
        return Err(crate::Error::InvalidInput(
            "workspace name must not contain path separators".into(),
        ));
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '.' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        return Err(crate::Error::InvalidInput(
            "workspace name contains invalid characters".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// Env vars are process-global; these two tests must not race.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn env_override_wins() {
        let _g = env_lock();
        std::env::set_var("OPENPAW_HOME", "/tmp/openpaw-test-home");
        assert_eq!(openpaw_home(), PathBuf::from("/tmp/openpaw-test-home"));
        std::env::remove_var("OPENPAW_HOME");
    }

    #[test]
    fn default_is_dot_openpaw_under_home() {
        let _g = env_lock();
        std::env::remove_var("OPENPAW_HOME");
        let h = openpaw_home();
        assert!(h.ends_with(".openpaw") || h.as_path() == std::path::Path::new("."));
    }

    #[test]
    fn workspace_name_validation() {
        assert!(validate_workspace_name("work").is_ok());
        assert!(validate_workspace_name("项目 A").is_ok());
        assert!(validate_workspace_name("").is_err());
        assert!(validate_workspace_name("a/b").is_err());
        assert!(validate_workspace_name("a\\b").is_err());
        assert!(validate_workspace_name("..").is_err());
        let long = "x".repeat(65);
        assert!(validate_workspace_name(&long).is_err());
    }
}
