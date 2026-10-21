//! Workspace path safety: resolve + validate that a path stays inside the
//! workspace root (same-source port; symlink-escape and `..` handling kept).

use std::path::{Path, PathBuf};

use openpaw_core::{Error, Result};

/// Resolve `user_path` against `workspace` and verify the result stays
/// inside the workspace. Non-existent targets canonicalize their longest
/// existing ancestor so intermediate symlink escapes are caught.
pub fn resolve(workspace: &Path, user_path: &str) -> Result<PathBuf> {
    let candidate = if Path::new(user_path).is_absolute() {
        PathBuf::from(user_path)
    } else {
        workspace.join(user_path)
    };

    let normalized = normalize_path(&candidate);

    let ws_canon = dunce_canonicalize(workspace)?;
    let norm_canon = if normalized.exists() {
        dunce_canonicalize(&normalized)?
    } else {
        let parent = normalized
            .parent()
            .ok_or_else(|| Error::ToolHost("path has no parent".into()))?;

        let mut ancestor = parent.to_path_buf();
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("/"));
        }
        let ancestor_canon = dunce_canonicalize(&ancestor)?;

        if !ancestor_canon.starts_with(&ws_canon) {
            return Err(Error::ToolHost(format!(
                "path escapes workspace: {user_path} is outside {}",
                workspace.display()
            )));
        }

        let ancestor_len = ancestor.components().count();
        let remaining: PathBuf = normalized.components().skip(ancestor_len).collect();
        ancestor_canon.join(remaining)
    };

    if !norm_canon.starts_with(&ws_canon) {
        return Err(Error::ToolHost(format!(
            "path escapes workspace: {user_path} resolves to {} which is outside {}",
            norm_canon.display(),
            ws_canon.display()
        )));
    }
    Ok(norm_canon)
}

/// `std::fs::canonicalize` with the Windows `\\?\` prefix stripped so path
/// comparisons against non-UNC forms work.
pub fn dunce_canonicalize(p: &Path) -> Result<PathBuf> {
    let canon = std::fs::canonicalize(p)
        .map_err(|e| Error::ToolHost(format!("cannot canonicalize {}: {e}", p.display())))?;
    #[cfg(windows)]
    {
        let s = canon.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{stripped}")));
        }
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(stripped));
        }
    }
    Ok(canon)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_inside_workspace_ok() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::write(ws.join("a.txt"), "x").unwrap();
        let p = resolve(ws, "a.txt").unwrap();
        assert!(p.starts_with(dunce_canonicalize(ws).unwrap()));
    }

    #[test]
    fn resolve_parent_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve(dir.path(), "../outside.txt").unwrap_err();
        assert!(err.to_string().contains("escapes workspace"));
    }

    #[test]
    fn resolve_nested_missing_target_uses_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::create_dir(ws.join("sub")).unwrap();
        let p = resolve(ws, "sub/new.txt").unwrap();
        assert!(p.starts_with(dunce_canonicalize(ws).unwrap()));
        assert!(p.ends_with("new.txt"));
    }

    #[test]
    fn absolute_path_outside_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let err = resolve(
            dir.path(),
            other.path().join("x.txt").to_string_lossy().as_ref(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("escapes workspace"));
    }
}
