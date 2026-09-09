//! Filesystem-backed memory store.
//!
//! Layout under each scope root (flat directory of markdown files):
//! ```text
//! <root>/
//!   2026-05-05-mem_<short>.md
//!   2026-05-05-mem_<short>.md
//!   ...
//! ```
//!
//! Filename convention is `<YYYY-MM-DD>-<id-prefix>.md` for human browsing
//! ordering; the authoritative id lives in YAML frontmatter, so reading
//! tolerates any filename ending in `.md`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use walkdir::WalkDir;

use crate::memory::{Confidence, LoadedMemory, MemoryFrontmatter, Scope, Source};
use openpaw_core::{MemoryEntry, MemoryProvider, MemorySearchQuery, MemorySearchResult};
use openpaw_store::{FrontmatterDoc, FrontmatterError};

#[derive(Debug, Error)]
pub enum MemoryStoreError {
    #[error("memory id already exists: {0:?}")]
    DuplicateId(String),

    #[error("invalid memory id {0:?}")]
    InvalidId(String),

    #[error(transparent)]
    Frontmatter(#[from] FrontmatterError),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("config error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, MemoryStoreError>;

pub trait MemoryStore: Send + Sync {
    /// Every memory on disk, both scopes, in id order. Includes superseded
    /// records for audit / curation flows.
    fn list(&self) -> Result<Vec<LoadedMemory>>;

    /// Memories not transitively superseded.
    fn list_active(&self) -> Result<Vec<LoadedMemory>>;

    /// Active and `pinned == true`.
    fn list_pinned(&self) -> Result<Vec<LoadedMemory>>;

    fn get(&self, id: &str) -> Result<Option<LoadedMemory>>;

    /// Persist a new memory. Refuses if the id already exists in either
    /// scope.
    fn put(&self, scope: Scope, frontmatter: MemoryFrontmatter, body: &str) -> Result<PathBuf>;

    fn delete(&self, scope: Scope, id: &str) -> Result<bool>;

    fn list_by_zone(&self, zone: &str) -> Result<Vec<LoadedMemory>>;

    /// Phase 7: 按看板任务 ID 检索关联记忆。
    fn list_by_task_id(&self, task_id: &str) -> Result<Vec<LoadedMemory>>;

    /// runloop-goal-system: 按目标 ID 检索关联记忆。
    fn list_by_goal_id(&self, goal_id: &str) -> Result<Vec<LoadedMemory>>;

    /// Autonomous goal generation: filter active memories by `MemoryType`.
    /// Default implementation filters `list_active()`; implementations may
    /// override for efficiency.
    fn list_by_type(&self, memory_type: crate::MemoryType) -> Result<Vec<LoadedMemory>> {
        Ok(self
            .list_active()?
            .into_iter()
            .filter(|m| m.frontmatter.memory_type == memory_type)
            .collect())
    }
}

pub struct FsMemoryStore {
    user_root: PathBuf,
    project_root: Option<PathBuf>,
    write_lock: Arc<std::sync::Mutex<()>>,
    /// Optional max memory count to prevent unbounded growth.
    max_memories: Option<usize>,
}

impl Clone for FsMemoryStore {
    fn clone(&self) -> Self {
        Self {
            user_root: self.user_root.clone(),
            project_root: self.project_root.clone(),
            write_lock: self.write_lock.clone(),
            max_memories: self.max_memories,
        }
    }
}

impl FsMemoryStore {
    pub fn new(user_root: PathBuf, project_root: Option<PathBuf>) -> Self {
        Self {
            user_root,
            project_root,
            write_lock: Arc::new(std::sync::Mutex::new(())),
            max_memories: None,
        }
    }

    /// Set max memory count. When set, `put()` will reject new
    /// memories if the active count exceeds this limit.
    pub fn with_max_memories(mut self, max: usize) -> Self {
        self.max_memories = Some(max);
        self
    }

    /// Private helper: delegate to `MemoryStore::list`.
    #[allow(dead_code)] // methods `store_list`, `store_put`, and `store_delete` are never used
    fn store_list(&self) -> Result<Vec<LoadedMemory>> {
        MemoryStore::list(self)
    }

    /// Private helper: delegate to `MemoryStore::list_active`.
    fn store_list_active(&self) -> Result<Vec<LoadedMemory>> {
        MemoryStore::list_active(self)
    }

    /// Private helper: delegate to `MemoryStore::get`.
    fn store_get(&self, id: &str) -> Result<Option<LoadedMemory>> {
        MemoryStore::get(self, id)
    }

    /// Private helper: delegate to `MemoryStore::put`.
    #[allow(dead_code)] // method `store_put` is never used
    fn store_put(
        &self,
        scope: Scope,
        frontmatter: MemoryFrontmatter,
        body: &str,
    ) -> Result<PathBuf> {
        MemoryStore::put(self, scope, frontmatter, body)
    }

    /// Private helper: delegate to `MemoryStore::delete`.
    #[allow(dead_code)] // method `store_delete` is never used
    fn store_delete(&self, scope: Scope, id: &str) -> Result<bool> {
        MemoryStore::delete(self, scope, id)
    }

    /// Standard roots: `~/.openpaw/memories` plus (optional)
    /// `./.openpaw/memories` when the project directory exists.
    pub fn standard() -> Result<Self> {
        let user = standard_user_root()?;
        let project = standard_project_root();
        Ok(Self::new(user, project))
    }

    pub fn search_memories(&self, query: &str, k: usize) -> Result<Vec<(LoadedMemory, f64)>> {
        let active = self.store_list_active()?;
        // Empty query matches everything with a constant score, preserving
        // the MemoryProvider::search "browse by category" semantics.
        if query.trim().is_empty() {
            let mut all: Vec<(LoadedMemory, f64)> = active.into_iter().map(|m| (m, 1.0)).collect();
            all.sort_by(|a, b| a.0.id().cmp(b.0.id()));
            all.truncate(k);
            return Ok(all);
        }
        // Real relevance scoring: TF-IDF cosine similarity with CJK bigram
        // tokenisation (relevance module).
        let scored = crate::relevance::search_memories_scored(&active, query, k);
        let results: Vec<(LoadedMemory, f64)> = scored
            .into_iter()
            .map(|(m, score)| (m.clone(), score))
            .collect();
        Ok(results)
    }

    /// Update only the frontmatter of an existing memory, preserving the body.
    ///
    /// Uses atomic write (write-to-tmp + rename) to avoid corruption on crash.
    ///
    /// If the caller changes `fm.id`, the stored document keeps its old
    /// filename while claiming a new id — use [`update_in_place`] instead in
    /// that case.
    ///
    /// [`update_in_place`]: Self::update_in_place
    pub fn update_frontmatter(&self, id: &str, fm: &MemoryFrontmatter) -> Result<()> {
        let _lock = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let memory = self
            .store_get(id)?
            .ok_or_else(|| MemoryStoreError::Config(format!("memory not found: {id}")))?;
        // Defensive: if the frontmatter carries a different id, the document
        // filename (derived from id) would become inconsistent.  Reject early.
        if fm.id != id {
            return Err(MemoryStoreError::Config(format!(
                "frontmatter id '{}' does not match requested id '{id}'",
                fm.id
            )));
        }
        let doc = FrontmatterDoc {
            frontmatter: fm.clone(),
            body: memory.body.clone(),
        };
        openpaw_store::write_doc_atomic(&memory.source_path, &doc)?;
        Ok(())
    }

    /// Update both frontmatter and body of an existing memory in-place.
    ///
    /// Uses atomic write.  This is the safe alternative to delete-then-put:
    /// no data loss if the write fails.
    pub fn update_in_place(&self, id: &str, fm: &MemoryFrontmatter, body: &str) -> Result<()> {
        let _lock = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let memory = self
            .store_get(id)?
            .ok_or_else(|| MemoryStoreError::Config(format!("memory not found: {id}")))?;
        let doc = FrontmatterDoc {
            frontmatter: fm.clone(),
            body: body.to_string(),
        };
        openpaw_store::write_doc_atomic(&memory.source_path, &doc)?;
        Ok(())
    }

    /// Apply SM-2 review to a memory.
    pub fn review_memory(&self, id: &str, quality: u8) -> Result<()> {
        let memory = self
            .store_get(id)?
            .ok_or_else(|| MemoryStoreError::Config(format!("memory not found: {id}")))?;
        let result = crate::sm2::sm2_calculate(
            memory.frontmatter.sm2_repetition,
            memory.frontmatter.sm2_ease_factor,
            memory.frontmatter.sm2_interval_days,
            quality,
            chrono::Utc::now(),
        );
        let mut fm = memory.frontmatter.clone();
        fm.sm2_repetition = result.repetition;
        fm.sm2_ease_factor = result.ease_factor;
        fm.sm2_interval_days = result.interval_days;
        fm.sm2_next_review = Some(result.next_review_at.format("%Y-%m-%d").to_string());
        self.update_frontmatter(id, &fm)
    }

    /// List memories due for SM-2 review.
    pub fn list_due_for_review(&self, scope: Scope) -> Result<Vec<LoadedMemory>> {
        let today = chrono::Utc::now().date_naive();
        Ok(self
            .store_list_active()?
            .into_iter()
            .filter(|m| m.scope == scope)
            .filter(|m| match &m.frontmatter.sm2_next_review {
                Some(date) => {
                    // Parse to NaiveDate for correct comparison.  Falls back
                    // to string comparison if the date is malformed (defensive).
                    match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
                        Ok(review_date) => review_date <= today,
                        Err(_) => date.as_str() <= today.format("%Y-%m-%d").to_string().as_str(),
                    }
                }
                None => true,
            })
            .collect())
    }

    fn root_for(&self, scope: Scope) -> Option<&Path> {
        match scope {
            Scope::User => Some(&self.user_root),
            Scope::Project => self.project_root.as_deref(),
        }
    }

    fn list_scope(root: &Path, scope: Scope) -> Result<Vec<LoadedMemory>> {
        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in WalkDir::new(root)
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
            match Self::load_one(path, scope) {
                Ok(m) => out.push(m),
                Err(_) => {
                    // Skip malformed memories rather than failing the whole
                    // listing; the offending file stays on disk for audit.
                }
            }
        }
        Ok(out)
    }

    fn load_one(path: &Path, scope: Scope) -> Result<LoadedMemory> {
        let doc: FrontmatterDoc<MemoryFrontmatter> = openpaw_store::read_doc(path)?;
        Ok(LoadedMemory {
            frontmatter: doc.frontmatter,
            body: doc.body,
            source_path: path.to_path_buf(),
            scope,
        })
    }

    fn filename_for(fm: &MemoryFrontmatter) -> String {
        let date = fm.created.format("%Y-%m-%d");
        let short = id_prefix(&fm.id, 12);
        format!("{date}-{short}.md")
    }
}

impl MemoryStore for FsMemoryStore {
    fn list(&self) -> Result<Vec<LoadedMemory>> {
        let mut out = Self::list_scope(&self.user_root, Scope::User)?;
        if let Some(root) = &self.project_root {
            out.extend(Self::list_scope(root, Scope::Project)?);
        }
        out.sort_by(|a, b| a.id().cmp(b.id()));
        Ok(out)
    }

    fn list_active(&self) -> Result<Vec<LoadedMemory>> {
        let all = self.list()?;
        let mut superseded: HashSet<&str> = HashSet::new();
        for m in &all {
            for old in &m.frontmatter.supersedes {
                superseded.insert(old.as_str());
            }
        }
        let superseded_owned: HashSet<String> = superseded.iter().map(|s| s.to_string()).collect();
        Ok(all
            .into_iter()
            .filter(|m| !superseded_owned.contains(m.id()))
            .collect())
    }

    fn list_pinned(&self) -> Result<Vec<LoadedMemory>> {
        Ok(self
            .list_active()?
            .into_iter()
            .filter(|m| m.frontmatter.pinned)
            .collect())
    }

    fn get(&self, id: &str) -> Result<Option<LoadedMemory>> {
        for m in MemoryStore::list(self)? {
            if m.id() == id {
                return Ok(Some(m));
            }
        }
        Ok(None)
    }

    fn put(&self, scope: Scope, frontmatter: MemoryFrontmatter, body: &str) -> Result<PathBuf> {
        validate_id(&frontmatter.id)?;
        // Write-side size check to prevent creating unreadable memories.
        // The read-side limit is 16 MB (MAX_FRONTMATTER_SIZE in frontmatter.rs);
        // a memory larger than that can be written but never read back.
        const MAX_MEMORY_WRITE_SIZE: usize = 16 * 1024 * 1024;
        let estimated_size = body.len() + 4096; // body + frontmatter overhead estimate
        if estimated_size > MAX_MEMORY_WRITE_SIZE {
            return Err(MemoryStoreError::Config(format!(
                "memory body too large: {} bytes (max {} bytes)",
                estimated_size, MAX_MEMORY_WRITE_SIZE
            )));
        }
        let _lock = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Check max_memories limit (if configured).
        if let Some(max) = self.max_memories {
            let active_count = self.list_active()?.len();
            if active_count >= max {
                return Err(MemoryStoreError::Config(format!(
                    "memory limit reached: {} active memories (max {})",
                    active_count, max
                )));
            }
        }
        // NOTE: TOCTOU — concurrent put of the same ID may race past this
        // check.  The duplicate-rejection semantics are best-effort; the
        // atomic write below ensures no corruption, but two concurrent puts
        // of the same ID could both succeed (last writer wins).
        if MemoryStore::get(self, &frontmatter.id)?.is_some() {
            return Err(MemoryStoreError::DuplicateId(frontmatter.id.clone()));
        }
        let root = self.root_for(scope).ok_or_else(|| {
            MemoryStoreError::Config(
                "project scope requested but no project root configured".into(),
            )
        })?;
        let path = root.join(Self::filename_for(&frontmatter));
        let doc = FrontmatterDoc {
            frontmatter,
            body: body.to_string(),
        };
        openpaw_store::write_doc_atomic(&path, &doc)?;
        Ok(path)
    }

    fn delete(&self, scope: Scope, id: &str) -> Result<bool> {
        validate_id(id)?;
        let _lock = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let root = self.root_for(scope).ok_or_else(|| {
            MemoryStoreError::Config(
                "project scope requested but no project root configured".into(),
            )
        })?;
        for entry in WalkDir::new(root)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if let Ok(m) = Self::load_one(path, scope) {
                if m.id() == id {
                    std::fs::remove_file(path).map_err(|source| MemoryStoreError::Io {
                        path: path.to_path_buf(),
                        source,
                    })?;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn list_by_zone(&self, zone: &str) -> Result<Vec<LoadedMemory>> {
        Ok(self
            .list_active()?
            .into_iter()
            .filter(|m| m.frontmatter.zone == zone)
            .collect())
    }

    /// Phase 7: 按看板任务 ID 检索关联记忆。
    fn list_by_task_id(&self, task_id: &str) -> Result<Vec<LoadedMemory>> {
        Ok(self
            .list_active()?
            .into_iter()
            .filter(|m| m.frontmatter.task_id.as_deref() == Some(task_id))
            .collect())
    }

    fn list_by_goal_id(&self, goal_id: &str) -> Result<Vec<LoadedMemory>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|m| m.frontmatter.goal_id.as_deref() == Some(goal_id))
            .collect())
    }
}

/// Bridge `MemoryProvider` (async, key/category model) to `FsMemoryStore`
/// (sync, id/scope model). This lets `FsMemoryStore` be used as a pluggable
/// memory backend.
#[async_trait]
impl MemoryProvider for FsMemoryStore {
    async fn get(&self, key: &str) -> Option<MemoryEntry> {
        MemoryStore::get(self, key)
            .ok()
            .flatten()
            .map(loaded_to_entry)
    }

    async fn set(&self, entry: MemoryEntry) {
        // If an entry with this key already exists, update it in-place.
        // Preserve all existing frontmatter fields that MemoryEntry doesn't
        // carry (pinned, tags, supersedes, sm2_*, memory_type, created, extra,
        // source) — only content and category are updated.
        if let Ok(Some(existing)) = MemoryStore::get(self, &entry.key) {
            let mut fm = existing.frontmatter.clone();
            fm.zone = entry.category.clone(); // category maps to zone
            fm.description = MemoryFrontmatter::derive_description(&entry.content);
            let _ = self.update_in_place(&entry.key, &fm, &entry.content);
            return;
        }

        // New entry: insert into User scope (default for MemoryProvider).
        let fm = entry_to_frontmatter(&entry);
        let _ = MemoryStore::put(self, Scope::User, fm, &entry.content);
    }

    async fn remove(&self, key: &str) -> bool {
        // Try both scopes since MemoryProvider doesn't carry scope info.
        // Even though IDs are unique by contract, a data inconsistency (e.g.
        // manual file copy) could place the same id in both scopes.  We
        // attempt deletion from BOTH scopes and return true if any succeeded.
        let mut removed = false;
        for scope in [Scope::User, Scope::Project] {
            if self.root_for(scope).is_some() {
                if let Ok(true) = MemoryStore::delete(self, scope, key) {
                    removed = true;
                }
            }
        }
        removed
    }

    async fn search(&self, query: &MemorySearchQuery) -> MemorySearchResult {
        let limit = query.limit.unwrap_or(50);
        // Search with a generous limit so we can report the true total after
        // category filtering (matching InMemoryProvider semantics where total
        // is computed before truncation).  Use the actual active memory count
        // as the ceiling instead of a hardcoded constant, so `total` is never
        // artificially capped for large workspaces.
        let active_count = MemoryStore::list_active(self).map(|m| m.len()).unwrap_or(0);
        let search_limit = limit.max(active_count);
        let results = match self.search_memories(&query.query, search_limit) {
            Ok(hits) => hits,
            Err(_) => {
                return MemorySearchResult {
                    entries: vec![],
                    total: 0,
                }
            }
        };

        let mut entries: Vec<MemoryEntry> = results
            .into_iter()
            .filter(|(m, _)| {
                query
                    .category
                    .as_ref()
                    .is_none_or(|cat| &m.frontmatter.zone == cat)
            })
            .map(|(m, _)| loaded_to_entry(m))
            .collect();
        let total = entries.len();
        entries.truncate(limit);
        MemorySearchResult { entries, total }
    }

    async fn list_categories(&self) -> Vec<String> {
        match MemoryStore::list_active(self) {
            Ok(memories) => {
                let mut cats: HashSet<String> = HashSet::new();
                for m in &memories {
                    cats.insert(m.frontmatter.zone.clone());
                }
                cats.into_iter().collect()
            }
            Err(_) => vec![],
        }
    }

    async fn count(&self) -> usize {
        MemoryStore::list_active(self).map(|m| m.len()).unwrap_or(0)
    }
}

fn entry_to_frontmatter(entry: &MemoryEntry) -> MemoryFrontmatter {
    MemoryFrontmatter {
        id: entry.key.clone(),
        created: entry.created_at,
        source: Source::User,
        confidence: Confidence::Medium,
        pinned: false,
        tags: vec![entry.category.clone()],
        supersedes: Vec::new(),
        zone: entry.category.clone(),
        description: MemoryFrontmatter::derive_description(&entry.content),
        memory_type: crate::memory::MemoryType::default(),
        sm2_repetition: 0,
        sm2_ease_factor: 2.5,
        sm2_interval_days: 0,
        sm2_next_review: None,
        task_id: None,
        goal_id: None,
        extra: serde_yml::Mapping::new(),
    }
}

fn loaded_to_entry(m: LoadedMemory) -> MemoryEntry {
    // Use the file's last-modified time as `updated_at` so consumers can tell
    // when a memory was last edited.  Falls back to `created` if the mtime
    // can't be read (e.g. on some filesystems).
    let updated_at = std::fs::metadata(&m.source_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, d.subsec_nanos()))
        .unwrap_or(m.frontmatter.created);
    MemoryEntry {
        key: m.frontmatter.id,
        content: m.body,
        category: m.frontmatter.zone,
        created_at: m.frontmatter.created,
        updated_at,
        metadata: serde_json::Value::Null,
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(MemoryStoreError::InvalidId(id.to_string()));
    }
    Ok(())
}

fn id_prefix(id: &str, n: usize) -> &str {
    &id[..n.min(id.len())]
}

pub fn standard_user_root() -> Result<PathBuf> {
    Ok(openpaw_core::openpaw_home().join("memories"))
}

pub fn standard_project_root() -> Option<PathBuf> {
    let p = PathBuf::from(".openpaw").join("memories");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Confidence, Source};

    fn fm(tags: Vec<&str>) -> MemoryFrontmatter {
        MemoryFrontmatter::new(
            Source::User,
            Confidence::Medium,
            tags.into_iter().map(String::from).collect(),
        )
    }

    #[test]
    fn put_get_list_delete_user_scope() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let m1 = fm(vec!["rust", "convention"]);
        let id1 = m1.id.clone();
        let path = MemoryStore::put(
            &store,
            Scope::User,
            m1,
            "use anyhow not thiserror at app layer\n",
        )
        .unwrap();
        assert!(path.to_string_lossy().ends_with(".md"));

        let loaded = MemoryStore::get(&store, &id1).unwrap().unwrap();
        assert_eq!(loaded.id(), id1);
        assert!(loaded.body.contains("anyhow"));
        assert_eq!(loaded.scope, Scope::User);

        let listed = MemoryStore::list(&store).unwrap();
        assert_eq!(listed.len(), 1);

        assert!(MemoryStore::delete(&store, Scope::User, &id1).unwrap());
        assert!(MemoryStore::get(&store, &id1).unwrap().is_none());
        assert!(!MemoryStore::delete(&store, Scope::User, &id1).unwrap());
    }

    #[test]
    fn duplicate_id_rejected() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let m1 = fm(vec!["x"]);
        let mut m2 = fm(vec!["y"]);
        m2.id = m1.id.clone();

        MemoryStore::put(&store, Scope::User, m1, "body 1").unwrap();
        let err = MemoryStore::put(&store, Scope::User, m2, "body 2").unwrap_err();
        assert!(matches!(err, MemoryStoreError::DuplicateId(_)));
    }

    #[test]
    fn supersedes_filters_active() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let m_old = fm(vec!["rust"]);
        let id_old = m_old.id.clone();
        MemoryStore::put(&store, Scope::User, m_old, "use thiserror everywhere\n").unwrap();

        let mut m_new = fm(vec!["rust"]);
        m_new.supersedes = vec![id_old.clone()];
        let id_new = m_new.id.clone();
        MemoryStore::put(
            &store,
            Scope::User,
            m_new,
            "use anyhow at app layer, thiserror in libs\n",
        )
        .unwrap();

        let active = MemoryStore::list_active(&store).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id(), id_new);

        let all = MemoryStore::list(&store).unwrap();
        assert_eq!(all.len(), 2, "list() must include superseded for audit");
    }

    #[test]
    fn list_pinned_returns_only_pinned_active() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let m_a = fm(vec!["a"]);
        let mut m_b = fm(vec!["b"]);
        m_b.pinned = true;
        let id_b = m_b.id.clone();

        MemoryStore::put(&store, Scope::User, m_a, "ephemeral").unwrap();
        MemoryStore::put(&store, Scope::User, m_b, "always loaded").unwrap();

        let pinned = MemoryStore::list_pinned(&store).unwrap();
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].id(), id_b);
    }

    #[test]
    fn project_scope_overrides_default_path() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(
            user.path().to_path_buf(),
            Some(project.path().to_path_buf()),
        );

        let m_u = fm(vec!["user"]);
        let id_u = m_u.id.clone();
        let m_p = fm(vec!["project"]);
        let id_p = m_p.id.clone();
        MemoryStore::put(&store, Scope::User, m_u, "u body").unwrap();
        MemoryStore::put(&store, Scope::Project, m_p, "p body").unwrap();

        let listed = MemoryStore::list(&store).unwrap();
        assert_eq!(listed.len(), 2);
        let scopes: std::collections::HashSet<Scope> = listed.iter().map(|m| m.scope).collect();
        assert!(scopes.contains(&Scope::User));
        assert!(scopes.contains(&Scope::Project));

        // Round-trip lookup.
        assert_eq!(
            MemoryStore::get(&store, &id_u).unwrap().unwrap().scope,
            Scope::User
        );
        assert_eq!(
            MemoryStore::get(&store, &id_p).unwrap().unwrap().scope,
            Scope::Project
        );
    }

    #[test]
    fn invalid_id_rejected() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let mut bad = fm(vec![]);
        bad.id = "../etc".into();
        let err = MemoryStore::put(&store, Scope::User, bad, "x").unwrap_err();
        assert!(matches!(err, MemoryStoreError::InvalidId(_)));
    }

    #[test]
    fn preserves_unknown_frontmatter_keys() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let mut m = fm(vec!["x"]);
        m.extra.insert(
            serde_yml::Value::String("custom_field".into()),
            serde_yml::Value::String("preserved".into()),
        );
        let id = m.id.clone();
        MemoryStore::put(&store, Scope::User, m, "body").unwrap();

        let loaded = MemoryStore::get(&store, &id).unwrap().unwrap();
        assert_eq!(
            loaded
                .frontmatter
                .extra
                .get(serde_yml::Value::String("custom_field".into())),
            Some(&serde_yml::Value::String("preserved".into()))
        );
    }

    #[test]
    fn empty_root_returns_empty_list() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().join("does-not-exist"), None);
        assert!(MemoryStore::list(&store).unwrap().is_empty());
        assert!(MemoryStore::list_active(&store).unwrap().is_empty());
        assert!(MemoryStore::list_pinned(&store).unwrap().is_empty());
    }

    #[test]
    fn review_memory_updates_sm2_fields() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let m = fm(vec!["rust"]);
        let id = m.id.clone();
        MemoryStore::put(&store, Scope::User, m, "use anyhow for errors\n").unwrap();

        // Initially, sm2 fields should be defaults
        let loaded = MemoryStore::get(&store, &id).unwrap().unwrap();
        assert_eq!(loaded.frontmatter.sm2_repetition, 0);
        assert!((loaded.frontmatter.sm2_ease_factor - 2.5).abs() < 0.01);
        assert!(loaded.frontmatter.sm2_next_review.is_none());

        // Review with quality 4 (good recall)
        store.review_memory(&id, 4).unwrap();

        let updated = MemoryStore::get(&store, &id).unwrap().unwrap();
        assert_eq!(updated.frontmatter.sm2_repetition, 1);
        assert!(updated.frontmatter.sm2_ease_factor > 2.4);
        assert!(updated.frontmatter.sm2_next_review.is_some());
        // Body should be preserved
        assert!(updated.body.contains("anyhow"));
    }

    #[test]
    fn list_due_for_review_includes_unreviewed() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let m = fm(vec!["test"]);
        let id = m.id.clone();
        MemoryStore::put(&store, Scope::User, m, "test body\n").unwrap();

        // Unreviewed memories should be due
        let due = store.list_due_for_review(Scope::User).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id(), id);
    }

    #[test]
    fn list_due_for_review_excludes_future() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let mut m = fm(vec!["test"]);
        // Set next review far in the future
        m.sm2_next_review = Some("2999-12-31".to_string());
        MemoryStore::put(&store, Scope::User, m, "test body\n").unwrap();

        let due = store.list_due_for_review(Scope::User).unwrap();
        assert!(due.is_empty(), "future-dated memory should not be due");
    }

    #[test]
    fn update_frontmatter_preserves_body() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let m = fm(vec!["test"]);
        let id = m.id.clone();
        MemoryStore::put(&store, Scope::User, m, "original body content\n").unwrap();

        let loaded = MemoryStore::get(&store, &id).unwrap().unwrap();
        let mut fm = loaded.frontmatter.clone();
        fm.description = "updated description".to_string();
        store.update_frontmatter(&id, &fm).unwrap();

        let reloaded = MemoryStore::get(&store, &id).unwrap().unwrap();
        assert_eq!(reloaded.frontmatter.description, "updated description");
        assert!(reloaded.body.contains("original body content"));
    }

    // Phase 7: list_by_task_id filters by task_id
    #[test]
    fn list_by_task_id_filters() {
        let user = tempfile::tempdir().unwrap();
        let store = FsMemoryStore::new(user.path().to_path_buf(), None);

        let mut m1 = fm(vec!["rust"]);
        m1.task_id = Some("task-T1".into());
        let mut m2 = fm(vec!["go"]);
        m2.task_id = Some("task-T2".into());
        let m3 = fm(vec!["python"]); // no task_id

        MemoryStore::put(&store, Scope::User, m1, "rust memory\n").unwrap();
        MemoryStore::put(&store, Scope::User, m2, "go memory\n").unwrap();
        MemoryStore::put(&store, Scope::User, m3, "python memory\n").unwrap();

        let t1_memories = store.list_by_task_id("task-T1").unwrap();
        assert_eq!(t1_memories.len(), 1);
        assert!(t1_memories[0].body.contains("rust memory"));

        let t2_memories = store.list_by_task_id("task-T2").unwrap();
        assert_eq!(t2_memories.len(), 1);
        assert!(t2_memories[0].body.contains("go memory"));

        let t3_memories = store.list_by_task_id("task-T3").unwrap();
        assert!(t3_memories.is_empty());
    }
}
