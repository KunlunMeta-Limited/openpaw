//! openpaw-store: plain-file persistence layer.
//!
//! Two responsibilities:
//! 1. Frontmatter documents (`--- yaml ---` + markdown body) — the storage
//!    format for memories, skills, and goal profiles.
//! 2. Session transcripts — append-only JSONL, one fsync per event.
//!
//! The frontmatter codec is a same-source file (S1): byte-format compatible
//! with the upstream store so documents are mutually readable.

/// 功能：导出共享 catalog 能力。
/// 参数：无。
/// 返回：模块接口。
/// 错误：由模块接口说明。
/// 状态：无独立状态。
/// 并发与取消：由模块接口说明。
/// 权限：保持既有边界。
/// 关联：CHG-2026-0023。
pub mod catalog;
pub mod frontmatter;
pub mod global_db;
pub mod session;
pub mod sqlite;
/// 功能：导出共享 transcript 能力。
/// 参数：无。
/// 返回：模块接口。
/// 错误：由模块接口说明。
/// 状态：无独立状态。
/// 并发与取消：由模块接口说明。
/// 权限：保持既有边界。
/// 关联：CHG-2026-0023。
pub mod transcript;

pub use frontmatter::{read_doc, write_doc_atomic, FrontmatterDoc, FrontmatterError};
pub use global_db::{ensure_global_db, global_db, init_global_db, is_global_db_initialized};
pub use session::{SessionError, SessionReader, SessionWriter};
pub use sqlite::{DbError, MessageRecord, OpenPawDb, SearchResult, SessionDb, SessionSummary};

/// 功能：提供共享 journal 能力；边界及错误见模块说明。
pub mod journal;
