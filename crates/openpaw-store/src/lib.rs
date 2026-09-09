//! openpaw-store: plain-file persistence layer.
//!
//! Two responsibilities:
//! 1. Frontmatter documents (`--- yaml ---` + markdown body) — the storage
//!    format for memories, skills, and goal profiles.
//! 2. Session transcripts — append-only JSONL, one fsync per event.
//!
//! The frontmatter codec is a same-source file (S1): byte-format compatible
//! with the upstream store so documents are mutually readable.

pub mod frontmatter;
pub mod global_db;
pub mod session;
pub mod sqlite;

pub use frontmatter::{read_doc, write_doc_atomic, FrontmatterDoc, FrontmatterError};
pub use global_db::{ensure_global_db, global_db, init_global_db, is_global_db_initialized};
pub use session::{SessionError, SessionReader, SessionWriter};
pub use sqlite::{DbError, MessageRecord, OpenPawDb, SearchResult, SessionDb, SessionSummary};
