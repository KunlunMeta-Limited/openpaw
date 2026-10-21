//! Goal store — profile persistence layer.
//!
//! Architecture mirrors the memory store: filesystem backend, frontmatter
//! format, atomic writes, flat directory scan. Differences: a single flat
//! directory (no user/project scopes) and an event ledger.
//!
//! Subset decisions versus the upstream goal store:
//! - no workspace-ref constructor (this crate receives explicit paths);
//! - only an in-process write mutex is kept; the cross-process run-lock
//!   facility is deliberately NOT ported (hard red line).

#[cfg(windows)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use walkdir::WalkDir;

use openpaw_store::{read_doc, write_doc_atomic, FrontmatterDoc};

use crate::error::{GoalError, Result};
use crate::ledger::{GoalEvent, GoalEventLedger};
use crate::models::{GoalFrontmatter, GoalStatus};

/// Goal store trait — synchronous interface in the memory-store style.
pub trait GoalStore: Send + Sync {
    /// Create a new goal. Returns an error if the id already exists.
    fn create_goal(&self, frontmatter: GoalFrontmatter, body: &str) -> Result<PathBuf>;

    /// Load a single goal. Returns None when absent.
    fn load_goal(&self, id: &str) -> Result<Option<FrontmatterDoc<GoalFrontmatter>>>;

    /// Save (overwrite) a goal. Returns the written file path.
    fn save_goal(&self, frontmatter: &GoalFrontmatter, body: &str) -> Result<PathBuf>;

    /// Delete a goal. Returns whether a file was removed.
    fn delete_goal(&self, id: &str) -> Result<bool>;

    /// List all goals, sorted by id.
    fn list_goals(&self) -> Result<Vec<FrontmatterDoc<GoalFrontmatter>>>;

    /// List only active goals (status == Active).
    fn list_active_goals(&self) -> Result<Vec<FrontmatterDoc<GoalFrontmatter>>>;

    /// Update the goal status.
    fn update_status(&self, id: &str, status: GoalStatus) -> Result<()>;

    /// Append an event to the event ledger (append-only).
    fn append_event(&self, goal_id: &str, event: &GoalEvent) -> Result<()>;

    /// Query a goal's event history.
    fn query_events(&self, goal_id: &str) -> Result<Vec<GoalEvent>>;
}

/// Filesystem-backed goal store.
///
/// Directory layout (flat `.md` files):
/// ```text
/// <goals_dir>/
///   2026-08-08-goal_a1b2.md
///   2026-08-08-goal_c3d4.md
///   ...
///   .events/
///     goal_a1b2.jsonl
///     goal_c3d4.jsonl
/// ```
pub struct FsGoalStore {
    /// Single goals directory.
    goals_dir: PathBuf,
    /// In-process write lock.
    write_lock: Arc<std::sync::Mutex<()>>,
}

impl FsGoalStore {
    /// Construct with an explicit directory.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            goals_dir: dir,
            write_lock: Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// 功能：严格只读扫描目标，保留旧列表的容错语义。
    /// 参数：self为显式目标目录的原Store。
    /// 返回：按目标ID排序的原frontmatter文档。
    /// 错误：扫描、读取、解析和根外链接错误不伪装空数据。
    /// 状态：不创建目标目录或.events，不调度目标。
    /// 并发与取消：同步只读；调用方等待读取完成。
    /// 权限：解析前验证每个目标文件仍在显式根内。
    /// 关联：需求/设计REQ-OP-014；DES-OP-006。
    pub fn list_readonly_checked(&self) -> Result<Vec<FrontmatterDoc<GoalFrontmatter>>> {
        let entries = match std::fs::read_dir(&self.goals_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(GoalError::Io {
                    path: self.goals_dir.clone(),
                    source,
                })
            }
        };
        let root = std::fs::canonicalize(&self.goals_dir).map_err(|source| GoalError::Io {
            path: self.goals_dir.clone(),
            source,
        })?;
        let mut docs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| GoalError::Io {
                path: self.goals_dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let target = std::fs::canonicalize(&path).map_err(|source| GoalError::Io {
                path: path.clone(),
                source,
            })?;
            if !target.starts_with(&root) {
                return Err(GoalError::Io {
                    path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "goal path escapes root",
                    ),
                });
            }
            docs.push(read_doc::<GoalFrontmatter>(&target)?);
        }
        docs.sort_by(|a, b| a.frontmatter.id.cmp(&b.frontmatter.id));
        Ok(docs)
    }

    /// File name format: `<YYYY-MM-DD>-<id-prefix>.md`
    fn filename_for(fm: &GoalFrontmatter) -> String {
        let date = fm.created.format("%Y-%m-%d");
        let short = id_prefix(&fm.id, 12);
        format!("{date}-{short}.md")
    }

    /// Find the file path for a goal id.
    fn find_path(&self, id: &str) -> Option<PathBuf> {
        if !self.goals_dir.exists() {
            return None;
        }
        for entry in WalkDir::new(&self.goals_dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if let Ok(doc) = read_doc::<GoalFrontmatter>(path) {
                if doc.frontmatter.id == id {
                    return Some(path.to_path_buf());
                }
            }
        }
        None
    }

    /// Scan all goal files.
    fn list_all(&self) -> Result<Vec<FrontmatterDoc<GoalFrontmatter>>> {
        if !self.goals_dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in WalkDir::new(&self.goals_dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            match read_doc::<GoalFrontmatter>(path) {
                Ok(doc) => out.push(doc),
                Err(_) => {
                    // Malformed documents are skipped; a broken file must not
                    // poison the whole listing (no tracing dependency here).
                }
            }
        }
        out.sort_by(|a, b| a.frontmatter.id.cmp(&b.frontmatter.id));
        Ok(out)
    }

    /// Get the event ledger (lazy).
    fn ledger(&self) -> GoalEventLedger {
        GoalEventLedger::new(&self.goals_dir)
    }

    /// Windows degraded write: fallback when `write_doc_atomic` fails
    /// (target held open by another process).
    #[cfg(windows)]
    fn write_goal_fallback(path: &Path, fm: &GoalFrontmatter, body: &str) -> Result<()> {
        let yaml = serde_yml::to_string(fm)?;
        let yaml = yaml.trim_end_matches('\n');
        let body_clean = body.trim_start_matches('\n');
        let content = format!("---\n{yaml}\n---\n{body_clean}");

        let _ = std::fs::remove_file(path);
        std::fs::write(path, content).map_err(|source| GoalError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }
}

/// Take an id prefix.
///
/// Char-safe slicing: `&id[..n]` panics when `n` lands inside a multi-byte
/// UTF-8 char (a quirky goal id would abort the process instead of
/// returning a shorter prefix). Fall back to the largest valid char
/// boundary <= n.
fn id_prefix(id: &str, n: usize) -> &str {
    let mut end = n.min(id.len());
    while end > 0 && !id.is_char_boundary(end) {
        end -= 1;
    }
    &id[..end]
}

/// Goal id format validation (same semantics as
/// `GoalEventLedger::validate_goal_id`): `goal_[0-9a-f]+`. Injection ids
/// are rejected before any file name or path construction.
pub(crate) fn validate_goal_id(id: &str) -> Result<()> {
    let hex = id
        .strip_prefix("goal_")
        .ok_or_else(|| GoalError::InvalidId(id.to_string()))?;
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(GoalError::InvalidId(id.to_string()));
    }
    Ok(())
}

impl GoalStore for FsGoalStore {
    fn create_goal(&self, frontmatter: GoalFrontmatter, body: &str) -> Result<PathBuf> {
        let _lock = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());

        // Reject malformed ids before the id is used in a filename.
        validate_goal_id(&frontmatter.id)?;

        if self.find_path(&frontmatter.id).is_some() {
            return Err(GoalError::AlreadyExists(frontmatter.id.clone()));
        }

        let path = self.goals_dir.join(Self::filename_for(&frontmatter));
        let doc = FrontmatterDoc {
            frontmatter,
            body: body.to_string(),
        };

        #[cfg(windows)]
        {
            if write_doc_atomic(&path, &doc).is_err() {
                Self::write_goal_fallback(&path, &doc.frontmatter, &doc.body)?;
            } else {
                return Ok(path);
            }
        }

        #[cfg(not(windows))]
        {
            write_doc_atomic(&path, &doc)?;
        }

        Ok(path)
    }

    fn load_goal(&self, id: &str) -> Result<Option<FrontmatterDoc<GoalFrontmatter>>> {
        match self.find_path(id) {
            Some(path) => {
                let doc = read_doc::<GoalFrontmatter>(&path)?;
                Ok(Some(doc))
            }
            None => Ok(None),
        }
    }

    fn save_goal(&self, frontmatter: &GoalFrontmatter, body: &str) -> Result<PathBuf> {
        let _lock = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());

        let path = match self.find_path(&frontmatter.id) {
            Some(existing) => existing,
            None => self.goals_dir.join(Self::filename_for(frontmatter)),
        };

        let doc = FrontmatterDoc {
            frontmatter: frontmatter.clone(),
            body: body.to_string(),
        };

        #[cfg(windows)]
        {
            if write_doc_atomic(&path, &doc).is_err() {
                Self::write_goal_fallback(&path, &doc.frontmatter, &doc.body)?;
            } else {
                return Ok(path);
            }
        }

        #[cfg(not(windows))]
        {
            write_doc_atomic(&path, &doc)?;
        }

        Ok(path)
    }

    fn delete_goal(&self, id: &str) -> Result<bool> {
        let _lock = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());

        match self.find_path(id) {
            Some(path) => {
                std::fs::remove_file(&path).map_err(|source| GoalError::Io {
                    path: path.clone(),
                    source,
                })?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn list_goals(&self) -> Result<Vec<FrontmatterDoc<GoalFrontmatter>>> {
        self.list_all()
    }

    fn list_active_goals(&self) -> Result<Vec<FrontmatterDoc<GoalFrontmatter>>> {
        Ok(self
            .list_all()?
            .into_iter()
            .filter(|doc| doc.frontmatter.status == GoalStatus::Active)
            .collect())
    }

    fn update_status(&self, id: &str, status: GoalStatus) -> Result<()> {
        let _lock = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());

        let path = self
            .find_path(id)
            .ok_or_else(|| GoalError::NotFound(format!("goal not found: {id}")))?;

        let mut doc = read_doc::<GoalFrontmatter>(&path)?;
        doc.frontmatter.status = status;
        if status == GoalStatus::Completed {
            doc.frontmatter.completed_at = Some(chrono::Utc::now());
        }

        write_doc_atomic(&path, &doc)?;
        Ok(())
    }

    fn append_event(&self, _goal_id: &str, event: &GoalEvent) -> Result<()> {
        self.ledger().append(event)
    }

    fn query_events(&self, goal_id: &str) -> Result<Vec<GoalEvent>> {
        self.ledger().query(goal_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_goal(id: &str, objective: &str) -> GoalFrontmatter {
        let mut fm = GoalFrontmatter::new(objective);
        fm.id = id.to_string();
        fm.status = GoalStatus::Active;
        fm
    }

    #[test]
    fn create_load_list_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsGoalStore::new(tmp.path().to_path_buf());

        // Create
        let goal = make_goal("goal_cafe001", "finish the unit tests");
        let path = store.create_goal(goal, "goal body content\n").unwrap();
        assert!(path.to_string_lossy().ends_with(".md"));

        // Load
        let loaded = store.load_goal("goal_cafe001").unwrap().unwrap();
        assert_eq!(loaded.frontmatter.objective, "finish the unit tests");
        assert!(loaded.body.contains("goal body content"));

        // List
        let listed = store.list_goals().unwrap();
        assert_eq!(listed.len(), 1);

        // Delete
        assert!(store.delete_goal("goal_cafe001").unwrap());
        assert!(store.load_goal("goal_cafe001").unwrap().is_none());
        assert!(!store.delete_goal("goal_cafe001").unwrap());
    }

    #[test]
    fn list_active_filters_by_status() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsGoalStore::new(tmp.path().to_path_buf());

        let active = make_goal("goal_ace001", "active goal");
        store.create_goal(active, "active\n").unwrap();

        let mut paused = make_goal("goal_fad002", "paused goal");
        paused.status = GoalStatus::Paused;
        store.create_goal(paused, "paused\n").unwrap();

        let active_list = store.list_active_goals().unwrap();
        assert_eq!(active_list.len(), 1);
        assert_eq!(active_list[0].frontmatter.id, "goal_ace001");
    }

    #[test]
    fn save_goal_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsGoalStore::new(tmp.path().to_path_buf());

        let goal = make_goal("goal_cafe002", "original goal");
        store.create_goal(goal, "original body\n").unwrap();

        // Overwrite
        let mut updated = make_goal("goal_cafe002", "updated goal");
        updated.objective = "updated goal".to_string();
        store.save_goal(&updated, "updated body\n").unwrap();

        let loaded = store.load_goal("goal_cafe002").unwrap().unwrap();
        assert_eq!(loaded.frontmatter.objective, "updated goal");
        assert!(loaded.body.contains("updated body"));
    }

    #[test]
    fn update_status_changes_status() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsGoalStore::new(tmp.path().to_path_buf());

        let goal = make_goal("goal_c0de001", "status test");
        store.create_goal(goal, "body\n").unwrap();

        store
            .update_status("goal_c0de001", GoalStatus::Completed)
            .unwrap();

        let loaded = store.load_goal("goal_c0de001").unwrap().unwrap();
        assert_eq!(loaded.frontmatter.status, GoalStatus::Completed);
        assert!(loaded.frontmatter.completed_at.is_some());
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsGoalStore::new(tmp.path().to_path_buf());
        assert!(store.load_goal("goal_nonexistent").unwrap().is_none());
    }

    #[test]
    fn empty_dir_returns_empty_list() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsGoalStore::new(tmp.path().join("does-not-exist"));
        assert!(store.list_goals().unwrap().is_empty());
        assert!(store.list_active_goals().unwrap().is_empty());
    }

    #[test]
    fn event_append_and_query() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsGoalStore::new(tmp.path().to_path_buf());

        let goal = make_goal("goal_e0e7001", "event test");
        store.create_goal(goal, "body\n").unwrap();

        let event1 = GoalEvent::new(
            "created",
            "goal_e0e7001",
            serde_json::json!({"objective": "event test"}),
        );
        store.append_event("goal_e0e7001", &event1).unwrap();

        let event2 = GoalEvent::new(
            "status_changed",
            "goal_e0e7001",
            serde_json::json!({"from": "active", "to": "paused"}),
        );
        store.append_event("goal_e0e7001", &event2).unwrap();

        let events = store.query_events("goal_e0e7001").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "created");
        assert_eq!(events[1].event_type, "status_changed");

        let empty = store.query_events("goal_deadbeef").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn filename_uses_date_id_pattern() {
        let fm = make_goal("goal_abcdefghij", "filename test");
        let name = FsGoalStore::filename_for(&fm);
        assert!(name.ends_with(".md"));
        assert!(name.contains(&fm.created.format("%Y-%m-%d").to_string()));
        assert!(name.contains("goal_abcdef")); // first 12 chars of the id
    }

    // Goal id validation: direct checks + create_goal injection rejection.

    #[test]
    fn validate_goal_id_accepts_hex_ids() {
        assert!(validate_goal_id("goal_cafe001").is_ok());
        assert!(validate_goal_id("goal_0123456789abcdef0123456789abcdef").is_ok());
    }

    #[test]
    fn validate_goal_id_rejects_traversal_and_paths() {
        assert!(validate_goal_id("../evil").is_err());
        assert!(validate_goal_id("goal_abc/../def").is_err());
        assert!(validate_goal_id("/absolute").is_err());
        assert!(validate_goal_id("goal_\\evil").is_err());
        assert!(validate_goal_id("C:\\evil").is_err());
    }

    #[test]
    fn create_goal_rejects_injection_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FsGoalStore::new(tmp.path().to_path_buf());

        for id in [
            "../evil",
            "/absolute",
            "C:\\evil",
            "goal_",
            "goal_bad/../id",
        ] {
            let goal = make_goal(id, "injection test");
            assert!(
                store.create_goal(goal, "body\n").is_err(),
                "id {id:?} must be rejected"
            );
        }

        // An injected id must not create any file inside goals_dir.
        let created: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            created.is_empty(),
            "no file may be created from an injected id"
        );
    }
}
