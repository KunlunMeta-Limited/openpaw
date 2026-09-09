//! MemoryProvider trait — pluggable memory backends.
//!
//! The default implementation uses in-memory storage with optional
//! MEMORY.md persistence. Third-party backends (Honcho, Mem0, etc.)
//! can implement this trait to provide their own storage.
//!
//! Stability: stable.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One memory record: a key/category-addressed content blob.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub key: String,
    pub content: String,
    pub category: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub metadata: serde_json::Value,
}

impl MemoryEntry {
    pub fn new(
        key: impl Into<String>,
        content: impl Into<String>,
        category: impl Into<String>,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            key: key.into(),
            content: content.into(),
            category: category.into(),
            created_at: now,
            updated_at: now,
            metadata: serde_json::Value::Null,
        }
    }
}

/// Query parameters for [`MemoryProvider::search`].
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchQuery {
    pub query: String,
    pub category: Option<String>,
    pub limit: Option<usize>,
}

/// Search results plus the total match count before any `limit`.
///
/// Stability: stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResult {
    pub entries: Vec<MemoryEntry>,
    pub total: usize,
}

/// Pluggable memory backend. Third parties implement this to swap the
/// storage layer without touching the agent loop.
///
/// Stability: stable.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    async fn get(&self, key: &str) -> Option<MemoryEntry>;
    async fn set(&self, entry: MemoryEntry);
    async fn remove(&self, key: &str) -> bool;
    async fn search(&self, query: &MemorySearchQuery) -> MemorySearchResult;
    async fn list_categories(&self) -> Vec<String>;
    async fn count(&self) -> usize;
}

/// Default in-memory [`MemoryProvider`] (optionally persisted by callers).
///
/// Stability: stable.
pub struct InMemoryProvider {
    entries: std::sync::RwLock<std::collections::HashMap<String, MemoryEntry>>,
}

impl InMemoryProvider {
    pub fn new() -> Self {
        Self {
            entries: std::sync::RwLock::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for InMemoryProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryProvider for InMemoryProvider {
    async fn get(&self, key: &str) -> Option<MemoryEntry> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()
    }

    async fn set(&self, entry: MemoryEntry) {
        self.entries
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(entry.key.clone(), entry);
    }

    async fn remove(&self, key: &str) -> bool {
        self.entries
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key)
            .is_some()
    }

    async fn search(&self, query: &MemorySearchQuery) -> MemorySearchResult {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let query_lower = query.query.to_lowercase();
        let mut results: Vec<MemoryEntry> = entries
            .values()
            .filter(|e| {
                if let Some(ref cat) = query.category {
                    if e.category != *cat {
                        return false;
                    }
                }
                e.content.to_lowercase().contains(&query_lower)
                    || e.key.to_lowercase().contains(&query_lower)
            })
            .cloned()
            .collect();

        let total = results.len();
        if let Some(limit) = query.limit {
            results.truncate(limit);
        }

        MemorySearchResult {
            entries: results,
            total,
        }
    }

    async fn list_categories(&self) -> Vec<String> {
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut cats: std::collections::HashSet<String> = std::collections::HashSet::new();
        for e in entries.values() {
            cats.insert(e.category.clone());
        }
        cats.into_iter().collect()
    }

    async fn count(&self) -> usize {
        self.entries.read().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_and_get() {
        let provider = InMemoryProvider::new();
        let entry = MemoryEntry::new("key1", "hello world", "general");
        provider.set(entry).await;
        let got = provider.get("key1").await.unwrap();
        assert_eq!(got.content, "hello world");
    }

    #[tokio::test]
    async fn remove() {
        let provider = InMemoryProvider::new();
        provider.set(MemoryEntry::new("key1", "val", "cat")).await;
        assert!(provider.remove("key1").await);
        assert!(provider.get("key1").await.is_none());
    }

    #[tokio::test]
    async fn search_by_content() {
        let provider = InMemoryProvider::new();
        provider
            .set(MemoryEntry::new("k1", "Rust programming", "tech"))
            .await;
        provider
            .set(MemoryEntry::new("k2", "Python data science", "tech"))
            .await;
        let result = provider
            .search(&MemorySearchQuery {
                query: "rust".into(),
                category: None,
                limit: None,
            })
            .await;
        assert_eq!(result.entries.len(), 1);
    }

    #[tokio::test]
    async fn search_by_category() {
        let provider = InMemoryProvider::new();
        provider
            .set(MemoryEntry::new("k1", "data", "important"))
            .await;
        provider
            .set(MemoryEntry::new("k2", "info", "general"))
            .await;
        let result = provider
            .search(&MemorySearchQuery {
                query: "".into(),
                category: Some("important".into()),
                limit: None,
            })
            .await;
        assert_eq!(result.entries.len(), 1);
    }

    #[tokio::test]
    async fn count_entries() {
        let provider = InMemoryProvider::new();
        assert_eq!(provider.count().await, 0);
        provider.set(MemoryEntry::new("k1", "v1", "c")).await;
        provider.set(MemoryEntry::new("k2", "v2", "c")).await;
        assert_eq!(provider.count().await, 2);
    }
}
