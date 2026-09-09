//! Goal ledger error types.

use std::path::PathBuf;

use thiserror::Error;

/// Goal ledger error type.
///
/// Mirrors the store-layer error style: `#[from]` conversions for the
/// serialization backends and a path-carrying IO variant.
#[derive(Debug, Error)]
pub enum GoalError {
    #[error("goal not found: {0}")]
    NotFound(String),

    #[error("goal already exists: {0}")]
    AlreadyExists(String),

    #[error(transparent)]
    Frontmatter(#[from] openpaw_store::FrontmatterError),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("yaml serialization error: {0}")]
    YamlSerialization(#[from] serde_yml::Error),

    /// Goal id does not match `goal_[0-9a-f]+`. Ids containing traversal
    /// segments, separators, or other characters are rejected before they
    /// can reach any file path construction.
    #[error("invalid goal id (expected goal_[0-9a-f]+): {0}")]
    InvalidId(String),
}

/// Unified result type for the goal ledger crate.
pub type Result<T> = std::result::Result<T, GoalError>;
