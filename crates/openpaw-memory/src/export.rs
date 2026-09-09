//! Memory export and import functionality.

use crate::memory::{MemoryFrontmatter, Scope};
use crate::store::{FsMemoryStore, MemoryStore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryExport {
    pub version: String,
    pub exported_at: String,
    pub memories: Vec<MemoryEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub frontmatter: MemoryFrontmatter,
    pub body: String,
    pub scope: Scope,
}

/// Export all active memories in a scope to JSON.
///
/// Uses `list_active()` so superseded memories are excluded — importing the
/// resulting file into another workspace will not resurrect stale records.
pub fn export_memories(store: &FsMemoryStore, scope: &Scope) -> Result<String, String> {
    let all = store.list_active().map_err(|e| e.to_string())?;
    let entries: Vec<MemoryEntry> = all
        .into_iter()
        .filter(|m| &m.scope == scope)
        .map(|m| MemoryEntry {
            frontmatter: m.frontmatter.clone(),
            body: m.body.clone(),
            scope: m.scope,
        })
        .collect();

    let export = MemoryExport {
        version: "1.0".to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        memories: entries,
    };

    serde_json::to_string_pretty(&export).map_err(|e| e.to_string())
}

/// Import memories from JSON.
pub fn import_memories(
    store: &FsMemoryStore,
    json: &str,
    scope: &Scope,
    overwrite: bool,
) -> Result<ImportResult, String> {
    let export: MemoryExport = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut result = ImportResult::default();

    for entry in export.memories {
        let fm = entry.frontmatter;

        // Check existence once — the memory may live in either scope.
        let existing = store.get(&fm.id).map_err(|e| e.to_string())?;

        if existing.is_some() && !overwrite {
            result.skipped += 1;
            continue;
        }

        // If overwriting, update in-place to avoid delete-then-put data loss.
        // The import may change scope (e.g. User → Project), so when the
        // existing memory is in a different scope than the target, we fall
        // back to delete + put.
        if let Some(ref existing_mem) = existing {
            if existing_mem.scope == *scope {
                // Same scope — safe in-place update.
                match store.update_in_place(&fm.id, &fm, &entry.body) {
                    Ok(_) => {
                        result.imported += 1;
                    }
                    Err(e) => {
                        result.errors.push(format!("import update: {e}"));
                    }
                }
                continue;
            }
            // Different scope — delete from old scope, then put to new scope.
            let _ = store.delete(existing_mem.scope, &fm.id);
        }

        match store.put(*scope, fm, &entry.body) {
            Ok(_) => result.imported += 1,
            Err(e) => {
                result.errors.push(format!("import: {e}"));
            }
        }
    }

    Ok(result)
}

#[derive(Debug, Default)]
pub struct ImportResult {
    pub imported: u32,
    pub skipped: u32,
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Confidence, Source};
    use crate::store::FsMemoryStore;

    #[test]
    fn export_import_roundtrip() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        // Create a memory
        let mut fm =
            MemoryFrontmatter::new(Source::User, Confidence::Medium, vec!["test".to_string()]);
        fm.description = "Test memory for export".to_string();
        let id = fm.id.clone();
        store
            .put(Scope::User, fm, "This is a test memory body")
            .unwrap();

        // Export
        let json = export_memories(&store, &Scope::User).unwrap();
        assert!(json.contains("\"version\": \"1.0\""));
        assert!(json.contains(&id));

        // Parse the export
        let parsed: MemoryExport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.memories.len(), 1);
        assert_eq!(parsed.memories[0].frontmatter.id, id);
        assert_eq!(parsed.memories[0].body, "This is a test memory body");
    }

    #[test]
    fn import_into_new_store() {
        let user1 = tempfile::tempdir().unwrap();
        let store1 = FsMemoryStore::new(user1.path().to_path_buf(), None);

        let mut fm =
            MemoryFrontmatter::new(Source::User, Confidence::High, vec!["rust".to_string()]);
        fm.description = "Use Rust for systems".to_string();
        let id = fm.id.clone();
        store1.put(Scope::User, fm, "Use Rust for systems").unwrap();

        let json = export_memories(&store1, &Scope::User).unwrap();

        // Import into a fresh store
        let user2 = tempfile::tempdir().unwrap();
        let store2 = FsMemoryStore::new(user2.path().to_path_buf(), None);

        let result = import_memories(&store2, &json, &Scope::User, false).unwrap();
        assert_eq!(result.imported, 1);
        assert_eq!(result.skipped, 0);
        assert!(result.errors.is_empty());

        let loaded = store2.get(&id).unwrap().unwrap();
        assert_eq!(loaded.frontmatter.description, "Use Rust for systems");
    }

    #[test]
    fn import_skip_existing() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let mut fm = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
        fm.description = "Original".to_string();
        let id = fm.id.clone();
        store.put(Scope::User, fm, "Original body").unwrap();

        // Build an export JSON with the same id
        let export = MemoryExport {
            version: "1.0".to_string(),
            exported_at: chrono::Utc::now().to_rfc3339(),
            memories: vec![MemoryEntry {
                frontmatter: {
                    let mut fm2 = MemoryFrontmatter::new(Source::User, Confidence::Medium, vec![]);
                    fm2.id = id.clone();
                    fm2.description = "Updated".to_string();
                    fm2
                },
                body: "Updated body".to_string(),
                scope: Scope::User,
            }],
        };
        let json = serde_json::to_string_pretty(&export).unwrap();

        let result = import_memories(&store, &json, &Scope::User, false).unwrap();
        assert_eq!(result.skipped, 1);
        assert_eq!(result.imported, 0);

        // Original should be unchanged
        let loaded = store.get(&id).unwrap().unwrap();
        assert_eq!(loaded.frontmatter.description, "Original");
    }

    #[test]
    fn import_malformed_json_reports_error() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);
        let err = import_memories(&store, "not json", &Scope::User, false).unwrap_err();
        assert!(!err.is_empty());
    }
}
